#!/usr/bin/env bash
#
# switch-instance-database — point ONE BOSS instance at a fresh database
# in its own postgres, while the converge is held: snapshot the current
# database whole, create the new one, rewrite the Secret that names it,
# and record all of it on the packet. It never rolls a pod.
#
# WHY IT EXISTS (design e652c7c6, David 2026-09-16: "Let's do Option 3";
# backlog 063dba4e)
# ---------------------------------------------------------------------
# Prod restarts from an EMPTY database seeded by the platform bundle and
# the Algedonic tenant; the brewery's database becomes an archive; real
# in-flight packets are brought over afterwards if found missing.
# Measured: prod's database is `boss` in the postgres StatefulSet
# (boss.yaml; POSTGRES_DB=boss, PGDATA on the 20Gi longhorn PVC), and
# every service reads BOSS_POSTGRES_URL from Secret boss-secrets key
# database-url. boss-init provisions the schema when `subject_kinds` is
# absent (infra/oss-quickstart/init.sh, first-start path), then the
# launcher seeds the platform bundle, the operator baseline and the
# tenant. So a fresh instance is: a NEW database in the same postgres,
# and the Secret's database-url naming it. Four kubectl round trips a
# person could type — with nothing recording what the old database
# held, whether the converge was held, or which packet asked. So it is
# an ops verb through the audited door (infra/ops/verbs/
# switch-instance-database.json), and this script is its whole
# mechanism. The reverse is the same verb with the old name.
#
# THE BOUNDS, in the order they are applied — each refuses loudly and
# names the bound; a refusal changes nothing:
#
#   1. THE ARGUMENTS. `--dry-run` or `--for-real`, a namespace of the
#      shape instances.toml admits (`boss` or `boss-<name>`; never
#      boss-dev, the pipeline's), and a database name that is a plain
#      lowercase identifier — so it is never quoted, escaped or split.
#      The allowlist checks these first; the script re-checks rather
#      than relying on one layer (the retire-second-stack convention).
#   2. THE HOLD. The real run refuses unless the converge is HELD
#      (converge-hold.sh's marker, the same file the deploy runner
#      reads). The roll must carry the tenant flip (instances.toml +
#      boss.yaml, landed on main while held) and the new database in
#      ONE converge, or the launcher seeds one tenant into the other's
#      database. The dry run reports the hold's state and goes on, so
#      the plan can be read before the hold is placed.
#   3. THE SECRET. `boss-secrets` key `database-url` in the namespace,
#      parsed in a variable: user, host, port, database, query. Every
#      line printed names user@host:port/db — THE PASSWORD IS NEVER
#      PRINTED, on any path, and a test asserts it. A Secret that
#      cannot be read or parsed is a refusal, never a pass. A host that
#      is not this instance's postgres Service is refused: the snapshot
#      and the create below go through sts/postgres in the namespace,
#      and a URL pointing elsewhere names a database this verb does
#      not reach. A URL already naming the target is refused.
#   4. THE FLIP. boss-init's PGDATABASE WAS a manifest literal in
#      boss.yaml (measured 2026-09-16: `{name: PGDATABASE, value:
#      boss}`) that init.sh's psql read — NOT the Secret — and a
#      release that rolled the old literal against the new Secret would
#      have converged the schema into the OLD database while every
#      service read the NEW, empty one. The sibling car the same day
#      (boss-init reads its database off DATABASE_URL) removed the
#      literal; this bound still reads the ref the release will build
#      (`forgejo/main`, which the held converge fetches on every tick):
#      no literal = derived from the Secret = pass, and a literal that
#      ever returns must name the target;
#      a manifest with no literal at all passes (derived from the
#      Secret). The instance's [section] of instances.toml on that ref
#      is printed beside it, so the packet shows which tenant the
#      release would seed — printed, not judged.
#   5. THE TARGET. On the `postgres` maintenance database: absent is
#      created (`CREATE DATABASE <new> OWNER boss`); present and EMPTY
#      (no table outside pg_catalog/information_schema) is reused;
#      present with tables is refused by count. Every read here opens
#      its session read-only, the census's way.
#   6. THE SNAPSHOT, BEFORE ANY CHANGE. `pg_dump --no-owner` of the
#      current database through the same kubectl-exec door the census
#      uses, gzipped onto the forge at
#      $BACKUP_DIR/<ns>-<db>-<stamp>.sql.gz. A non-zero pg_dump, or a
#      file smaller than the floor (1 MiB — the prod database is GBs),
#      is a refusal and the file is removed: a switch whose old state
#      was not captured is one nobody can reverse with confidence. The
#      dry run rehearses it schema-only to /dev/null, which proves the
#      door and writes nothing.
#
# Then, `--for-real` only: create (bound 5's verdict), PATCH the Secret
# (`kubectl patch secret boss-secrets --type merge -p <json>` — the
# document is built in a variable and never echoed; only the argv the
# kernel sees carries it), READ IT BACK and refuse to claim success
# unless the read-back names the target, and print the record: old and
# new names, the snapshot path, size and sha256, and the NATS JetStream
# stream/consumer positions read through the nats pod's monitoring port
# (8222, `/jsz?consumers=true` via wget inside the container — the
# forge cannot reach the port itself). Positions it cannot read are
# recorded as `unmeasured` with the reason; never faked, and never a
# refusal, because the switch does not depend on them.
#
# WHAT IT DOES NOT DO. It does NOT roll the pod — release-converge does,
# and the packet says so. It deletes nothing: the old database stays in
# postgres as the archive until a separate decision drops it. It does
# not touch instances.toml or boss.yaml — the flip is a car on main,
# read here, never written.
#
# USAGE
#   switch-instance-database.sh --dry-run | --for-real <namespace> <new-db>
#
# EXIT
#   0  done (or, with --dry-run, every bound passed and this is the plan)
#   2  refused — the reason names the bound; nothing changed. A dry run
#      whose real run would be refused exits 2 too, saying which bound.
#   1  failed part-way — the record states what stands (the snapshot,
#      the created database) and that the Secret is unchanged; or a
#      tool this needs could not answer
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_CONVERGE_HOLD              the hold file (converge-hold.sh's default)
#   BOSS_SWITCH_BACKUP_DIR          where the snapshot lands
#                                   (default: /var/backups/boss/instance-db)
#   BOSS_SWITCH_MIN_SNAPSHOT_BYTES  the floor (default: 1048576)
#   BOSS_SWITCH_TREE                the checkout whose fetched main ref is
#                                   read (default: the one this script is in)
#   BOSS_SWITCH_MAIN_REF            the ref the release will build
#                                   (default: forgejo/main)
#   BOSS_KUBECTL / KUBECONFIG       see undeclared-objects.sh; resolved once

set -uo pipefail

ME="switch-instance-database"
say() { echo "$ME: $*" >&2; }
refuse() { say "REFUSED — $*"; say "  Nothing was changed."; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"
# HOLD_FILE, PG_WORKLOAD, PG_CONTAINER, PG_USER: forge-defaults.sh, one
# definition for every verb that reads the hold or the instance database.
. "$SELF_DIR/forge-defaults.sh"
BACKUP_DIR="${BOSS_SWITCH_BACKUP_DIR:-/var/backups/boss/instance-db}"
MIN_BYTES="${BOSS_SWITCH_MIN_SNAPSHOT_BYTES:-1048576}"
TREE="${BOSS_SWITCH_TREE:-$REPO}"
MAIN_REF="${BOSS_SWITCH_MAIN_REF:-forgejo/main}"

# The targets, as infra/cluster/manifests/boss.yaml declares them: the
# StatefulSet `postgres`, container `postgres`, POSTGRES_USER=boss (the
# PG_* trio, from forge-defaults.sh above); the StatefulSet `nats`,
# container `nats`, monitoring on 8222. The namespace is the packet's.
# switch_instance_database_sh.rs holds the equality test between these
# words and the manifest (§9a).
NATS_WORKLOAD="sts/nats"
NATS_CONTAINER="nats"
SECRET_NAME="boss-secrets"
SECRET_KEY="database-url"

# --- bound 1: the arguments -------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real <namespace> <new-db>"
    say "  --dry-run   every bound, the plan, a schema-only rehearsal of the snapshot; changes nothing"
    say "  --for-real  snapshot the current database, create <new-db>, repoint the Secret; needs the converge HELD"
    exit 2
}
[ "$#" -eq 3 ] || usage
DRY=""
case "$1" in
    --dry-run) DRY=1 ;;
    --for-real) DRY=0 ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac
