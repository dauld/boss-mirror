#!/usr/bin/env bash
# A migration written after 2026-09-08 carries a fourteen-digit UTC
# prefix (`YYYYMMDDHHMMSS-`), not a twelve-digit one.
#
# THE CONVENTION. `20260908234904-migration-prefixes-carry-seconds.sql`
# grew the stamp from minutes to seconds after two builders collided on
# `202609082130` (backlog bc7cac00) and stated the rule: take the stamp
# with `date -u +%Y%m%d%H%M%S` when you write the file. Nothing checked
# it. Six migrations since have been stamped to the minute anyway — by
# hand, from memory of the older convention — and by 2026-09-12 that
# had two costs, both measured:
#
#   - THE APPLY ORDER IS NOT THE CALENDAR. The order is a numeric sort
#     on the prefix (migrate.sh, boss-testing's build.rs), and any
#     twelve-digit stamp is smaller than every fourteen-digit one, so
#     `202609121800-…` (written 2026-09-12) applies BEFORE
#     `20260908234904-…` (written 2026-09-08). Harmless so far because
#     none of the six depended on a later seconds-stamped file; the day
#     one does, a fresh database applies them backwards.
#   - A DATE CUTOVER MISREAD IT. `no-migration-writes-a-dispatcher-rule`
#     compares prefixes against `20260911000000`; the minute-width
#     `202609122000-a-sweep-arrives-with-its-measurement.sql` read as
#     2.0e11 < 2.0e13 — "applied history" — and the INSERT that lint
#     exists to refuse gated green. It is fixed to compare at one width;
#     this lint removes the mixed width that made the compare wrong.
#
# WHAT IT REFUSES. Any `infra/postgres/schema/*.sql` whose prefix is
# twelve digits and later than the seconds migration's own minute, and
# is not one of the six named below. Those six are applied history —
# checksum-guarded on every live database (the 2026-08-13 outage is
# what renaming an applied file does) — so they keep their names and
# this set is closed: adding to it is the wrong fix for a new file,
# which is renamed with a fresh `date -u +%Y%m%d%H%M%S`.
#
# Usage:
#   infra/lint/a-migration-prefix-carries-seconds.sh
#   infra/lint/a-migration-prefix-carries-seconds.sh --tree DIR
#
# Exit 0 = clean, 1 = a minute-width prefix later than the convention.
set -uo pipefail

NAME="a-migration-prefix-carries-seconds"
SCHEMA_REL="infra/postgres/schema"
# The seconds migration's own minute: a twelve-digit stamp above this
# was written after the convention it ignores.
SECONDS_SINCE=202609082349
# Applied history, stamped to the minute after the convention. Closed.
APPLIED_MINUTE_WIDTH=(
    202609090025-a-flush-job-records-who-worked-it.sql
    202609101200-the-flush-pipeline-is-deleted.sql
    202609101900-the-corpus-index-is-deleted.sql
    202609112240-cp-2-does-not-pin-the-dev-pod.sql
    202609120300-a-node-declares-its-roles.sql
    202609121800-the-forge-declares-cluster-operator.sql
)

check_dir() { # <dir>  — prints offenders, returns 1 if any
    local dir="$1" found=0 base prefix
    while IFS= read -r base; do
        [ -n "$base" ] || continue
        prefix="${base%%-*}"
        case "$prefix" in ''|*[!0-9]*) continue ;; esac
        [ "${#prefix}" -eq 12 ] || continue
        [ "$((10#$prefix))" -gt "$SECONDS_SINCE" ] || continue
        case " ${APPLIED_MINUTE_WIDTH[*]} " in *" $base "*) continue ;; esac
        found=1
        echo "$NAME: $dir/$base is stamped to the minute, after the convention moved to seconds" >&2
    done <<LIST
$(find "$dir" -maxdepth 1 -name '*.sql' -type f -printf '%f\n' 2>/dev/null | LC_ALL=C sort)
LIST
    return $found
}

# ---------------------------------------------------------------------------
# Self-test — fixtures in a temp directory, never in infra/lint/.
# ---------------------------------------------------------------------------
tmp="$(mktemp -d)" || exit 1
trap 'rm -rf "$tmp"' EXIT
: > "$tmp/41-legacy.sql"
: > "$tmp/202608241600-a-minute-stamp-before-the-convention.sql"
: > "$tmp/20260908234904-migration-prefixes-carry-seconds.sql"
: > "$tmp/20260912200000-a-seconds-stamp.sql"
: > "$tmp/202609121800-the-forge-declares-cluster-operator.sql"
if ! check_dir "$tmp" 2>/dev/null; then
    echo "$NAME: SELF-TEST FAILED — legacy numbers, pre-convention minute stamps, seconds stamps and the applied six must pass" >&2
    exit 1
fi
: > "$tmp/202609122000-a-new-minute-stamp.sql"
if check_dir "$tmp" 2>/dev/null; then
    echo "$NAME: SELF-TEST FAILED — a minute-width stamp after the convention must be refused" >&2
    exit 1
fi
rm -f "$tmp"/*.sql

# ---------------------------------------------------------------------------
# The tree.
# ---------------------------------------------------------------------------
tree="$(cd "$(dirname "$0")/../.." && pwd)"
if [ "${1:-}" = "--tree" ]; then tree="${2:?--tree needs a directory}"; fi
dir="$tree/$SCHEMA_REL"
[ -d "$dir" ] || { echo "$NAME: $dir not found — a wrong path, not a clean tree" >&2; exit 1; }
n=$(find "$dir" -maxdepth 1 -name '[0-9]*-*.sql' -type f | wc -l | tr -d ' ')
if [ "$n" -lt 10 ]; then
    echo "$NAME: only $n prefixed migrations in $dir — the scrape broke, so a green result would mean nothing" >&2
    exit 1
fi
for base in "${APPLIED_MINUTE_WIDTH[@]}"; do
    [ -f "$dir/$base" ] || { echo "$NAME: $base is named as applied history but is not in $dir — the closed set is wrong" >&2; exit 1; }
done
if ! check_dir "$dir"; then
    echo "" >&2
    echo "  Rename it before it applies anywhere: a fresh stamp from" >&2
    echo "  \`date -u +%Y%m%d%H%M%S\` is later than every prefix in this tree, so" >&2
    echo "  the apply order is preserved. Once a live database has applied a" >&2
    echo "  file its name is checksum-guarded history and this set closes over it." >&2
    exit 1
fi
echo "$NAME: clean — $n migrations; every stamp after $SECONDS_SINCE carries seconds (six minute-width files are applied history)"
