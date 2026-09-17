#!/usr/bin/env bash
#
# retire-example-reference-rows — MUTATING (--for-real), bounded:
# evict the example tenants' migration-seeded reference rows from ONE
# instance's database, where nothing references them, in one
# transaction per table, with the record on the packet
# (infra/forge/retire-example-reference-rows.sh <mode> <namespace>).
#
# WHY IT EXISTS (backlog 718ac982; design e2580840 car 3, folding
# 83a873e8; the eviction shape from the cutover, e652c7c6)
# -----------------------------------------------------------------
# 01-registries.sql and 40-ledger.sql seed the two worked examples'
# reference data on every instance — the used-device shop's 26 roles
# and ten departments, the brewery's location kinds, account types,
# equipment categories and two production sites, the brewery-shaped
# starter chart, a companies row each. Measured on prod on
# 2026-09-17: "Brewery Taproom" in /api/locations of the company's
# own instance. Applied migrations are history (migrate.sh refuses a
# changed checksum), so on an instance that has already booted the
# rows leave through this verb — never through a migration, which
# would delete without a packet, and never by hand.
#
# WHAT IS A CANDIDATE — READ, NEVER TYPED. The set is
# infra/postgres/example-reference-rows.sh's, derived from the
# example tenants' own seeds in this checkout (examples/*/seeds/
# classes.*, locations.toml, chart_of_accounts.toml, tenant.toml) —
# the same derivation init.sh runs on a fresh instance's first boot,
# so the two doors cannot disagree about what is an example row.
#
# THE BOUNDS, in order, each a refusal that changes nothing:
#   1. the arguments — `--dry-run` or `--for-real`, a namespace of
#      the shape instances.toml admits; boss-dev refused;
#   2. the instance is NOT an example — infra/cluster/instances.toml
#      in this checkout must name the namespace with a `tenant_repo`
#      (a company's own tenant). An image-sourced instance
#      (`tenant_dir = "examples/…"`, the playground) is refused: its
#      engine's prepare does not republish locations or the chart,
#      so evicting its rows would break it;
#   3. the database is read off Secret boss-secrets key database-url
#      (parsed in a variable, THE PASSWORD IS NEVER PRINTED — the
#      switch-instance-database shape; a test asserts it), and the
#      host must be the instance's postgres Service;
#   4. DELETABLE ONLY WHEN UNREFERENCED: the plan is one read-only
#      psql (SET default_transaction_read_only = on) exec'd in the
#      postgres container, judging every candidate — an employee
#      wearing the role, a location wearing the kind, an account of
#      the type, a journal line on the account, a tax kind naming it,
#      a job about the location or company, … — and a referenced row
#      is KEPT and NAMED with its reasons; the real run re-judges
#      inside each table's own transaction, so a reference that
#      appeared between plan and run is honoured.
#
# OUTPUT ORDER IS LOAD-BEARING (backlog 5323f3ef: the ops-runner
# keeps the first 100 KB). The VERDICT LINE PRINTS FIRST —
#   retire-example-reference-rows: <dry-run|for-real> namespace=<ns>
#   db=<db> candidates=<n> present=<n> deletable=<n> kept=<n> deleted=<n>
# — then the ONE JSON record line on stdout (the plan, and for a real
# run each table's deleted keys and the read-back), then the per-row
# lines (kept rows with their reasons), so a cut listing never costs
# the verdict. A dry run's `deleted` is 0 by construction and
# `deletable` is what the real run would delete.
#
# USAGE
#   retire-example-reference-rows.sh --dry-run | --for-real <namespace>
#
# EXIT
#   0  done (or, with --dry-run, the plan)
#   2  refused — the reason names the bound; nothing changed
#   1  cannot answer (no kubectl, the plan query failed), or a real
#      run in which a table's transaction failed — the record states
#      which tables completed; the failed one rolled back whole
#
# ENV (test seams — the ops-runner passes no packet-supplied
# environment, only an argv built from the allowlist)
#   BOSS_RETIRE_TREE      the checkout whose examples/ and
#                         infra/cluster/instances.toml are read
#                         (default: the one this script is in)
#   BOSS_KUBECTL / KUBECONFIG  see undeclared-objects.sh; resolved once

set -uo pipefail

