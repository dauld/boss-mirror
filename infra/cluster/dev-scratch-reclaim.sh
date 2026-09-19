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
#   * SCRATCH (/scratch, the node-local emptyDir): CARGO_TARGET_DIR.
#     Reclaim = drop the regenerable incremental-compilation cache.
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
# WHAT RUNS IT. The `reclaim` sidecar in boss-dev.yaml fires it hourly
# (the disk-floor-sweep.timer cadence: above the floor a pass is one
# log line; below it a pass frees GBs, well ahead of the fill rate).
# It is also safe to run by hand:
#   kubectl exec -n boss-dev deploy/boss-dev -c reclaim -- \
#     bash /work/boss/infra/cluster/dev-scratch-reclaim.sh
#
# Tunables (env, with in-sidecar defaults):
#   BOSS_SCRATCH_FLOOR_GB    free GB to keep on /scratch     (default 50)
#   BOSS_STALE_TARGET_H      hours before a sibling target dir is dead (default 12)
#   BOSS_WORK_FLOOR_GB       free GB to keep on /work        (default 6)
#   BOSS_WORKTREE_MAX_AGE_H  only prune worktrees older than; also the
#                            git quiet before an UNPUSHED worktree (no
#                            origin/ ref, not on origin/main) counts as
#                            abandoned                      (default 48)
#   BOSS_WORKTREE_GRACE_H    hours of git quiet before a LANDED
#                            worktree is removable          (default 12)
#   BOSS_WORKTREE_IDLE_H     hours of git quiet before a main/detached
#                            worktree is removable          (default 168)
#   BOSS_JOBS_URL            the system of record the pass records on;
#                            else the line in REPO_DIR/infra/dev/sor-url
# Paths (env, defaulted to the boss-dev layout):
#   REPO_DIR (/work/boss) WORKTREES_DIR (REPO_DIR/.claude/worktrees)
#   CARGO_TARGET_DIR (/scratch/target)
#   SCRATCH_MOUNT (/scratch) WORK_MOUNT (/work)
#   BOSS_CLI_STORE (WORK_MOUNT/tools/image-cli) — the image CLI's
#     generations, what the shim reads; BOSS_CLI_LINK
#     (WORK_MOUNT/tools/bin/boss-image) — the image CLI on PATH by its
#     own name, whatever the shim decides; BOSS_CLI_INSTALLER — the
#     installer to run (the estate's, beside this checkout; a test
#     stubs it)
set -euo pipefail

SCRATCH_FLOOR_GB="${BOSS_SCRATCH_FLOOR_GB:-50}"
# Hours a SIBLING target dir may go untouched before it is a dead cache.
# Every builder gets its own CARGO_TARGET_DIR under the scratch mount
# (boss brief says so), and a landed branch's target outlives it by
# days; 12h is longer than any build and shorter than the next morning.
STALE_TARGET_H="${BOSS_STALE_TARGET_H:-12}"
WORK_FLOOR_GB="${BOSS_WORK_FLOOR_GB:-6}"
WORKTREE_MAX_AGE_H="${BOSS_WORKTREE_MAX_AGE_H:-48}"
# Hours of git QUIET (no commit, no HEAD, index or reflog write) before a
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

REPO_DIR="${REPO_DIR:-/work/boss}"
WORKTREES_DIR="${WORKTREES_DIR:-$REPO_DIR/.claude/worktrees}"
TARGET_DIR="${CARGO_TARGET_DIR:-/scratch/target}"
SCRATCH_MOUNT="${SCRATCH_MOUNT:-/scratch}"
WORK_MOUNT="${WORK_MOUNT:-/work}"

for name in SCRATCH_FLOOR_GB WORK_FLOOR_GB WORKTREE_MAX_AGE_H STALE_TARGET_H WORKTREE_GRACE_H WORKTREE_IDLE_H; do
    case "${!name}" in
        ''|*[!0-9]*)
            echo "dev-scratch-reclaim: $name must be a whole number, got '${!name}'" >&2
            exit 64
            ;;
    esac
done

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

problems=0

