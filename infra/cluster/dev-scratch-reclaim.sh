#!/usr/bin/env bash
#
# dev-scratch-reclaim — keep the dev pod's two workspaces above a
# free-space floor by reclaiming REGENERABLE artifacts, and nothing
# else. The cluster-side sibling of infra/forge/disk-floor-sweep.sh.
#
# WHY THIS EXISTS. The dev pod's `scratch` is a node-local emptyDir
# with NO sizeLimit and NO reclaim (boss-dev.yaml). Measured
# 2026-09-06: /scratch/target/debug had grown past 136G and dozens of
# git worktrees had piled up under .claude/worktrees/, pushing the
# build node into repeated "system low on memory" pressure — "the
# filesystems bog down over time." Nothing ever reclaimed either:
#   - the gate runner's per-run sizeLimit backstop (gate-runner.yaml)
#     lives on a DIFFERENT, disposable pod; it does nothing for this
#     long-lived one.
#   - a scratch sizeLimit here would only take effect on a pod restart
#     (which kills the live session) and evicts rather than reclaims —
#     a separate operator decision, deliberately out of scope for this
#     script.
# This is the missing actuator: the SAFE half — measure, then reclaim
# bounded, regenerable space, stopping as soon as the floor is met.
#
# TWO WORKSPACES, TWO FLOORS, because the pod mounts two filesystems
# and each fills a different way:
#   * WORK  (/work, the ReadWriteOnce PVC): the git clone + its
#     worktrees. Reclaim = prune stale git worktrees.
#   * SCRATCH (/scratch, the node-local emptyDir): CARGO_TARGET_DIR and
#     every builder's sibling target. Reclaim = the idle siblings, least
#     recently used first, then the primary's incremental cache.
# Each is checked against its own floor and reclaimed independently.
#
# AND SINCE 2026-09-18, ONE PASS THAT SPANS BOTH AND IS NOT
# FLOOR-TRIGGERED (backlog 1933db9e, audit H11): a worktree whose
# branch is GONE — no origin/ ref left in the checkout, and either
# landed on origin/main or abandoned unpushed — clean and idle, is
# removed together with the per-worktree cargo target wt-cargo made
# for it. The judgement reads the checkout's OWN refs and never the
# forge (backlog b50a65ef, below). Measured
# that day: 203 worktrees under /work/boss, 175 of their 182 branches
# no longer on the forge, 36 target-* dirs — 364 GB on a 929 GB disk at
# 77% — and nothing here could see any of it: the floor passes wait for
# a floor that 221 GB of headroom never reaches, and the age pass
# below retires a target but never the checkout that keeps minting it.
# The disk floor is what stops a host, so the bound is kept BEFORE the
# floor, the way the stale-target pass already does.
#
# WHAT IT NEVER DOES: it never discards work. `git worktree remove`
# runs WITHOUT --force, so a worktree with uncommitted changes is
# refused and left standing; and committed work lives in the repo's
# object store regardless of the worktree, so removing a checkout only
# ever costs a `git worktree add` to recover. It never removes a
# LOCKED worktree, the main worktree, or the worktree this run is
# executing from. It never touches the build cache while a build is
# running (checked through the pod's shared PID namespace), and even
# then it removes only `incremental/` dirs, which cargo regenerates.
# It touches no path outside the two workspaces. If a floor stays
# unmet after every bounded remediation it exits non-zero LOUDLY and
# stops: escalating to `cargo clean` or deleting linked artifacts is a
# human's call, never this script's.
#
# WHAT IT RECORDS. A pass that reclaimed something, or could not meet a
# floor, files a `maintenance-dev-scratch-reclaim` packet on the system
# of record and completes its `run` step with the totals — worktrees
# removed and their size, the dirty trees it kept BY NAME, targets and
# caches dropped — through the same boss-maintenance-wrap.sh +
# boss-step.sh pair every timer chore uses. A pass that found nothing
# to do files nothing: that is a reading, not work (David, 2026-09-16),
# and twenty-four identical packets a day would bury the one that
# matters. The record never gates the work — an unreachable system of
# record costs this pass its visibility, not its reclaim.
#
# AND SINCE 2026-09-18 THE ONE PASS THAT INSTALLS RATHER THAN RECLAIMS
# (backlog c35eda6c, retro 27fad542): the pod's `boss` CLI, taken out of
# the cluster image at origin/main with infra/estate/install-cli-from-
# image.sh — the installer boss-gcp and the forge already run — into
# /work/tools/image-cli, where the shim (infra/dev/boss) prefers it
# over the hand-built /scratch/target binary. The retro counted that
# binary rebuilt by hand three times in one window because a landed
# car had changed what the CLI validates locally, and a stale one
# refused a valid verb. This pass runs here, and not in the manifest,
# because a manifest change rolls the pod and ends the operator's live
# session (memory: boss-dev-manifest-cars-restart-the-session); the
# sidecar already runs THIS script from the checkout every hour with
# git, curl, jq and tar in its image and /work writable. `--cli` runs
# that leg alone (the ten-second fix the shim's refusal names).
#
# AND SINCE 2026-09-20, ONE PASS THAT MOVES THE CHECKOUT ITSELF
# (backlog 033d1fd3): /work/boss is fast-forwarded to the origin/main it
# has already fetched, so the pod doors symlinked into its infra/dev
# stop answering from a copy the tree has moved past. It defers while a
# gate is LAUNCHING — a gate renders its runner from a tree as it starts
# — and whenever it cannot read that fact; a deferral that outlives its
# deadline is a problem on the packet, never a silent stall. The long
# form is at the pass itself.
#
# WHAT RUNS IT. The `reclaim` sidecar in boss-dev.yaml fires it hourly
# (the disk-floor-sweep.timer cadence: above the floor a pass is one
# log line; below it a pass frees GBs, well ahead of the fill rate).
# "Well ahead" was false on 2026-09-24 (backlog 3f2a08ab: the floor
# crossed to the eviction line in under 27 minutes), so since then
# infra/dev/wt-cargo also runs `--scratch-floor` — the floor's sibling
# pass alone — before every build; the long form is at that mode.
# It is also safe to run by hand:
#   kubectl exec -n boss-dev deploy/boss-dev -c reclaim -- \
#     bash /work/boss/infra/cluster/dev-scratch-reclaim.sh
#
# Tunables (env, with in-sidecar defaults):
#   BOSS_SCRATCH_FLOOR_PCT   share of /scratch's filesystem to keep
#                            free, as a percentage          (default 25)
#   BOSS_LIVE_TARGET_MIN     minutes since a sibling target was touched
#                            before the floor pass may take it (default 30)
#   BOSS_STALE_TARGET_H      hours before a sibling target dir is dead (default 12)
#   (the /work floor is not a tunable: it is the gate's floor plus a
#    margin, from infra/build-floor.env — BOSS_WORK_FLOOR_GB is retired
#    and ignored; see WORK_FLOOR_GB below)
#   BOSS_WORKTREE_MAX_AGE_H  only prune worktrees older than; also the
#                            git quiet before an UNPUSHED worktree (no
#                            origin/ ref, not on origin/main) counts as
#                            abandoned                      (default 48)
#   BOSS_WORKTREE_GRACE_H    hours of git quiet before a LANDED
#                            worktree (by sha or by content) is
#                            removable; under the /work
#                            floor it yields to BOSS_LIVE_TARGET_MIN
#                                                           (default 12)
#   BOSS_WORKTREE_IDLE_H     hours of git quiet before a main/detached
#                            worktree is removable          (default 168)
#   BOSS_FF_LAUNCH_WINDOW_SECS  how recently a gate-run must have opened
#                            to count as still LAUNCHING, and so as
#                            reading the tree                (default 120)
#   BOSS_FF_DEADLINE_SECS    how long the checkout may stay behind before
#                            a deferral stops being a wait and becomes a
#                            finding                        (default 7200)
#   BOSS_JOBS_URL            the system of record the pass records on;
#                            else the line in REPO_DIR/infra/dev/sor-url
# Paths (env, defaulted to the boss-dev layout):
#   REPO_DIR (/work/boss) WORKTREES_DIR (REPO_DIR/.claude/worktrees)
#   CARGO_TARGET_DIR (/scratch/target)
#   SCRATCH_MOUNT (/scratch) WORK_MOUNT (/work)
#   PROC_ROOT (/proc) — the process table a worktree lock is judged
#     against (a test fakes it)
#   BOSS_CLI_STORE (WORK_MOUNT/tools/image-cli) — the image CLI's
#     generations, what the shim reads; BOSS_CLI_LINK
#     (WORK_MOUNT/tools/bin/boss-image) — the image CLI on PATH by its
#     own name, whatever the shim decides; BOSS_CLI_INSTALLER — the
#     installer to run (the estate's, beside this checkout; a test
#     stubs it)
set -euo pipefail

# The scratch floor is a SHARE of the filesystem, because the line it
# must stay ahead of is one: the kubelet evicts the pod when the node
# fs falls below 15% free (measured on w-1 2026-09-23: 139 GiB of 929).
# A floor in GB sat at 50 — ninety GB BELOW that line — so the kubelet
# evicted the whole pod six times in eight days while this pass never
# fired once. 25% keeps ~90 GiB of margin on w-1, over two hours of the
# ~40 GB/h the builders were measured filling it at, on any node size.
SCRATCH_FLOOR_PCT="${BOSS_SCRATCH_FLOOR_PCT:-25}"
# A sibling target touched this recently belongs to a build in flight;
# the floor pass never takes it (the mtime is the per-dir liveness
# check, as in the stale-target pass).
LIVE_TARGET_MIN="${BOSS_LIVE_TARGET_MIN:-30}"
# Hours a SIBLING target dir may go untouched before it is a dead cache.
# Every builder gets its own CARGO_TARGET_DIR under the scratch mount
# (boss brief says so), and a landed branch's target outlives it by
# days; 12h is longer than any build and shorter than the next morning.
STALE_TARGET_H="${BOSS_STALE_TARGET_H:-12}"
# THE /work FLOOR IS DERIVED, NOT CONFIGURED (backlog 99ce8744): the
# gate's own floor plus a margin, both read from infra/build-floor.env,
# the one definition infra/gate.sh reads too. Measured 2026-09-24
# ~23:00Z: /work at 12 GB free of 40, the gate refusing below 12 and
# this pass acting only below 6 (`BOSS_WORK_FLOOR_GB`, default 6 and 6
# again in the sidecar's env) — so the automatic pass never fired before
# a builder's pre-flight was refused, and two builders lowered the
# gate's floor by hand in one hour. Adding the margin to the gate's
# number is what makes "the reclaim acts first" true by construction.
#
# BOSS_WORK_FLOOR_GB IS RETIRED AND IGNORED. The live sidecar still sets
# it to 6 (boss-dev.yaml), and a manifest edit rolls the pod and ends
# the operator's session (memory: boss-dev-manifest-cars-restart-the-
# session), so it is this script, which the sidecar runs from the
# checkout every hour, that stops reading it. The file is found beside
# this script's own checkout, so the sidecar reads the floor of the tree
# it is running.
BUILD_FLOOR_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)/build-floor.env"
build_floor() {
    awk -F= -v k="$1" '$1 == k {v = $2} END {print v}' "$BUILD_FLOOR_FILE" 2>/dev/null
}
GATE_MIN_FREE_GB="$(build_floor GATE_MIN_FREE_GB)"
WORK_RECLAIM_MARGIN_GB="$(build_floor WORK_RECLAIM_MARGIN_GB)"
for name in GATE_MIN_FREE_GB WORK_RECLAIM_MARGIN_GB; do
    case "${!name:-empty}" in
        empty|*[!0-9]*)
            echo "dev-scratch-reclaim: $BUILD_FLOOR_FILE must define $name as a whole number, got '${!name}'" >&2
            exit 64
            ;;
    esac
