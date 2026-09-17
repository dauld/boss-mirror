#!/usr/bin/env bash
#
# sweep-archive-branches — delete the forge branches whose cars landed
# BEFORE the database switch, on the evidence the ARCHIVE database
# holds, by exactly the rule the arriving train's sweep applies — and
# record what went, what was kept, and why.
#
# WHY IT EXISTS (backlog dfd83788, measured 2026-09-17 06:30Z on the pod)
# ---------------------------------------------------------------------
# `boss orient` listed 50 forge heads no packet claims. The arrival
# sweep deletes a landed car's branch on the JOB RECORD's evidence
# (crates/orchestrators/boss-cli/src/train.rs: `deletable_branches`
# proves the content landed — a closed car with outcome `merged` —
# and `sweep_guard` proves the branch still holds only that content —
# its head on the forge equals the head recorded at boarding). Every
# car that landed before the 2026-09-16 23:55Z switch
# (switch-instance-database.sh) lives in the ARCHIVE database, not in
# the system of record the sweep reads, so the sweep will never see
# them and those branches are permanent by construction. `boss merged`
# over all 50 could judge only 9 by content (train PRs squash-merge, so
# ancestry never proves a landing); the evidence that decides them is
# the archive's job records, readable through the kubectl-exec psql
# door tenant-census.sh already uses. So: one bounded verb, the
# arrival sweep's own rule, applied to the archive's records.
#
# THE RULE, which is the sweep's and is not loosened here. A branch
# comes off iff
#   - the archive holds a `ship-a-change` job with status `closed` and
#     metadata.outcome `merged` that NAMES it: metadata.branch with
#     metadata.boarded_head, or an entry of metadata.rerail_origins
#     with its branch and head (train.rs `recorded_branches` — the
#     rerail original is the second half, packet 473fda1b);
#   - AND the branch's current head on the forge EQUALS the recorded
#     head (train.rs `sweep_guard`: car 23923b40 is what the head guard
#     costs when it is missing — two commits pushed after boarding,
#     deleted on a job record that was entirely correct).
# Everything else is KEPT and NAMED: a head that differs is `moved`
# (the commits live nowhere else); a claim with no recorded head is
# `no head` (an unknown head is not evidence); a forge head no
# archive car claims is `unrecorded` — an abandoned attempt, a branch
# never filed as a car, or a LIVE car in the system of record, which
# this verb does not read: the archive decides what the archive can
# decide, and the rest is a human's decision, listed for one. A claim
# whose branch is not on the forge is `gone`: counted, nothing to do,
# no line (train.rs `sweep_note`, job 1bd1fb3d).
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
#   2. THE ARCHIVE IS NOT THE LIVE DATABASE. Secret `boss-secrets` key
#      `database-url` in the namespace is parsed in a variable (the
#      switch verb's parsing, copied); every line printed names
#      user@host:port/db — THE PASSWORD IS NEVER PRINTED, on any path,
#      and a test asserts it. The database that Secret names is the
#      live system of record, and the LIVE sweep owns it: asking to
#      sweep from it is refused. A Secret that cannot be read or
#      parsed is a refusal, never a pass; a host that is not the
#      instance's postgres Service is refused, because the read below
#      goes through sts/postgres in the namespace.
#   3. THE RECORD. ONE psql round trip (json_agg) through the same
#      kubectl-exec door the census uses, the session set read-only in
#      its own -c before the query — the archive is never written.
#      An archive that cannot be read is a refusal.
#   4. THE FORGE. The heads are read with `git ls-remote --heads
#      forgejo` in the converged checkout — the remote the converge
#      itself fetches through (cluster-deploy-lib.sh, tenant_source_
#      check). That remote's URL may carry a token in its userinfo, so
#      THE URL IS NEVER PRINTED and every git message is redacted
#      before it reaches the packet (the lib's redact_url shape). A
#      forge that cannot be read is a refusal. The checkout is a
#      user's and the ops-runner is root, so git runs as the
#      directory's owner (delete-orphan-object.sh's as_owner).
#   5. THE NAME. A branch name is data the forge handed back; only a
#      name of the shape `[A-Za-z0-9][A-Za-z0-9._/-]*` is ever handed
#      to git, and always as the full ref `refs/heads/<name>` so it can
#      never read as an option. A deletable branch whose name is
#      outside that shape is kept and named `unsafe name`.
#
# Then, `--for-real` only: `git push forgejo --delete refs/heads/<b>`
# for each planned branch, each one READ BACK with ls-remote — a forge
# answer is not a forge effect — and recorded as `deleted` (absent on
# read-back) or `failed` (the push refused, or the ref still there).
# The dry run prints the same plan and pushes nothing.
#
# OUTPUT ORDER IS LOAD-BEARING (backlog 5323f3ef: the ops-runner keeps
# the first 100 KB of a verb's output, OPS_OUTPUT_CAP). The VERDICT
# LINE PRINTS FIRST — `sweep-archive-branches: <mode> archive=<db>
# recorded=<n> planned=<n> deleted=<n> moved=<n> unrecorded=<n>
# gone=<n>` — then the ONE JSON record line on stdout, then the
# per-branch lines, so a cut listing never costs the verdict. In a dry
# run `deleted` is 0 by construction and `planned` is the count the
# real run would delete.
#
# USAGE
#   sweep-archive-branches.sh --dry-run | --for-real <namespace> <archive-db>
#
# EXIT
#   0  done (or, with --dry-run, the plan)
#   2  refused — the reason names the bound; nothing changed
#   1  a real run in which a deletion failed or could not be read
#      back — the record states each branch's outcome
#
# ENV (test seams — the ops-runner passes no packet-supplied environment,
# only an argv built from the allowlist, so a packet cannot set these)
#   BOSS_SWEEP_TREE            the checkout whose `forgejo` remote is
#                              the forge (default: the one this script
#                              is in)
#   BOSS_KUBECTL / KUBECONFIG  see undeclared-objects.sh; resolved once

