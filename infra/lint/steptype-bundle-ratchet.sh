#!/usr/bin/env bash
#
# steptype-bundle-ratchet — a StepType field bundle may loosen, never
# tighten (backlog cdc23602).
#
# THE ASYMMETRY THIS GUARDS
# -------------------------
# A completion contract lives in two places and only one is versioned.
# A Workflow spec is append-only: in-flight packets stay PINNED to the
# version they were admitted under, so editing it cannot disturb work
# already moving. The StepType bundle is the other half of the same
# contract — the completion validator checks the UNION of the kind
# bundle's fields and the step's own authored fields — and it is one
# global Arc<StepRegistry> built from seeds/step_types.toml at startup.
# No version column, no pinning, no convertibility check.
#
# So adding a `required` field to an existing kind retightens every
# in-flight step of that kind at the next restart: a step that was
# completable a minute ago now refuses, which is precisely the failure
# protocol_conversion exists to prevent, reached through a door that
# check cannot see (it compares two WorkflowSpecs; the bundle is in
# neither). How close it came, 2026-09-01: backlog-item v2 moved
# `design-review` from `task` to `answer-question` — the versioned
# path, correctly pinned. Achieving the same effect by adding
# `verdict` to the `task` bundle would have frozen every live task
# step in every protocol, and both edits look equally routine in a
# diff.
#
# THE CHECKED PROPERTY
# --------------------
# Against the merge-base with the trunk, for every step kind that
# already exists there:
#   - a field added to it must be `required = false` (new-optional is
#     the one always-safe bundle edit);
#   - an existing field may not move optional -> required;
#   - an existing field may not change `field_type` (a pipe-enum IS
#     the value contract — shrinking it tightens, and telling a safe
#     widening from a tightening needs judgment a lint should refuse
#     rather than guess);
#   - an existing field may not be REMOVED (the UNION validator stops
#     requiring it, other consumers — sim faker, surfaces — stop
#     seeing it; removal is a contract change that belongs behind the
#     versioned path, not a restart).
# A brand-new kind may declare anything: nothing in flight carries it.
# required -> optional stays legal — loosening strands nobody.
#
# The real fix, if bundle edits stop being rare, is versioning the
# bundle and pinning steps to it the way jobs pin workflow versions
# (cdc23602 option 2). This ratchet is option 1: a few mechanical
# lines that turn an invisible hazard into a refused commit.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

BUNDLE="crates/core/boss-jobs/seeds/step_types.toml"
[ -f "$BUNDLE" ] || { echo "steptype-bundle-ratchet: $BUNDLE not found" >&2; exit 1; }

# Trunk resolution, exactly the migrations-append-only walk: the first
# ref that exists wins; none found is a refusal, not a silent pass.
trunk=""
for ref in ${BOSS_TRUNK_REF:-} forge/main origin/main main; do
    if git rev-parse --verify --quiet "$ref" >/dev/null; then trunk="$ref"; break; fi
done
if [ -z "$trunk" ]; then
    echo "steptype-bundle-ratchet: no trunk ref found (tried origin/main, forge/main, main)" >&2
    echo "  A ratchet that cannot see the trunk cannot certify anything; refusing." >&2
    exit 1
fi
mb=$(git merge-base "$trunk" HEAD 2>/dev/null) || mb="$trunk"

# The trunk may predate the bundle file itself (it does not today, but
# a ratchet should say what it assumes): no trunk copy = nothing to
# tighten against.
if ! git cat-file -e "$mb:$BUNDLE" 2>/dev/null; then
    echo "steptype-bundle-ratchet: $BUNDLE absent at merge-base — nothing to ratchet"
    exit 0
fi

# Flatten a step_types.toml into TAB-separated
# `kind<TAB>field<TAB>field_type<TAB>required` rows (plus
# `kind<TAB>-<TAB>KIND<TAB>-` rows marking each kind's existence —
# DASH placeholders, never empty slots: tab is IFS whitespace, so
# `read` COLLAPSES adjacent tabs and an empty field shifts every
# column after it (the third bug the red-battery caught).
# TAB, not `|`: a pipe-enum field_type (`brew|oversupply`) CARRIES the
# obvious delimiter, and the first draft used it — every enum field
# shredded into the wrong columns and two deliberate tightenings
# passed. The
# same first-word-level parse the repo's other toml scrapers use: the
# file is machine-written by hand in a stable idiom, and the awk state
# machine reads exactly that idiom — a reformat that breaks it shows
# up as the non-vacuity refusal below, not a silent green.
flatten() {
    awk '
        /^\[\[step_type\]\]/        { kind=""; infield=0; next }
        /^[[:space:]]*\[\[step_type\.fields\]\]/ { infield=1; fname=""; ftype=""; freq="false"; next }
        /^kind = /                   { gsub(/^kind = "|"$/,""); kind=$0; printf "%s\t-\tKIND\t-\n", kind; next }
        infield && /^[[:space:]]*name = /       { line=$0; sub(/^[[:space:]]*name = "/,"",line); sub(/"$/,"",line); fname=line; next }
        infield && /^[[:space:]]*field_type = / { line=$0; sub(/^[[:space:]]*field_type = "/,"",line); sub(/"$/,"",line); ftype=line; next }
        infield && /^[[:space:]]*required = /   { line=$0; sub(/^[[:space:]]*required = /,"",line); freq=line;
                                                  printf "%s\t%s\t%s\t%s\n", kind, fname, ftype, freq; infield=0; next }
    '
}