done
WORK_FLOOR_GB=$((GATE_MIN_FREE_GB + WORK_RECLAIM_MARGIN_GB))
WORKTREE_MAX_AGE_H="${BOSS_WORKTREE_MAX_AGE_H:-48}"
# Hours of git QUIET (no commit, HEAD write or reflog entry) before a
# worktree whose branch has LANDED — head on origin/main, no origin/
# ref left — may go. Landed is necessary, not sufficient: a builder's
# tree is CLEAN in the minutes between its commit and its push, and an
# unpushed branch has no origin/ ref either, which is why an unpushed
# tree waits the longer WORKTREE_MAX_AGE_H instead. The same 12h as
# the target pass — longer than any session's silence between two git
# commands, shorter than the next morning.
WORKTREE_GRACE_H="${BOSS_WORKTREE_GRACE_H:-12}"
# A worktree on `main` or a detached HEAD has no branch for the forge
# to have forgotten, so idleness is the whole judgement: 7 days.
WORKTREE_IDLE_H="${BOSS_WORKTREE_IDLE_H:-168}"
# How recently an open gate-run must have been filed for its gate to
# still be LAUNCHING — see `gates_quiet` for why that is the whole
# hazard. 120s against a launch that is one file read plus one POST.
FF_LAUNCH_WINDOW_SECS="${BOSS_FF_LAUNCH_WINDOW_SECS:-120}"
# How long the checkout may stay behind origin/main before a deferral
# stops being "wait for the next hour" and becomes a finding. Trains
# land roughly every 50 minutes, so two hours is at least two passes
# and two trains — long enough that a busy afternoon is not an alarm,
# short enough that a checkout which has stopped catching up is named
# on the same working day.
FF_DEADLINE_SECS="${BOSS_FF_DEADLINE_SECS:-7200}"

REPO_DIR="${REPO_DIR:-/work/boss}"
WORKTREES_DIR="${WORKTREES_DIR:-$REPO_DIR/.claude/worktrees}"
TARGET_DIR="${CARGO_TARGET_DIR:-/scratch/target}"
SCRATCH_MOUNT="${SCRATCH_MOUNT:-/scratch}"
WORK_MOUNT="${WORK_MOUNT:-/work}"
PROC_ROOT="${PROC_ROOT:-/proc}"

for name in SCRATCH_FLOOR_PCT LIVE_TARGET_MIN WORK_FLOOR_GB WORKTREE_MAX_AGE_H STALE_TARGET_H WORKTREE_GRACE_H WORKTREE_IDLE_H FF_LAUNCH_WINDOW_SECS FF_DEADLINE_SECS; do
    case "${!name}" in
        ''|*[!0-9]*)
            echo "dev-scratch-reclaim: $name must be a whole number, got '${!name}'" >&2
            exit 64
            ;;
    esac
done
if [ "$SCRATCH_FLOOR_PCT" -gt 100 ]; then
    echo "dev-scratch-reclaim: SCRATCH_FLOOR_PCT is a percentage, got '$SCRATCH_FLOOR_PCT'" >&2
    exit 64
fi

log() { echo "dev-scratch-reclaim: $*"; }

# Is a build live? Scans /proc for cargo/rustc — no pgrep dependency
# (the CI image ships neither procps nor psmisc), and the pod's
# shareProcessNamespace means this sidecar's /proc lists the dev
# container's processes too. /proc/<pid>/comm is the executable's
# name (truncated to 15 chars, ample for these). A read that races a
# vanishing pid just `continue`s.
build_running() {
    local c n
    for c in /proc/[0-9]*/comm; do
        read -r n < "$c" 2>/dev/null || continue
        case "$n" in cargo|rustc) return 0 ;; esac
    done
    return 1
}

# Free space on a mount, in KB — `df -Pk` for POSIX columns, same as
# the forge sweep. A missing mount is reported and treated as "no
# pressure" so a partial pod layout never turns into a hard failure.
free_kb() {
    local m="$1"
    if [ ! -d "$m" ]; then
        echo ""
        return
    fi
    df -Pk "$m" | awk 'NR==2 {print $4}'
}

# The filesystem's size, in KB — the base the scratch floor's share is
# taken of.
size_kb() {
    df -Pk "$1" | awk 'NR==2 {print $2}'
}

problems=0

# Totals every pass leaves for the record at the end.
WT_PASS=skipped; WT_PASS_REASON=""
WT_MAIN_SHA=""; WT_MAIN_TS=""
WT_REMOVED=0; WT_REMOVED_MIB=0; WT_REMOVED_BY_CONTENT=0
WT_KEPT_DIRTY=0; WT_KEPT_DIRTY_NAMES=""
WT_KEPT_UNREFERENCED=0; WT_KEPT_UNREFERENCED_NAMES=""
WT_KEPT_LIVE=0; WT_KEPT_RECENT=0; WT_KEPT_LOCKED=0; WT_KEPT_REFUSED=0
WT_STALE_LOCKS=0; WT_STALE_LOCK_NAMES=""
WT_PRUNED=0
WT_TARGETS_REMOVED=0; WT_TARGETS_MIB=0
FLOOR_WORKTREES_REMOVED=0; FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES=""
STALE_TARGETS_RECLAIMED=0
FLOOR_TARGETS_RECLAIMED=0; FLOOR_TARGETS_MIB=0
INCREMENTAL_DROPPED=0

# ---------------------------------------------------------------------
# THE CHECKOUT ITSELF: /work/boss catches up to origin/main.
# ---------------------------------------------------------------------
# WHY (backlog 033d1fd3, 2026-09-19). Every pod door — boss-api, the
# boss shim, wt-cargo, wt-web — is a symlink into THIS checkout's
# infra/dev, so each runs whatever the checkout last held. Car
# fix/a-door-refuses-to-answer-from-a-stale-checkout gave each door
# door-freshness.sh, which turns a stale copy's silently wrong answer
# into a loud one: a read WARNS and a boss-api write is REFUSED. That
# is the correctness half. The repair was still a human act — `git -C
# /work/boss merge --ff-only origin/main` — and the operator session
# typed it FOUR TIMES on 2026-09-19 alone (session start, 17:05 after
# the 404, 17:40, 18:00) while trains land roughly every 50 minutes. A
# warning everyone expects is a warning nobody reads, which is how the
# class comes back. Mechanical operations belong to the machine
# (CLAUDE.md §Diagnosis), and this sidecar is the machine already
# standing in front of this checkout every hour.
#
# NO NETWORK, for the same reason the passes below take none: the
# sidecar mounts only /work and /scratch, so it has no forge credential
# and an anonymous fetch is `could not read Username` (b50a65ef). It
# fast-forwards to the refs/remotes/origin/main the checkout ALREADY
# has — the very ref door-freshness.sh compares against — so the door
# can call a copy stale only in the window where this pass can repair
# it, and a checkout nobody has fetched for is one neither of them can
# judge. The dev container's own sessions keep the ref fresh (worktrees
# share the object store).
#
# WHAT IT MUST NOT DO is move the tree under a LAUNCHING gate: `boss
# gate` renders its runner manifest from the tree at launch, which is
# the never-stash-while-a-gate-runs hazard. The quiet is READ FROM THE
# SYSTEM OF RECORD — a recently-opened `gate-run` packet — rather than
# from a lock file, because the gate-runs are already in the record and
# a lock file would be a second copy of a fact (CLAUDE.md §9a). An
# answer it CANNOT take — no system of record named, the API erroring,
# a reply with no `.data`, a page that returns fewer rows than it says
# are open — defers too, because a safety check that did not run is not
# a safety check.
#
# LAUNCHING, NOT RUNNING (backlog 475fbd10, 2026-09-22). This used to
# defer on ANY open gate-run, and at 12 builders that condition was true
# for 251 of 300 minutes — 84% — so an hourly pass landed about one time
# in six and the checkout sat five commits behind while every door
# warned and every door WRITE was refused at exit 78. Raising throughput
# had made the catch-up unreachable. The narrower condition is also the
# truer one: `boss gate` takes everything it will ever take from a tree
# in ONE `read_to_string` of the runner manifest — the first statement
# of `gate::run` in crates/orchestrators/boss-cli/src/gate.rs, BEFORE
# the gate-run packet is filed — and the runner Job itself clones from
# the forge into a per-run emptyDir and never touches /work at all
# (infra/gate-runner/gate-runner.yaml). So a gate-run older than
# FF_LAUNCH_WINDOW_SECS belongs to a gate that has already rendered, and
# holding the checkout for it buys nothing. Re-measured over the same
# history: a 120-second launch window is occupied 15% of five hours and
# 14% of a day, against 73% and more for "any open gate-run".
#
# AND THE DEFERRAL HAS AN UPPER BOUND, because a deferral that can
# repeat forever never errors — it just stops being true, which is the
# silent-failure class CLAUDE.md names. Past FF_DEADLINE_SECS behind,
# a deferral is a PROBLEM: it reds the pass and rides the packet with
# how long and what held it, the same channel git's own refusal uses.
# HOW LONG comes from git alone — the committer time of the oldest
# commit the checkout is missing — so nothing keeps a counter file that
# could disagree with the two refs that decide the fast-forward. The
# bound makes a stuck deferral LOUD; it never overrides the hazard,
# because a gate lost to a moved tree costs more than an hour of stale
# doors and the doors refuse a write rather than land a wrong one.
#
# THREE MORE THINGS IT REFUSES, each the same line door-freshness.sh
# draws: a checkout not on `main` (somebody put it on a branch), one
# that has DIVERGED (a commit of its own is a developer's, never this
# pass's to rewind), and one with no origin/main at all. And the last
# lock is git's: `merge --ff-only` refuses rather than overwrite a
# local edit, and that refusal is a PROBLEM — loud, on stderr and on
# the packet — because the checkout then stays behind until someone
# looks. Measured 2026-09-20: /work/boss carried a modified, TRACKED
# .claude/settings.json, so a "clean tree" precondition would have made
# this pass a permanent no-op; git's own narrower refusal (only files
# the merge would overwrite) is the one that keeps it working.
FF_RESULT=skipped
# Git's own complaint when a fast-forward is refused, for the packet.
FF_DETAIL=
FF_TO=""
FF_REASON=""

