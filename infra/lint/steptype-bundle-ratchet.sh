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
#   - an existing field may not change `field_type`, with one
#     exception: a pipe-enum that only GAINS values. The type IS the
#     value contract, so shrinking one tightens — but a strict superset
#     needs no judgment to recognise, and every value the trunk
#     accepted is still accepted, so nothing in flight becomes
#     uncompletable. That is the same property that makes `required ->
#     optional` legal here. This rule used to refuse the widening too,
#     saying the distinction "needs judgment a lint should refuse
#     rather than guess"; the measured cost (ff5b9634) was the
#     `gate-verdict` bundle unable to gain `refused` to match a
#     Workflow row already published with it, leaving the two halves of
#     one contract disagreeing and a gate refusal recorded as `lost`.
#     Every other shape — a value removed, a rename, a change between
#     enum and scalar — is still refused;
#   - an existing field may not be REMOVED (the UNION validator stops
#     requiring it, other consumers — sim faker, surfaces — stop
#     seeing it; removal is a contract change that belongs behind the
#     versioned path, not a restart);
#   - a kind may not be removed while it declares a required field; a
#     kind that required nothing may leave (2026-09-24, a8991c86) —
#     its steps then validate permissively, which only loosens.
# A brand-new kind may declare anything: nothing in flight carries it.
# required -> optional stays legal — loosening strands nobody.
#
# The real fix, if bundle edits stop being rare, is versioning the
# bundle and pinning steps to it the way jobs pin workflow versions
# (cdc23602 option 2). This ratchet is option 1: a few mechanical
# lines that turn an invisible hazard into a refused commit.
set -uo pipefail

LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$LINT_DIR/lib/trunk-ref.sh" || exit 3
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3

LINT=steptype-bundle-ratchet
BUNDLE="crates/core/boss-jobs/seeds/step_types.toml"
[ -f "$BUNDLE" ] || { echo "$LINT: $BUNDLE not found" >&2; exit 1; }

# Trunk resolution is lib/trunk-ref.sh, shared with the other two
# baseline-comparing lints (a third, a-kind-bundle-does-not-tighten,
# checked a strict subset of this ratchet and was deleted on 2026-09-18,
# backlog cdf2d959). Absent trunk refs are a refusal here, not a
# silent pass — a ratchet that cannot see the trunk certifies nothing.
# A git that could not ANSWER is a different refusal, and saying "fetch
# the trunk" at it is a wrong remediation: measured on 2026-09-11 in a
# gate workspace where the trunk was present and every git command was
# exiting 128 (backlog 6b2f4a1a).
trunk=$(resolve_trunk_ref "$LINT"); rc=$?
if [ "$rc" -eq "$LINT_CANNOT_ANSWER" ]; then
    exit "$LINT_CANNOT_ANSWER"
elif [ "$rc" -ne 0 ]; then
    echo "$LINT: no trunk ref found (tried $(trunk_candidates))" >&2
    echo "  A ratchet that cannot see the trunk cannot certify anything; refusing." >&2
    exit 1
fi
mb=$(resolve_merge_base "$LINT" "$trunk" HEAD) || exit $?

# The trunk may predate the bundle file itself (it does not today, but
# a ratchet should say what it assumes): no trunk copy = nothing to
# tighten against.
#
# `git ls-tree`, NOT `git cat-file -e`, and the difference is the whole
# packet: `cat-file -e <tree>:<path>` exits 128 both for "that path is
# not in that tree" and for "this repository is unreadable", so the two
# cannot be told apart and an unreadable repo took this `exit 0`.
# `ls-tree --name-only` answers absence with exit 0 and empty output,
# and reserves non-zero for a real failure.
#
# The status is bound to a variable rather than tested inside `[ -z
# "$(…)" ]`: a command substitution inside a test discards the exit
# status, which is the same mistake one layer down.
bundle_at_base=$(git_answer "$LINT" 0 ls-tree --name-only "$mb" -- "$BUNDLE") || exit $?
if [ -z "$bundle_at_base" ]; then
    echo "$LINT: $BUNDLE absent at merge-base — nothing to ratchet"
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

# Read the baseline file, THEN flatten it. `git show … | flatten` put git
# inside a pipeline, where its status is not the one bash reports, so a
# failed read arrived as an empty parse — caught below by the non-vacuity
# guard, but reported as "the parse broke, fix the scraper", which is a
# wrong diagnosis a reader then has to go disprove.
base_file=$(git_answer "$LINT" 0 show "$mb:$BUNDLE") || exit $?
base_rows=$(printf '%s\n' "$base_file" | flatten)
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

