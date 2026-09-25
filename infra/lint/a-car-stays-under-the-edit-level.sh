#!/usr/bin/env bash
# preflight: serial — reads the edit level off the live jobs API through lib/sor-read.sh; one reader of the record per pre-flight
#
# a-car-stays-under-the-edit-level — the gate half of the hosting door
# (a479faf7; design 01c3cc3f "Tiers are one registry, read three
# ways", reader 3): a car whose diff against the trunk touches a path
# closer to the core than the instance's edit level admits is refused,
# naming the FIRST offending path, its tier and the level.
#
# WHY THIS EXISTS
# ---------------
# David, 2026-09-18: "We should use crate tiers as safety levels for
# our hosted model. Confident users can allow IT department editing of
# all files. Otherwise they won't touch core, and maybe even a data
# only mode." The level is TENANT data — `[meta] edit_level` in the
# tenant manifest, a tier NAME from infra/platform/tiers.toml (`data`
# is data-only, `tenants` adds the tenant's own crate and site,
# `modules` the company layer, `core` everything) — and it is read at
# BOTH doors the design names with ONE predicate: `boss dispatch`
# refuses a packet that declares `metadata.paths` above the level
# before an agent spends, and this lint refuses what slipped past a
# hand-run, on the diff the gate is actually judging.
#
# THE ONE PREDICATE. boss_core::tiers::TierMap::first_above is the
# Rust half; `edit_level_first_above` in infra/lint/lib/tiers.sh is
# the shell half this lint calls, held equal to the Rust by
# crates/core/boss-testing/tests/tiers_sh.rs over a fixture of paths
# at every level. This file parses nothing about tiers itself.
#
# WHERE THE LEVEL COMES FROM. The instance, through the jobs API —
# `GET $BOSS_JOBS_URL/api/tenant/edit-level`, which answers the
# manifest's declared word off the file the launcher hands every
# service. Not the checkout: the product tree has no tenant.toml at
# its root, and the instance's tenant (the tenant repo) is in no
# checkout a gate sees. Not a hosting default: the reader takes the
# DECLARED word, and an instance that declares none (`null`) — every
# instance today — or has no level door at all (404, a server from
# before this car) enforces nothing, and says so. The hosted default
# (`data`) is written where a hosted tenant is made (`boss tenant init`
# scaffolds it), never assumed here, because assuming it would have
# refused every code car on the operator's own pipeline the converge
# after this landed (measured: the tenant repo's manifest declares no
# level, 2026-09-19).
#
# WHAT IS JUDGED. The paths this car lands: the branch's commits over
# the trunk (lib/trunk-ref.sh resolves it; BOSS_TRUNK_REF overrides)
# plus whatever is staged or dirty, so a builder's `--quick` before
# the commit sees the same verdict the gate will. Empty on the trunk
# by construction, which is why this is not a scanner
# (a_lint_that_scanned_nothing_is_red.rs lists it).
#
# EXITS. 0 every path admitted (or nothing enforced); 1 a crossing —
# a fact about the BRANCH, the author's to fix by narrowing the car or
# raising the level in the manifest; 3 (LINT_CANNOT_ANSWER) the level
# could not be read — the instance dark, an answer that is not JSON, a
# 5xx, a level the map does not know — or git could not answer. Never
# a verdict on a guess: the gate turns 3 into a refusal receipt, not a
# red, and `--quick` warns.
#
#   target: <url> (override with BOSS_JOBS_URL)   is printed on a skip
#   so the gate's warning names the instance it could not read.

set -uo pipefail

LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.." || exit 1
. "$LINT_DIR/lib/git-answer.sh" || exit 3
. "$LINT_DIR/lib/trunk-ref.sh" || exit 3
. "$LINT_DIR/lib/tiers.sh" || exit 3
# The read waits out a rollout before it refuses (backlog 834ddb7c).
. "$LINT_DIR/lib/sor-read.sh" || exit 3

LINT=a-car-stays-under-the-edit-level
BASE="${BOSS_JOBS_URL:-http://boss-jobs-internal.boss.svc.cluster.local:7900}"
URL="$BASE/api/tenant/edit-level"