NS="$2"
NEW="$3"
# The namespace shape instances.toml admits; boss-dev is the pipeline's
# and is refused there too.
if ! [[ "$NS" =~ ^boss(-[a-z0-9]+)*$ ]]; then
    refuse "\`$NS\` is not an instance namespace (boss, or boss-<name>)"
fi
[ "$NS" != "boss-dev" ] || refuse "boss-dev is the pipeline's namespace, not an instance's"
# A plain identifier: never quoted, never escaped, never split. This is
# what lets the CREATE below carry the name bare.
if ! [[ "$NEW" =~ ^[a-z][a-z0-9_]{0,62}$ ]]; then
    refuse "\`$NEW\` is not a database name this verb will create: lowercase letters, digits and _ only, leading letter, 63 at most"
fi

command -v jq >/dev/null 2>&1 || { say "jq is not on PATH, so nothing can be read. Nothing was changed."; exit 1; }
command -v gzip >/dev/null 2>&1 || { say "gzip is not on PATH, so the snapshot cannot land. Nothing was changed."; exit 1; }

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# The dry run collects the bounds the real run would refuse on and
# reports them together at the end, so one read of the packet shows
# every problem rather than the first.
WOULD_REFUSE=()
would_refuse() { # <reason>
    if [ "$DRY" = 1 ]; then
        say "would REFUSE — $*"
        WOULD_REFUSE+=("$*")
    else
        refuse "$*"
    fi
}

