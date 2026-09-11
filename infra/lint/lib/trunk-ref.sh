# trunk-ref.sh — which ref a baseline-comparing lint compares against,
# resolved once, in one place.
#
# WHY THIS EXISTS (§9a: a fact that lives four times). The same
# `resolve_base` walk was copy-pasted into FOUR lints —
# migrations-append-only, steptype-bundle-ratchet,
# a-new-style-has-a-caller, a-kind-bundle-does-not-tighten — each
# carrying the same `2>&1`-suppressed `git rev-parse`, and so each
# carrying the same defect: a git that could not answer read as "the ref
# is not here". Two of the four then told the operator to fetch a trunk
# that was already present; the other two skipped and exited 0, which a
# gate records as a pass. One walk, one fix.
#
# ORDER MATTERS, and it is the comment migrations-append-only carried:
# the forge is the trunk every car actually merges into; `origin` on a
# dev box is the GitHub mirror, a periodic backup that was 24 commits
# stale the day this was written, and comparing against it reports every
# already-merged change as a fresh modification. In CI the checkout's
# `origin` IS the forge, so the first hit there is correct.
# BOSS_TRUNK_REF overrides for the case where neither name applies.
#
# WHAT IT DOES NOT DECIDE. Whether a genuinely ABSENT trunk is a refusal
# or a skip stays with each lint, because the lints disagree on purpose:
# a ratchet that cannot see the trunk certifies nothing and refuses
# (exit 1), while a "what did this branch add" check with no baseline
# would flag every pre-existing line as this branch's fault and so skips
# (exit 0). Both are defensible. Neither is a thing to say when git did
# not answer at all.

# shellcheck source=infra/lint/lib/git-answer.sh
. "$(dirname "${BASH_SOURCE[0]}")/git-answer.sh"

# The candidates, and the ONE list the message is also built from, so the
# sentence a lint prints cannot drift from the walk it describes (the
# three lints' messages each said "(tried origin/main, forge/main, main)"
# while the code tried forge/main first).
TRUNK_CANDIDATES=("forge/main" "origin/main" "main")

# trunk_candidates — the refs that were tried, in order, for a message.
trunk_candidates() {
    printf '%s' "${BOSS_TRUNK_REF:+$BOSS_TRUNK_REF }${TRUNK_CANDIDATES[*]}"
}

# resolve_trunk_ref <lint>
#
#   stdout  the ref name that resolved
#   return  0 resolved
#           1 every candidate is genuinely ABSENT — git looked and said
#             so; the caller's own judgement applies
#           3 ($LINT_CANNOT_ANSWER) git could not answer; the refusal is
#             already on stderr, naming the command and git's words
resolve_trunk_ref() {
    local lint="$1" ref status
    for ref in ${BOSS_TRUNK_REF:-} "${TRUNK_CANDIDATES[@]}"; do
        git_answer "$lint" 0,1 rev-parse --verify --quiet "$ref" >/dev/null
        status=$?
        case "$status" in
            0) printf '%s\n' "$ref"; return 0 ;;
            1) continue ;;
            *) return "$LINT_CANNOT_ANSWER" ;;
        esac
    done
    return 1
}

# resolve_merge_base <lint> <base> [head]
#
# The point to compare against. No common ancestor (status 1) is an
# ANSWER — fall back to the base itself, which is what all four callers
# did — and only a git failure refuses.
#
#   stdout  the merge-base, or <base> when there is no common ancestor
#   return  0 / 3
resolve_merge_base() {
    local lint="$1" base="$2" head="${3:-HEAD}" mb status
    mb=$(git_answer "$lint" 0,1 merge-base "$base" "$head")
    status=$?
    [ "$status" -eq "$LINT_CANNOT_ANSWER" ] && return "$LINT_CANNOT_ANSWER"
    [ -n "$mb" ] || mb="$base"
    printf '%s\n' "$mb"
    return 0
}