skip() {
    echo "$LINT: $LINT_CANNOT_ANSWER_MARKER — the instance's edit level could not be read — $1" >&2
    echo "  target: $URL (override with BOSS_JOBS_URL)" >&2
    echo "  NOTHING IS CLAIMED about whether this car stays under the level: no path was" >&2
    echo "  judged. An infrastructure refusal, not a verdict on the branch." >&2
    exit "$LINT_CANNOT_ANSWER"
}

# ---------------------------------------------------------------------------
# The level, off the instance.
# ---------------------------------------------------------------------------
command -v curl >/dev/null 2>&1 || skip "curl is not on this box"
command -v jq >/dev/null 2>&1 || skip "jq is not on this box"

body=$(mktemp) || exit 1
trap 'rm -f "$body"' EXIT
code=$(lint_sor_read "$LINT" "the jobs API" "$URL" "$body")
case "$code" in
    200) ;;
    404)
        # A server from before a479faf7: it has no level door, so it
        # declares no level. Said out loud, never silent.
        echo "$LINT: no edit level — $URL answered 404 (this instance has no level door), so nothing is enforced"
        echo "$LINT: clean"
        exit 0
        ;;
    *) skip "$URL answered HTTP $code" ;;
esac

level=$(jq -r 'if type == "object" and has("edit_level") then (.edit_level // "") else error("not an edit-level answer") end' "$body" 2>/dev/null) \
    || skip "$URL answered something other than an edit-level object"
if [ -z "$level" ]; then
    echo "$LINT: no edit level — the instance's manifest declares none ($(jq -r '.manifest // "no manifest"' "$body")), so nothing is enforced"
    echo "$LINT: clean"
    exit 0
fi

# ---------------------------------------------------------------------------
# The paths this car lands: commits over the trunk, plus staged and dirty.
# ---------------------------------------------------------------------------
git_can_answer "$LINT" || exit "$LINT_CANNOT_ANSWER"
trunk=$(resolve_trunk_ref "$LINT")
status=$?
if [ "$status" -eq "$LINT_CANNOT_ANSWER" ]; then exit "$status"; fi
if [ "$status" -ne 0 ]; then
    skip "no trunk ref to diff against (tried $(trunk_candidates)) — set BOSS_TRUNK_REF"
fi
mb=$(resolve_merge_base "$LINT" "$trunk") || exit $?
committed=$(git_answer "$LINT" 0 diff --name-only "$mb" HEAD) || exit $?
dirty=$(git_answer "$LINT" 0 diff --name-only HEAD) || exit $?
untracked=$(git_answer "$LINT" 0 ls-files --others --exclude-standard) || exit $?
paths=$(printf '%s\n%s\n%s\n' "$committed" "$dirty" "$untracked" | awk 'NF && !seen[$0]++')

# ---------------------------------------------------------------------------
# The verdict, from the one predicate.
# ---------------------------------------------------------------------------
above=$(printf '%s\n' "$paths" | edit_level_first_above "$level")
status=$?
case "$status" in
    0)
        echo "$LINT: edit level \`$level\` admits every path this car touches ($(printf '%s\n' "$paths" | awk 'NF' | wc -l | tr -d ' ') path(s))"
        echo "$LINT: clean"
        exit 0
        ;;
    1) ;;
    *) skip "the instance declares an edit level the tier map does not know (\`$level\`)" ;;
esac

path=${above%%$'\t'*}
tier=${above#*$'\t'}
rank=$(tier_rank "$level")
echo "VIOLATION: $LINT"
if [ -n "$tier" ]; then
    echo "    edit level \`$level\` (rank $rank) does not admit \`$path\` — it is \`$tier\` (rank $(tier_rank "$tier")), closer to the core"
    echo "    than the level allows this tenant's changes to reach."
else
    echo "    edit level \`$level\` (rank $rank) does not admit \`$path\` — no tier claims it (the tree's own"
    echo "    root), which only the innermost level admits."
fi
echo "    The level is this instance's tenant manifest ([meta] edit_level, docs/tenant-contract.md);"
echo "    narrow the car to the tiers it admits, or raise the level there — a decision on the"
echo "    instance, not a quiet edit. The same predicate refused nothing at dispatch only because"
echo "    the packet declared no metadata.paths; the gate judges the diff."
exit 1
