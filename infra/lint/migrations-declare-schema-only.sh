#!/usr/bin/env bash
# migrations-declare-schema-only.sh — a migration newer than the cutover
# declares SCHEMA; a registry row lives in its platform bundle.
#
# WHY THIS EXISTS (backlog 393d3234, design 42277636, consolidation H4).
# MEASURED 2026-09-18 on origin/main 1a09f660: registry rows whose ONLY
# home was a migration — stations (7 files), step_plugins (7),
# cadence_rules (9), delivery_policy (2). A migration is the wrong home
# for a registry row: it runs once, so a fresh instance cannot
# re-declare the row without replaying history (the playground's fresh
# database inherited 31 example-tenant rule names that way the same day);
# nothing drift-checks it against the live row (protocol-drift compares
# only workflows); and every edit is a contended timestamped file.
# Dispatcher rules left that shape on 2026-09-11 (one file per rule
# under infra/dispatcher/rules/) and the Workflow bundle before them
# (infra/platform/workflows/, seeded insert-if-missing every start).
#
# THE RULE. Each registry in REGISTRY_TABLES has a bundle directory
# under infra/platform/ that the platform seed publishes at every start,
# insert-if-missing by (name, version). From the CUTOVER stamp on, a
# migration may not `INSERT INTO <that table>`: the row goes in the
# bundle, and a change to a live row is a version bump there. The
# historical inserts stay exactly as they are — migrations are
# append-only history (migrations-append-only.sh) — which is why the
# rule is dated rather than absolute. A file whose leading numeric
# prefix is NEWER than the cutover is checked; one older is history.
#
# THE STAMP is written ONCE, here, and it is the car's own write time
# (`date -u +%Y%m%d%H%M%S`, the fourteen-digit prefix a migration
# carries — a-migration-prefix-is-fourteen-digits). It is not "today":
# a stamp re-derived at run time would move, and a rule that moves is
# not a ratchet. Cars 2–4 of the same packet append their table to
# REGISTRY_TABLES and leave the stamp alone; a table added later gets
# its own dated entry beside it if its history must be exempt.
#
# WHAT IS READ. Every tracked infra/postgres/schema/*.sql, its SQL with
# `--` comments removed (a comment may SAY "INSERT INTO stations" to
# tell the story; several do). `UPDATE stations` is not refused: an
# in-place column fill on a row the bundle declares is still a
# migration's business when the column is new (119 and 138 are the
# precedent), and the equality pin in boss-jobs holds the bundle equal
# to whatever the migrations produce.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  the tree was read and no post-cutover migration inserts a row
#   1  the tree was read and a violation was found — the author's to fix
#   3  the tree was never read — a fact about the MACHINE; never `clean`
#
# USAGE
#   infra/lint/migrations-declare-schema-only.sh
#   infra/lint/migrations-declare-schema-only.sh --self-test
set -uo pipefail

NAME="migrations-declare-schema-only"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh"
cd "$LINT_DIR/../.." || exit 1

# shellcheck source=/dev/null
. "$LINT_DIR/lib/git-answer.sh"

SCHEMA_DIR="infra/postgres/schema"

# The cutover: written 2026-09-18 11:21:34 UTC by the car that made
# infra/platform/stations/ the home of every platform station (backlog
# 393d3234, car 1 of 4). A migration with a newer prefix declares
# schema only.
CUTOVER="20260918112134"

# The registries whose rows now live in a bundle, space-separated.
# stations: car 1 (infra/platform/stations/). step_plugins: car 2
# (infra/platform/step-plugins/ — the newest insert it replaces,
# 202609082130-sign-off-plugin-v3.sql, is older than the cutover, so
# the stamp did not move). cadence_rules: car 3 (infra/platform/cadence/
# — newest insert 202609042110-a-lone-car-still-ships.sql, older than
# the cutover; the stamp stays). Car 4 of 393d3234 adds
# delivery_policy.
REGISTRY_TABLES="stations step_plugins cadence_rules"

# --- the scanner -------------------------------------------------------
# One file's findings, as `<line>\t<table>`. Empty output = clean. The
# SQL is read with `--` comments removed, so prose about an insert is
# not an insert; the match is `INSERT INTO [public.]<table>` at a word
# boundary, any case, any whitespace.
findings_in() { # file
    awk -v tables="$REGISTRY_TABLES" '
        BEGIN { n = split(tables, t, /[ \n]+/); for (k = 1; k <= n; k++) if (t[k] != "") want[t[k]] = 1 }
        {
            line = $0
            sub(/--.*$/, "", line)
            low = tolower(line)
            if (match(low, /insert[ \t]+into[ \t]+(public\.)?[a-z_]+/) > 0) {
                tbl = substr(low, RSTART, RLENGTH)
                sub(/^insert[ \t]+into[ \t]+/, "", tbl)
                sub(/^public\./, "", tbl)
                if (tbl in want) printf "%d\t%s\n", NR, tbl
            }
        }
    ' "$1"
}

# The leading numeric prefix of a migration file name, or `none`.
prefix_of() { # basename
    local p="${1%%-*}"
    case "${p:-empty}" in
        empty|*[!0-9]*) echo none ;;
        *) echo "$p" ;;
    esac
}

# Whether a migration is NEWER than the cutover. Numeric on the prefix
# (three-digit, twelve-digit and fourteen-digit prefixes all sort the
# way migrate.sh sorts them); a file with no numeric prefix sorts last
# in migrate.sh too, so it is newer.
is_after_cutover() { # prefix
    [ "$1" = none ] && return 0
    [ "$1" -gt "$CUTOVER" ]
}

