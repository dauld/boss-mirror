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
# This script is the missing actuator: hourly, bounded by construction,
# and — for everything but step (0) below — below-floor only.
#
# AND SINCE 2026-09-10, ONE PASS THAT IS NOT FLOOR-TRIGGERED
# (backlog e5dc60e4). Per-train `boss-ci:<sha>` images in the SYSTEM
# daemon accumulated until they caused pressure MID-BUILD, because the
# only thing that ever removed one was this script below its floor.
# Measured on this unit's own journal, Sep 03 -> Sep 09: 145 runs, 45
# below the floor, 26 ending FLOOR UNMET, and the system-daemon image
# prune ran 41 times and freed 238GB — mean 5.8GB, peak 21.5GB a pass —
# every one of them below the floor, while the other 100 runs logged
# "nothing to do" and the pile grew back. On 2026-09-05 the pile was
# 81GB of a 228GB disk. A floor refusal happens BEFORE any check runs,
# so it says nothing about a branch and still strikes every car aboard:
# four clean cars, five departures (2026-08-22), and a held day
# (2026-09-05).
#
# So step (0) below runs EVERY hour, floor or no floor. It collects a
# per-train image whose TRAIN IS DONE at any age (the system of record
# knows that exactly — backlog 9195a2a6, and the long version is in
# landed-train-shas.lib.sh), and falls back to a LOOSER age window than
# the emergency pass in (a) for everything the record cannot vouch for.
# Routine pruning keeps the floor far away; the floor sweep still catches
# what pass (0) did not anticipate — a tighter window over every unused
# image in the daemon rather than one repo's sha tags. The two are
# complements, and the ordering between their windows is pinned by
# infra/lint/ci-images-are-pruned-by-age.sh.
#
# WHAT IT DOES, IN ORDER, stopping as soon as the floor is met and
# logging each action with the space it freed:
#   0. per-train CI images in the SYSTEM daemon whose TRAIN IS DONE,
#      plus anything the record cannot vouch for that is older than
#      CI_IMAGE_AGE_HOURS, keeping the newest few — NOT floor-gated
#   a. docker builder prune -af  (ALL build cache, no age filter)
#      (build cache is regenerable by definition; the converge runner
#      uses a gentler filter because it runs above the floor — below it,
#      a slower next build is the cheapest thing on the menu, so take
#      all of it. A 24h filter here left the floor unmet on 2026-09-04.)
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
# AGENT-WORKABLE TOO: once the ops allowlist (now infra/ops/verbs/) lands (branch
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