# --- bound 2: the hold ------------------------------------------------------
# The same file, read the same way the deploy runner reads it
# (cluster-deploy-lib.sh converge_held): present = held, its content the
# reason. Not sourced — the lib carries the whole converge and this
# needs one line of it.
HOLD_REASON=""
if [ -f "$HOLD_FILE" ]; then
    HOLD_REASON=$(cat "$HOLD_FILE")
    say "hold: HELD — $HOLD_REASON ($HOLD_FILE)"
else
    say "hold: NOT held ($HOLD_FILE absent)"
    would_refuse "the converge is not held, so the release could roll before the flip lands and seed one tenant into the other's database. Hold it first: boss ops forge hold-converge --wait -- switch-instance-database"
fi

# --- the kubectl, resolved once, by the derivation the census uses ---------
KUBECTL_LINE=$("$RESOLVE" --kubectl) || {
    say "no kubectl to act with (see above). Nothing was changed."
    exit 1
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"
k() { "${KUBECTL[@]}" -n "$NS" "$@"; }

# --- bound 3: the Secret ----------------------------------------------------
# Parsed into variables and never printed whole. `redact` masks the
# credential in any URL-shaped text; `scrub` removes the literal
# password from anything a tool said back, so even an error message
# that quoted the connection string cannot carry it onto the packet.
redact() { sed -E 's#(://[^:/@]+):[^@]*@#\1:***@#'; }
PG_PASS=""
scrub() { # stdin -> stdout, the literal password replaced
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        if [ -n "$PG_PASS" ]; then line="${line//"$PG_PASS"/***}"; fi
        printf '%s\n' "$line" | redact
    done
}
read_secret_url() { # -> stdout: the decoded URL; stderr: kubectl's words
    k get secret "$SECRET_NAME" -o "jsonpath={.data.$SECRET_KEY}" | base64 -d
}
if ! URL=$(read_secret_url 2> "$TMP/secret.err") || [ -z "$URL" ]; then
    say "REFUSED — cannot read Secret $SECRET_NAME key $SECRET_KEY in $NS:"
    scrub < "$TMP/secret.err" | sed 's/^/    /' >&2
    say "  A Secret that cannot be read is a bound that cannot be evaluated. Nothing was changed."
    exit 2
fi
# postgres://user:pass@host[:port]/db[?query]
SCHEME="${URL%%://*}"
REST="${URL#*://}"
AUTHORITY="${REST%%/*}"
PATHQ="${REST#"$AUTHORITY"}"
PATHQ="${PATHQ#/}"
USERINFO="${AUTHORITY%@*}"
HOSTPORT="${AUTHORITY##*@}"
PG_USER_IN_URL="${USERINFO%%:*}"
PG_PASS="${USERINFO#*:}"
PG_HOST="${HOSTPORT%%:*}"
PG_PORT="${HOSTPORT#*:}"
[ "$PG_PORT" != "$HOSTPORT" ] || PG_PORT=5432
OLD="${PATHQ%%\?*}"
QUERY="${PATHQ#"$OLD"}"
if [ "$REST" = "$URL" ] || [ "$AUTHORITY" = "$HOSTPORT" ] || [ -z "$PG_USER_IN_URL" ] || [ -z "$PG_HOST" ] || [ -z "$OLD" ] || [ "$USERINFO" = "$PG_PASS" ]; then
    refuse "Secret $SECRET_NAME key $SECRET_KEY in $NS is not of the shape postgres://user:pass@host[:port]/db (read as $(printf '%s' "$URL" | redact)), so this verb cannot rewrite it"
fi
SHOWN_OLD="$PG_USER_IN_URL@$PG_HOST:$PG_PORT/$OLD"
SHOWN_NEW="$PG_USER_IN_URL@$PG_HOST:$PG_PORT/$NEW"
say "secret: $SECRET_NAME/$SECRET_KEY in $NS names $SHOWN_OLD"
case "$PG_HOST" in
    postgres|postgres.*) ;;
    *) refuse "the Secret's host is $PG_HOST, not this instance's postgres Service — the snapshot and the create go through $PG_WORKLOAD in $NS, which is not where that URL points" ;;