# Who this pass reads the system of record as. The same platform-admin
# shape the maintenance helpers use, under this sidecar's own id: an
# empty answer from a scope that cannot see gate-runs would read as
# "quiet" and is exactly the wrong way to be wrong here.
FF_USER='{"id":"automation:dev-scratch-reclaim","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# Is any gate LAUNCHING right now? 0 = quiet, 1 = a gate is launching,
# 2 = could not tell — which is not quiet. Sets FF_REASON either way.
#
# The page is asked for more than it can plausibly need (open gate-runs
# are bounded by the gate concurrency plus its queue) and the rows are
# then compared against `.total`: a truncated page answers a smaller
# question, and the runs it did not send are exactly the ones that could
# have launched a second ago.
gates_quiet() {
    local here url api reply rc count total now_s cutoff at at_s launching
    here="$(dirname "$(readlink -f "$0")")"
    url="${BOSS_JOBS_URL:-$(head -n1 "$here/../dev/sor-url" 2>/dev/null || true)}"
    if [ -z "$url" ]; then
        FF_REASON="no system of record named (BOSS_JOBS_URL unset, $here/../dev/sor-url absent), so nothing can say whether a gate is launching"
        return 2
    fi
    api="$here/../boss-api-curl.sh"
    [ -x "$api" ] || api=boss-api-curl.sh
    rc=0
    reply=$("$api" -fsS -H "x-boss-user: $FF_USER" \
        "$url/api/jobs?kind=gate-run&status=open&limit=50" 2>/dev/null) || rc=$?
    if [ "$rc" -ne 0 ]; then
        FF_REASON="the jobs API at $url could not say whether a gate is launching (curl exit $rc)"
        return 2
    fi
    count=$(printf '%s' "$reply" | jq '.data | if . == null then error("no .data") else length end' 2>/dev/null) || count=
    case ${count:-empty} in
        empty | *[!0-9]*)
            FF_REASON="the jobs API at $url answered a shape with no .data array — a changed contract, not an empty queue"
            return 2
            ;;
    esac
    total=$(printf '%s' "$reply" | jq '.total // empty' 2>/dev/null) || total=
    case ${total:-empty} in
        empty | *[!0-9]*) total="$count" ;;
    esac
    if [ "$total" -gt "$count" ]; then
        FF_REASON="the jobs API at $url reports $total open gate-run packets but returned $count — a truncated page cannot say whether one of the rest is launching"
        return 2
    fi
    [ "$count" -gt 0 ] || return 0

    now_s=$(date -u +%s)
    cutoff=$((now_s - FF_LAUNCH_WINDOW_SECS))
    launching=0
    # `jq -r` first, into a here-doc, so a `return` below leaves this
    # function rather than a pipeline's subshell — and so no producer is
    # still writing when the loop stops (the SIGPIPE coin, rule 12).
    while IFS= read -r at; do
        # An open run with no opened_at is a shape this pass cannot
        # judge, and an unjudgeable safety check is not a safety check.
        [ -n "$at" ] || {
            FF_REASON="an open gate-run at $url carries no opened_at, so nothing can say whether it is still reading a tree"
            return 2
        }
        # Guarded FIRST: `date -d ''` answers midnight rather than
        # erroring (CLAUDE.md), so an empty parse must never become a 0.
        at_s=$(date -u -d "$at" +%s 2>/dev/null) || at_s=
        case ${at_s:-empty} in
            empty | *[!0-9]*)
                FF_REASON="an open gate-run at $url carries an opened_at this pass cannot read ($at)"
                return 2
                ;;
        esac
        [ "$at_s" -lt "$cutoff" ] || launching=$((launching + 1))
    done <<EOF
$(printf '%s' "$reply" | jq -r '.data[].metadata.opened_at // ""')
EOF

    if [ "$launching" -gt 0 ]; then
        FF_REASON="$launching of $count open gate-run packet(s) at $url opened within the last ${FF_LAUNCH_WINDOW_SECS}s, and a gate renders its runner from a tree as it launches"
        return 1
    fi
    return 0
}

fast_forward_checkout() {
    local branch head main behind out rc behind_word behind_since behind_secs
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "fast-forward skipped: $REPO_DIR is not a git checkout"
        FF_RESULT="skipped: not a checkout"
        return 0
    fi
    branch=$(git -C "$REPO_DIR" symbolic-ref --quiet --short HEAD 2>/dev/null) || branch=""
    if [ "$branch" != main ]; then
        log "fast-forward skipped: $REPO_DIR is on ${branch:-a detached HEAD}, not main — a checkout somebody moved is theirs to move back"
        FF_RESULT="skipped: not on main"
        return 0
    fi
    head=$(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null) || head=""
    main=$(git -C "$REPO_DIR" rev-parse --verify -q refs/remotes/origin/main 2>/dev/null) || main=""
    if [ -z "$head" ] || [ -z "$main" ]; then
        log "fast-forward skipped: $REPO_DIR has no HEAD or no refs/remotes/origin/main (git fetch origin in the dev container)" >&2
        FF_RESULT="skipped: no origin/main"
        return 0
    fi
    if [ "$head" = "$main" ]; then
        FF_RESULT=current
        return 0
    fi
    if ! git -C "$REPO_DIR" merge-base --is-ancestor "$head" "$main" >/dev/null 2>&1; then
        log "fast-forward skipped: $REPO_DIR at ${head:0:8} has diverged from origin/main ${main:0:8} — a commit of its own is a developer's, never this pass's to rewind"
        FF_RESULT=diverged
        return 0
    fi
    FF_TO="$main"
    behind=$(git -C "$REPO_DIR" rev-list --count "$head..$main" 2>/dev/null) || behind=""
    case ${behind:-empty} in
        empty | *[!0-9]*) behind=1 ;;
    esac
    if [ "$behind" = 1 ]; then behind_word=commit; else behind_word=commits; fi

    # HOW LONG IT HAS BEEN BEHIND, from the same two refs that decide
    # the fast-forward: the committer time of the OLDEST commit this
    # checkout is missing. No counter file, so nothing can disagree with
    # git about it (CLAUDE.md §9a). `tail` drains the list rather than
    # cutting it short, so pipefail sees no SIGPIPE.
    behind_since=$(git -C "$REPO_DIR" log --format=%ct "$head..$main" 2>/dev/null | tail -n1) || behind_since=
    behind_secs=
    case ${behind_since:-empty} in
        empty | *[!0-9]*) ;;
        *) behind_secs=$(($(date -u +%s) - behind_since)) ;;
    esac

    rc=0
    gates_quiet || rc=$?
    if [ "$rc" -ne 0 ]; then
        if [ "$rc" = 1 ]; then
            FF_RESULT="deferred: a gate is launching"
        else
            FF_RESULT="deferred: the gate check could not be read"
        fi
        # THE UPPER BOUND. Deferring is a wait until the checkout has
        # been behind longer than a deadline; past that it is a FINDING,
        # because a deferral that can repeat forever never errors — it
        # just stops being true while every door warns and every door
        # write is refused at exit 78. The tree still does not move: the
        # bound makes the stall loud, it does not overrule the hazard.
        if [ -n "$behind_secs" ] && [ "$behind_secs" -gt "$FF_DEADLINE_SECS" ]; then
            FF_RESULT="$FF_RESULT, past its ${FF_DEADLINE_SECS}s deadline"
            # Flattened for the JSON the step writer interpolates it
            # into, the same way git's own complaint is below — at the
            # LAST step before storage, with the full text already in
            # the log line beside it.
            FF_DETAIL=$(printf '%s has been behind origin/main for %s minutes (%s %s), past the %s-minute deadline: %s' \
                "$REPO_DIR" "$((behind_secs / 60))" "$behind" "$behind_word" "$((FF_DEADLINE_SECS / 60))" "$FF_REASON" \
                | tr -d '"\\' | tr '[:cntrl:]' ' ' | tr -s ' ' | cut -c1-400)
            log "fast-forward DEFERRED PAST ITS DEADLINE: $FF_DETAIL — every door here answers from that tree and a write is refused at exit 78" >&2
            problems=$((problems + 1))
            return 0
        fi
        if [ "$rc" = 1 ]; then
            log "fast-forward deferred: $FF_REASON — $REPO_DIR stays ${behind} ${behind_word} behind until the next pass"
        else
            log "fast-forward deferred: $FF_REASON — a safety check that did not run is not a safety check, so the tree stays where it is" >&2
        fi
        return 0
    fi

    rc=0
    out=$(git -C "$REPO_DIR" merge --ff-only "$main" 2>&1) || rc=$?
    if [ "$rc" -ne 0 ]; then
        log "fast-forward FAILED (git exit $rc) taking $REPO_DIR from ${head:0:8} to ${main:0:8}; the doors keep warning until someone looks. git said: $out" >&2
        # GIT'S OWN WORDS RIDE THE PACKET, not only the journal
        # (backlog 6db0b658; David, 2026-09-22 — accept the jam and make
        # the refusal loud). `ff_result` carried `failed: exit 1`, which
        # is a verdict a reader must go and re-derive: CLAUDE.md
        # §Diagnosis, "a verdict must name what failed". The journal had
        # the answer all along and the packet is what anyone reads.
        #
        # BOUNDED AND FLATTENED, because this is interpolated into JSON
        # by the step writer: newlines and tabs to spaces, quotes and
        # backslashes dropped, 400 characters. The reduction is at the
        # LAST step before storage and the full text is already in the
        # log above — never the only copy.
        FF_DETAIL=$(printf '%s' "$out" | tr -d '"\\' | tr '[:cntrl:]' ' ' | tr -s ' ' | cut -c1-400)
        FF_RESULT="failed: exit $rc"
        problems=$((problems + 1))
        return 0
    fi
    FF_RESULT=ok
    log "fast-forward: $REPO_DIR moved ${head:0:8} -> ${main:0:8} ($behind $behind_word), so every door symlinked into its infra/dev runs the tree's own copy"
}

