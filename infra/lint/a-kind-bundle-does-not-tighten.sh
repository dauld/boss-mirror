#!/usr/bin/env bash
#
# a-kind-bundle-does-not-tighten — half of a step's completion contract
# is unversioned, so tightening it is a retroactive demand on live work.
#
# THE ASYMMETRY
# -------------
# A completion contract lives in two places and only one of them is
# versioned:
#
#   WORKFLOW SPEC   — versioned, append-only, in-flight packets PINNED
#                     to the version they were admitted under. Editing
#                     it cannot disturb work already moving, and
#                     re-pinning is guarded by
#                     protocol_conversion::convertibility.
#
#   STEPTYPE BUNDLE — this file. One Arc<StepRegistry>, built at
#                     startup, shared by the whole API. There is no
#                     step_type_version anywhere in the tree: a step
#                     does not record which bundle it materialized
#                     under, so it cannot be pinned to one.
#
# The completion validator checks the UNION of the two (http/steps.rs,
# "the union of the kind bundle's fields and the step's own authored
# fields"). So adding a `required` field to an existing kind retightens
# every in-flight packet of that kind at the next restart — no version
# bump, no pinning, no convertibility check, and no way for a packet to
# stay on the contract it was admitted under. A step that was
# completable a minute ago starts refusing.
#
# convertibility() cannot see this. It compares two WorkflowSpecs, and
# the bundle is in neither of them.
#
# HOW CLOSE THIS CAME
# -------------------
# 2026-08-29. backlog-item v2 moved `design-review` from kind `task` to
# kind `answer-question` to make a decision record its verdict. That is
# the VERSIONED path: correctly pinned, in-flight v1 packets untouched.
# The same effect could have been had by adding `verdict` to `task`'s
# bundle — one line, in this file — and every live `task` step in every
# protocol would have started refusing completion. Both edits look
# equally routine in a diff. This check is the difference.
#
# WHAT IS ALLOWED
# ---------------
# Adding an OPTIONAL field: asks nothing of anyone. Adding a whole NEW
# kind, with required fields or without: nothing has materialized under
# it, so there is no live packet to strand. Removing a requirement, or
# relaxing one to optional: strictly looser.
#
# REFUSED: a field that becomes required on a kind that already exists.
#
# This is a ratchet, not a ban. When a bundle genuinely must tighten,
# the answer is to version the bundle (backlog-item cdc23602) — not to
# delete this check.

set -uo pipefail
LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
# shellcheck source=infra/lint/lib/trunk-ref.sh
. "$LINT_DIR/lib/trunk-ref.sh"

LINT=a-kind-bundle-does-not-tighten
SEEDS="crates/core/boss-jobs/seeds/step_types.toml"

# Trunk resolution is lib/trunk-ref.sh, shared with the other three
# baseline-comparing lints (§9a: four copies of this walk, one defect in
# all four). An absent trunk ref stays a skip; a git that could not
# answer does not, because that skip exits 0 and a gate reads 0 as a pass
# on a tree nothing looked at (measured 2026-09-11, backlog 6b2f4a1a).
BASE=$(resolve_trunk_ref "$LINT"); rc=$?
if [ "$rc" -eq "$LINT_CANNOT_ANSWER" ]; then
    exit "$LINT_CANNOT_ANSWER"
elif [ "$rc" -ne 0 ]; then
    echo "$LINT: no trunk ref found (tried $(trunk_candidates)) — skipping"
    exit 0
fi
MB=$(resolve_merge_base "$LINT" "$BASE" HEAD) || exit $?

