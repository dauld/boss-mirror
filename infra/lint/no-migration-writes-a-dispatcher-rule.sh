#!/usr/bin/env bash
#
# no-migration-writes-a-dispatcher-rule — the second home stays closed.
#
# WHY THIS EXISTS
# ---------------
# `infra/dispatcher/rules/` is the dispatcher-rule registry's definition,
# and `boss_dispatcher::rules::seed` publishes it into the
# `dispatcher_rules` table at boot (backlog 41ba00cd). Before that, a rule
# was declared twice — a file there AND a row written by a migration — with
# nothing deriving one from the other, which is the §9a defect in its worst
# shape, because one copy lived in the production database where no test
# could reach it.
#
# The collapse removed the REASON to write a migration. It did not remove
# the ABILITY, and CLAUDE.md §9a is explicit that a comment asking the next
# person not to is not a mechanism. This is the mechanism: a migration
# written after the collapse may not write dispatcher-rule rows. A rule
# lands by dropping a file in; a change is a `version` bump in that file; a
# retirement is deleting it.
#
# THE CHECKED PROPERTY
# --------------------
# No migration whose ordering prefix is at or after the CUTOVER below
# contains a DML write against `dispatcher_rules`. The thirty-one
# migrations that predate it are applied history: they are never edited
# (the checksum guard in migrate.sh refuses it — the 2026-08-13 outage)
# and they are left exactly alone.
#
# ONE CONSTANT, NOT A LIST. The cutover is a single timestamp, and every
# new migration takes a `YYYYMMDDHHMMSS-` prefix, so a file added tomorrow
# is covered without anyone adding it anywhere — the BASELINE=<n> lesson
# (§9a) applied to this check's own shape. Legacy `NNN-` prefixes sort far
# below it, which is also why they are exempt by construction.
#
# DDL IS NOT A RULE WRITE. `ALTER TABLE dispatcher_rules`, an index, a
# constraint — all fine; the table's SHAPE is schema. What is refused is
# INSERT / UPDATE / DELETE against its rows, because those are the
# registry's content, and the registry's content comes from the tree.
#
# Usage:  infra/lint/no-migration-writes-a-dispatcher-rule.sh

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

# The collapse landed 2026-09-11. Migrations at or after this prefix are
# checked; everything before it is applied history.
CUTOVER=20260911000000

# Migrations allowed a dispatcher_rules DML write anyway, each with its
# reason. A named set, not a count, so adding one never edits a shared
# line. Expected to stay empty: the only legitimate case is a schema
# change that has to backfill the table's own new column, which is about
# the table's shape and not about which rules exist.
ALLOWLIST=()

SCHEMA_DIR="infra/postgres/schema"
[ -d "$SCHEMA_DIR" ] || { echo "no-migration-writes-a-dispatcher-rule: $SCHEMA_DIR does not exist" >&2; exit 1; }

# Does $1 hold a DML write against dispatcher_rules? Whole-line comments
# are skipped so the prose in a migration's header can describe one; an
# `-- INSERT INTO dispatcher_rules` trailing a statement is deliberately
# still read, because failing closed on a comment costs a rewording and
# failing open costs the property.
writes_a_rule() {
    LC_ALL=C awk '
        { line = $0 }
        line ~ /^[ \t]*--/ { next }
        tolower(line) ~ /(insert[ \t]+into|update|delete[ \t]+from)[ \t]+dispatcher_rules([ \t(;]|$)/ {
            print FNR ": " line
        }
    ' "$1"
}

check_dir() {
    local dir="$1" found=0 base prefix hit
    while IFS= read -r path; do
        [ -n "$path" ] || continue
        base="$(basename "$path")"
        prefix="${base%%-*}"
        # Not a numeric-prefixed migration: nothing to order it by.
        case "$prefix" in ''|*[!0-9]*) continue ;; esac
        # 10# forces base-10: a prefix with a leading zero is not octal.
        [ "$((10#$prefix))" -ge "$CUTOVER" ] || continue
        case " ${ALLOWLIST[*]} " in *" $base "*) continue ;; esac
        hit="$(writes_a_rule "$path")"
        if [ -n "$hit" ]; then
            found=1
            echo "no-migration-writes-a-dispatcher-rule: $path writes dispatcher-rule rows:" >&2
            printf '    %s\n' "$hit" >&2
        fi
    done <<EOF
$(find "$dir" -maxdepth 1 -name '*.sql' -type f | LC_ALL=C sort)
EOF
    return $found
}

# ---------------------------------------------------------------------------
# Self-test — fixtures in a temp directory, never in infra/lint/.
# ---------------------------------------------------------------------------
tmp="$(mktemp -d)" || exit 1
trap 'rm -rf "$tmp"' EXIT

printf "INSERT INTO dispatcher_rules (name) VALUES ('x');\n" > "$tmp/41-legacy-history.sql"
printf -- "-- prose about INSERT INTO dispatcher_rules\nALTER TABLE dispatcher_rules ADD COLUMN note TEXT;\n" \
    > "$tmp/20260912000000-ddl-and-prose.sql"
if ! check_dir "$tmp"; then
    echo "no-migration-writes-a-dispatcher-rule: SELF-TEST FAILED — applied history and a DDL/comment-only migration must pass" >&2
    exit 1
fi
printf "UPDATE dispatcher_rules SET status = 'retired' WHERE name = 'x';\n" \
    > "$tmp/20260913000000-a-rule-retirement.sql"
if check_dir "$tmp" 2>/dev/null; then
    echo "no-migration-writes-a-dispatcher-rule: SELF-TEST FAILED — a post-cutover rule write must be refused" >&2
    exit 1
fi
rm -f "$tmp"/*.sql

# ---------------------------------------------------------------------------
# The tree.
# ---------------------------------------------------------------------------
if ! check_dir "$SCHEMA_DIR"; then
    echo "" >&2
    echo "  A dispatcher rule's one home is infra/dispatcher/rules/ — one file per" >&2
    echo "  rule, carrying its \`why\`. The dispatcher publishes that directory into" >&2
    echo "  the dispatcher_rules table at boot and retires what no file names, so:" >&2
    echo "" >&2
    echo "    adding a rule    = drop <rule-name>.toml in" >&2
    echo "    changing a rule  = bump its \`version\` in that file" >&2
    echo "    retiring a rule  = delete the file" >&2
    echo "" >&2
    echo "  None of those is a migration. Writing one puts the rule in two places" >&2
    echo "  again, one of which is a production database no test can read — the" >&2
    echo "  defect backlog 41ba00cd closed (CLAUDE.md §9a). If the write is really" >&2
    echo "  about the TABLE rather than about which rules exist, add the file to" >&2
    echo "  ALLOWLIST in this script with its reason, in the diff a reviewer reads." >&2
    exit 1
fi

checked=$(find "$SCHEMA_DIR" -maxdepth 1 -name '*.sql' -type f | wc -l | tr -d ' ')
echo "no-migration-writes-a-dispatcher-rule: self-test ok — applied history and DDL pass, a post-cutover rule write is refused by name and line"
echo "no-migration-writes-a-dispatcher-rule: clean — no migration at or after $CUTOVER writes dispatcher-rule rows ($checked migrations)"
exit 0