# ---------------------------------------------------------------------
# WORK + SCRATCH, the 364 GB: worktrees whose branch the forge no
# longer has, and the target each one kept alive.
# ---------------------------------------------------------------------
# Measured 2026-09-18 (backlog 1933db9e): 203 worktrees under /work/boss
# — 83 of them coding agents' under .claude/worktrees/, 20 on a detached
# HEAD — carrying 182 distinct branches of which 175 no longer existed
# on the forge; 36 target-* dirs; 364 GB of a 929 GB disk at 77%. The
# forge deletes a car's branch when it lands, so "the forge has no such
# branch" is the fact that says the checkout's work is over — and the
# floor passes could not act on it: /scratch stood 221 GB above its
# floor with a third of the disk holding caches for branches that had
# landed days before. This pass runs on that fact regardless of free
# space, the way the stale-target pass runs on age.
#
# THREE THINGS EVERY REMOVAL REQUIRES, and any one missing keeps it:
#   1. the branch is GONE, read from the checkout's OWN refs and never
#      from the forge: a worktree's branch is gone when the checkout
#      holds no refs/remotes/origin/<branch> AND EITHER its head is an
#      ancestor of refs/remotes/origin/main (LANDED — the forge sweeps
#      a car's branch when it merges) OR git has been quiet in it for
#      WORKTREE_MAX_AGE_H (ABANDONED unpushed — the harness's own
#      `worktree-agent-*` branches never reach the forge at all). A
#      worktree on `main` or a detached HEAD has no branch to be gone,
#      so it is judged by (2) alone with the longer WORKTREE_IDLE_H
#      window. The pass first shipped (H11) took this from ONE `git
#      ls-remote --heads origin`, and on the pod that read failed
#      every hour: the reclaim sidecar mounts only /work and /scratch
#      (boss-dev.yaml) — no forge token, no HOME carrying the
#      credential helper — and the forge answers an anonymous
#      info/refs with 401, so git asked for a username it had no
#      terminal to read (`fatal: could not read Username for
#      'http://10.20.0.15:3000'`, exit 128; measured 2026-09-18,
#      backlog b50a65ef). The pass skipped in silence and the three
#      newest packets read `worktree_pass=skipped` with no reason.
#      The refs are what the operator's session and every builder
#      worktree fetch (they share one object store), so the pass is
#      ONLY AS FRESH AS THE DEV CONTAINER'S LAST FETCH — it records
#      the origin/main sha and its commit time on the packet so a
#      stale read is visible — and a landed branch's origin/ ref
#      outlives the forge's copy until a fetch prunes it (fetch.prune
#      is not set on the pod), which keeps a worktree, never removes
#      one: every error here is on the side of keeping. SINCE 2026-09-25
#      (backlog 4a738ca5) a head NOT on origin/main is still LANDED when
#      its work is — every file it changed byte-identical on main, or
#      every commit of its own there by patch-id (`landed_by_content`)
#      — because a rebase and a train's squash leave the checkout's own
#      sha on no origin ref at all, and 56 of 58 measured heads were;
#   2. git has been QUIET in it for the window (1) chose — no commit,
#      no HEAD or index write, no new reflog ENTRY (an entry's own
#      time, never the file's mtime, which a gc rewrites in every
#      worktree at once — backlog adce5171) — because a builder's tree
#      is clean for the minutes between its commit and its push;
#   3. the tree is CLEAN: `git status --porcelain` empty, untracked
#      files included. A dirty tree is kept and NAMED with its count,
#      in the log and on the packet, so an operator can decide. A
#      DETACHED tree must also have its head held by some ref, or it
#      is kept and named the same way: the checkout is the only thing
#      naming those commits (adce5171).
# Live-locked worktrees, the main checkout and the one this run stands
# in are never candidates — and the lock is what keeps a RUNNING
# agent's tree: the Claude harness locks each agent worktree with its
# pid (`claude agent agent-<id> (pid N start T)`) for the session's
# life. A lock is a claim by a PROCESS, so it is judged against the
# process table (`lock_stale_why`): a harness lock whose pid is gone,
# or started at another time than it recorded, is STALE, and the tree
# is judged like any other — (1) to (3) all still apply — and named.
# Only a tree about to be removed is unlocked, and it is re-locked if
# git then refuses, so a stale lock the pass keeps stays as it was.
# `git worktree remove` still runs without --force, a second lock on
# (3).
#
# A PASS THAT CANNOT ANSWER — no refs/remotes/origin/main, git refusing
# — removes nothing AND RECORDS IT: `worktree_pass=skipped` with the
# reason beside it, counted as a problem so the packet is filed
# (result=incomplete). A skipped pass is a finding, not silence.
#
# WHAT GOES WITH IT: the per-worktree cargo target. wt-cargo names it
# `$WT_TARGET_ROOT/target-<basename of the worktree>` (infra/dev/
# wt-cargo), so `worktree_target` derives the same name here — one
# shape in two files, pinned by the test at each end; the primary
# $TARGET_DIR is never a candidate. A `.seeding` sibling wt-cargo left
# from a killed copy goes too. The local BRANCH is left alone: it
# costs nothing, and deleting refs is a different decision.

# Newest git activity in a worktree, as epoch seconds: its HEAD
# commit's time, the mtimes of the worktree's own HEAD and of its
# directory, and the time of the NEWEST ENTRY in its reflog.
#
# The INDEX is not read (backlog e14a741c). A `git status` rewrites a
# CLEAN tree's index to refresh its stat cache — the harness snapshots
# every session's status as it starts, and this pass's own dirty check
# is a status — so its mtime says someone LOOKED: measured 2026-09-23,
# five worktrees' index files written 2026-09-21 17:11, days after
# their last commit. What the index can hold that HEAD does not is a
# staged change, and the dirty guard keeps that at any age; a commit,
# checkout or reset that writes it also writes a reflog entry, read
# below for its own time.
#
# The reflog is read for what it SAYS, never for its mtime (backlog
# adce5171). Until 2026-09-23 this took the mtime of $gitdir/logs/HEAD,
# and a repo-wide `git gc` rewrites that file in EVERY worktree at once
# — its `reflog expire --all` copies each worktree's entries to a new
# file whether or not one expires. Measured 2026-09-21: the pass kept
# 373 of 397 worktrees "with git activity inside the window", and three
# unrelated ones whose HEAD, index and directory were 73h, 80h and 101h
# quiet all read logs/HEAD EXACTLY 32h old. Re-measured 2026-09-23: 154
# reflogs rewritten inside four seconds at 2026-09-22 05:43:30Z, beside
# gc's writes of info/refs and objects/info. Because the measure is a
# MAX, one such touch reset the idle clock of the whole population, so
# a pass could only ever remove what a window shorter than the gc
# interval let through. Each reflog line carries the time git wrote it
# (`<old> <new> <ident> <epoch> <tz><TAB><message>`), and an expire
# copies lines without redating them, so the last line's epoch is the
# last act in THIS worktree and nothing another command did to the file.
worktree_last_activity() {
    local path="$1" gitdir f t newest
    newest=$(git -C "$path" log -1 --format=%ct 2>/dev/null || echo 0)
    gitdir=$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null || true)
    for f in "$gitdir/HEAD" "$path"; do
        [ -e "$f" ] || continue
        t=$(stat -c %Y "$f" 2>/dev/null || echo 0)
        [ "$t" -gt "$newest" ] && newest=$t
    done
    t=0
    if [ -f "$gitdir/logs/HEAD" ]; then
        t=$(tail -n 1 "$gitdir/logs/HEAD" 2>/dev/null | cut -f1 | awk '{print $(NF-1)}')
    fi
    case ${t:-empty} in empty|*[!0-9]*) t=0 ;; esac
    [ "$t" -gt "$newest" ] && newest=$t
    echo "${newest:-0}"
}

# Is the process table this pass reads the POD's? The sidecar sees the
# dev container's processes only because the pod sets
# shareProcessNamespace (infra/cluster/manifests/boss-dev.yaml), and
# then pid 1 is the pod's `pause`. In a namespace of its own pid 1 is
# the sidecar's own entrypoint, every harness pid would read as gone,
# and every LIVE agent's lock as stale — so no lock is judged there.
proc_view_is_pods() {
    local c=""
    read -r c < "$PROC_ROOT/1/comm" 2>/dev/null || return 1
    [ "$c" = pause ]
}

# Is a worktree lock STALE? Prints why and succeeds when it is; fails —
# keep the lock — for a live one AND for any lock it cannot judge.
# Only the harness's shape is judged, `... (pid N start T)` with T the
# process's start time in clock ticks since boot (field 22 of
# /proc/<pid>/stat, world-readable where /proc/<pid>/cwd is not): pid
# gone, or pid alive with another start, means the locker is gone —
# measured 2026-09-23 (backlog e14a741c), two locks named pid 355 start
# 94472290 while pid 355 had started at 95092646, and their trees were
# kept forever. Any other reason is a human's lock and is never judged.
# The comm field is split at its LAST `) `, since a comm may hold one.
lock_stale_why() {
    local reason="$1" pid start line rest now_start
    [[ "$reason" =~ \(pid\ ([0-9]+)\ start\ ([0-9]+)\) ]] || return 1
    pid="${BASH_REMATCH[1]}"; start="${BASH_REMATCH[2]}"
    if [ ! -e "$PROC_ROOT/$pid" ]; then
        echo "pid $pid is gone"
        return 0
    fi
    read -r line < "$PROC_ROOT/$pid/stat" 2>/dev/null || return 1
    rest="${line##*) }"
    now_start=$(awk '{print $20}' <<<"$rest")
    case ${now_start:-empty} in empty|*[!0-9]*) return 1 ;; esac
    [ "$now_start" = "$start" ] && return 1
    echo "pid $pid started at $now_start, not the $start the lock recorded"
}

# The first ref that holds a commit — the one question the detached-HEAD
# guard asks, in whichever pass is about to remove a checkout (backlog
# adce5171 for the gone-worktree pass, 5da0428a for the floor pass).
# Empty for an empty sha, for a commit no ref holds, and for a git that
# cannot answer: every one of them reads as "no ref", and keeps.
# `--count=1` stops at the first ref that holds it.
ref_holding() {
    [ -n "$1" ] || return 0
    git -C "$REPO_DIR" for-each-ref --count=1 --format='%(refname)' --contains "$1" 2>/dev/null || true
}

