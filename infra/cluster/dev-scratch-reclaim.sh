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
#   BOSS_WORKTREE_MAX_AGE_H  only prune worktrees older than (default 48)
# Paths (env, defaulted to the boss-dev layout):
#   REPO_DIR (/work/boss) WORKTREES_DIR (REPO_DIR/.claude/worktrees)
#   CARGO_TARGET_DIR (/scratch/target)
#   SCRATCH_MOUNT (/scratch) WORK_MOUNT (/work)
set -euo pipefail

SCRATCH_FLOOR_GB="${BOSS_SCRATCH_FLOOR_GB:-50}"
# Hours a SIBLING target dir may go untouched before it is a dead cache.
# Every builder gets its own CARGO_TARGET_DIR under the scratch mount
# (boss brief says so), and a landed branch's target outlives it by
# days; 12h is longer than any build and shorter than the next morning.
STALE_TARGET_H="${BOSS_STALE_TARGET_H:-12}"
WORK_FLOOR_GB="${BOSS_WORK_FLOOR_GB:-6}"
WORKTREE_MAX_AGE_H="${BOSS_WORKTREE_MAX_AGE_H:-48}"

REPO_DIR="${REPO_DIR:-/work/boss}"
WORKTREES_DIR="${WORKTREES_DIR:-$REPO_DIR/.claude/worktrees}"
TARGET_DIR="${CARGO_TARGET_DIR:-/scratch/target}"
SCRATCH_MOUNT="${SCRATCH_MOUNT:-/scratch}"
WORK_MOUNT="${WORK_MOUNT:-/work}"

for name in SCRATCH_FLOOR_GB WORK_FLOOR_GB WORKTREE_MAX_AGE_H STALE_TARGET_H; do
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
}

reclaim_work
reclaim_stale_targets
reclaim_scratch

if [ "$problems" -gt 0 ]; then
    exit 1
fi
log "both workspaces above their floors — nothing more to do"
exit 0