base_rows=$(git show "$mb:$BUNDLE" | flatten)
head_rows=$(flatten < "$BUNDLE")

# Non-vacuity: the trunk bundle has dozens of kinds; a parse that sees
# almost none means the idiom moved and this green would mean nothing.
base_kinds=$(printf '%s\n' "$base_rows" | grep -cF "$(printf '\t-\tKIND\t-')"  || true)
base_fields=$(printf '%s\n' "$base_rows" | grep -cvF "$(printf '\t-\tKIND\t-')"  || true)
if [ "$base_kinds" -lt 10 ] || [ "$base_fields" -lt 30 ]; then
    echo "steptype-bundle-ratchet: parsed $base_kinds kinds / $base_fields fields from the trunk bundle —" >&2
    echo "  the parse broke, so a green result would certify nothing. Fix the scraper." >&2
    echo "  (This guard is not decorative: the first draft anchored the fields header at" >&2
    echo "  column 0, parsed zero fields, and passed a deliberate tightening.)" >&2
    exit 1
fi

problems=0
say() { printf '%s\n' "$*" >&2; problems=$((problems + 1)); }

while IFS=$'\t' read -r kind field ftype freq; do
    [ -n "$kind" ] || continue
    if [ "$ftype" = "KIND" ]; then
        # A kind existing on trunk must still exist: removal changes
        # the contract of every in-flight step of that kind (unknown
        # kinds validate permissively) through the unversioned door.
        if ! printf '%s\n' "$head_rows" | grep -qxF "$(printf '%s\t-\tKIND\t-' "$kind")"; then
            say "steptype-bundle-ratchet: kind \`$kind\` exists on the trunk and is removed here." \
                " In-flight steps of that kind lose their contract at the next restart;" \
                " retire behaviour through the versioned workflow path instead (cdc23602)."
        fi
        continue
    fi
    head_row=$(printf '%s\n' "$head_rows" | awk -F'\t' -v k="$kind" -v f="$field" '$1==k && $2==f' | head -1)
    if [ -z "$head_row" ]; then
        say "steptype-bundle-ratchet: field \`$field\` on kind \`$kind\` exists on the trunk and is removed here — a bundle contract change with no version to pin against."
        continue
    fi
    head_type=$(printf '%s' "$head_row" | cut -f3)
    head_req=$(printf '%s' "$head_row" | cut -f4)
    if [ "$head_type" != "$ftype" ]; then
        say "steptype-bundle-ratchet: \`$kind.$field\` changes field_type \`$ftype\` -> \`$head_type\` — the type IS the value contract; change it through the versioned workflow path."
    fi
    if [ "$freq" = "false" ] && [ "$head_req" = "true" ]; then
        say "steptype-bundle-ratchet: \`$kind.$field\` moves optional -> required. Every in-flight \`$kind\` step retightens at the next restart with no version bump and no conversion (cdc23602's exact hazard)."
    fi
done <<EOF
$base_rows
EOF

# New fields on kinds that already existed on the trunk must be optional.
while IFS=$'\t' read -r kind field ftype freq; do
    [ -n "$kind" ] && [ "$ftype" != "KIND" ] || continue
    printf '%s\n' "$base_rows" | grep -qxF "$(printf '%s\t-\tKIND\t-' "$kind")" || continue  # new kind: free
    [ -n "$(printf '%s\n' "$base_rows" | awk -F'\t' -v k="$kind" -v f="$field" '$1==k && $2==f')" ] && continue  # existed: handled above
    if [ "$freq" = "true" ]; then
        say "steptype-bundle-ratchet: NEW required field \`$field\` on existing kind \`$kind\` — this retightens every in-flight \`$kind\` step at the next restart. Declare it required = false, or carry the contract on a new workflow version instead."
    fi
done <<EOF
$head_rows
EOF

if [ "$problems" -gt 0 ]; then
    echo "steptype-bundle-ratchet: $problems tightening(s) refused — the bundle is the unversioned half of the completion contract." >&2
    exit 1
fi
echo "steptype-bundle-ratchet: bundle only loosened or grew optional fields ($base_kinds kinds checked against $trunk)"
