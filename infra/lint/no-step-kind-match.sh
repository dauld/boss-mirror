#!/usr/bin/env bash
# no-step-kind-match — no core code may match on a step-kind name
# (docs/architecture-decisions.md §Step types are property bundles).
# Code dispatches on properties and mechanism enums; kind names are
# data (registry rows + rule topics + seeds).
#
# Enforcement shape mirrors infra/lint/tier-import-audit.sh: grep,
# explicit allow-list, non-zero exit on anything new. The allow-list
# is a RATCHET — today it holds only the one permanent platform
# pin; remove an entry in the same change that converts its site.
# Never add an entry without a decision recorded in
# docs/architecture-decisions.md justifying it.
#
# Scope: crates/core, crates/modules, apps/web/src. Tests and seeds
# are exempt (tests pin fixtures; seeds ARE the data).
#
# The allow-list is held to lib/allowlist.sh's two rules: every entry
# names a file that exists, and every entry excused at least one match
# this run. DesignReviewPage.svelte sat here with a budget of 1 and 0
# matches from the day its `kind === 'review-design'` comparison moved
# to a surface id (backlog cdf2d959, audit §4) — a hole shaped like a
# file, waiting for the next match to land in it unrefused.
set -euo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh"
# shellcheck source=infra/lint/lib/allowlist.sh
. "$LINT_DIR/lib/allowlist.sh"

LINT=no-step-kind-match

# The kind vocabulary, read live from the registry seed so the lint
# tracks the catalog without a second hand-maintained list.
KINDS=$(grep '^kind = ' crates/core/boss-jobs/seeds/step_types.toml \
    | sed 's/kind = "\(.*\)"/\1/' | paste -sd'|' -)

PATTERN="kind[)]?[[:space:]]*(==|===|!=|!==)[[:space:]]*[\"'](${KINDS})[\"']|matches!\([^,]*kind[^,]*,[[:space:]]*\"(${KINDS})\"|WHERE[[:space:]]+s\.kind[[:space:]]*=[[:space:]]*'(${KINDS})'"

# Allow-list: file => max permitted match count.
#   - boss-jobs http/steps.rs: the platform-pinned workflow-publish
#     row — a permanent pin. (steps.rs is where workflow-publish landed
#     after http.rs was split into the http/ module directory.)
#   - apps/web/src/debug/: dev-only demo driver, not a core surface.
declare -A ALLOW=(
    ["crates/core/boss-jobs/src/http/steps.rs"]=1
)
allowlist_paths_exist "$LINT" "${!ALLOW[@]}"

declare -A counts files
fail=0
while IFS= read -r line; do
    f="${line%%:*}"
    case "$f" in
        */tests/*|*/debug/*) continue ;;
    esac
    counts["$f"]=$(( ${counts["$f"]:-0} + 1 ))
    files["$f"]="${files[$f]:-}"$'\n'"  $line"
done < <(grep -rnE "$PATTERN" crates/core crates/modules apps/web/src \
    --include="*.rs" --include="*.svelte" --include="*.ts" 2>/dev/null \
    | grep -v 'surfaceOf(' || true)
# surfaceOf(kind) === '<id>' compares SURFACE ids (registry data),
# not kind names — the ids share spellings with the kinds they ship
# for, so the call shape is excluded explicitly.

# The scan set, counted the way the grep walks it: every source file
# under the three roots. Zero means the roots moved, not a clean tree.
scanned=$(find crates/core crates/modules apps/web/src -type f \
    \( -name '*.rs' -o -name '*.svelte' -o -name '*.ts' \) | wc -l | tr -d ' ')
lint_scanned "$LINT" "$scanned" "source file(s) under crates/core, crates/modules and apps/web/src"

printf '%s\n' "[no-step-kind-match] kinds tracked: $(grep -c '^kind = ' crates/core/boss-jobs/seeds/step_types.toml)"
used=""
for f in "${!counts[@]}"; do
    allowed="${ALLOW[$f]:-0}"
    if (( counts[$f] > allowed )); then
        echo "FAIL $f — ${counts[$f]} kind-name match(es), allow-list permits $allowed:"
        echo "${files[$f]}"
        fail=1
    elif (( allowed > 0 )); then
        used="$used"$'\n'"$f"
    fi
done

if (( fail )); then
    echo
    echo "Step-kind names are data (architecture-decisions.md, the"
    echo "step-types-are-property-bundles section). Dispatch on a property"
    echo "(executor, completion authority, a metadata field, ux id) or"
    echo "move the behavior to a rule/registry row. Do not grow the"
    echo "allow-list without a recorded decision."
    exit 1
fi
allowlist_entries_used "$LINT" "$used" "${!ALLOW[@]}"
echo "ok: no unapproved step-kind matches"