# --- self-test ---------------------------------------------------------
# Runs on every invocation: a scanner whose regex stopped matching
# passes every file, and only a fixture it must refuse tells that from
# a clean tree. Fixtures live in a mktemp dir this run owns.
self_test() {
    local t hits
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # shellcheck disable=SC2064
    trap "rm -rf '$t'" RETURN

    # Accepted: schema statements, a comment that names the insert, an
    # UPDATE, an insert into a table that merely starts with the name.
    {
        printf -- '-- A comment may say INSERT INTO stations to tell the story.\n'
        printf 'ALTER TABLE stations ADD COLUMN IF NOT EXISTS lens JSONB;\n'
        printf 'UPDATE stations SET lens = NULL WHERE name = %s; -- INSERT INTO stations, in prose\n' "'x'"
        printf 'INSERT INTO stations_archive (name) VALUES (%s);\n' "'x'"
        printf 'INSERT INTO event_kinds (kind_pattern) VALUES (%s);\n' "'jobs.station.x'"
    } >"$t/good.sql"
    hits="$(findings_in "$t/good.sql")"
    [ -z "$hits" ] || {
        echo "$NAME: self-test FAILED — an accepted shape was flagged:" >&2
        printf '%s\n' "$hits" >&2
        return 1
    }

    # Refused: each spelling a migration in this tree has used.
    printf 'INSERT INTO stations (name, version) VALUES (%s, 1)\n' "'x'"   >"$t/bad1.sql"
    printf 'insert into\tpublic.stations (name)\nSELECT %s\n' "'x'"        >"$t/bad2.sql"
    printf 'INSERT   INTO   Stations (name) VALUES (%s);\n' "'x'"          >"$t/bad3.sql"
    printf 'INSERT INTO step_plugins (\n    kind, version\n) VALUES (%s, 1)\n' "'x'" >"$t/bad4.sql"
    # The retire-by-name supersede idiom (202609032030): the INSERT's
    # rows come from a SELECT, not a VALUES list.
    printf 'INSERT INTO cadence_rules\n    (name, version, status, verb, basis)\nSELECT %s, COALESCE(MAX(version), 0) + 1, %s, %s, %s\n  FROM cadence_rules WHERE name = %s;\n' \
        "'x'" "'active'" "'board'" "'queue-depth'" "'x'" >"$t/bad5.sql"
    local f
    for f in bad1.sql bad2.sql bad3.sql bad4.sql bad5.sql; do
        [ -n "$(findings_in "$t/$f")" ] || {
            echo "$NAME: self-test FAILED — the scanner passed:" >&2
            sed 's/^/    /' "$t/$f" >&2
            return 1
        }
    done

    # The cutover, both sides: a three-digit and a twelve-digit prefix
    # are history, a fourteen-digit one newer than the stamp is not, a
    # file with no numeric prefix sorts last and is not.
    ! is_after_cutover "$(prefix_of 116-stations.sql)" \
        && ! is_after_cutover "$(prefix_of 202609101900-the-corpus-index-is-deleted.sql)" \
        && ! is_after_cutover "$(prefix_of 20260915023614-a-machine-owned-proof.sql)" \
        && ! is_after_cutover "$(prefix_of "$CUTOVER-the-cutover-itself.sql")" \
        && is_after_cutover "$(prefix_of 20260918112135-one-second-later.sql)" \
        && is_after_cutover "$(prefix_of 20261001000000-next-month.sql)" \
        && is_after_cutover "$(prefix_of no-prefix.sql)" || {
        echo "$NAME: self-test FAILED — the cutover comparison answers wrongly" >&2
        return 1
    }
    echo "$NAME: self-test ok — schema statements, prose, an UPDATE and a prefixed table name pass; five spellings of INSERT INTO a registry table are refused; the cutover splits history from new"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
# `git ls-files` answers 0 for a listing (empty included) and non-zero
# only when it could not look — git_answer keeps those apart.
files="$(git_answer "$NAME" 0 ls-files -- "$SCHEMA_DIR/*.sql")"
status=$?
[ "$status" -eq 0 ] || exit "$status"

scanned=0
findings=0
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    scanned=$((scanned + 1))
    is_after_cutover "$(prefix_of "$(basename "$file")")" || continue
    while IFS=$'\t' read -r lineno tbl; do
        [ -n "${lineno:-}" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$lineno inserts into $tbl" >&2
    done < <(findings_in "$file")
done <<EOF
$files
EOF

if [ "$findings" -gt 0 ]; then
    cat >&2 <<MSG

FAIL — the migration(s) above are newer than the cutover ($CUTOVER) and
insert a registry row. Since 2026-09-18 (backlog 393d3234) a migration
declares SCHEMA only; a registry row lives in its platform bundle, which
the seed publishes insert-if-missing at every start:

  * stations — infra/platform/stations/<name>.toml, one file per station,
    every column the row has. boss-platform-workflow-seed finds the
    directory beside its --seed-path and publishes it by (name, version);
    a row that differs from the live active row of the same (name,
    version) is refused, and a version bump is the edit path.
  * step_plugins — infra/platform/step-plugins/<kind>.toml, one file per
    plugin, every column of the row; the JS it names stays at
    infra/step-plugins/<frontend_url>. Same seed, same sibling lookup,
    same refusal, same edit path.
  * cadence_rules — infra/platform/cadence/<name>.toml, one file per
    rule, every column of the row. Same seed, same sibling lookup, same
    refusal, same edit path. The table stays live-editable: a rule
    re-versioned live is reported as ahead of its file, never rewritten,
    and a rule the operator retired stays retired.

Leave the migration to its ALTERs and put the row in the bundle. The
historical inserts before the cutover are history and stay as they are.
MSG
    exit 1
fi

lint_scanned "$NAME" "$scanned" "migration(s) checked against the cutover"
echo "$NAME: ok — every migration newer than $CUTOVER declares schema only"
exit 0