# THE TWO CI-IMAGE WINDOWS, NAMED SO THEIR ORDER CAN BE CHECKED.
# CI_IMAGE_AGE_HOURS is the ROUTINE window, applied hourly whatever the
# disk looks like and only to per-train sha tags the record cannot vouch
# for; CI_IMAGE_FLOOR_AGE_HOURS is the EMERGENCY one, applied below the
# floor to every unused image in the daemon. The routine number MUST stay
# the looser of the two — otherwise the emergency pass has nothing left
# to do and the backstop is just the cadence again. The ordering is
# pinned by infra/lint/ci-images-are-pruned-by-age.sh.
#
# 6h, and it is now the FALLBACK rather than the rule. The evidence that
# chose it: a train's whole CI is under an hour, so six hours is six
# times the longest thing that could still want the image; the next job's
# pull is a 3.47GB LAN re-pull of something the registry holds; and at
# the THEN-measured 5-14 trains a day (~3.5GB each) it bounded the pile
# at roughly 7-14GB instead of the 81GB found on 2026-09-05. A day was
# the first cut and a busy day defeated it; 168h (the converge runner's
# build-CACHE filter) is right for a cache every build reuses and wrong
# here, because a per-train image is pulled by exactly one train.
#
# AND THEN THE RATE ROSE AND THE WINDOW STOPPED BEING ABLE TO COLLECT
# ANYTHING (backlog 9195a2a6). Measured 2026-09-11 via ops-request
# 28d3599c: 13 `boss-ci:<sha>` tags in the system daemon, 21.18GB with
# 18.58GB reclaimable, aged fifteen minutes to FIVE HOURS — every one
# inside this six-hour window, so the hourly pass was correctly declining
# all of them. At the measured ~2.6 trains an hour (~60 a day, against
# the 5-14 the window was sized for) the arithmetic alone keeps ~16
# images permanently in-window, which is the whole pile. An age window is
# a PROXY for "this image will not be needed again", and the proxy broke
# when the rate changed.
#
# So the rule is now the FACT the record already holds — a tag whose
# `pr-train` packet is CLOSED is collectable, read per-pass by
# landed-train-shas.lib.sh — and this window covers only what the record
# cannot vouch for: an in-flight train's image, a gate-run's, a hand
# build's, and every image at all when the system of record cannot be
# read. Shortening it was the other candidate fix and it is the worse
# one: it would need re-deriving every time the train rate moves, which
# is exactly the failure above.
CI_IMAGE_AGE_HOURS="${BOSS_CI_IMAGE_AGE_HOURS:-6}"
CI_IMAGE_FLOOR_AGE_HOURS=4
# The newest few survive whatever their age: they are what a job that is
# starting right now pulls. Same keep-N idiom as the registry-tag loop.
CI_IMAGE_KEEP_NEWEST="${BOSS_CI_IMAGE_KEEP_NEWEST:-3}"
# The per-train CI image repo (.forgejo/workflows/ci.yml stamps
# `boss-ci:${GITHUB_SHA}` on every train's build-image job).
CI_IMAGE_REPO="${BOSS_CI_IMAGE_REPO:-10.20.0.15:3000/david/boss-ci}"
# HOW MANY TRAIN PACKETS TO READ. A limit is not a filter: this is a
# window on the newest packets, not the whole record, and an image whose
# train is older than the window resolves to nothing and falls to the age
# rule — today's behaviour, so the window is a reclaim bound and never a
# safety one. Measured 2026-09-11: the newest SIXTY packets spanned 20.7
# hours (a window opens every ~15 minutes and only about half of them
# board a car — 28 of those 60 assembled), and all 13 images then in the
# daemon resolved inside them. 120 is therefore ~40 hours against a pile
# whose oldest member was five hours and whose age fallback is six.
CI_TRAIN_LOOKBACK="${BOSS_CI_TRAIN_LOOKBACK:-120}"
# The reader, overridable so the pass is testable without a network.
SWEEP_CURL_CMD="${BOSS_SWEEP_CURL_CMD:-curl}"
# WHICH DAEMON, EXPLICITLY, BOTH HALVES. `sudo -n docker` is root's
# docker — the SYSTEM daemon the Actions jobs run in — and sudo's
# env_reset is what keeps the DOCKER_HOST exported above from following
# it there. That is load-bearing enough to state twice: the expected
# data-root goes with it and the library refuses when the daemon it
# reached does not match, because a prune aimed at the rootless daemon
# reports success and frees nothing.
SYSTEM_DOCKER="${BOSS_CI_IMAGE_DOCKER:-sudo -n docker}"
SYSTEM_DOCKER_ROOT="${BOSS_CI_IMAGE_DAEMON_ROOT:-/var/lib/docker}"
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

# (0) THE ROUTINE PASS — per-train CI images whose TRAIN IS DONE, plus
# the age window for everything the record cannot vouch for, every hour,
# floor or no floor. First because it is the gentlest remediation on the
# menu and the one aimed at the largest measured consumer; everything
# below it then decides against the space this has already freed.
#
# THE LOOKUP CANNOT MAKE THIS RUN FAIL, AND CANNOT MAKE IT DELETE MORE.
# `landed_train_shas` always returns 0 and prints nothing when it cannot
# answer — an unset BOSS_JOBS_URL, an unreachable or erroring system of
# record, a reply that does not parse, a reply that lists no trains — and
# an empty set leaves the prune loop applying the age window alone, which
# is what it did before this existed. Visibility is best-effort;
# destruction is not. Everything below this line is likewise independent
# of the record, so an SoR outage never weakens the floor defence.
# shellcheck source=infra/forge/landed-train-shas.lib.sh
. "$(dirname "$0")/landed-train-shas.lib.sh"
LANDED_SHAS="$(landed_train_shas "$SWEEP_CURL_CMD" "${BOSS_JOBS_URL:-}" \
    "$CI_TRAIN_LOOKBACK" disk-floor-sweep)"
# shellcheck source=infra/forge/prune-ci-images.lib.sh
. "$(dirname "$0")/prune-ci-images.lib.sh"
AGE_PRUNE_RC=0
prune_ci_images "$SYSTEM_DOCKER" "$SYSTEM_DOCKER_ROOT" "$CI_IMAGE_REPO" \
    "$CI_IMAGE_AGE_HOURS" "$CI_IMAGE_KEEP_NEWEST" disk-floor-sweep \
    "$LANDED_SHAS" || AGE_PRUNE_RC=$?
