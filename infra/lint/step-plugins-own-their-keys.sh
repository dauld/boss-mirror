#!/usr/bin/env bash
#
# step-plugins-own-their-keys — a step plugin writes only the metadata
# keys it owns; it never re-sends the page-load snapshot.
#
# WHY. On 2026-09-02 the review-design surface silently reverted step
# metadata (the carried title + markdown) and 400'd the reviewer twice.
# The defect was the GET-spread-PUT idiom every plugin then used:
# snapshot step.metadata at page load, then PUT
# `{ ...step, status, metadata: { ...snapshot, ownKeys } }`. PUT
# metadata is replaced WHOLESALE, so any key another writer added after
# the page loaded rode the stale snapshot back out of existence — a
# classic lost update, invisible until someone misses their data. Six
# of the twelve bundles carried the idiom when this lint was written.
#
# The door that retires it: PATCH /api/jobs/{id}/steps/{step_id}/metadata
# merges top-level keys server-side against the row as it stands (a
# null value DELETES its key; every other value replaces that key
# wholesale; status and the other step fields are untouchable through
# it). A plugin sends ONLY the keys it owns and the server preserves
# the rest. A completion PUT that must attest the step's final shape
# reads the row back fresh first — never the snapshot.
#
# WHAT IT CHECKS. Greps infra/step-plugins/*.js for the snapshot-write
# idioms, each taken verbatim from the pre-migration code so this lint
# would have caught the 2026-09-02 defect:
#   `...step.metadata` / `...(step.metadata`   spread of the snapshot
#   `Object.assign({}, step.metadata`          same, assign form
#   `metadata: step.metadata`                  snapshot re-sent whole
#   `JSON.stringify({ ...step,`                whole-snapshot body
#   `Object.assign({}, step,`                  whole-snapshot body
#
# WHY A TEXTUAL CHECK IS HONEST HERE. `step` in a plugin is the
# mount-prop snapshot by the plugin contract itself —
# mount(container, { step, jobId, onUpdate }) — so writing that
# identifier's metadata into a request body IS the defect, whatever the
# surrounding code meant. A body built from a just-fetched row (named
# anything but `step`) does not match, and that is the sanctioned
# completion shape. Comment lines are exempt: the migrated plugins
# quote the old idiom verbatim in the WHY comments beside their fix,
# and a commented line cannot send a request.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

DIR="infra/step-plugins"
[ -d "$DIR" ] || {
    echo "step-plugins-own-their-keys: $DIR not found — skipping"
    exit 0
}

# name|extended-regex, matched per line so the failure says what was
# found, not just where.
PATTERNS=(
    'spread of the page-load metadata snapshot|\.\.\.\(?step\.metadata'
    'Object.assign over the metadata snapshot|Object\.assign\(\{\},[[:space:]]*step\.metadata'
    'page-load metadata snapshot re-sent whole|metadata:[[:space:]]*step\.metadata'
    'whole-step snapshot spread into a body|JSON\.stringify\(\{[[:space:]]*\.\.\.step,'
    'whole-step snapshot assigned into a body|Object\.assign\(\{\},[[:space:]]*step,'
)

fails=0
for entry in "${PATTERNS[@]}"; do
    name="${entry%%|*}"
    regex="${entry#*|}"
    hits=$(grep -nE "$regex" "$DIR"/*.js 2>/dev/null \
        | grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|\*|/\*)')
    [ -z "$hits" ] && continue
    if [ "$fails" -eq 0 ]; then
        echo "step-plugins-own-their-keys: a plugin re-sends the page-load metadata" >&2
        echo "  snapshot in a write body — the lost-update idiom that silently" >&2
        echo "  reverted server-side step metadata on 2026-09-02 (review-design" >&2
        echo "  title/markdown). Send ONLY the keys the plugin owns through" >&2
        echo "  PATCH /api/jobs/{id}/steps/{step_id}/metadata (the server merges;" >&2
        echo "  null deletes a key), and build a completion PUT from a freshly" >&2
        echo "  fetched row, never from the mount-prop \`step\`." >&2
    fi
    echo "  [$name]" >&2
    printf '%s\n' "$hits" | sed 's/^/    /' >&2
    fails=$((fails + 1))
done

if [ "$fails" -gt 0 ]; then
    exit 1
fi

echo "step-plugins-own-their-keys: clean — no plugin re-sends the page-load metadata snapshot"
