#!/usr/bin/env bash
#
# disk-floor-sweep — keep the forge host's root volume above a
# free-space floor by reclaiming REGENERABLE DOCKER CACHES, and
# nothing else.
#
# Why this exists: on 2026-09-02 the forge/CI host's disk filled and
# blocked CI for every train. The three mechanisms that already knew
# about disk pressure could not act:
#   - cluster-deploy-runner.sh prunes images + build cache, but only
#     when a converge runs — and a converge needs forge main to move,
#     which needs CI, which needs disk. Circular exactly when it
#     matters.
#   - locomotive.sh (the CI preflight) REFUSES below its floor and
#     documents the remediation, but refusing is all a preflight can
#     do.
#   - the disk-headroom sweep FILES packets but acts on nothing.
# This script is the missing actuator: hourly, below-floor only,
# bounded by construction.
#
# WHAT IT DOES, IN ORDER, stopping as soon as the floor is met and
# logging each action with the space it freed:
#   a. docker builder prune -f --filter until=24h
#      (build cache is regenerable by definition; the converge runner
#      uses a gentler 168h because it runs above the floor — below it,
#      a slower next build is the cheapest thing on the menu)
#   b. docker image prune -f            (dangling images only, no -a)
#   c. registry-verified old-tag removal — the SAME loop as its
#      sibling cluster-deploy-runner.sh, via the shared
#      prune-registry-tags.lib.sh: keep the N newest tags, verify a
#      candidate is present in the registry before rmi, never touch
#      `latest`.
#
# WHAT IT NEVER DOES: volumes (a CI job's workspace volume is NAMED
# and may be the corpse of a crashed job — reap-dead-ci-jobs owns
# those), non-docker paths, or anything not regenerable by
# definition. If the floor is still unmet after (a)–(c) it exits
# non-zero LOUDLY and stops: a failed unit plus the disk-headroom
# sweep's packets are the alarm. Escalating to more aggressive
# deletion is a human's call, never this script's.
#
# AGENT-WORKABLE TOO: once infra/ops/verbs.json lands (branch
# feat/ops-request-the-host-answers), `reclaim-disk` registers THIS
# script as the first mutating ops verb — authorized by David,
# 2026-09-03, bounded to regenerable caches by construction of what
# it calls. One definition of the remediation (§9a) whether the timer
# fires it or a packet does; the registration rides
# feat/reclaim-disk-is-a-verb so it can land with (or after) the ops
# branch without this car depending on it.
#
# Install (forge host): disk-floor-sweep is in install.sh's UNITS
# list, so the standing idiom covers it —
#   ssh 10.20.0.15 'cd /home/david/boss && git pull && sudo infra/forge/install.sh'
#
# Usage: disk-floor-sweep.sh [floor_gb]
#   floor_gb overrides BOSS_DISK_FLOOR_GB (default 70, = CI's floor). The optional
#   arg is what the reclaim-disk ops verb passes.
set -euo pipefail

REGISTRY="${BOSS_FORGE_REGISTRY:-10.20.0.15:3000/david/boss}"
export DOCKER_HOST="${DOCKER_HOST:-unix:///run/user/1000/docker.sock}"

# 70, not 25, and it MUST match locomotive.sh's BOSS_CI_MIN_FREE_GB (§9a
# — one number wearing two names). The sweep's job is to keep at least
# what CI needs to START a cold build. A 25GB floor defended NOTHING in
# the 65-70GB band where CI actually refuses (LOCOMOTIVE RED, need 70):
# the sweep logged "nothing to do" at 67GB free while train after train
# died there on 2026-09-04. If these two floors ever diverge, the sweep
# keeps less than CI needs and every build gambles on luck.
FLOOR_GB="${1:-${BOSS_DISK_FLOOR_GB:-70}}"
case "$FLOOR_GB" in
    ''|*[!0-9]*)
        echo "disk-floor-sweep: floor must be a whole number of GB, got '$FLOOR_GB'" >&2
        exit 64
        ;;
esac

# Free space on the ROOT volume — this host's docker store, OCI
# registry and CI workspaces all live on it; it is the disk that
# filled. `df -P` for POSIX columns.
free_kb() { df -Pk / | awk 'NR==2 {print $4}'; }

last_kb=$(free_kb)
if [ $((last_kb / 1024 / 1024)) -ge "$FLOOR_GB" ]; then
    echo "disk-floor-sweep: $((last_kb / 1024 / 1024))GB free >= ${FLOOR_GB}GB floor — nothing to do"
    exit 0
fi
echo "disk-floor-sweep: $((last_kb / 1024 / 1024))GB free < ${FLOOR_GB}GB floor — reclaiming regenerable docker caches"

# Measure one remediation's effect and answer "is the floor met now?".
# df's KB granularity means a small reclaim can round to 0MiB; the
# docker output above each line is the ground truth, this is the
# running account.
floor_met_after() { # step-name
    local now_kb
    now_kb=$(free_kb)
    echo "disk-floor-sweep: $1 freed $(((now_kb - last_kb) / 1024))MiB (now $((now_kb / 1024 / 1024))GB free)"
    last_kb=$now_kb
    [ $((now_kb / 1024 / 1024)) -ge "$FLOOR_GB" ]
}

done_at() { # step-name
    echo "disk-floor-sweep: floor met after $1 — stopping"
    exit 0
}

# (a) Build cache, 24h age filter. `|| true`: a docker hiccup here
# must not stop the remaining remediations — the unmet floor at the
# end is what fails loudly.
docker builder prune -f --filter until=24h || true
if floor_met_after "builder cache prune (until=24h)"; then
    done_at "builder cache prune"
fi

# (b) Dangling images only — deliberately no -a, which would take
# tagged images the registry loop below handles with verification.
docker image prune -f || true
if floor_met_after "dangling image prune"; then
    done_at "dangling image prune"
fi

# (c) Old boss tags, keep-N-newest, verified in the registry before
# rmi — the one shared definition (see the lib header for why the
# verification is load-bearing).
# shellcheck source=infra/forge/prune-registry-tags.lib.sh
. "$(dirname "$0")/prune-registry-tags.lib.sh"
prune_registry_verified_tags "$REGISTRY" "${BOSS_RUNNER_KEEP_IMAGES:-5}" disk-floor-sweep
if floor_met_after "registry-verified old-tag removal"; then
    done_at "registry-verified old-tag removal"
fi

echo "disk-floor-sweep: FLOOR UNMET — $((last_kb / 1024 / 1024))GB free < ${FLOOR_GB}GB after every bounded remediation." >&2
echo "disk-floor-sweep: this script will NOT touch volumes or non-docker paths on its own." >&2
echo "disk-floor-sweep: likely culprits are NAMED volumes of dead CI jobs (reap-dead-ci-jobs)" >&2
echo "disk-floor-sweep: or genuine growth — see locomotive.sh's remediation notes. A human decides next." >&2
exit 1