# Is `head` the same pipe-enum as `base` plus zero or more values? Both
# sides must BE pipe-enums (a scalar on either side is a change of
# shape, not a widening), and every value the trunk declared must still
# be there. Anything else answers no and is refused above.
enum_widens() {
    local base="$1" head="$2" v
    case "$base" in *'|'*) ;; *) return 1 ;; esac
    case "$head" in *'|'*) ;; *) return 1 ;; esac
    local IFS='|'
    for v in $base; do
        case "|$head|" in *"|$v|"*) ;; *) return 1 ;; esac
    done
    return 0
}

# Kinds that LEAVE and may: on the trunk, absent here, and declaring no
# required field there. An unknown kind validates permissively, so a
# kind that required nothing tightens no in-flight step when it goes,
# and it withdraws no promise either — nothing was promised present at
# done. A kind with a required field still may not leave (below): a
# rule or surface reading its done metadata was promised that field.
# Added 2026-09-24 for the `marketing-launch` kind, retired with the
# launch calendar (design 2ea444f5, backlog a8991c86) — until then a
# kind could never leave the bundle at all. Whether anything is still
# IN FLIGHT on a leaving kind is the car's measurement, not this
# lint's: the tree cannot see the live registry, and it matters — the
# dispatcher reads an unknown kind as decision-shaped, so an in-flight
# step of a removed kind would be routed to a person.
retiring=$(awk -F'\t' '
    NR == FNR { if ($3 == "KIND") here[$1] = 1; next }
    $3 == "KIND" && !($1 in here) { gone[$1] = 1 }
    $3 != "KIND" && $4 == "true"  { req[$1] = 1 }
    END { for (k in gone) if (!(k in req)) print k }
' <(printf '%s\n' "$head_rows") <(printf '%s\n' "$base_rows"))

while IFS=$'\t' read -r kind field ftype freq; do
    [ -n "$kind" ] || continue
    if grep -qxF "$kind" <<< "$retiring"; then
        [ "$ftype" = "KIND" ] &&
            echo "steptype-bundle-ratchet: kind \`$kind\` leaves the bundle — it required no field, so no in-flight step tightens"
        continue
    fi
    if [ "$ftype" = "KIND" ]; then
        # A kind existing on trunk must still exist unless it required
        # nothing (above): removal changes the contract of every
        # in-flight step of that kind (unknown kinds validate
        # permissively) through the unversioned door.
        # Here-strings, not `printf | grep -q`: under pipefail a `grep -q`
        # that exits at its match SIGPIPEs the multi-line writer and the
        # pipeline reports 141 for a row that IS present — here a false
        # "kind removed"; below, a real tightening waved through as a
        # new kind (measured in a-kind-bundle-does-not-tighten, 28af807c).
        if ! grep -qxF "$(printf '%s\t-\tKIND\t-' "$kind")" <<< "$head_rows"; then
            say "steptype-bundle-ratchet: kind \`$kind\` exists on the trunk and is removed here." \
                " It declares a required field, which every reader of its done metadata was promised;" \
                " retire behaviour through the versioned workflow path instead (cdc23602)."
        fi
        continue
    fi
    head_row=$(awk -F'\t' -v k="$kind" -v f="$field" '$1==k && $2==f { print; exit }' <<<"$head_rows")
    if [ -z "$head_row" ]; then
        say "steptype-bundle-ratchet: field \`$field\` on kind \`$kind\` exists on the trunk and is removed here — a bundle contract change with no version to pin against."
        continue
    fi
    head_type=$(printf '%s' "$head_row" | cut -f3)
    head_req=$(printf '%s' "$head_row" | cut -f4)
    if [ "$head_type" != "$ftype" ] && ! enum_widens "$ftype" "$head_type"; then
        say "steptype-bundle-ratchet: \`$kind.$field\` changes field_type \`$ftype\` -> \`$head_type\` — the type IS the value contract; change it through the versioned workflow path. (A pipe-enum that only GAINS values is the one exception, and this is not one.)"
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
    grep -qxF "$(printf '%s\t-\tKIND\t-' "$kind")" <<< "$base_rows" || continue  # new kind: free
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
lint_scanned "$LINT" "$base_kinds" "step kind(s) at the merge-base with $trunk"
echo "steptype-bundle-ratchet: bundle only loosened or grew optional fields ($base_kinds kinds checked against $trunk)"