esac
[ "$PG_USER_IN_URL" = "$PG_USER" ] || say "note: the Secret's user is $PG_USER_IN_URL; the manifest's POSTGRES_USER is $PG_USER — the create will be owned by $PG_USER"
[ "$OLD" != "$NEW" ] || refuse "the Secret already names $NEW ($SHOWN_OLD); there is nothing to switch"

# --- bound 4: the flip, read off the ref the release will build ------------
# The forge checkout is a user's and the ops-runner is root; git refuses
# to READ across that boundary ("dubious ownership"), so every git call
# drops to the directory's owner, read off the directory — never
# hardcoded (delete-orphan-object.sh's as_owner, ops-request c9877f75).
case "$TREE" in
    *[[:space:]\'\"]*) refuse "the tree path \`$TREE\` contains whitespace or a quote; name one without" ;;
esac
[ -d "$TREE" ] || refuse "the tree $TREE does not exist, so the flip cannot be read"
OWNER="$(stat -c %U "$TREE" 2>/dev/null)"
if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ] || ! id -u "$OWNER" >/dev/null 2>&1; then
    refuse "cannot resolve the owner of $TREE (stat says '${OWNER:-}'), so there is no account to read its git as"
fi
OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}
if ! MAIN_SHA=$(as_owner "git -C '$TREE' rev-parse --short '$MAIN_REF'" 2> "$TMP/git.err"); then
    say "REFUSED — cannot read $MAIN_REF in $TREE (as $OWNER); git said:"
    sed 's/^/    /' "$TMP/git.err" >&2
    say "  The held converge fetches forgejo/main on every tick, so this ref is what release-converge would build. Nothing was changed."
    exit 2
fi
if ! as_owner "git -C '$TREE' show '$MAIN_REF:infra/cluster/manifests/boss.yaml'" > "$TMP/boss.yaml" 2> "$TMP/git.err"; then
    say "REFUSED — cannot read infra/cluster/manifests/boss.yaml at $MAIN_REF ($MAIN_SHA); git said:"
    sed 's/^/    /' "$TMP/git.err" >&2
    say "  Nothing was changed."
    exit 2
fi
# Every `{name: PGDATABASE, value: X}` literal in the manifest: none is
# a pass (derived from the Secret); any value other than the target is
# the old-schema-new-services split the hold exists to prevent.
PGDB_LITERALS=$(grep -oE '\{name: PGDATABASE, value: [^}]+\}' "$TMP/boss.yaml" | sed -E 's/.*value: *//; s/ *\}$//' | sort -u)
if [ -z "$PGDB_LITERALS" ]; then
    say "flip: boss-init carries no PGDATABASE literal on $MAIN_REF ($MAIN_SHA) — derived from the Secret"
else
    for v in $PGDB_LITERALS; do
        if [ "$v" = "$NEW" ]; then
            say "flip: boss-init PGDATABASE on $MAIN_REF ($MAIN_SHA) names $NEW"
        else
            would_refuse "boss-init's PGDATABASE literal in infra/cluster/manifests/boss.yaml on $MAIN_REF ($MAIN_SHA) names $v, not $NEW — the release would converge the schema into $v while every service read $NEW. Land the flip (instances.toml + boss.yaml) on main while held, then ask again"
        fi
    done