# Totals every pass leaves for the record at the end.
WT_PASS=skipped; WT_PASS_REASON=""
WT_MAIN_SHA=""; WT_MAIN_TS=""
WT_REMOVED=0; WT_REMOVED_MIB=0
WT_KEPT_DIRTY=0; WT_KEPT_DIRTY_NAMES=""
WT_KEPT_LIVE=0; WT_KEPT_RECENT=0; WT_KEPT_LOCKED=0; WT_KEPT_REFUSED=0
WT_PRUNED=0
WT_TARGETS_REMOVED=0; WT_TARGETS_MIB=0
FLOOR_WORKTREES_REMOVED=0
STALE_TARGETS_RECLAIMED=0
INCREMENTAL_DROPPED=0

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
#      one: every error here is on the side of keeping;
#   2. git has been QUIET in it for the window (1) chose — no commit,
#      no HEAD, index or reflog write — because a builder's tree is
#      clean for the minutes between its commit and its push;
#   3. the tree is CLEAN: `git status --porcelain` empty, untracked
#      files included. A dirty tree is kept and NAMED with its count,
#      in the log and on the packet, so an operator can decide.
# Locked worktrees, the main checkout and the one this run stands in
# are never candidates. `git worktree remove` still runs without
# --force, a second lock on (3).
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
# commit's time and the mtimes of the worktree's own HEAD, index and
# reflog — every git command that could mean "in use" touches one of
# those. Read BEFORE `git status`, which may itself refresh the index.
worktree_last_activity() {
    local path="$1" gitdir f t newest
    newest=$(git -C "$path" log -1 --format=%ct 2>/dev/null || echo 0)
    gitdir=$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null || true)
    for f in "$gitdir/HEAD" "$gitdir/index" "$gitdir/logs/HEAD" "$path"; do
        [ -e "$f" ] || continue
        t=$(stat -c %Y "$f" 2>/dev/null || echo 0)
        [ "$t" -gt "$newest" ] && newest=$t
    done
    echo "${newest:-0}"
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

    local self now
    self="$(pwd -P 2>/dev/null || echo /nonexistent)"
    now=$(date +%s)

    # `path<TAB>branch<TAB>locked` per worktree, `detached` standing in
    # for a HEAD with no branch; the first block is the main worktree.
    local first=1 path branch locked window why last idle_h dirty kb
    while IFS=$'\t' read -r path branch locked; do
        [ -z "$path" ] && continue
        if [ "$first" = 1 ]; then first=0; continue; fi
        [ "$path" = "$REPO_DIR" ] && continue
        case "$self" in "$path"|"$path"/*) continue ;; esac
        [ -d "$path" ] || continue
        if [ "$locked" = 1 ]; then
            WT_KEPT_LOCKED=$((WT_KEPT_LOCKED + 1))
            continue
        fi

        case "$branch" in
            detached|main)
                window=$WORKTREE_IDLE_H; why="on $branch"
                ;;
            *)
                if git -C "$REPO_DIR" rev-parse --verify -q "refs/remotes/origin/$branch" >/dev/null 2>&1; then
                    WT_KEPT_LIVE=$((WT_KEPT_LIVE + 1))
                    continue
                fi
                if git -C "$REPO_DIR" merge-base --is-ancestor "refs/heads/$branch" "$main_sha" 2>/dev/null; then
                    window=$WORKTREE_GRACE_H; why="branch $branch has no origin/ ref and its head is on origin/main (landed)"
                else
                    window=$WORKTREE_MAX_AGE_H; why="branch $branch has no origin/ ref and is not on origin/main (unpushed)"
                fi
                ;;
        esac

        last=$(worktree_last_activity "$path")
        idle_h=$(( (now - last) / 3600 ))
        if [ "$idle_h" -lt "$window" ]; then
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

        kb=$(du -sk "$path" 2>/dev/null | awk '{print $1}')
        if ! git -C "$REPO_DIR" worktree remove "$path" 2>/dev/null; then
            log "  kept $path (git refused to remove it without --force; $why, idle ${idle_h}h)"
            WT_KEPT_REFUSED=$((WT_KEPT_REFUSED + 1))
            continue
        fi
        WT_REMOVED=$((WT_REMOVED + 1))
        WT_REMOVED_MIB=$((WT_REMOVED_MIB + ${kb:-0} / 1024))
        log "  removed worktree $path ($((${kb:-0} / 1024))MiB; $why, idle ${idle_h}h, clean)"
        remove_worktree_target "$path"
    done < <(
        git -C "$REPO_DIR" worktree list --porcelain 2>/dev/null | awk '
            /^worktree / { if (p != "") print p "\t" b "\t" l; p=substr($0, 10); b="detached"; l=0 }
            /^branch /   { b=substr($0, 8); sub("^refs/heads/", "", b) }
            /^locked/    { l=1 }
            END { if (p != "") print p "\t" b "\t" l }
        '
    )

    log "worktree pass: removed $WT_REMOVED worktree(s) (${WT_REMOVED_MIB}MiB) and $WT_TARGETS_REMOVED target(s) (${WT_TARGETS_MIB}MiB); kept $WT_KEPT_LIVE with an origin/ ref still in the checkout, $WT_KEPT_RECENT with git activity inside the window, $WT_KEPT_DIRTY dirty, $WT_KEPT_LOCKED locked, $WT_KEPT_REFUSED refused by git; pruned $WT_PRUNED entries whose directory was gone"
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
        log "$WORK_MOUNT ${gb}GB free >= ${WORK_FLOOR_GB}GB floor — no worktree reclaim"
        return 0
    fi
    if [ ! -d "$REPO_DIR/.git" ] && [ ! -f "$REPO_DIR/.git" ]; then
        log "$REPO_DIR is not a git checkout — cannot prune worktrees"
        return 0
    fi
    log "$WORK_MOUNT ${gb}GB free < ${WORK_FLOOR_GB}GB floor — pruning stale git worktrees"

    # Metadata first: drop admin entries for worktree dirs that are
    # already gone. Cheap and always safe.
    git -C "$REPO_DIR" worktree prune -v 2>&1 | sed 's/^/dev-scratch-reclaim:   /' || true

    # This run's own worktree is off limits — never saw the axe fall on
    # the branch it is standing on.
    local self
    self="$(pwd -P 2>/dev/null || echo /nonexistent)"

    # `path<TAB>locked` per worktree; the first block is the main
    # worktree. Locked worktrees carry a `locked` line — skip those.
    local removed=0 skipped=0 first=1 path locked
    while IFS=$'\t' read -r path locked; do
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

        # No --force: a dirty worktree is refused and left standing, so
        # uncommitted work is never discarded. Committed work is in the
        # object store either way.
        if git -C "$REPO_DIR" worktree remove "$path" 2>/dev/null; then
            log "  removed stale worktree $path"
            removed=$((removed + 1))
        else
            log "  kept $path (uncommitted changes or in use — not forced)"
            skipped=$((skipped + 1))
        fi
    done < <(
        git -C "$REPO_DIR" worktree list --porcelain 2>/dev/null | awk '
            /^worktree / { if (p != "") print p "\t" l; p=substr($0, 10); l=0 }
            /^locked/    { l=1 }
            END { if (p != "") print p "\t" l }
        '
    )

    git -C "$REPO_DIR" worktree prune 2>/dev/null || true
    FLOOR_WORKTREES_REMOVED=$removed

    kb=$(free_kb "$WORK_MOUNT")
    gb=$((kb / 1024 / 1024))
    log "worktree reclaim: removed $removed, kept $skipped — $WORK_MOUNT now ${gb}GB free"
    if [ "$gb" -lt "$WORK_FLOOR_GB" ]; then
        log "WORK FLOOR UNMET — ${gb}GB free < ${WORK_FLOOR_GB}GB after pruning every eligible worktree." >&2
        log "  Remaining use is the clone, locked worktrees, or worktrees with uncommitted work. A human decides next." >&2
        problems=$((problems + 1))
    fi
}

# ---------------------------------------------------------------------
# SCRATCH: the regenerable incremental cache under CARGO_TARGET_DIR.
# ---------------------------------------------------------------------
reclaim_scratch() {
    local kb gb
    kb=$(free_kb "$SCRATCH_MOUNT")
    if [ -z "$kb" ]; then
        log "$SCRATCH_MOUNT not mounted — skipping build-cache reclaim"
        return 0
    fi
    gb=$((kb / 1024 / 1024))
    if [ "$gb" -ge "$SCRATCH_FLOOR_GB" ]; then
        log "$SCRATCH_MOUNT ${gb}GB free >= ${SCRATCH_FLOOR_GB}GB floor — no build-cache reclaim"
        return 0
    fi
    log "$SCRATCH_MOUNT ${gb}GB free < ${SCRATCH_FLOOR_GB}GB floor — reclaiming regenerable build cache"

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
    acted=$((WT_REMOVED + WT_TARGETS_REMOVED + WT_PRUNED + FLOOR_WORKTREES_REMOVED + STALE_TARGETS_RECLAIMED + INCREMENTAL_DROPPED))
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
            "origin_main_sha=$WT_MAIN_SHA" "origin_main_ref_ts=$WT_MAIN_TS" \
            "worktrees_removed=$WT_REMOVED" "worktrees_removed_mib=$WT_REMOVED_MIB" \
            "worktrees_kept_dirty=$WT_KEPT_DIRTY" "worktrees_kept_dirty_names=$WT_KEPT_DIRTY_NAMES" \
            "worktrees_kept_live=$WT_KEPT_LIVE" "worktrees_kept_recent=$WT_KEPT_RECENT" \
            "worktrees_kept_locked=$WT_KEPT_LOCKED" "worktrees_kept_refused=$WT_KEPT_REFUSED" \
            "worktrees_pruned=$WT_PRUNED" \
            "targets_removed=$WT_TARGETS_REMOVED" "targets_removed_mib=$WT_TARGETS_MIB" \
            "floor_worktrees_removed=$FLOOR_WORKTREES_REMOVED" \
            "stale_targets_reclaimed=$STALE_TARGETS_RECLAIMED" \
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