# Is a head's WORK on origin/main although the head itself is not?
# Prints how — `content` or `patch-id` — and succeeds; fails for
# anything else, including a git that cannot answer, so every error
# here reads as "not landed" and keeps.
#
# WHY (backlog 4a738ca5). Measured 2026-09-25 04:10Z: under the /work
# floor a pass removed 11 worktrees and kept 286 "recent" while /work
# sat at 12 GB free of 40, and of 58 sampled worktree heads only 2 were
# on any origin ref. Builders commit on local `worktree-agent-*`
# branches, `boss gate --rebase` replays the commit onto a new sha, and
# a train squashes its cars into one commit — so no sha a worktree holds
# ever reaches origin/main, the ancestry test above called every landed
# tree UNPUSHED, and each waited the 48h abandoned window, not the
# landed one.
#
# The two rules are `boss merged`'s (crates/orchestrators/boss-cli/src/
# merged.rs), which settled this question for cars, in the same terms:
#   CONTENT — every file the head changed since its merge-base with main
#     is byte-identical on main. The only signal that survives a train:
#     a squash of N cars has a patch-id equal to no single car's.
#   PATCH-ID — `git cherry` finds every commit of the head's own on main
#     by patch-id. The one that survives a LATER car editing the file.
# A head with no commits of its own is the ancestry case, not this one;
# a head whose commits change no file answers nothing and is kept. A
# SOME-files match is not a landing either (a sibling car editing one
# of the files looks the same), so it keeps, as `boss merged` reports
# Unknown for it. Content runs first because it is two diffs; the
# cherry computes a patch-id for every main commit since the base.
landed_by_content() {
    local head="$1" main="$2" base cherry
    local -a files=()
    base=$(git -C "$REPO_DIR" merge-base "$main" "$head" 2>/dev/null) || return 1
    [ -n "$base" ] || return 1
    mapfile -d '' files < <(git -C "$REPO_DIR" diff --name-only -z "$base" "$head" 2>/dev/null)
    if [ "${#files[@]}" -gt 0 ] &&
        git --literal-pathspecs -C "$REPO_DIR" diff --quiet "$main" "$head" -- "${files[@]}" 2>/dev/null; then
        echo content
        return 0
    fi
    cherry=$(git -C "$REPO_DIR" cherry "$main" "$head" 2>/dev/null) || return 1
    [ -n "$cherry" ] || return 1
    case $'\n'"$cherry" in *$'\n+'*) return 1 ;; esac
    echo patch-id
}

worktree_target() { echo "$SCRATCH_MOUNT/target-$(basename "$1")"; }

remove_worktree_target() {
    local target t kb
    target=$(worktree_target "$1")
    for t in "$target" "$target.seeding"; do
        [ -d "$t" ] || continue
        [ "$t" = "$TARGET_DIR" ] && continue
        kb=$(du -sk "$t" 2>/dev/null | awk '{print $1}')
        if rm -rf "$t"; then
            log "  removed target $t ($((${kb:-0} / 1024))MiB, its worktree is gone)"
            WT_TARGETS_REMOVED=$((WT_TARGETS_REMOVED + 1))
            WT_TARGETS_MIB=$((WT_TARGETS_MIB + ${kb:-0} / 1024))
        else
            log "could not remove target $t" >&2
            problems=$((problems + 1))
        fi
    done
}