fi
# The instance's section of instances.toml on the same ref: what the
# release would seed. Printed for the packet, not judged here.
if as_owner "git -C '$TREE' show '$MAIN_REF:infra/cluster/instances.toml'" > "$TMP/instances.toml" 2>/dev/null; then
    section=$(awk -v ns="$NS" '
        /^\[/ { if (want) { print block }; block = $0; want = 0; next }
        /^[a-z_]+ *=/ { block = block "\n" $0; if ($0 ~ ("^namespace *= *\"" ns "\"")) want = 1 }
        END { if (want) print block }' "$TMP/instances.toml")
    if [ -n "$section" ]; then
        say "instance on $MAIN_REF ($MAIN_SHA) — what the release would seed into $NEW:"
        printf '%s\n' "$section" | sed 's/^/    /' >&2
    else
        say "instance: no section of infra/cluster/instances.toml on $MAIN_REF names namespace $NS"
    fi
else
    say "instance: infra/cluster/instances.toml is not on $MAIN_REF"
fi

# --- bound 5: the target ----------------------------------------------------
# Reads open their session read-only in their own -c, the census's way;
# `-At` prints the one value bare. The database name is bound 1's plain
# identifier, so it rides in the SQL bare too.
psql_read() { # <db> <sql> -> stdout
    k exec "$PG_WORKLOAD" -c "$PG_CONTAINER" -- \
        psql -X -q -At -v ON_ERROR_STOP=1 -U "$PG_USER" -d "$1" \
        -c 'SET default_transaction_read_only = on' \
        -c "$2"
}
if ! EXISTS=$(psql_read postgres "-- switch:db_exists
SELECT datname FROM pg_database WHERE datname = '$NEW'" 2> "$TMP/psql.err"); then
    say "REFUSED — cannot ask postgres in $NS whether $NEW exists; psql said:"
    scrub < "$TMP/psql.err" | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
CREATE_NEEDED=1
if [ -n "$EXISTS" ]; then
    if ! TABLES=$(psql_read "$NEW" "-- switch:db_tables
SELECT count(*) FROM pg_tables WHERE schemaname NOT IN ('pg_catalog', 'information_schema')" 2> "$TMP/psql.err"); then
        say "REFUSED — $NEW exists but its tables could not be counted; psql said:"
        scrub < "$TMP/psql.err" | sed 's/^/    /' >&2
        say "  Nothing was changed."
        exit 2
    fi
    case "$TABLES" in
        ''|*[!0-9]*) refuse "the table count for $NEW came back as '$TABLES', not a number" ;;
    esac
    if [ "$TABLES" -gt 0 ]; then
        refuse "$NEW exists and holds $TABLES table(s); this verb creates or reuses an EMPTY database only. Name another, or drop that one by a separate decision"
    fi
    CREATE_NEEDED=0
    say "target: $NEW exists and is empty — would reuse it, no CREATE"
else
    say "target: $NEW is absent — would CREATE DATABASE $NEW OWNER $PG_USER"
fi

# --- the NATS positions, best effort, before anything moves ---------------
# The dispatcher's durable consumers (boss_nats::durable, stream
# BOSS_EVENTS) keep server-side positions; a reader of the packet later
# wants to know where they stood at the switch. The forge cannot reach
# nats.<ns>.svc:8222, so the read is a wget inside the nats container
# (nats:2.10-alpine carries busybox wget). Unreadable = unmeasured,
# with the reason; never a refusal.
NATS_JSON='{"measured": false, "reason": "not read"}'
if k exec "$NATS_WORKLOAD" -c "$NATS_CONTAINER" -- wget -qO- 'http://127.0.0.1:8222/jsz?consumers=true' > "$TMP/jsz" 2> "$TMP/jsz.err" \
    && jq -e . < "$TMP/jsz" > /dev/null 2>&1; then
    NATS_JSON=$(jq -c '{
        measured: true,
        streams: [ (.account_details // [])[] | (.stream_detail // [])[] | {
            name, messages: .state.messages, first_seq: .state.first_seq, last_seq: .state.last_seq,
            consumers: [ (.consumer_detail // [])[] | {
                name, delivered: .delivered.stream_seq, ack_floor: .ack_floor.stream_seq, pending: .num_pending } ] } ] }' "$TMP/jsz")
    say "nats: $(printf '%s' "$NATS_JSON" | jq -r '.streams[] | "\(.name) messages=\(.messages) first=\(.first_seq) last=\(.last_seq)" + ((.consumers // []) | map(" \(.name) delivered=\(.delivered) ack_floor=\(.ack_floor) pending=\(.pending)") | join(";"))' | tr '\n' ' ')"
else
    # awk drains its input; `head -n 1` would exit at the first line and
    # SIGPIPE the scrub under pipefail (backlog 76d04429).
    reason=$(scrub < "$TMP/jsz.err" | awk 'NR == 1')
    [ -n "$reason" ] || reason="no JSON from $NATS_WORKLOAD:8222/jsz"
    NATS_JSON=$(jq -nc --arg r "$reason" '{measured: false, reason: $r}')
    say "nats: unmeasured — $reason"
fi

# --- bound 6: the snapshot, before any change --------------------------------
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DUMP="$BACKUP_DIR/$NS-$OLD-$STAMP.sql.gz"
pg_dump_exec() { # [extra pg_dump args] -> stdout: the dump
    k exec "$PG_WORKLOAD" -c "$PG_CONTAINER" -- \
        pg_dump -U "$PG_USER" -d "$OLD" --no-owner "$@"
}
if [ "$DRY" = 1 ]; then
    if [ -d "$BACKUP_DIR" ]; then
        [ -w "$BACKUP_DIR" ] || would_refuse "$BACKUP_DIR exists but is not writable by $(id -un), so the snapshot could not land there"
    else
        parent="$(dirname "$BACKUP_DIR")"
        while [ ! -d "$parent" ] && [ "$parent" != "/" ]; do parent="$(dirname "$parent")"; done
        [ -w "$parent" ] || would_refuse "$BACKUP_DIR does not exist and $parent is not writable by $(id -un), so the snapshot could not land there"
    fi
    say "rehearsing the snapshot: pg_dump --schema-only of $OLD to /dev/null (writes nothing)"
    if ! pg_dump_exec --schema-only > /dev/null 2> "$TMP/pg.err"; then
        say "the snapshot rehearsal failed against $SHOWN_OLD; pg_dump said:"
        scrub < "$TMP/pg.err" | sed 's/^/    /' >&2
        would_refuse "the snapshot of $OLD cannot be taken through $PG_WORKLOAD in $NS"
    else
        say "would snapshot $OLD to $DUMP (whole database, gzipped; refused under $MIN_BYTES bytes)"
    fi
    echo "would patch Secret $SECRET_NAME key $SECRET_KEY to $SHOWN_NEW (same user, password, host and port)"
    if [ "${#WOULD_REFUSE[@]}" -gt 0 ]; then
        say "DRY RUN — the real run would be REFUSED on ${#WOULD_REFUSE[@]} bound(s):"
        for r in "${WOULD_REFUSE[@]}"; do say "  * $r"; done
        say "  This verb does NOT roll the pod — release-converge does. Nothing was changed."
        exit 2
    fi
    say "DRY RUN — every bound passed: would snapshot $SHOWN_OLD, $( [ "$CREATE_NEEDED" = 1 ] && echo "create $NEW" || echo "reuse the empty $NEW" ), repoint $SECRET_NAME to $SHOWN_NEW. This verb does NOT roll the pod — release-converge does. Nothing was changed."
    exit 0
fi

if ! mkdir -p "$BACKUP_DIR" 2> "$TMP/mk.err"; then
    say "REFUSED — cannot create $BACKUP_DIR for the snapshot:"
    sed 's/^/    /' "$TMP/mk.err" >&2
    say "  Nothing was changed."
    exit 2
fi
say "snapshotting $SHOWN_OLD to $DUMP"
if ! pg_dump_exec 2> "$TMP/pg.err" | gzip -c > "$DUMP"; then
    say "REFUSED — the snapshot of $OLD failed, so nothing was changed; pg_dump said:"
    scrub < "$TMP/pg.err" | sed 's/^/    /' >&2
    rm -f "$DUMP"
    say "  A switch whose old state was not captured is one nobody can reverse with confidence. Nothing was changed."
    exit 2
fi
DUMP_BYTES=$(wc -c < "$DUMP")
if [ "$DUMP_BYTES" -lt "$MIN_BYTES" ]; then
    rm -f "$DUMP"
    refuse "the snapshot of $OLD is $DUMP_BYTES bytes gzipped, smaller than the floor of $MIN_BYTES — the prod database is GBs, so a small dump is a dump of the wrong thing. The file was removed"
fi
DUMP_SHA=$(sha256sum "$DUMP" | cut -c1-64)
echo "snapshot $DUMP ($DUMP_BYTES bytes, sha256 $DUMP_SHA)"

# --- the create --------------------------------------------------------------
CREATED=false
if [ "$CREATE_NEEDED" = 1 ]; then
    if ! k exec "$PG_WORKLOAD" -c "$PG_CONTAINER" -- \
            psql -X -q -v ON_ERROR_STOP=1 -U "$PG_USER" -d postgres \
            -c "-- switch:create_db
CREATE DATABASE $NEW OWNER $PG_USER" > "$TMP/create.out" 2> "$TMP/create.err"; then
        say "FAILED — CREATE DATABASE $NEW OWNER $PG_USER was refused by postgres:"
        scrub < "$TMP/create.err" | sed 's/^/    /' >&2
        say "  the Secret still names $SHOWN_OLD; the snapshot at $DUMP stands. Nothing else was changed."
        exit 1
    fi
    CREATED=true
    echo "created database $NEW (OWNER $PG_USER) in $NS"
else
    echo "reusing the empty database $NEW in $NS"
fi

# --- the patch, and the read-back that is the verdict ----------------------
NEW_URL="$SCHEME://$USERINFO@$HOSTPORT/$NEW$QUERY"
PATCH=$(jq -nc --arg k "$SECRET_KEY" --arg v "$(printf '%s' "$NEW_URL" | base64 -w0)" '{data: {($k): $v}}')
if ! k patch secret "$SECRET_NAME" --type merge -p "$PATCH" > "$TMP/patch.out" 2> "$TMP/patch.err"; then
    say "FAILED — the patch of Secret $SECRET_NAME in $NS was refused:"
    scrub < "$TMP/patch.err" | sed 's/^/    /' >&2
    say "  the Secret still names $SHOWN_OLD; the snapshot at $DUMP stands; CREATE DATABASE $NEW OWNER $PG_USER stands (created: $CREATED). Fix the credential and ask again."
    exit 1
fi
if ! BACK=$(read_secret_url 2> "$TMP/back.err"); then
    say "FAILED — the patch returned success but the Secret could not be read back:"
    scrub < "$TMP/back.err" | sed 's/^/    /' >&2
    say "  what the cluster holds is unverified; the snapshot at $DUMP stands; created: $CREATED."
    exit 1
fi
BACK_DB="${BACK#*://}"; BACK_DB="${BACK_DB#*/}"; BACK_DB="${BACK_DB%%\?*}"
if [ "$BACK" != "$NEW_URL" ]; then
    say "FAILED — the patch returned success and the read-back names $PG_USER_IN_URL@$PG_HOST:$PG_PORT/$BACK_DB, not $SHOWN_NEW."
    say "  the snapshot at $DUMP stands; created: $CREATED. This verb will not try again."
    exit 1
fi

# --- the record --------------------------------------------------------------
jq -nc \
    --arg verb "$ME" --arg ns "$NS" --arg old "$OLD" --arg new "$NEW" \
    --arg shown_old "$SHOWN_OLD" --arg shown_new "$SHOWN_NEW" \
    --arg dump "$DUMP" --argjson bytes "$DUMP_BYTES" --arg sha "$DUMP_SHA" \
    --argjson created "$CREATED" --arg hold "$HOLD_REASON" --arg main "$MAIN_REF@$MAIN_SHA" \
    --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson nats "$NATS_JSON" \
    '{verb: $verb, namespace: $ns, old_db: $old, new_db: $new, secret_before: $shown_old, secret_after: $shown_new,
      snapshot: $dump, snapshot_bytes: $bytes, snapshot_sha256: $sha, created: $created,
      hold: $hold, main: $main, switched_at: $at, nats: $nats}'
say "OK — Secret $SECRET_NAME in $NS now names $SHOWN_NEW (read back); snapshot of $OLD at $DUMP ($DUMP_BYTES bytes); the old database $OLD stays in postgres as the archive until a separate decision drops it."
say "  This verb does NOT roll the pod — release-converge does: boss ops forge release-converge --wait. The reverse is this verb with $OLD as the name."
exit 0