ME="retire-example-reference-rows"
say() { echo "$ME: $*" >&2; }
# Findings are HELD until the verdict has printed, so the verdict is
# the packet's first line; a refusal prints them first — there they
# are the diagnosis.
NOTES=()
note() { NOTES+=("$*"); }
flush_notes() { local n; for n in "${NOTES[@]+"${NOTES[@]}"}"; do say "$n"; done; NOTES=(); }
refuse() { flush_notes; say "REFUSED — $*"; say "  Nothing was changed."; exit 2; }
cannot() { flush_notes; say "$*"; say "  Nothing was changed."; exit 1; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF_DIR/../.." && pwd)"
TREE="${BOSS_RETIRE_TREE:-$REPO}"
RESOLVE="$REPO/infra/cluster/undeclared-objects.sh"
DERIVE="$REPO/infra/postgres/example-reference-rows.sh"
INSTANCES="$TREE/infra/cluster/instances.toml"

# The target, as infra/cluster/manifests/boss.yaml declares it: the
# StatefulSet `postgres`, container `postgres`, POSTGRES_USER=boss; the
# namespace is the packet's; the database is the Secret's.
PG_WORKLOAD="sts/postgres"
PG_CONTAINER="postgres"
PG_USER="boss"
SECRET_NAME="boss-secrets"
SECRET_KEY="database-url"

# --- bound 1: the arguments -------------------------------------------------
usage() {
    say "usage: $ME --dry-run | --for-real <namespace>"
    say "  --dry-run   the plan: every candidate judged, nothing deleted"
    say "  --for-real  delete the unreferenced candidates, one transaction per table"
    exit 2
}
[ "$#" -eq 2 ] || usage
DRY=""
case "$1" in
    --dry-run) DRY=1; MODE=dry-run ;;
    --for-real) DRY=0; MODE=for-real ;;
    *) say "the only modes are --dry-run and --for-real, not \`$1\`"; usage ;;
esac
NS="$2"
if ! [[ "$NS" =~ ^boss(-[a-z0-9]+)*$ ]]; then
    refuse "\`$NS\` is not an instance namespace (boss, or boss-<name>)"
fi
[ "$NS" != "boss-dev" ] || refuse "boss-dev is the pipeline's namespace, not an instance's"

command -v jq >/dev/null 2>&1 || cannot "jq is not on PATH, so nothing can be read."
[ -x "$DERIVE" ] || cannot "$DERIVE is missing or not executable — the candidate set has no derivation."

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