reclaim_gone_worktrees() {
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "$REPO_DIR is not a git checkout — worktree pass skipped"
        return 0
    fi
    # ONE read of origin/main, up front, from the checkout's own refs.
    # Without it "landed" has no meaning — under a missing ref every
    # branch would read as abandoned-or-not by age alone — so the pass
    # skips, says so, and RECORDS the skip (below) rather than return
    # in silence.
    local main_sha main_ts
    if ! main_sha=$(git -C "$REPO_DIR" rev-parse --verify -q refs/remotes/origin/main 2>&1) || [ -z "$main_sha" ]; then
        WT_PASS_REASON="$REPO_DIR has no refs/remotes/origin/main (git fetch origin in the dev container; ${main_sha:-empty answer})"
        log "worktree pass skipped: $WT_PASS_REASON" >&2
        problems=$((problems + 1))
        return 0
    fi
    if ! main_ts=$(git -C "$REPO_DIR" log -1 --format=%ct "$main_sha" 2>&1) || [ -z "$main_ts" ]; then
        WT_PASS_REASON="git could not read refs/remotes/origin/main at $main_sha (${main_ts:-empty answer})"
        log "worktree pass skipped: $WT_PASS_REASON" >&2
        problems=$((problems + 1))
        return 0
    fi
    WT_MAIN_SHA="$main_sha"; WT_MAIN_TS="$main_ts"
    WT_PASS=ran
    log "worktree pass: judging against origin/main ${main_sha:0:8} (committed $(date -u -d "@$main_ts" +%FT%TZ 2>/dev/null || echo "@$main_ts"), read from $REPO_DIR's refs — only as fresh as the dev container's last fetch)"

    # Admin entries whose directory is already gone — a worktree an
    # operator rm -rf'd — hold the branch checked out and nothing else.
    local pruned
    pruned=$(git -C "$REPO_DIR" worktree prune -v 2>&1 || true)
    if [ -n "$pruned" ]; then
        printf '%s\n' "$pruned" | sed 's/^/dev-scratch-reclaim:   pruned: /'
        WT_PRUNED=$(printf '%s\n' "$pruned" | grep -c . || true)
    fi

    local self now judge_locks=1
    self="$(pwd -P 2>/dev/null || echo /nonexistent)"
    now=$(date +%s)

    # THE GRACE YIELDS TO THE FLOOR (backlog 99ce8744), as it already
    # does for targets: above the floor a stale target waits
    # STALE_TARGET_H, below it only LIVE_TARGET_MIN. A landed worktree's
    # 12h grace exists for a tree someone may still be standing in, and
    # the same live window answers that for a tree as for a target —
    # while /work under its floor is what refuses the next builder's
    # pre-flight. One reading, up front: the pass removes every landed
    # tree past the live window rather than stopping at the floor,
    # because each would go at the 12h mark anyway and holds no work
    # (clean, on origin/main).
    local under_floor=0 wkb
    wkb=$(free_kb "$WORK_MOUNT")
    if [ -n "$wkb" ] && [ $((wkb / 1024 / 1024)) -lt "$WORK_FLOOR_GB" ]; then
        under_floor=1
        log "worktree pass: $WORK_MOUNT $((wkb / 1024 / 1024))GB free < ${WORK_FLOOR_GB}GB floor — a landed worktree waits only the ${LIVE_TARGET_MIN}-minute live window, not the ${WORKTREE_GRACE_H}h grace"
    fi
    if ! proc_view_is_pods; then
        judge_locks=0
        log "worktree pass: locks not judged — $PROC_ROOT/1 is not the pod's pause, so this process table cannot say a harness pid is gone; every locked tree is kept"
    fi

    # `path<TAB>branch<TAB>locked<TAB>reason` per worktree, `detached`
    # standing in for a HEAD with no branch; the first block is the main
    # worktree.
    local first=1 path branch locked reason stale window_s why last idle_s idle_h dirty kb head held
    local landed_window_s floor_note unpushed how by_content
    landed_window_s=$((WORKTREE_GRACE_H * 3600)); floor_note=""
    if [ "$under_floor" = 1 ]; then
        landed_window_s=$((LIVE_TARGET_MIN * 60)); floor_note=", under the /work floor"
    fi
    while IFS=$'\t' read -r path branch locked reason; do
        [ -z "$path" ] && continue
        if [ "$first" = 1 ]; then first=0; continue; fi
        [ "$path" = "$REPO_DIR" ] && continue
        case "$self" in "$path"|"$path"/*) continue ;; esac
        [ -d "$path" ] || continue
        stale=""
        if [ "$locked" = 1 ]; then
            if [ "$judge_locks" = 0 ] || ! stale=$(lock_stale_why "$reason"); then
                WT_KEPT_LOCKED=$((WT_KEPT_LOCKED + 1))
                continue
            fi
            log "  $(basename "$path"): stale lock ($stale) — judged as an ordinary candidate"
            WT_STALE_LOCKS=$((WT_STALE_LOCKS + 1))
            WT_STALE_LOCK_NAMES="${WT_STALE_LOCK_NAMES:+$WT_STALE_LOCK_NAMES, }$(basename "$path")"
        fi

        unpushed=0; by_content=0
        case "$branch" in
            detached|main)
                window_s=$((WORKTREE_IDLE_H * 3600)); why="on $branch"
                ;;
            *)
                if git -C "$REPO_DIR" rev-parse --verify -q "refs/remotes/origin/$branch" >/dev/null 2>&1; then
                    WT_KEPT_LIVE=$((WT_KEPT_LIVE + 1))
                    continue
                fi
                if git -C "$REPO_DIR" merge-base --is-ancestor "refs/heads/$branch" "$main_sha" 2>/dev/null; then
                    window_s=$landed_window_s; why="branch $branch has no origin/ ref and its head is on origin/main (landed)$floor_note"
                else
                    unpushed=1
                    window_s=$((WORKTREE_MAX_AGE_H * 3600)); why="branch $branch has no origin/ ref and is not on origin/main (unpushed)"
                fi
                ;;
        esac

        last=$(worktree_last_activity "$path")
        idle_s=$((now - last))
        idle_h=$((idle_s / 3600))
        # A head that is not on origin/main may still have LANDED — its
        # work replayed and squashed onto main under other shas (backlog
        # 4a738ca5, `landed_by_content`). Asked only where the answer
        # can change the outcome: idle past the landed window but inside
        # the unpushed one, which is where the 286 kept trees stood.
        if [ "$unpushed" = 1 ] && [ "$idle_s" -ge "$landed_window_s" ] && [ "$idle_s" -lt "$window_s" ] &&
            how=$(landed_by_content "refs/heads/$branch" "$main_sha"); then
            by_content=1
            window_s=$landed_window_s
            why="branch $branch has no origin/ ref and its work is on origin/main by $how (landed — rebased or squashed under other shas)$floor_note"
        fi
        if [ "$idle_s" -lt "$window_s" ]; then
            WT_KEPT_RECENT=$((WT_KEPT_RECENT + 1))
            continue
        fi

        dirty=$(git -C "$path" status --porcelain 2>/dev/null | grep -c . || true)
        if [ "${dirty:-0}" -gt 0 ]; then
            log "  kept $path ($dirty dirty: uncommitted or untracked files; $why, idle ${idle_h}h)"
            WT_KEPT_DIRTY=$((WT_KEPT_DIRTY + 1))
            WT_KEPT_DIRTY_NAMES="${WT_KEPT_DIRTY_NAMES:+$WT_KEPT_DIRTY_NAMES, }$(basename "$path"):$dirty"
            continue
        fi

        # A DETACHED head names its commit in two places only — the
        # worktree's HEAD and its reflog — and `git worktree remove`
        # deletes both, handing any commit no ref holds to the next gc.
        # That is unpushed work, as surely as a dirty tree is uncommitted
        # work, so it is kept and NAMED the same way (backlog adce5171).
        # A branch needs no such check: its ref outlives the checkout.
        # Measured 2026-09-23: of the 16 detached trees the corrected
        # idle clock makes due, 13 sit on a ref (a forge PR ref, a
        # branch) and 3 on none. A git that cannot answer reads as no
        # ref, and keeps (`ref_holding`).
        if [ "$branch" = detached ]; then
            head=$(git -C "$path" rev-parse -q --verify HEAD 2>/dev/null || true)
            held=$(ref_holding "$head")
            if [ -z "$held" ]; then
                log "  kept $path (detached at ${head:0:8}, a head no ref holds: removing the checkout would leave its commits to gc; idle ${idle_h}h)"
                WT_KEPT_UNREFERENCED=$((WT_KEPT_UNREFERENCED + 1))
                WT_KEPT_UNREFERENCED_NAMES="${WT_KEPT_UNREFERENCED_NAMES:+$WT_KEPT_UNREFERENCED_NAMES, }$(basename "$path")"
                continue
            fi
        fi

        kb=$(du -sk "$path" 2>/dev/null | awk '{print $1}')
        # A stale lock comes off only here, at the removal it would
        # refuse — and goes back on, with its own reason, if git refuses
        # anyway, so a tree the pass keeps keeps its lock (e14a741c).
        if [ -n "$stale" ] && ! git -C "$REPO_DIR" worktree unlock "$path" 2>/dev/null; then
            log "  kept $path (git would not unlock its stale lock; $why, idle ${idle_h}h)"
            WT_KEPT_REFUSED=$((WT_KEPT_REFUSED + 1))
            continue
        fi
        if ! git -C "$REPO_DIR" worktree remove "$path" 2>/dev/null; then
            if [ -n "$stale" ] && ! git -C "$REPO_DIR" worktree lock --reason "$reason" "$path" 2>/dev/null; then
                log "  could not put $path's lock back ($reason)" >&2
                problems=$((problems + 1))
            fi
            log "  kept $path (git refused to remove it without --force; $why, idle ${idle_h}h)"
            WT_KEPT_REFUSED=$((WT_KEPT_REFUSED + 1))
            continue
        fi
        WT_REMOVED=$((WT_REMOVED + 1))
        WT_REMOVED_MIB=$((WT_REMOVED_MIB + ${kb:-0} / 1024))
        if [ "$by_content" = 1 ]; then WT_REMOVED_BY_CONTENT=$((WT_REMOVED_BY_CONTENT + 1)); fi
        log "  removed worktree $path ($((${kb:-0} / 1024))MiB; $why, idle ${idle_h}h, clean)"
        remove_worktree_target "$path"
    done < <(
        git -C "$REPO_DIR" worktree list --porcelain 2>/dev/null | awk '
            /^worktree / { if (p != "") print p "\t" b "\t" l "\t" r; p=substr($0, 10); b="detached"; l=0; r="" }
            /^branch /   { b=substr($0, 8); sub("^refs/heads/", "", b) }
            /^locked/    { l=1; r=substr($0, 8) }
            END { if (p != "") print p "\t" b "\t" l "\t" r }
        '
    )

    log "worktree pass: removed $WT_REMOVED worktree(s) (${WT_REMOVED_MIB}MiB; $WT_REMOVED_BY_CONTENT of them landed by content, not by sha) and $WT_TARGETS_REMOVED target(s) (${WT_TARGETS_MIB}MiB); kept $WT_KEPT_LIVE with an origin/ ref still in the checkout, $WT_KEPT_RECENT with git activity inside the window, $WT_KEPT_DIRTY dirty, $WT_KEPT_UNREFERENCED with a head no ref holds, $WT_KEPT_LOCKED locked, $WT_KEPT_REFUSED refused by git; pruned $WT_PRUNED entries whose directory was gone; judged $WT_STALE_LOCKS with a stale lock"
}

# ---------------------------------------------------------------------
# WORK: stale git worktrees on the ReadWriteOnce PVC.
# ---------------------------------------------------------------------
reclaim_work() {
    local kb gb
    kb=$(free_kb "$WORK_MOUNT")
    if [ -z "$kb" ]; then
        log "$WORK_MOUNT not mounted — skipping worktree reclaim"
        return 0
    fi
    gb=$((kb / 1024 / 1024))
    if [ "$gb" -ge "$WORK_FLOOR_GB" ]; then
        log "$WORK_MOUNT ${gb}GB free >= ${WORK_FLOOR_GB}GB floor (the gate's ${GATE_MIN_FREE_GB} + ${WORK_RECLAIM_MARGIN_GB} margin, infra/build-floor.env) — no worktree reclaim"
        return 0
    fi
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "$REPO_DIR is not a git checkout — cannot prune worktrees"
        return 0
    fi
    log "$WORK_MOUNT ${gb}GB free < ${WORK_FLOOR_GB}GB floor (the gate's ${GATE_MIN_FREE_GB} + ${WORK_RECLAIM_MARGIN_GB} margin, infra/build-floor.env) — pruning stale git worktrees"

    # Metadata first: drop admin entries for worktree dirs that are
    # already gone. Cheap and always safe.
    git -C "$REPO_DIR" worktree prune -v 2>&1 | sed 's/^/dev-scratch-reclaim:   /' || true

    # This run's own worktree is off limits — never saw the axe fall on
    # the branch it is standing on.
    local self
    self="$(pwd -P 2>/dev/null || echo /nonexistent)"

    # `path<TAB>locked<TAB>detached` per worktree; the first block is the
    # main worktree. Locked worktrees carry a `locked` line — skip those.
    local removed=0 skipped=0 first=1 path locked detached head
    while IFS=$'\t' read -r path locked detached; do
        [ -z "$path" ] && continue
        if [ "$first" = 1 ]; then first=0; continue; fi   # main worktree
        [ "$locked" = 1 ] && { skipped=$((skipped + 1)); continue; }
        [ "$path" = "$REPO_DIR" ] && continue
        case "$self" in "$path"|"$path"/*) continue ;; esac  # our own
        [ -d "$path" ] || continue

        # Age gate: skip anything touched within the cutoff — an active
        # session writes into its worktree, so a recent mtime means
        # "in use". `-mmin +N` = the dir's own mtime is older than N
        # minutes.
        if [ -z "$(find "$path" -maxdepth 0 -mmin +$((WORKTREE_MAX_AGE_H * 60)) 2>/dev/null)" ]; then
            skipped=$((skipped + 1))
            continue
        fi

        # Committed work outlives the checkout only if a REF names it. A
        # branch's ref survives `git worktree remove`; a detached HEAD's
        # only names are the worktree's HEAD and reflog, which the remove
        # deletes, handing the commits to the next gc. So a detached tree
        # whose head no ref holds is kept and named, as the gone-worktree
        # pass keeps it (adce5171). This pass took such trees at 48h with
        # no reading at all until backlog 5da0428a (2026-09-24), found by
        # the builder of 99ce8744 — the car that raises this floor from 6
        # to 18 GB, so this pass fires far more often.
        if [ "$detached" = 1 ]; then
            head=$(git -C "$path" rev-parse -q --verify HEAD 2>/dev/null || true)
            if [ -z "$(ref_holding "$head")" ]; then
                log "  kept $path (detached at ${head:0:8}, a head no ref holds: removing the checkout would leave its commits to gc)"
                skipped=$((skipped + 1))
                FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES="${FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES:+$FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES, }$(basename "$path")"
                continue
            fi
        fi

        # No --force: a dirty worktree is refused and left standing, so
        # uncommitted work is never discarded.
        if git -C "$REPO_DIR" worktree remove "$path" 2>/dev/null; then
            log "  removed stale worktree $path"
            removed=$((removed + 1))
        else
            log "  kept $path (uncommitted changes or in use — not forced)"
            skipped=$((skipped + 1))
        fi
    done < <(
        git -C "$REPO_DIR" worktree list --porcelain 2>/dev/null | awk '
            /^worktree / { if (p != "") print p "\t" l "\t" d; p=substr($0, 10); l=0; d=0 }
            /^locked/    { l=1 }
            /^detached/  { d=1 }
            END { if (p != "") print p "\t" l "\t" d }
        '
    )

    git -C "$REPO_DIR" worktree prune 2>/dev/null || true
    FLOOR_WORKTREES_REMOVED=$removed

    kb=$(free_kb "$WORK_MOUNT")
    gb=$((kb / 1024 / 1024))
    log "worktree reclaim: removed $removed, kept $skipped — $WORK_MOUNT now ${gb}GB free"
    if [ "$gb" -lt "$WORK_FLOOR_GB" ]; then
        log "WORK FLOOR UNMET — ${gb}GB free < ${WORK_FLOOR_GB}GB after pruning every eligible worktree." >&2
        log "  Remaining use is the clone, locked worktrees, worktrees with uncommitted work, or detached heads no ref holds${FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES:+ ($FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES)}. A human decides next." >&2
        problems=$((problems + 1))
    fi
}

# ---------------------------------------------------------------------
# SCRATCH: the regenerable incremental cache under CARGO_TARGET_DIR.
# ---------------------------------------------------------------------
reclaim_scratch() {
    local kb gb SCRATCH_FLOOR_GB
    kb=$(free_kb "$SCRATCH_MOUNT")
    if [ -z "$kb" ]; then
        log "$SCRATCH_MOUNT not mounted — skipping build-cache reclaim"
        return 0
    fi
    gb=$((kb / 1024 / 1024))
    SCRATCH_FLOOR_GB=$(( $(size_kb "$SCRATCH_MOUNT") / 1024 / 1024 * SCRATCH_FLOOR_PCT / 100 ))
    if [ "$gb" -ge "$SCRATCH_FLOOR_GB" ]; then
        log "$SCRATCH_MOUNT ${gb}GB free >= ${SCRATCH_FLOOR_GB}GB floor (${SCRATCH_FLOOR_PCT}%) — no build-cache reclaim"
        return 0
    fi
    log "$SCRATCH_MOUNT ${gb}GB free < ${SCRATCH_FLOOR_GB}GB floor (${SCRATCH_FLOOR_PCT}%) — reclaiming regenerable build cache"

    # The siblings first: every builder's own target, least recently
    # touched first, until the floor is met. They hold the bulk (the
    # 2026-09-23 sawtooth was ~450 GB of them), each is regenerable
    # (wt-cargo reseeds one from the primary), and each carries its own
    # liveness in its mtime — so no build_running() guard, which would
    # see some builder's cargo on a busy pod and defer forever.
    reclaim_targets_to_floor "$SCRATCH_FLOOR_GB"
    gb=$(( $(free_kb "$SCRATCH_MOUNT") / 1024 / 1024 ))
    if [ "$gb" -ge "$SCRATCH_FLOOR_GB" ]; then
        log "$SCRATCH_MOUNT ${gb}GB free — floor met by the sibling targets"
        return 0
    fi

    if [ ! -d "$TARGET_DIR" ]; then
        log "$TARGET_DIR does not exist — nothing to reclaim"
        return 0
    fi

    # NEVER trim the cache out from under a running build. The pod sets
    # shareProcessNamespace, so this sidecar sees the dev container's
    # processes; a live cargo/rustc means "leave the target alone".
    if build_running; then
        log "a build is running (cargo/rustc live) — skipping build-cache trim this pass" >&2
        log "build-cache reclaim deferred: ${gb}GB < ${SCRATCH_FLOOR_GB}GB floor, retried next pass." >&2
        problems=$((problems + 1))
        return 0
    fi

    # Incremental-compilation caches only: regenerable by definition,
    # and separate from the linked artifacts under deps/. cargo simply
    # rebuilds a missing incremental dir on the next compile.
    local n=0 d
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        rm -rf "$d" && n=$((n + 1))
    done < <(find "$TARGET_DIR" -type d -name incremental -prune 2>/dev/null)
    INCREMENTAL_DROPPED=$n

    kb=$(free_kb "$SCRATCH_MOUNT")
    gb=$((kb / 1024 / 1024))
    log "build-cache reclaim: dropped $n incremental cache dir(s) — $SCRATCH_MOUNT now ${gb}GB free"
    if [ "$gb" -lt "$SCRATCH_FLOOR_GB" ]; then
        log "SCRATCH FLOOR UNMET — ${gb}GB free < ${SCRATCH_FLOOR_GB}GB after dropping every incremental cache." >&2
        log "  The remainder is linked artifacts under $TARGET_DIR/*/deps. Reclaiming those is a" >&2
        log "  full rebuild's worth of cost — run \`cargo clean\` deliberately, or an operator decides." >&2
        problems=$((problems + 1))
    fi
}

