#!/usr/bin/env bash
# checkout-lock — ONE lock for every git user of the forge checkout.
#
# Sourced, never run. Functions only; nothing happens at source time.
# Sourced by cluster-deploy-runner.sh and forge-converge.sh; tested by
# crates/core/boss-testing/tests/checkout_lock_sh.rs.
#
# WHY. Three git users share /home/david/boss on the forge host:
# cluster-deploy-runner.sh (its :01/:11/:21 timer AND the
# merge-triggered `converge` ops-request, which starts the same unit
# within a second of any merge), forge-converge.sh (a fetch + checkout
# on the same tick, as the checkout's owner), and whoever runs git by
# hand. On 2026-09-07 22:01 train #257 merged, the converge fired at
# 22:01:23 and died on `cannot lock ref 'refs/remotes/forgejo/main'`
# (forge-converge's fetch held the ref); the 22:11 timer retry died on
# a stale `.git/index.lock`; the 22:21 retry built. Two lost cycles, a
# twenty-minute converge lag, and the next board held on an occupied
# track (backlog d66f92b2).
#
# WHAT. Every fetch/pull/checkout of the checkout goes through
# `checkout_git`, which
#   1. takes `.git/boss-converge.lock` with flock(1) — the same file
#      whichever user (root's forge-converge drops to the owner; the
#      runner IS the owner; a human is usually the owner) — so lock-
#      aware users never contend on git's own ref/index locks at all;
#   2. sweeps a STALE `.git/index.lock` first: a lock file with no live
#      git process working in the checkout is a corpse from a killed
#      run, and git will refuse forever until someone removes it.
#      Removed only when /proc shows no `git` with its cwd or command
#      line in the checkout, and only once it is older than
#      CHECKOUT_STALE_AFTER — and the removal is logged;
#   3. retries the git command on the two lock errors (`cannot lock
#      ref`, `index.lock`) with a short backoff — a non-lock-aware git
#      (a human's) can still collide, and a collision is transient by
#      nature. Any OTHER error is a verdict and is not retried.
#
#   checkout_git REPO GIT-ARGS...     the door: lock + sweep + retry
#   with_checkout_lock REPO CMD...    run CMD holding the lock
#   git_retry REPO GIT-ARGS...        the retry alone (caller holds the lock)
#   clear_stale_index_lock REPO       the sweep alone
#
# Knobs (env, all optional):
#   CHECKOUT_LOCK_WAIT   seconds to wait for the lock      (600)
#   CHECKOUT_RETRIES     tries per git command             (5)
#   CHECKOUT_BACKOFF     seconds between tries             (6)
#   CHECKOUT_STALE_AFTER index.lock age before it is stale (30)
#
# The lock is a FILE lock, not a wrapper around the whole converge: a
# build takes minutes and forge-converge must not queue behind it. The
# critical section is the git command and nothing else.

checkout_lock_file() { printf '%s/.git/boss-converge.lock\n' "$1"; }

_checkout_log() { echo "checkout-lock: $*" >&2; }

# with_checkout_lock REPO CMD... — run CMD holding the checkout's lock.
# Waits up to CHECKOUT_LOCK_WAIT; a lock that never comes is EX_TEMPFAIL
# (75), loud. CMD may be a function of the sourcing shell.
with_checkout_lock() {
    local repo="$1"; shift
    local lock wait="${CHECKOUT_LOCK_WAIT:-600}"
    [ -d "$repo/.git" ] || { _checkout_log "$repo has no .git — nothing to lock"; return 1; }
    lock=$(checkout_lock_file "$repo")
    # Create if absent, then open READ-ONLY: flock(2) wants a
    # descriptor, not write access, and the file may belong to another
    # user (root created it, the owner takes it, or the reverse).
    [ -e "$lock" ] || { : >> "$lock"; } 2>/dev/null || true
    (
        exec 9< "$lock" || exit 1
        if ! flock -w "$wait" 9; then
            _checkout_log "could not take $lock within ${wait}s — another git user holds the checkout"
            exit 75
        fi
        "$@"
    )
}

# git_holds_checkout REPO — 0 when a live git process is working in
# REPO (cwd inside it, or the path on its command line). Reads /proc;
# a `git` whose cwd AND command line are both unreadable counts as
# holding it — the sweep must never guess in favour of deleting.
# Is this /proc entry still a live process?
#
# Extracted so the rule can be TESTED. The race it settles cannot be
# driven from fixtures — it needs a pid to disappear between one read
# and the next inside a single loop iteration — so the decision itself
# is what a test can hold onto, and a test that cannot reach the branch
# it claims to cover is worse than none.
pid_is_present() { [ -d "$1" ]; }