age_kb=$(free_kb)
echo "disk-floor-sweep: CI-image prune freed $(((age_kb - last_kb) / 1024))MiB on / (now $((age_kb / 1024 / 1024))GB free)"
last_kb=$age_kb

# A PASS THAT COULD NOT LOOK IS NOT A CLEAN RUN. The unit goes red and
# its packet closes "Maintenance failed", because the alternative is the
# exact failure this pass exists to end: a sweep that reports success
# while the images pile up (e5dc60e4). Every healthy exit goes through
# here; the FLOOR UNMET exit at the bottom is already non-zero.
finish() { # $1 = the exit code the floor logic reached
    if [ "$AGE_PRUNE_RC" -ne 0 ] && [ "${1:-0}" -eq 0 ]; then
        echo "disk-floor-sweep: the floor is fine, but the hourly CI-image prune could not reach the daemon it was asked to prune (reason named above) — that pass is what keeps the floor far away, so this run is a failure, not a quiet success. A system of record it could not READ is a different case and is not a failure: that one only means less was collected." >&2
        exit 1
    fi
    exit "${1:-0}"
}

if [ $((last_kb / 1024 / 1024)) -ge "$FLOOR_GB" ]; then
    echo "disk-floor-sweep: $((last_kb / 1024 / 1024))GB free >= ${FLOOR_GB}GB floor — nothing to do"
    finish 0
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
    finish 0
}

# (a) Build cache — ALL of it, no age filter. We only reach here when
# already BELOW the floor (the early return above handles the healthy
# case), and below the floor a slower next cold build is the cheapest
# thing on the menu — the whole point of the sweep. The old
# `--filter until=24h` kept recent cache and so left the floor unmet in
# the exact band where CI blocks (2026-09-04: 65GB, needed 70, cache all
# <24h, sweep freed nothing — it took a hand `docker builder prune -af`).
# `|| true`: a docker hiccup here must not stop the remaining
# remediations — the unmet floor at the end is what fails loudly.
# THE DAEMON CI RUNS IN, FIRST. This host has two docker daemons
# (reap-dead-ci-jobs header): david's rootless one, where the converge
# builds and which every step below prunes, and the SYSTEM one that
# Forgejo Actions jobs run in. Every train's build-image job leaves a
# 10.20.0.15:3000/david/boss-ci:<sha> tag in the system daemon and
# nothing ever removed one: measured 2026-09-05 03:19 (disk-report),
# 68 images, 85.88GB, 83.28GB reclaimable (96%) — the bulk of a 228GB
# disk with 71GB free, while this script pruned the rootless daemon
# to the floor and reported "a human decides next". Unused images
# older than FOUR HOURS: the running job's image is in use and docker
# will not prune it; a train's whole CI is under an hour; the newest
# tag is what the next job pulls and is a LAN re-pull (3.47GB) away if
# pruned. A day was the first cut, and a day is exactly what a busy
# day defeats: on 2026-09-05 eight trains left eight per-train images
# (~40GB) under the age filter, the forge slid 109 → 71GB free between
# builds, and this sweep at 17:10 reported FLOOR UNMET at 98GB with
# nothing left it was allowed to remove. `sudo -n`
# because the system socket is root's — the same sudo the converge
# runner uses for `docker run`; refused means the step is skipped and
# says so, never a silent zero.
# THE FILTER AND ITS LABEL ARE ONE FACT (§9a). This logged "older than
# 24h" for an `until=4h` filter for days — a record describing work
# nobody did, in the journal an operator reads during a disk incident.
# shellcheck disable=SC2086
if $SYSTEM_DOCKER image prune -af --filter "until=${CI_IMAGE_FLOOR_AGE_HOURS}h" 2>&1 | tail -1; then
    :
else
    echo "disk-floor-sweep: system-daemon image prune skipped — \`$SYSTEM_DOCKER\` refused"
fi
if floor_met_after "system-daemon unused-image prune (older than ${CI_IMAGE_FLOOR_AGE_HOURS}h)"; then
    done_at "system-daemon image prune"
fi
docker builder prune -af || true
if floor_met_after "builder cache prune (all)"; then
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