set -uo pipefail

ME="sweep-archive-branches"
say() { echo "$ME: $*" >&2; }
# What the reads found (the live database, the archive's count, the
# forge's count) is HELD until the verdict has printed, so the verdict
# is the packet's first line; a refusal prints the notes first, because
# there the notes are the diagnosis.
NOTES=()
note() { NOTES+=("$*"); }
flush_notes() { local n; for n in "${NOTES[@]+"${NOTES[@]}"}"; do say "$n"; done; NOTES=(); }
refuse() { flush_notes; say "REFUSED — $*"; say "  Nothing was changed."; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"
TREE="${BOSS_SWEEP_TREE:-$REPO}"
REMOTE="forgejo"

# The target, as infra/cluster/manifests/boss.yaml declares it: the
# StatefulSet `postgres`, container `postgres`, POSTGRES_USER=boss; the
# namespace is the packet's. The cars are `ship-a-change` packets
# (infra/platform/workflows/ship-a-change.toml), the kind the sweep reads.
PG_WORKLOAD="sts/postgres"
PG_CONTAINER="postgres"
PG_USER="boss"
SECRET_NAME="boss-secrets"
SECRET_KEY="database-url"
CAR_KIND="ship-a-change"

# --- bound 1: the arguments -------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real <namespace> <archive-db>"
    say "  --dry-run   read the archive and the forge, print the plan; pushes nothing"
    say "  --for-real  delete the planned branches on the forge, read each back"
    exit 2
}
[ "$#" -eq 3 ] || usage
DRY=""
case "$1" in
    --dry-run) DRY=1 ;;
    --for-real) DRY=0 ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac
MODE="$1"
NS="$2"
ARCHIVE="$3"
if ! [[ "$NS" =~ ^boss(-[a-z0-9]+)*$ ]]; then
    refuse "\`$NS\` is not an instance namespace (boss, or boss-<name>)"
fi
[ "$NS" != "boss-dev" ] || refuse "boss-dev is the pipeline's namespace, not an instance's"
if ! [[ "$ARCHIVE" =~ ^[a-z][a-z0-9_]{0,62}$ ]]; then
    refuse "\`$ARCHIVE\` is not a database name this verb will read: lowercase letters, digits and _ only, leading letter, 63 at most"
