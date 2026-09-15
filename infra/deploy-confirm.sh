#!/usr/bin/env bash
# deploy-confirm.sh — the deploy dead-man switch (deployment-as-network
# Q4), run by boss-deploy-confirm.service off its timer.
#
# deploy-services.sh ACTIVATES a generation (symlink flip) and arms
# boss-deploy-confirm.timer; this script EVALUATES it. Two readings —
# the timer fires at +2m and +8m after the flip; the delayed second
# reading catches the silent-death class a start-time probe misses
# (a dispatcher that boots clean and dies minutes later):
#
#   reading 1 green  -> note it on the marker, keep waiting
#   reading 2 green  -> CONFIRMED: verdict recorded, marker cleared
#   any reading red  -> REVERT: `deploy-services.sh revert` flips
#                       current back to previous + restarts, the
#                       verdict is recorded, and this exits NONZERO so
#                       the failure is loud in the unit's status
#
# The health evaluation is `deploy-services.sh probe prod` — the same
# probe_one roster the deploy prints, so the confirm can never drift
# from the deploy list (CLAUDE.md §9a).
#
# No pending marker means nothing to evaluate (a boot-time firing, or
# a verdict already reached) — exit 0 quietly. Deliberately a separate
# unit, never in-process waiting inside the deployer: a dead-man that
# dies with the deployer reverts nothing.

set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/generation.sh
. "$DIR/generation.sh"

log() { echo "deploy-confirm: $*"; }

if [[ ! -f "$GEN_PENDING" ]]; then
    log "no pending deploy — nothing to confirm"
    exit 0
fi

marker_get() {
    grep "^$1=" "$GEN_PENDING" | sed -n 1p | cut -d= -f2-
}

SHA="$(marker_get sha || true)"
PREVIOUS="$(marker_get previous || true)"
FIRST_OK="$(marker_get first_ok || true)"
READING=1
[[ -n "$FIRST_OK" ]] && READING=2

log "evaluating generation ${SHA:-unknown} (previous ${PREVIOUS:-none}, reading $READING)"

if PROBE_OUT="$("$DIR/deploy-services.sh" probe prod 2>&1)"; then
    echo "$PROBE_OUT"
    if [[ "$READING" == "1" ]]; then
        echo "first_ok=$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$GEN_PENDING"
        gen_log "confirm sha=${SHA:-unknown} reading=1 ok"
        log "reading 1 green — second reading decides at +8m"
        exit 0
    fi
    rm -f "$GEN_PENDING"
    gen_log "confirm sha=${SHA:-unknown} verdict=confirmed"
    log "reading 2 green — generation ${SHA:-unknown} CONFIRMED"
    exit 0
fi

# A reading failed. Say exactly what, then roll back.
echo "$PROBE_OUT"
log "reading $READING FAILED for generation ${SHA:-unknown}"

if [[ -z "$PREVIOUS" || "$PREVIOUS" == "none" ]]; then
    # First-ever generation, or a store with no revert target: nothing
    # to flip back to. Record the verdict, clear the marker so this
    # does not re-fire forever, and fail loudly — a human owns this one.
    rm -f "$GEN_PENDING"
    gen_log "confirm sha=${SHA:-unknown} verdict=failed detail=no-previous-generation"
    log "NO previous generation to revert to — leaving ${SHA:-unknown} live; investigate by hand"
    exit 1
fi

# `revert` flips current <-> previous, restarts the prod fleet, and
# clears the pending marker itself (so the dead-man cannot fire on the
# revert). If the revert itself fails, set -e surfaces that here.
"$DIR/deploy-services.sh" revert
gen_log "confirm sha=${SHA:-unknown} verdict=reverted to=$PREVIOUS"
log "REVERTED to generation $PREVIOUS — deploy of ${SHA:-unknown} is rolled back"
exit 1