git_holds_checkout() {
    local repo p comm cwd cmd
    repo=$(cd "$1" 2>/dev/null && pwd -P) || return 1
    # The process table's root, overridable ONLY so the race below can
    # be tested with planted fixtures: a real /proc cannot be made to
    # drop a pid at a chosen instant, and a rule this subtle should not
    # rest on a comment.
    for p in "${PROC_ROOT:-/proc}"/[0-9]*; do
        # `2>/dev/null` BEFORE the input redirect, not after. Redirects
        # are applied left to right, so with the old order a vanished
        # pid printed `/proc/N/comm: No such file or directory` to the
        # real stderr on every scan — noise that a caller could not
        # silence and that made a clean sweep look like a fault.
        { read -r comm < "$p/comm"; } 2>/dev/null || continue
        [ "$comm" = "git" ] || continue
        cwd=$(readlink "$p/cwd" 2>/dev/null) || cwd=""
        case "$cwd" in "$repo" | "$repo"/*) return 0 ;; esac
        cmd=$({ tr '\0' ' ' < "$p/cmdline"; } 2>/dev/null) || cmd=""
        case "$cmd" in *"$repo"*) return 0 ;; esac
        # BOTH UNREADABLE MEANS ONE OF TWO THINGS, and only one of them
        # is a reason to keep the lock. A LIVE git we cannot inspect is
        # genuinely unknown, and the sweep must never guess in favour of
        # deleting. A git that EXITED between the `comm` read above and
        # these two reads holds nothing at all — it is not there any
        # more. The two are told apart by asking whether the process
        # still exists, which is what /proc/<pid> being gone means.
        #
        # Conflating them was a race, and it fired: on 2026-09-09 at
        # 05:20 the CI box was busy enough that some git exited mid-scan
        # and `a_stale_index_lock_is_removed_only_when_no_git_process_
        # holds_the_checkout` failed, reporting a live holder for a lock
        # 0 seconds old with nothing holding it. On the forge the same
        # race leaves a genuinely stale index.lock in place — the exact
        # condition this sweep exists to clear, and the one that cost
        # two converge cycles on 2026-09-07.
        if [ -z "$cwd" ] && [ -z "$cmd" ]; then
            pid_is_present "$p" || continue   # it exited; holds nothing
            return 0                          # there, and unreadable
        fi
    done
    return 1
}

# clear_stale_index_lock REPO — remove .git/index.lock when nobody
# holds it: older than CHECKOUT_STALE_AFTER and no live git in REPO.
# Returns 0 when no lock remains, 1 when one was left in place.
clear_stale_index_lock() {
    local repo="$1" lock="$1/.git/index.lock" stale_after="${CHECKOUT_STALE_AFTER:-30}" mtime age
    [ -e "$lock" ] || return 0
    mtime=$(stat -c %Y "$lock" 2>/dev/null) || mtime=0
    age=$(( $(date +%s) - mtime ))
    if [ "$age" -lt "$stale_after" ]; then
        _checkout_log "index.lock in $repo is ${age}s old (stale after ${stale_after}s) — leaving it"
        return 1
    fi
    if git_holds_checkout "$repo"; then
        _checkout_log "index.lock in $repo is ${age}s old but a git process is live there — leaving it"
        return 1
    fi
    if rm -f "$lock"; then
        _checkout_log "removed stale $lock (${age}s old, no git process in $repo)"
        return 0
    fi
    _checkout_log "could not remove $lock"
    return 1
}

_git_lock_error() {
    grep -qE "cannot lock ref|index\.lock|could not lock|Unable to create .*\.lock" "$1"
}

# git_retry REPO GIT-ARGS... — `git -C REPO ARGS`, retried on the two
# lock errors up to CHECKOUT_RETRIES times, CHECKOUT_BACKOFF apart,
# sweeping a stale index.lock between tries. stdout is git's; stderr
# is git's plus one line per retry.
git_retry() {
    local repo="$1"; shift
    local tries="${CHECKOUT_RETRIES:-5}" backoff="${CHECKOUT_BACKOFF:-6}" n=1 rc errf
    errf=$(mktemp -t checkout-lock.XXXXXX)
    while :; do
        rc=0
        git -C "$repo" "$@" 2> "$errf" || rc=$?
        if [ "$rc" -eq 0 ]; then
            cat "$errf" >&2; rm -f "$errf"; return 0
        fi
        if ! _git_lock_error "$errf"; then
            cat "$errf" >&2; rm -f "$errf"; return "$rc"
        fi
        if [ "$n" -ge "$tries" ]; then
            cat "$errf" >&2; rm -f "$errf"
            _checkout_log "git $1 in $repo still lock-blocked after $tries tries — giving up (exit $rc)"
            return "$rc"
        fi
        _checkout_log "git $1 in $repo hit a git lock (try $n of $tries: $(head -n1 "$errf")) — retrying in ${backoff}s"
        sleep "$backoff"
        clear_stale_index_lock "$repo" || true
        n=$((n + 1))
    done
}

_checkout_git_locked() {
    local repo="$1"; shift
    clear_stale_index_lock "$repo" || true
    git_retry "$repo" "$@"
}

# checkout_git REPO GIT-ARGS... — the door every fetch/pull/checkout of
# the shared checkout goes through.
checkout_git() {
    local repo="$1"; shift
    with_checkout_lock "$repo" _checkout_git_locked "$repo" "$@"
}
