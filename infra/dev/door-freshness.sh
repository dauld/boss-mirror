# door-freshness.sh — does the door that just ran come from a checkout
# that is behind origin/main? Sourced by every door in this directory;
# it is not executable and runs nothing by itself.
#
# WHY (backlog 0b36dd65, 2026-09-19). /work/tools/bin/boss-api, the
# boss shim, wt-cargo and wt-web are symlinks into THIS directory in
# the operator's main checkout, so every door runs whatever that
# checkout's working copy last held — and nothing keeps it on
# origin/main. CLAUDE.md §Doors asserts the opposite in its own words
# ("which is why the main checkout stays on origin/main"); measured at
# 17:05 that day the checkout was two commits behind, and a builder
# dogfooding the new BOSS_SOR_SERVICE route got HTTP:404 from the jobs
# port through the pod's boss-api while its own worktree copy answered
# HTTP:200. A 404 from the wrong port is indistinguishable from a
# surface that does not exist — CLAUDE.md's own "a wrong target answers
# instead of erroring". The checkout was fast-forwarded BY HAND twice
# that day; a door that says nothing between those two moments is the
# defect, so the door says it.
#
# NO NETWORK, EVER. A door is called constantly, and a `git fetch` per
# call would be slow and a new failure mode (a dark forge must not take
# every door down with it — the same rule the shim states for its own
# origin/main read). The comparison is against
# refs/remotes/origin/main, whatever the last fetch left there — and
# worktrees SHARE remote-tracking refs, so a builder's `git fetch
# origin` in its own worktree refreshes the answer this helper gives
# about the main checkout, at no cost here. ONE local git call decides
# the common case — a checkout standing on origin/main is a string
# comparison after it — and the whole door measured 62 ms end to end
# against the live jobs API, the LAN round trip included.
#
# BEHIND, not merely DIFFERENT. Three questions, and all must say so:
#   * HEAD is an ANCESTOR of origin/main — a checkout that has simply
#     not pulled, never one that has diverged;
#   * the file on disk is HEAD's version of it, not a local edit;
#   * and HEAD's version differs from origin/main's.
# The middle two are what keep a builder quiet. A worktree on its own
# branch is not an ancestor of origin/main, and a door being EDITED
# (this car was written that way) is not HEAD's version — both are a
# developer changing the door, not a checkout that forgot to pull, and
# the first draft of this helper warned about both. Measured against
# its own author at 17:38: origin/main had moved one commit while the
# branch was uncommitted, and the door called itself stale. So does a
# copy that no git can answer about — the gate's fetched workspace, a
# file copied to a host with no clone: visibility is best-effort
# (CLAUDE.md §Diagnosis), and a door that refuses because it cannot SEE
# is worse than the stale answer it prevents.
#
# WHAT THE CALLER DOES WITH IT is the caller's decision, and it splits
# by what an answer costs: boss-api warns on a read and REFUSES a write
# (exit 78), because a read's warning rides beside the answer while a
# write lands an immutable fact in the audit log; boss, wt-cargo and
# wt-web warn and proceed, because refusing `boss gate` mid-build would
# strand a builder at the one moment it matters (the shim's own
# history, backlog 49d9e99d). BOSS_DOOR_FRESHNESS=off silences the
# whole helper for someone who means it.
#
# A CALLER OUTSIDE THE ROSTER means it: hooks/session-end.sh closes its
# own session's packet through boss-api with the override set, and
# first calls door_is_stale itself so each write past a stale door is
# one journal line (backlog 584dc9da). A guard added here changes every
# caller of a door, and the hooks are callers no roster enumerates.
#
# Pinned by crates/core/boss-testing/tests/a_door_knows_its_copy_is_stale.rs,
# which also refuses a new executable in this directory that does not
# source this file (CLAUDE.md §9a: a comment asking the next person to
# remember is not a mechanism).

# door_is_stale "$0" — prints one sentence of facts and returns 0 when
# the door's own copy is behind origin/main; returns 1, silently, in
# every other case including every case it cannot judge.
door_is_stale() {
    local file dir top rel both head main ours theirs disk behind plural
    if [ "${BOSS_DOOR_FRESHNESS:-}" = off ]; then return 1; fi
    if ! command -v git >/dev/null 2>&1; then return 1; fi
    file=$(readlink -f "$1" 2>/dev/null) || return 1
    if [ -z "$file" ] || [ ! -e "$file" ]; then return 1; fi
    dir=$(dirname "$file")

    # ONE git call decides the common case: both shas at once, and a
    # checkout standing exactly on origin/main is answered by a string
    # comparison with no second fork.
    both=$(git -C "$dir" rev-parse HEAD refs/remotes/origin/main 2>/dev/null) || return 1
    # Split on whitespace with the shell, not with $'\n' or a second
    # fork: this file is sourced, and a caller under dash reads a
    # bashism as a literal (the a-sh-script-parses-under-sh lesson).
    # A function's `set --` touches only its own positional parameters,
    # and $1 is already read.
    # shellcheck disable=SC2086
    set -- $both
    head=${1:-}
    main=${2:-}
    if [ -z "$head" ] || [ -z "$main" ] || [ "$head" = "$main" ]; then return 1; fi
    # Behind means ANCESTOR OF: a diverged branch is a developer's, and
    # a checkout ahead of origin/main is nobody's problem.
    if ! git -C "$dir" merge-base --is-ancestor "$head" "$main" >/dev/null 2>&1; then return 1; fi

    top=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) || return 1
    if [ -z "$top" ]; then return 1; fi
    rel=${file#"$top"/}
    # A door origin/main does not have is a new one, not a stale one;
    # a door that differs from the checkout's OWN commit is being
    # edited, and an edit is not staleness.
    theirs=$(git -C "$dir" rev-parse --verify -q "$main:$rel" 2>/dev/null) || return 1
    ours=$(git -C "$dir" rev-parse --verify -q "$head:$rel" 2>/dev/null) || return 1
    disk=$(git -C "$dir" hash-object -- "$file" 2>/dev/null) || return 1
    if [ -z "$theirs" ] || [ -z "$ours" ] || [ -z "$disk" ]; then return 1; fi
    if [ "$disk" != "$ours" ] || [ "$theirs" = "$disk" ]; then return 1; fi

    behind=$(git -C "$dir" rev-list --count "$head..$main" 2>/dev/null) || behind=
    case ${behind:-empty} in
        empty | *[!0-9]*) behind=1 ;;
    esac
    if [ "$behind" = 1 ]; then plural=commit; else plural=commits; fi

    # %.8s, not ${x:0:8} and not a `cut` fork: see the split above —
    # a sourced file may be read by a shell that has no such expansion.
    printf '%s ran from %s, whose checkout is at %.8s — %s %s behind origin/main %.8s — and %s differs there, so this copy is not the tree'"'"'s and its answer can be well-formed and wrong (backlog 0b36dd65). Fix: git -C %s merge --ff-only origin/main' \
        "${file##*/}" "$file" "$head" "$behind" "$plural" "$main" "$rel" "$top"
}