# --- bound 2: the instance is a company's, not an example's ---------------
[ -f "$INSTANCES" ] || refuse "no infra/cluster/instances.toml at $TREE, so the instance's tenant source cannot be read"
SECTION=$(awk -v ns="$NS" '
    /^\[/ { if (want) { print block }; block = $0; want = 0; next }
    /^[a-z_]+ *=/ { block = block "\n" $0; if ($0 ~ ("^namespace *= *\"" ns "\"")) want = 1 }
    END { if (want) print block }' "$INSTANCES")
[ -n "$SECTION" ] || refuse "no section of infra/cluster/instances.toml names namespace $NS"
TENANT_REPO=$(printf '%s\n' "$SECTION" | sed -n 's/^tenant_repo *= *"\([^"]*\)".*/\1/p' | head -1)
TENANT_DIR=$(printf '%s\n' "$SECTION" | sed -n 's/^tenant_dir *= *"\([^"]*\)".*/\1/p' | head -1)
if [ -z "$TENANT_REPO" ]; then
    refuse "instance $NS is image-sourced (tenant_dir = \"${TENANT_DIR:-?}\") — an example tenant's own rows are not residue there, and its engine's prepare does not republish locations or the chart. Only a tenant_repo instance is retired"
fi
note "instance: $NS runs tenant_repo $TENANT_REPO — the example rows are residue there"

# --- the candidate set, from this checkout's examples ----------------------
SEEDS=$(BOSS_EXAMPLES_DIR="$TREE/examples" "$DERIVE" seeds 2> "$TMP/seeds.err") || {
    flush_notes; say "cannot derive the candidate set from $TREE/examples:"; sed 's/^/    /' "$TMP/seeds.err" >&2; say "  Nothing was changed."; exit 1; }
PLAN_SQL=$(BOSS_EXAMPLES_DIR="$TREE/examples" "$DERIVE" plan-sql) || cannot "the plan SQL could not be derived (see above)."
note "candidates: $(printf '%s' "$SEEDS" | jq -r '"\(.classes | length) classes, \(.locations | length) locations, \(.gl_accounts | length) accounts, \(.companies | length) companies, from \(.sources | join(", "))"')"

# --- the kubectl, resolved once, by the derivation the census uses ---------
KUBECTL_LINE=$("$RESOLVE" --kubectl) || cannot "no kubectl to act with (see above)."
read -r -a KUBECTL <<<"$KUBECTL_LINE"
k() { "${KUBECTL[@]}" -n "$NS" "$@"; }

# --- bound 3: the Secret names the database ---------------------------------
redact() { sed -E 's#(://[^:/@]+):[^@]*@#\1:***@#'; }
PG_PASS=""
scrub() { # stdin -> stdout, the literal password replaced
    local line
    while IFS= read -r line || [ -n "$line" ]; do
        if [ -n "$PG_PASS" ]; then line="${line//"$PG_PASS"/***}"; fi
        printf '%s\n' "$line" | redact
    done
}
if ! URL=$(k get secret "$SECRET_NAME" -o "jsonpath={.data.$SECRET_KEY}" 2> "$TMP/secret.err" | base64 -d) || [ -z "$URL" ]; then
    flush_notes
    say "REFUSED — cannot read Secret $SECRET_NAME key $SECRET_KEY in $NS:"
    scrub < "$TMP/secret.err" | sed 's/^/    /' >&2
    say "  Nothing was changed."
    exit 2
fi
REST="${URL#*://}"
AUTHORITY="${REST%%/*}"
PATHQ="${REST#"$AUTHORITY"}"; PATHQ="${PATHQ#/}"
USERINFO="${AUTHORITY%@*}"
HOSTPORT="${AUTHORITY##*@}"
PG_USER_IN_URL="${USERINFO%%:*}"
PG_PASS="${USERINFO#*:}"
PG_HOST="${HOSTPORT%%:*}"
DB="${PATHQ%%\?*}"
if [ "$REST" = "$URL" ] || [ "$AUTHORITY" = "$HOSTPORT" ] || [ -z "$PG_USER_IN_URL" ] || [ -z "$PG_HOST" ] || [ -z "$DB" ] || [ "$USERINFO" = "$PG_PASS" ]; then
    refuse "Secret $SECRET_NAME key $SECRET_KEY in $NS is not of the shape postgres://user:pass@host[:port]/db (read as $(printf '%s' "$URL" | redact))"
fi
case "$PG_HOST" in
    postgres|postgres.*) ;;
    *) refuse "the Secret's host is $PG_HOST, not this instance's postgres Service — the reads and the deletes go through $PG_WORKLOAD in $NS, which is not where that URL points" ;;
esac
note "database: $PG_USER_IN_URL@$PG_HOST/$DB (from $SECRET_NAME/$SECRET_KEY)"

# --- the plan: one read-only psql, SQL on stdin ---------------------------
# stdin, not -c: the eviction is several transactions and -c prints only
# the last statement's result. `-i` carries stdin through kubectl exec.
psql_in() { # <sql on stdin> -> stdout
    k exec -i "$PG_WORKLOAD" -c "$PG_CONTAINER" -- \
        psql -X -q -At -v ON_ERROR_STOP=1 -U "$PG_USER" -d "$DB"
}
read_plan() { # -> $TMP/plan.json
    { echo "SET default_transaction_read_only = on;"; printf '%s\n' "$PLAN_SQL"; } \
        | psql_in > "$TMP/plan.out" 2> "$TMP/plan.err" || return 1
    jq -c . < "$TMP/plan.out" > "$TMP/plan.json" 2> "$TMP/plan.jq" || return 1
}
if ! read_plan; then
    flush_notes
    say "cannot judge the candidates — the plan query failed; psql said:"
    scrub < "$TMP/plan.err" | sed 's/^/    /' >&2
    [ -s "$TMP/plan.jq" ] && sed 's/^/    /' "$TMP/plan.jq" >&2
    say "  Nothing was changed."
    exit 1
