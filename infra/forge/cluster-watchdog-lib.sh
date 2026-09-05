#!/usr/bin/env bash
# cluster-watchdog-lib — the decision the watchdog makes, as a pure
# function a lint can exercise. Sourced by cluster-watchdog.sh.
#
# WHY. "Every morning I come back to a broken system instead of one
# that knows it is working" (David, 2026-09-05). That morning the
# system of record had been dark for four hours: a head bricked its
# boot, the converge's rollback rolled to the wrong image, and the one
# loop that could have restored it could not start because its own
# visibility step needed the API. Every watcher reported THROUGH the
# thing it watched. This loop owes it nothing: it reads the API from
# the outside, compares what the cluster serves with what the converge
# last stamped as good, and acts — rolling the deployment to that named
# build when the API has been dark longer than a deploy takes — before
# a human is awake to notice.
#
# The decision, given four facts:
#   live      "up" | "down"           the API answered /api/jobs/health
#   image     the deployment's boss image tag right now
#   stamp     the last converged tag (the runner's stamp file) or "none"
#   dark      consecutive checks the API has been down (this one included)
# and the threshold DARK_LIMIT (checks; at a 5-minute cadence, 3 = a
# deploy's worth of dark plus margin):
#   ok               the API answers — say so, do nothing
#   wait             dark, but not yet past the threshold
#   roll-to-stamp    dark past the threshold and the cluster serves an
#                    image other than the last converged one: roll it
#                    there by name — the named lever, run by the machine
#   hands            dark past the threshold on the last converged build
#                    itself (or with no stamp to roll to): nothing this
#                    loop can safely do — say so as loudly as it can
watchdog_decision() {
    local live="$1" image="$2" stamp="$3" dark="$4" limit="${5:-3}"
    if [ "$live" = "up" ]; then echo ok; return; fi
    if [ "$dark" -lt "$limit" ]; then echo wait; return; fi
    if [ -n "$stamp" ] && [ "$stamp" != "none" ] && [ "$image" != "$stamp" ]; then
        echo roll-to-stamp; return
    fi
    echo hands
}