fi

command -v jq >/dev/null 2>&1 || { say "jq is not on PATH, so nothing can be read. Nothing was changed."; exit 1; }
command -v git >/dev/null 2>&1 || { say "git is not on PATH, so the forge cannot be read. Nothing was changed."; exit 1; }

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# --- the kubectl, resolved once, by the derivation the census uses ---------
KUBECTL_LINE=$("$RESOLVE" --kubectl) || {
    say "no kubectl to read with (see above). Nothing was changed."
    exit 1
}
read -r -a KUBECTL <<<"$KUBECTL_LINE"
k() { "${KUBECTL[@]}" -n "$NS" "$@"; }

# --- bound 2: the archive is not the live database -------------------------
# The Secret, parsed into variables and never printed whole. `redact`
# masks the credential in any URL-shaped text (the converge lib's
# redact_url shape, so a forge remote's token is covered too); `scrub`
# removes the literal password from anything a tool said back.
redact() { sed -E 's#://[^/@[:space:]]+@#://<redacted>@#g'; }
PG_PASS=""
scrub() { # stdin -> stdout, the literal password replaced, URLs redacted
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
    flush_notes
    say "REFUSED — cannot read Secret $SECRET_NAME key $SECRET_KEY in $NS:"
    scrub < "$TMP/secret.err" | sed 's/^/    /' >&2
    say "  The live database is the bound this verb must not cross, and it cannot be read. Nothing was changed."
    exit 2
fi
# postgres://user:pass@host[:port]/db[?query]
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
LIVE="${PATHQ%%\?*}"
if [ "$REST" = "$URL" ] || [ "$AUTHORITY" = "$HOSTPORT" ] || [ -z "$PG_USER_IN_URL" ] || [ -z "$PG_HOST" ] || [ -z "$LIVE" ] || [ "$USERINFO" = "$PG_PASS" ]; then
    refuse "Secret $SECRET_NAME key $SECRET_KEY in $NS is not of the shape postgres://user:pass@host[:port]/db (read as $(printf '%s' "$URL" | redact)), so the live database cannot be named"
fi
SHOWN_LIVE="$PG_USER_IN_URL@$PG_HOST:$PG_PORT/$LIVE"
note "live: $SECRET_NAME/$SECRET_KEY in $NS names $SHOWN_LIVE"
case "$PG_HOST" in
    postgres|postgres.*) ;;
    *) refuse "the Secret's host is $PG_HOST, not this instance's postgres Service — the archive is read through $PG_WORKLOAD in $NS, which is not where that URL points" ;;
esac
[ "$ARCHIVE" != "$LIVE" ] || refuse "$ARCHIVE is the database the Secret names ($SHOWN_LIVE) — the live system of record, whose branches the arriving train's own sweep owns. This verb reads an ARCHIVE"

# --- bound 3: the record, one read -----------------------------------------
# The rows the sweep's rule needs and nothing else: every closed,
# merged ship-a-change car's id, branch, boarded head and rerail
# origins, oldest first (the order `decided_branches` lets a first
# claim win in). Read-only in its own -c, the census's way; `-At`
# prints the one json value bare. The database name is bound 1's plain
# identifier and the kind is a literal, so the SQL carries no quoting.
SQL="-- $ME:merged_cars
SELECT coalesce(json_agg(json_build_object(
    'id', id,
    'branch', metadata->>'branch',
    'boarded_head', metadata->>'boarded_head',
    'rerail_origins', coalesce(metadata->'rerail_origins', '[]'::jsonb)
) ORDER BY created_at, id), '[]'::json)
FROM jobs
WHERE kind = '$CAR_KIND' AND status = 'closed' AND metadata->>'outcome' = 'merged'"
if ! k exec "$PG_WORKLOAD" -c "$PG_CONTAINER" -- \
        psql -X -q -At -v ON_ERROR_STOP=1 -U "$PG_USER" -d "$ARCHIVE" \
        -c 'SET default_transaction_read_only = on' \
        -c "$SQL" > "$TMP/cars.out" 2> "$TMP/psql.err"; then
    flush_notes
    say "REFUSED — cannot read the archive $ARCHIVE through $PG_WORKLOAD in $NS; psql said:"
    scrub < "$TMP/psql.err" | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