# Under the floor: sibling target dirs, least recently touched first,
# until free space reaches $1 GB. A dir touched inside LIVE_TARGET_MIN
# is a build in flight and is never taken; the primary TARGET_DIR is
# the incremental trim's and never removed whole.
reclaim_targets_to_floor() {
    local floor="$1" d kb gb n=0 mib=0 cutoff
    cutoff="@$(( $(date +%s) - LIVE_TARGET_MIN * 60 ))"
    while IFS=$'\t' read -r _ d; do
        [ -n "$d" ] || continue
        gb=$(( $(free_kb "$SCRATCH_MOUNT") / 1024 / 1024 ))
        [ "$gb" -lt "$floor" ] || break
        if [ -n "$(find "$d" -maxdepth 3 -newermt "$cutoff" -print -quit 2>/dev/null)" ]; then
            log "  kept $d — touched within ${LIVE_TARGET_MIN}m, a build in flight"
            continue
        fi
        kb=$(du -sk "$d" 2>/dev/null | awk '{print $1}')
        if rm -rf "$d"; then
            n=$((n + 1))
            mib=$((mib + ${kb:-0} / 1024))
            log "floor target reclaimed: $d ($((${kb:-0} / 1024))MiB, least recently used; ${gb}GB free < ${floor}GB floor)"
        else
            log "could not remove target $d" >&2
            problems=$((problems + 1))
        fi
    done < <(
        for d in "$SCRATCH_MOUNT"/*/; do
            d="${d%/}"
            [ -d "$d" ] || continue
            [ "$d" = "$TARGET_DIR" ] && continue
            [ -f "$d/CACHEDIR.TAG" ] || [ -d "$d/debug" ] || [ -d "$d/release" ] || continue
            # The newest mtime at depth <= 3 is when a build last wrote.
            printf '%s\t%s\n' "$(find "$d" -maxdepth 3 -printf '%T@\n' 2>/dev/null | sort -n | tail -n1)" "$d"
        done | sort -n
    )
    FLOOR_TARGETS_RECLAIMED=$n
    FLOOR_TARGETS_MIB=$mib
    log "floor-target pass: $n sibling target(s) reclaimed (${mib}MiB), least recently used first"
}

# ---------------------------------------------------------------------
# SCRATCH, the other 62 GB: every OTHER builder's target dir.
# ---------------------------------------------------------------------
# Measured 2026-09-11 01:40Z (backlog 0efbd69e): /scratch held 90 GB,
# $TARGET_DIR was 28 GB of it, and twenty sibling dirs — one per coding
# agent, as their briefs instruct — held 62 GB for branches that had
# landed the day before. The floor pass above scans one path and would
# not have fired anyway at 304 GB free: a dead cache is not a headroom
# problem, it is an AGE problem, so this pass runs on age regardless of
# free space. A target dir is one that carries cargo's CACHEDIR.TAG or
# a debug/ or release/ profile; the primary $TARGET_DIR is never a
# candidate (the floor pass owns it, and only ever trims incremental/).
#
# NO build_running() GUARD HERE, on purpose: a build in progress writes
# to its target continuously, so a dir untouched for STALE_TARGET_H
# hours has no live build in it — the mtime IS the liveness check, per
# dir, which the process scan cannot be (it sees every builder's cargo
# at once and would defer this pass forever on a busy pod).
reclaim_stale_targets() {
    local d n=0 kb since
    [ -d "$SCRATCH_MOUNT" ] || return 0
    since="@$(( $(date +%s) - STALE_TARGET_H * 3600 ))"
    for d in "$SCRATCH_MOUNT"/*/; do
        d="${d%/}"
        [ -d "$d" ] || continue
        [ "$d" = "$TARGET_DIR" ] && continue
        if [ ! -f "$d/CACHEDIR.TAG" ] && [ ! -d "$d/debug" ] && [ ! -d "$d/release" ]; then
            continue
        fi
        # Anything inside touched within the window means a builder is
        # (or was just) using it. Depth-bounded: a target dir has
        # hundreds of thousands of files and the fingerprints at depth 3
        # move on every compile.
        if [ -n "$(find "$d" -maxdepth 3 -newermt "$since" -print -quit 2>/dev/null)" ]; then
            continue
        fi
        kb=$(du -sk "$d" 2>/dev/null | awk '{print $1}')
        if rm -rf "$d"; then
            n=$((n + 1))
            log "stale target reclaimed: $d ($((${kb:-0} / 1024))MiB, untouched for more than ${STALE_TARGET_H}h)"
        else
            log "could not remove stale target $d" >&2
            problems=$((problems + 1))
        fi
    done
    log "stale-target pass: $n stale target dir(s) reclaimed under $SCRATCH_MOUNT (age > ${STALE_TARGET_H}h, $TARGET_DIR exempt)"
    STALE_TARGETS_RECLAIMED=$n
}

# ---------------------------------------------------------------------
# THE CLI: the tree's `boss`, out of the cluster image, into /work.
# ---------------------------------------------------------------------
# Measured 2026-09-18 14:4xZ (backlog c35eda6c): /scratch/target/debug/
# boss said `built from 6286857f` (#448) while origin/main stood at
# cb053ed6 (#450), and the same installer this pass runs pulled the CLI
# out of the registry's david/boss:cb053ed into a scratch store in 10
# seconds from this pod — no docker, no root, no manifest change. So:
#
#   * origin/main's sha is the checkout's OWN remote-tracking ref
#     (refs/remotes/origin/main), not a `git ls-remote` of the forge:
#     the sidecar mounts only /work and /scratch (boss-dev.yaml) — no
#     forge token, no HOME carrying the credential helper — and the
#     forge answers an anonymous read of david/boss with 401, so a
#     network read here would skip every hour. The ref is what the
#     operator's session and every builder worktree fetch (they share
#     it), and it is THE SAME ref the shim compares a candidate against
#     — one definition of "main" on the pod, so the sidecar installs
#     exactly the sha the shim will call current (CLAUDE.md §9a). A
#     checkout without the ref installs nothing and says so.
#   * the registry host reaches the installer the way it reaches every
#     managed host: a sor.env rendered from infra/estate/estate.toml
#     (render-sor-env.sh --to), handed as BOSS_SOR_ENV, so
#     forge-defaults.sh builds the image repo from it. Never a literal
#     (the lint the-estate-address-lives-once refuses one).
#   * the store and the link are under WORK_MOUNT: the sidecar runs
#     without root and /usr/local/bin is its own container overlay,
#     invisible to the dev container; /work is the shared mount.
#   * the installer's verdicts are the forge's install.sh's: 0 is the
#     tree's CLI confirmed through the link; 75 is "not yet" — the
#     deploy runner builds the image a few minutes after each train,
#     and this pass runs hourly, so the first pass after a train often
#     lands here; a wait, logged, never a problem; anything else is a
#     refusal (registry dark, digest mismatch, a binary naming another
#     commit) and a problem, loud and on the packet with the exit named.
#     The installer leaves `current` at the previous confirmed
#     generation in every non-zero case.
#
# The installer's output is captured whole and printed under a `cli:`
# prefix — a tail or a digest would throw away the only copy (CLAUDE.md
# §Diagnosis).
CLI_STORE="${BOSS_CLI_STORE:-$WORK_MOUNT/tools/image-cli}"
CLI_LINK="${BOSS_CLI_LINK:-$WORK_MOUNT/tools/bin/boss-image}"
CLI_SHA=""
CLI_RESULT=skipped
install_tree_cli() {
    local here installer render sha log rc
    here="$(dirname "$(readlink -f "$0")")"
    installer="${BOSS_CLI_INSTALLER:-$here/../estate/install-cli-from-image.sh}"
    render="$here/../estate/render-sor-env.sh"
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "CLI install skipped: $REPO_DIR is not a git checkout" >&2
        return 0
    fi
    if ! sha=$(git -C "$REPO_DIR" rev-parse --verify -q refs/remotes/origin/main 2>&1) || [ -z "$sha" ]; then
        log "CLI install skipped: $REPO_DIR has no refs/remotes/origin/main (git fetch origin in the checkout; ${sha:-empty answer})" >&2
        return 0
    fi
    case "$sha" in
        *[!0-9a-f]*|"") log "CLI install skipped: origin/main resolved to '$sha', not a sha" >&2; return 0 ;;
    esac
    if [ "${#sha}" -ne 40 ]; then
        log "CLI install skipped: origin/main resolved to '$sha' (${#sha} chars), not a full sha" >&2
        return 0
    fi
    CLI_SHA="$sha"
    if [ ! -f "$installer" ] || [ ! -f "$render" ]; then
        log "CLI install skipped: $installer or $render is missing beside this script" >&2
        return 0
    fi
    if ! mkdir -p "$CLI_STORE" || ! bash "$render" --to "$CLI_STORE/sor.env" >/dev/null; then
        log "CLI install FAILED: could not render $CLI_STORE/sor.env from infra/estate/estate.toml" >&2
        CLI_RESULT="failed: no sor.env"
        problems=$((problems + 1))
        return 0
    fi
    log "CLI: installing the tree's boss at ${sha:0:8} (origin/main) from the cluster image into $CLI_STORE"
    log=$(mktemp "${TMPDIR:-/tmp}/dev-scratch-reclaim-cli.XXXXXX")
    rc=0
    BOSS_SOR_ENV="$CLI_STORE/sor.env" BOSS_CLI_STORE="$CLI_STORE" BOSS_CLI_LINK="$CLI_LINK" \
        bash "$installer" "$sha" >"$log" 2>&1 || rc=$?
    sed 's/^/dev-scratch-reclaim:   cli: /' "$log"
    rm -f "$log"
    case "$rc" in
        0)  CLI_RESULT=ok
            log "CLI: the tree's boss at ${sha:0:8} is confirmed at $CLI_LINK (store $CLI_STORE); the shim runs it" ;;
        75) CLI_RESULT="not yet"
            log "CLI: not yet — the image for ${sha:0:8} is not in the registry (the deploy runner builds it a few minutes after each train); the next pass retries, and the store stays at $(readlink "$CLI_STORE/current" 2>/dev/null || echo none)" ;;
        *)  CLI_RESULT="failed: exit $rc"
            log "CLI install FAILED (exit $rc) at ${sha:0:8} — the installer's complete output is above; the store stays at $(readlink "$CLI_STORE/current" 2>/dev/null || echo none)" >&2
            problems=$((problems + 1)) ;;
    esac
}