fi
PLAN=$(cat "$TMP/plan.json")
sum() { printf '%s' "$PLAN" | jq -r "[.[] | $1] | add"; }
CANDIDATES=$(sum '.candidates')
PRESENT=$(sum '.present')
DELETABLE=$(sum '(.deletable | length)')
KEPT=$(sum '(.kept | length)')

# --- the run (--for-real): one transaction per table --------------------------
DELETED=0
DELETED_JSON='[]'
READBACK='null'
RC=0
if [ "$DRY" = 0 ]; then
    DELETE_SQL=$(BOSS_EXAMPLES_DIR="$TREE/examples" "$DERIVE" delete-sql) || cannot "the delete SQL could not be derived (see above)."
    if printf '%s\n' "$DELETE_SQL" | psql_in > "$TMP/delete.out" 2> "$TMP/delete.err"; then
        :
    else
        RC=1
        note "a table's transaction FAILED and rolled back whole; the tables recorded as deleted below completed before it. psql said:"
        while IFS= read -r line; do note "    $line"; done < <(scrub < "$TMP/delete.err")
    fi
    # Each completed table printed one JSON line.
    if [ -s "$TMP/delete.out" ]; then
        DELETED_JSON=$(jq -c -s '.' "$TMP/delete.out" 2>/dev/null) || { RC=1; DELETED_JSON='[]'; note "the run's output was not JSON lines:"; while IFS= read -r line; do note "    $line"; done < "$TMP/delete.out"; }
    fi
    DELETED=$(printf '%s' "$DELETED_JSON" | jq -r '[.[] | (.deleted | length)] | add // 0')
    # The read-back is the verdict: the same plan, after.
    if read_plan; then
        READBACK=$(cat "$TMP/plan.json")
        left=$(printf '%s' "$READBACK" | jq -r '[.[] | (.deletable | length)] | add')
        [ "$left" = 0 ] || { RC=1; note "read-back: $left candidate(s) still deletable after the run — a table's transaction did not complete"; }
    else
        RC=1
        note "read-back FAILED — the plan query did not answer after the run; psql said:"
        while IFS= read -r line; do note "    $line"; done < <(scrub < "$TMP/plan.err")
    fi
fi

RECORD=$(jq -n -c \
    --arg verb "$ME" --arg mode "$MODE" --arg ns "$NS" --arg db "$DB" --arg tenant_repo "$TENANT_REPO" \
    --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson candidates "$CANDIDATES" --argjson present "$PRESENT" --argjson deletable "$DELETABLE" \
    --argjson kept "$KEPT" --argjson deleted "$DELETED" \
    --argjson plan "$PLAN" --argjson runs "$DELETED_JSON" --argjson readback "$READBACK" \
    --argjson seeds "$SEEDS" \
    '{verb: $verb, mode: $mode, namespace: $ns, database: $db, tenant_repo: $tenant_repo,
      candidates: $candidates, present: $present, deletable: $deletable, kept: $kept, deleted: $deleted,
      plan: $plan, deleted_by_table: $runs, read_back: $readback,
      sources: $seeds.sources, at: $at}')

# --- the verdict FIRST, then the record, then the lines ----------------------
VERDICT="$MODE namespace=$NS db=$DB candidates=$CANDIDATES present=$PRESENT deletable=$DELETABLE kept=$KEPT deleted=$DELETED"
[ "$RC" = 0 ] || VERDICT="$VERDICT FAILED"
say "$VERDICT"
printf '%s\n' "$RECORD"
flush_notes
printf '%s' "$PLAN" | jq -r 'to_entries[] | .key as $t | .value.kept[] | "kept \($t) \(.key): \(.reasons | join(", "))"' | while IFS= read -r l; do say "$l"; done
printf '%s' "$PLAN" | jq -r 'to_entries[] | "\(.key): \(.value.present) of \(.value.candidates) candidates present, \(.value.deletable | length) deletable, \(.value.kept | length) kept"' | while IFS= read -r l; do say "$l"; done
if [ "$DRY" = 0 ]; then
    printf '%s' "$DELETED_JSON" | jq -r '.[] | "deleted \(.table): \(.deleted | join(" "))"' | while IFS= read -r l; do say "$l"; done
fi
exit "$RC"