if ! jq -c 'if type == "array" then . else error("not an array") end' < "$TMP/cars.out" > "$TMP/cars.json" 2> "$TMP/jq.err"; then
    flush_notes
    say "REFUSED — the archive $ARCHIVE answered, but not with one JSON array:"
    sed 's/^/    /' "$TMP/jq.err" >&2
    head -c 400 "$TMP/cars.out" | scrub | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
CARS_READ=$(jq 'length' "$TMP/cars.json")
note "archive: $ARCHIVE holds $CARS_READ closed, merged $CAR_KIND car(s)"

# --- bound 4: the forge ----------------------------------------------------
case "$TREE" in
    *[[:space:]\'\"]*) refuse "the tree path \`$TREE\` contains whitespace or a quote; name one without" ;;
esac
[ -d "$TREE" ] || refuse "the tree $TREE does not exist, so the forge cannot be read"
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
# GIT_TERMINAL_PROMPT=0: a unit has no terminal, and a prompt would
# hang, not fail. The remote's URL is never asked for and never shown.
if ! as_owner "GIT_TERMINAL_PROMPT=0 git -C '$TREE' ls-remote --heads '$REMOTE'" > "$TMP/heads.tsv" 2> "$TMP/git.err"; then
    flush_notes
    say "REFUSED — cannot read the forge's heads through remote $REMOTE of $TREE (as $OWNER); git said:"
    scrub < "$TMP/git.err" | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
HEADS_JSON=$(jq -R -n '[inputs | select(length > 0) | split("\t") | select(length == 2 and (.[1] | startswith("refs/heads/"))) | {key: (.[1] | sub("^refs/heads/"; "")), value: .[0]}] | from_entries' < "$TMP/heads.tsv")
note "forge: $(printf '%s' "$HEADS_JSON" | jq 'length') head(s) on remote $REMOTE"

# --- the decision, pure -----------------------------------------------------
# train.rs `recorded_branches`, then `decided_branches`, then
# `sweep_guard`, in jq: per car, its own branch first (with its boarded
# head) then each rerail origin oldest first (with its head); `main`,
# unnamed and duplicate branches drop out; across cars the FIRST claim
# wins; then the head guard against the forge's answer. Bound 5 — the
# name shape — is judged here too, so an unsafe name never reaches a
# git argv. `unclaimed` is every forge head no claim names, `main`
# excepted.
jq -c --argjson heads "$HEADS_JSON" '
    def nonempty: if . == null or . == "" then null else . end;
    def safe_name: test("^[A-Za-z0-9][A-Za-z0-9._/-]*$") and (contains("..") | not);
    ([ .[] | . as $c
        | ([{branch: (.branch // ""), head: (.boarded_head | nonempty), rerail_origin: false}]
           + [ (.rerail_origins // [])[] | select(type == "object")
               | {branch: (.branch // ""), head: (.head | nonempty), rerail_origin: true} ])
        | reduce .[] as $b ([]; if $b.branch == "" or $b.branch == "main" or (map(.branch) | index($b.branch)) != null then . else . + [$b] end)
        | .[] | . + {car: ($c.id // "")} | select(.car != "")
     ]
     | reduce .[] as $b ([]; if (map(.branch) | index($b.branch)) != null then . else . + [$b] end)
     | map(. + {current: ($heads[.branch] // null)}
           | .verdict = (if .current == null then "gone"
                         elif .head == null then "no_head"
                         elif .head != .current then "moved"
                         elif (.branch | safe_name | not) then "unsafe"
                         else "delete" end))
    ) as $claims
    | { claims: $claims,
        unclaimed: ([ $heads | to_entries[] | select(.key != "main") | select(.key as $b | $claims | map(.branch) | index($b) | not)
                      | {branch: .key, current: .value} ] | sort_by(.branch)) }
' "$TMP/cars.json" > "$TMP/plan.json"

# --- the deletes, --for-real only --------------------------------------------
# Each planned branch: push the delete as the full ref, then READ IT
# BACK — absent is `deleted`, anything else is `failed` with the reason.
# Outcomes land in a TSV the record is built from; nothing is printed
# until the verdict line can carry the counts.
: > "$TMP/done.tsv"
if [ "$DRY" = 0 ]; then
    while IFS=$'\t' read -r branch car head; do
        [ -n "$branch" ] || continue
        if ! as_owner "GIT_TERMINAL_PROMPT=0 git -C '$TREE' push '$REMOTE' --delete 'refs/heads/$branch'" > "$TMP/push.out" 2> "$TMP/push.err"; then
            reason=$(scrub < "$TMP/push.err" | grep -v '^\s*$' | awk 'NR == 1')
            printf '%s\t%s\t%s\tfailed\t%s\n' "$branch" "$car" "$head" "push refused: ${reason:-no message}" >> "$TMP/done.tsv"
            continue
        fi
        if ! as_owner "GIT_TERMINAL_PROMPT=0 git -C '$TREE' ls-remote --heads '$REMOTE' 'refs/heads/$branch'" > "$TMP/back.out" 2> "$TMP/back.err"; then
            reason=$(scrub < "$TMP/back.err" | grep -v '^\s*$' | awk 'NR == 1')
            printf '%s\t%s\t%s\tfailed\t%s\n' "$branch" "$car" "$head" "push answered but the read-back failed: ${reason:-no message}" >> "$TMP/done.tsv"
            continue
        fi
        if [ -s "$TMP/back.out" ]; then
            printf '%s\t%s\t%s\tfailed\t%s\n' "$branch" "$car" "$head" "push answered but the ref is still on the forge at $(awk -F'\t' 'NR == 1 { print substr($1, 1, 40) }' "$TMP/back.out")" >> "$TMP/done.tsv"
        else
            printf '%s\t%s\t%s\tdeleted\tabsent\n' "$branch" "$car" "$head" >> "$TMP/done.tsv"
        fi
    done < <(jq -r '.claims[] | select(.verdict == "delete") | [.branch, .car, .head] | @tsv' "$TMP/plan.json")
fi

# --- the record, assembled ---------------------------------------------------
RECORD=$(jq -c -n -R --rawfile done "$TMP/done.tsv" --slurpfile plan "$TMP/plan.json" \
    --arg verb "$ME" --argjson dry "$( [ "$DRY" = 1 ] && echo true || echo false )" \
    --arg ns "$NS" --arg archive "$ARCHIVE" --arg live "$SHOWN_LIVE" --arg remote "$REMOTE" \
    --argjson cars "$CARS_READ" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
    ($plan[0]) as $p
    | ([ $done | split("\n")[] | select(length > 0) | split("\t")
         | {branch: .[0], car: .[1], head: .[2], outcome: .[3], read_back: .[4]} ]) as $d
    | ($p.claims | map(select(.verdict == "delete"))) as $planned
    | { verb: $verb, dry_run: $dry, namespace: $ns, archive_db: $archive, live_db: $live, remote: $remote,
        cars_read: $cars,
        recorded: ($p.claims | length),
        planned: ($planned | length),
        deleted: ($d | map(select(.outcome == "deleted")) | length),
        failed: ($d | map(select(.outcome == "failed")) | length),
        moved: ($p.claims | map(select(.verdict == "moved")) | length),
        unrecorded: (($p.claims | map(select(.verdict == "no_head")) | length) + ($p.unclaimed | length)),
        gone: ($p.claims | map(select(.verdict == "gone")) | length),
        unsafe: ($p.claims | map(select(.verdict == "unsafe")) | length),
        branches: {
          delete: [ $planned[] | {branch, car, head, rerail_origin} ],
          deleted: [ $d[] | select(.outcome == "deleted") | {branch, car, head, read_back} ],
          failed: [ $d[] | select(.outcome == "failed") | {branch, car, head, reason: .read_back} ],
          moved: [ $p.claims[] | select(.verdict == "moved") | {branch, car, recorded: .head, current, rerail_origin} ],
          no_head: [ $p.claims[] | select(.verdict == "no_head") | {branch, car, current, rerail_origin} ],
          unclaimed: [ $p.unclaimed[] | {branch, current} ],
          gone: [ $p.claims[] | select(.verdict == "gone") | {branch, car, head, rerail_origin} ],
          unsafe: [ $p.claims[] | select(.verdict == "unsafe") | {branch, car, head} ] },
        at: $at }')
n() { printf '%s' "$RECORD" | jq -r ".$1"; }

# --- the verdict FIRST, then the record, then the lines ----------------------
VERDICT="$MODE archive=$ARCHIVE recorded=$(n recorded) planned=$(n planned) deleted=$(n deleted) moved=$(n moved) unrecorded=$(n unrecorded) gone=$(n gone)"
[ "$(n unsafe)" = 0 ] || VERDICT="$VERDICT unsafe=$(n unsafe)"
[ "$(n failed)" = 0 ] || VERDICT="$VERDICT failed=$(n failed)"
say "$VERDICT"
printf '%s\n' "$RECORD"
flush_notes

id8() { printf '%s' "$1" | cut -c1-8; }
if [ "$DRY" = 1 ]; then
    printf '%s' "$RECORD" | jq -r '.branches.delete[] | "\(.branch)\t\(.car)\t\(.head)\t\(.rerail_origin)"' | while IFS=$'\t' read -r b c h o; do
        [ "$o" = true ] && kind="rerail original" || kind="branch"
        say "would delete $b ($kind of car $(id8 "$c"), at $(id8 "$h") = boarded)"
    done
else
    printf '%s' "$RECORD" | jq -r '.branches.deleted[] | "\(.branch)\t\(.car)\t\(.head)"' | while IFS=$'\t' read -r b c h; do
        say "deleted $b (car $(id8 "$c"), was at $(id8 "$h") = boarded; read back: absent)"
    done
    printf '%s' "$RECORD" | jq -r '.branches.failed[] | "\(.branch)\t\(.car)\t\(.reason)"' | while IFS=$'\t' read -r b c r; do
        say "FAILED $b (car $(id8 "$c")) — $r"
    done
fi
printf '%s' "$RECORD" | jq -r '.branches.moved[] | "\(.branch)\t\(.car)\t\(.recorded)\t\(.current)"' | while IFS=$'\t' read -r b c r cur; do
    say "moved $b (car $(id8 "$c") boarded $(id8 "$r"), the forge now holds $(id8 "$cur")) — kept: the commits after boarding live nowhere else"
done
printf '%s' "$RECORD" | jq -r '.branches.no_head[] | "\(.branch)\t\(.car)\t\(.current)"' | while IFS=$'\t' read -r b c cur; do
    say "no head $b (car $(id8 "$c") landed, recorded no head; the forge holds $(id8 "$cur")) — kept: an unknown head is not evidence"
done
printf '%s' "$RECORD" | jq -r '.branches.unsafe[] | "\(.branch)\t\(.car)"' | while IFS=$'\t' read -r b c; do
    say "unsafe name $b (car $(id8 "$c")) — kept: the name is outside the shape this verb hands to git"
done
printf '%s' "$RECORD" | jq -r '.branches.unclaimed[] | "\(.branch)\t\(.current)"' | while IFS=$'\t' read -r b cur; do
    say "unrecorded $b (at $(id8 "$cur")) — kept: no closed, merged $CAR_KIND car in $ARCHIVE names it; an abandoned attempt, a branch never filed, or a live car — a human's decision"
done

if [ "$DRY" = 1 ]; then
    say "DRY RUN — would delete $(n planned) of $(n recorded) recorded branch(es) from remote $REMOTE; $(n moved) moved, $(n unrecorded) unrecorded, $(n gone) gone. Nothing was changed."
    exit 0
fi
if [ "$(n failed)" != 0 ]; then
    say "FAILED — deleted $(n deleted) of $(n planned) planned branch(es); $(n failed) could not be deleted or read back (named above). The rest stand as recorded."
    exit 1
fi
say "OK — deleted $(n deleted) of $(n planned) planned branch(es) from remote $REMOTE, each read back absent; $(n moved) moved and $(n unrecorded) unrecorded kept and named above."
exit 0
