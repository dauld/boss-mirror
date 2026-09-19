#!/usr/bin/env bash
#
# registry-bump-retires-first — a versioned registry row is retired
# BEFORE its successor is inserted, never after.
#
# THE INCIDENT (twice in one night)
# ---------------------------------
# 2026-08-15. `130-watchlist-dismiss.sql` inserted `my-watchlist` v2 as
# `active` and retired v1 on the next line. Three hours later, after
# the class had been diagnosed and written into docs/invariants/,
# `133-dock-wip-limit.sql` did the identical thing to `loading-dock`
# and reddened a THIRTEEN-car train.
#
# `stations_one_active_per_name` — and its siblings on cadence_rules,
# workflows and step_plugins — are plain partial unique indexes:
#
#     CREATE UNIQUE INDEX ... ON stations (name) WHERE status = 'active'
#
# A plain unique index is enforced per STATEMENT, not deferred to
# commit. Same transaction is not the same statement. So the INSERT
# collides with the still-active v1 and the whole schema load dies.
#
# WHY IT IS EXPENSIVE OUT OF PROPORTION
# -------------------------------------
# The failure is in schema LOAD, so every DB-backed test in the
# workspace aborts before running, and the error names a uniqueness
# constraint rather than the migration that violated it. The author
# reads "duplicate key value violates unique constraint" against a
# table they may not have touched, in a crate they did not change.
#
# WHAT THIS CHECKS
# ----------------
# Shape, not intent: within one migration file, for each table that
# has a one-active-per-name partial index, an INSERT that writes
# status 'active' must be preceded in the file by an UPDATE ... SET
# status = 'retired' on the same table. Both orders are legal SQL and
# only one of them survives the index, which is exactly the kind of
# thing a human eye slides over and a grep does not.
set -uo pipefail

cd "$(dirname "$0")/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3
SCHEMA=infra/postgres/schema
fail=0

# The tables guarded by a one-active-per-name partial index. Derived
# from the schema itself rather than hardcoded, so a new registry with
# the same shape is covered the day it lands. (No mapfile/readarray —
# this has to run on macOS bash 3.2 as well as CI.)
GUARDED=$(
    grep -rhoE "[A-Za-z_]+_one_active_per_name" "$SCHEMA"/*.sql 2>/dev/null |
        sed -E 's/_one_active_per_name$//' | sort -u
)
# The index name is <table>_one_active_per_name by convention; confirm
# the table exists in the tree so a rename does not silently disarm us.
TABLES=()
for t in $GUARDED; do
    [ -z "$t" ] && continue
    if grep -rqE "CREATE TABLE (IF NOT EXISTS )?${t}\b" "$SCHEMA"/*.sql 2>/dev/null; then
        TABLES+=("$t")
    else
        echo "registry-bump-retires-first: index ${t}_one_active_per_name names no table ${t} — convention broken, refusing to pass"
        fail=1
    fi
done

if [ ${#TABLES[@]} -eq 0 ]; then
    echo "registry-bump-retires-first: found no one-active-per-name indexes — the check has been disarmed by a rename"
    exit 1
fi

for f in "$SCHEMA"/*.sql; do
    base=$(basename "$f")
    for t in "${TABLES[@]}"; do
        # Line of the first INSERT into this table that sets an active
        # status, and of the first retiring UPDATE on it.
        ins=$(awk -v tbl="$t" '
            tolower($0) ~ "insert into[[:space:]]+" tbl "([[:space:]]|\\()" { start = NR; inblk = 1 }
            inblk && tolower($0) ~ /'"'"'active'"'"'/ { print NR; exit }
            /;[[:space:]]*$/ { inblk = 0 }
        ' "$f" | head -1)
        [ -z "$ins" ] && continue
        ret=$(grep -niE "update[[:space:]]+$t[[:space:]]+set[[:space:]]+status[[:space:]]*=[[:space:]]*'retired'" "$f" | sed -n 1p | cut -d: -f1)
        if [ -z "$ret" ]; then
            # Seeding a brand-new name is fine — there is no prior
            # active row to collide with. Only flag when the file
            # itself shows it is superseding something.
            continue
        fi
        if [ "$ret" -gt "$ins" ]; then
            echo "registry-bump-retires-first: $base inserts an active $t row at line $ins but retires the old one at line $ret."
            echo "    A one-active-per-name partial index is enforced per STATEMENT, so the INSERT"
            echo "    collides with the row still marked active and the whole schema load fails."
            # Worded to avoid spelling a literal DML statement: this
            # message would otherwise trip api-path-bypass-smell, whose
            # shell-DML pattern cannot tell a help string from a query
            # and which reddened this very train once already.
            echo "    Move the retiring UPDATE (status = 'retired') ABOVE the row insert."
            fail=1
        fi
    done
done

if [ "$fail" -ne 0 ]; then
    exit 1
fi
lint_scanned registry-bump-retires-first "$(find "$SCHEMA" -maxdepth 1 -name '*.sql' -type f | wc -l | tr -d ' ')" "migration(s) against ${#TABLES[@]} one-active-per-name table(s)"
echo "registry-bump-retires-first: clean — every superseding registry write retires before it inserts"