# Emits "<kind>\t<field>" for every REQUIRED field. Hand-parsed rather
# than via tomllib, which is 3.11+ and this runs on whatever the gate
# host has; the file's shape is regular and the parser fails loudly
# rather than guessing.
extract() {
    python3 -c '
import re, sys
kind = None
field = None
required = False
out = []
def flush():
    if kind and field and required:
        out.append(kind + "\t" + field)
for line in sys.stdin:
    s = line.strip()
    if s == "[[step_type]]":
        flush(); kind = None; field = None; required = False
    elif s == "[[step_type.fields]]":
        flush(); field = None; required = False
    else:
        m = re.match(r"^kind\s*=\s*\"([^\"]+)\"", s)
        if m and field is None:
            kind = m.group(1)
        m = re.match(r"^name\s*=\s*\"([^\"]+)\"", s)
        if m:
            field = m.group(1)
        if re.match(r"^required\s*=\s*true", s):
            required = True
flush()
print("\n".join(sorted(set(out))))
'
}

# Every kind declared, required fields or not. This is deliberately NOT
# derived from the required-field list: a kind whose fields are all
# optional contributes no pairs, so deriving "kinds that existed" from
# the pairs would classify it as brand new and wave through its FIRST
# required field — the single most dangerous edit this check exists to
# refuse, and the one it missed when self-tested against `task`.
extract_kinds() {
    python3 -c '
import re, sys
in_type = False
out = []
for line in sys.stdin:
    s = line.strip()
    if s == "[[step_type]]":
        in_type = True
        continue
    if s.startswith("[["):
        in_type = False
        continue
    if in_type:
        m = re.match(r"^kind\s*=\s*\"([^\"]+)\"", s)
        if m:
            out.append(m.group(1))
            in_type = False
print("\n".join(sorted(set(out))))
'
}

# This read already refused rather than passing vacuously — it is the one
# place in these four lints that did. What it could not say is WHY, because
# `2>/dev/null` dropped git's words: "could not read" covered an absent
# baseline, an unreadable repository and a full disk alike. `git_answer`
# keeps git's own message and separates the branch-shaped failure (exit 1,
# below) from the machine-shaped one (exit 3, printed there).
BASE_FILE=$(git_answer "$LINT" 0 show "$MB:$SEEDS") || exit $?
if [ -z "$BASE_FILE" ]; then
    echo "$LINT: $SEEDS is empty at $MB — refusing rather than" >&2
    echo "  passing vacuously. An unreadable baseline proves nothing, and this check's whole" >&2
    echo "  job is to compare against one." >&2
    exit 1
fi

BEFORE=$(printf '%s\n' "$BASE_FILE" | extract)
KINDS_BEFORE=$(printf '%s\n' "$BASE_FILE" | extract_kinds)
AFTER=$(extract < "$SEEDS")

if [ -z "$AFTER" ]; then
    echo "a-kind-bundle-does-not-tighten: parsed zero required fields from $SEEDS — refusing rather than passing vacuously" >&2
    exit 1
fi

violations=0
while IFS=$'\t' read -r kind field; do
    [ -z "${kind:-}" ] && continue
    printf '%s\n' "$BEFORE" | grep -qxF "$kind	$field" && continue
    printf '%s\n' "$KINDS_BEFORE" | grep -qxF "$kind" || continue
    if [ "$violations" -eq 0 ]; then
        echo "a-kind-bundle-does-not-tighten: a required field was added to an existing step kind." >&2
        echo "  A StepType bundle is GLOBAL and UNPINNED — no step_type_version exists — so this" >&2
        echo "  retightens every in-flight packet of that kind at the next restart, with no" >&2
        echo "  version bump and no way to stay on the old contract." >&2
        echo "  Allowed instead: an OPTIONAL field, or a requirement on the WORKFLOW step" >&2
        echo "  (versioned, and in-flight packets stay pinned). See backlog-item cdc23602." >&2
    fi
    echo "    $kind.$field is now required" >&2
    violations=$((violations + 1))
done <<< "$AFTER"

if [ "$violations" -gt 0 ]; then
    exit 1
fi

echo "a-kind-bundle-does-not-tighten: clean — $(printf '%s\n' "$AFTER" | wc -l | tr -d ' ') required bundle fields, none newly required on an existing kind"