# ---------------------------------------------------------------------
# THE RECORD: a packet for a pass that did something.
# ---------------------------------------------------------------------
# The sidecar's log is read by nobody (`kubectl logs -c reclaim`, by
# hand, on a pod whose restarts lose it) — CLAUDE.md §Diagnosis, a check
# nobody reads is a check that is not running. So a pass that removed
# anything, ended with a floor unmet, or could not answer (the worktree
# pass skipped, counted as a problem), opens a maintenance packet and
# completes its `run` step with the totals, through the SAME two
# helpers every timer chore uses (the timer is the executor, the Job is
# the visibility — boss-maintenance-wrap.sh) rather than a third
# packet-filing idiom. Where it files is not a default: BOSS_JOBS_URL,
# else the one spelling of the system of record every pod door reads,
# infra/dev/sor-url beside this checkout. Neither named, the pass runs
# unrecorded and says so; an unreachable API costs the same — the
# executor never waits on its visibility.
RECLAIM_KIND=maintenance-dev-scratch-reclaim
record_pass() {
    local acted
    acted=$((WT_REMOVED + WT_TARGETS_REMOVED + WT_PRUNED + FLOOR_WORKTREES_REMOVED + STALE_TARGETS_RECLAIMED + FLOOR_TARGETS_RECLAIMED + INCREMENTAL_DROPPED))
    if [ "$acted" -eq 0 ] && [ "$problems" -eq 0 ]; then
        log "nothing reclaimed and no floor unmet — a pass that only looked files no packet"
        return 0
    fi
    local here wrap step url result
    here="$(dirname "$(readlink -f "$0")")"
    wrap="$here/../boss-maintenance-wrap.sh"
    step="$here/../boss-step.sh"
    url="${BOSS_JOBS_URL:-$(head -n1 "$here/../dev/sor-url" 2>/dev/null || true)}"
    if [ -z "$url" ]; then
        log "no system of record named (BOSS_JOBS_URL unset, $here/../dev/sor-url absent) — this pass is unrecorded" >&2
        return 0
    fi
    if [ ! -f "$wrap" ] || [ ! -f "$step" ]; then
        log "$wrap or $step is missing beside this script — this pass is unrecorded" >&2
        return 0
    fi
    result=ok
    [ "$problems" -gt 0 ] && result=incomplete
    if ! BOSS_JOBS_URL="$url" bash "$wrap" "$RECLAIM_KIND" "Dev pod scratch reclaim"; then
        log "could not open the $RECLAIM_KIND packet — this pass is unrecorded" >&2
        return 0
    fi
    BOSS_JOBS_URL="$url" BOSS_STEP_ACTOR=automation:dev-scratch-reclaim \
        bash "$step" "$RECLAIM_KIND" run \
            "result=$result" "problems=$problems" \
            "worktree_pass=$WT_PASS" "worktree_pass_reason=$WT_PASS_REASON" \
            "ff_result=$FF_RESULT" "ff_to=$FF_TO" "ff_detail=$FF_DETAIL" \
            "origin_main_sha=$WT_MAIN_SHA" "origin_main_ref_ts=$WT_MAIN_TS" \
            "worktrees_removed=$WT_REMOVED" "worktrees_removed_mib=$WT_REMOVED_MIB" \
            "worktrees_removed_by_content=$WT_REMOVED_BY_CONTENT" \
            "worktrees_kept_dirty=$WT_KEPT_DIRTY" "worktrees_kept_dirty_names=$WT_KEPT_DIRTY_NAMES" \
            "worktrees_kept_unreferenced=$WT_KEPT_UNREFERENCED" "worktrees_kept_unreferenced_names=$WT_KEPT_UNREFERENCED_NAMES" \
            "worktrees_kept_live=$WT_KEPT_LIVE" "worktrees_kept_recent=$WT_KEPT_RECENT" \
            "worktrees_kept_locked=$WT_KEPT_LOCKED" "worktrees_kept_refused=$WT_KEPT_REFUSED" \
            "worktrees_stale_locks=$WT_STALE_LOCKS" "worktrees_stale_lock_names=$WT_STALE_LOCK_NAMES" \
            "worktrees_pruned=$WT_PRUNED" \
            "targets_removed=$WT_TARGETS_REMOVED" "targets_removed_mib=$WT_TARGETS_MIB" \
            "floor_worktrees_removed=$FLOOR_WORKTREES_REMOVED" "floor_worktrees_kept_unreferenced_names=$FLOOR_WORKTREES_KEPT_UNREFERENCED_NAMES" \
            "stale_targets_reclaimed=$STALE_TARGETS_RECLAIMED" \
            "floor_targets_reclaimed=$FLOOR_TARGETS_RECLAIMED" "floor_targets_mib=$FLOOR_TARGETS_MIB" \
            "incremental_dirs_dropped=$INCREMENTAL_DROPPED" \
            "cli_sha=$CLI_SHA" "cli_result=$CLI_RESULT" \
        || log "could not complete the $RECLAIM_KIND run step — its packet stays open for the next acting pass to complete" >&2
}

# `--cli`: the CLI leg alone, its exit the verdict — 0 confirmed, 75
# not yet (the installer's own code), 1 failed. The hourly pass above
# counts not yet as a wait, exit 0; here it is a STATUS, because since
# 2026-09-18 the shim (infra/dev/boss) runs this leg itself before a
# write verb and must tell "installed, proceed" from "wait for the
# deploy runner" without parsing the log (backlog 49d9e99d). Runnable
# from the dev container by hand as well.
if [ "${1:-}" = "--cli" ]; then
    install_tree_cli
    [ "$problems" -eq 0 ] || exit 1
    [ "$CLI_RESULT" != "not yet" ] || exit 75
    exit 0
fi

# `--scratch-floor`: the scratch floor's sibling pass ALONE, run by
# infra/dev/wt-cargo before every build (backlog 3f2a08ab). WHY: the
# dev pod was evicted at 2026-09-24 01:27Z with the 25% floor in force.
# The gate receipts' free_gb (GiB free on w-1's ephemeral xfs, one
# reading per verdict) date the drain: 487 at 21:47Z, 378 at 00:01,
# 284 at 00:53, 218 at 01:14, 159 at 01:20, 136 at the eviction, 567
# at 01:31 — ~430 GiB of it this pod's own /scratch. The hourly passes
# at 21:58, 22:59 and 23:59 took no floor target, and the ~01:00 pass
# filed nothing, so it found the floor met; from there w-1 went from
# above 232 GiB free to the kubelet's 139 in under 27 minutes. A 93 GiB
# margin read once an hour cannot hold a drain that crosses it in 27
# minutes, so the reclaim follows the EVENT that fills the disk, as the
# forge's did (the_reclaim_follows_the_build.rs): each build is where
# /scratch grows, so each build checks first.
#
# WHY THE FLOOR, AND NOT THE PRIORITY, IS THE PROTECTION. The kubelet
# ranks eviction candidates by "usage exceeds request" FIRST and by
# priority only within that class. This pod requests 100Gi and its
# /scratch held ~430 GiB of real bytes — more by the kubelet's measure,
# which walks st_blocks and so counts every reflink-seeded sibling at
# full size — while each gate stayed inside its 90Gi. So the pod was
# the only low-priority pod over its request, and the boss-dev-session
# class never came into play: once w-1 reaches the 15% line, this pod
# is first whatever its priority. Staying off that line is the only
# lever, and it is this one.
#
# What it does NOT do, on purpose: the worktree, age, checkout and CLI
# legs (the hourly pass's; a build must not wait on git or a registry),
# and the incremental trim (it defers whenever any cargo runs, which on
# a busy pod is always — that deferral is the hourly pass's to report).
# Above the floor it is one df and says nothing. Concurrent builds race
# to it, so one takes the lock and the rest build on.
if [ "${1:-}" = "--scratch-floor" ]; then
    kb=$(free_kb "$SCRATCH_MOUNT")
    [ -n "$kb" ] || exit 0
    floor_gb=$(( $(size_kb "$SCRATCH_MOUNT") / 1024 / 1024 * SCRATCH_FLOOR_PCT / 100 ))
    [ $((kb / 1024 / 1024)) -lt "$floor_gb" ] || exit 0
    if command -v flock >/dev/null 2>&1 && { exec 9>>"$SCRATCH_MOUNT/.dev-scratch-reclaim.lock"; } 2>/dev/null; then
        flock -n 9 || exit 0
    fi
    log "$SCRATCH_MOUNT $((kb / 1024 / 1024))GB free < ${floor_gb}GB floor (${SCRATCH_FLOOR_PCT}%) — the build-triggered floor pass"
    WT_PASS_REASON="scratch-floor: the build-triggered pass runs only the floor's sibling reclaim"
    reclaim_targets_to_floor "$floor_gb"
    record_pass
    exit 0
fi

fast_forward_checkout
install_tree_cli
reclaim_gone_worktrees
reclaim_work
reclaim_stale_targets
reclaim_scratch
record_pass

if [ "$problems" -gt 0 ]; then
    exit 1
fi
log "both workspaces above their floors — nothing more to do"
exit 0
