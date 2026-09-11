#!/usr/bin/env bash
# tenant-launch.sh — publish the tenant, then start the sim; DEGRADE,
# never brick. Sourced by services-launcher.sh; exercised by
# infra/lint/a-failed-prepare-degrades-the-pod.sh under stubs.
#
# WHY THIS EXISTS. On 2026-09-02 a Workflow row landed whose `task`
# step required a field the prepare walker did not send. The brewery
# prepare answered 400 three times, seed-brewery-tenant.sh exited 1
# (correctly — it refuses to stamp a reset baseline over a failed
# prepare, which is what saved the tenant model), and the launcher's
# `set -e` turned that exit into ITS exit. The container died, Kubernetes
# restarted it, and it died again: the jobs API was down for 65 minutes
# over a seed that was one field short — while every API had been up
# and serving on localhost throughout the walk, and the gateway, which
# starts AFTER the sim in the launch order, never started at all
# (packets 88a07cc4, 089a99bc).
#
# THE CONTRACT. A failed tenant prepare is a DEGRADED pod, not a dead
# one: every API stays up and the gateway starts, the sim stays down
# (it would post jobs the registry cannot admit), the reset baseline
# stays untouched (the seed script's own guard, unchanged), and prepare
# retries on a cadence until it succeeds — at which point the sim
# starts, late, exactly as it would have. The abort-before-baseline
# behaviour is the thing being preserved here, not worked around.
#
# HOW THE RETRY STAYS A CHILD THAT NEVER "EXITS". The launcher's
# `wait -n` ends the pod when ANY child leaves. The retry loop runs as
# a background subshell whose PID joins the launcher's list, and on
# success it EXECS the sim in place — the same PID becomes the sim, so
# the launcher sees one long-lived child, never a departure.
#
# LOUD. Every attempt and the degraded state itself print a line that
# starts with `DEGRADED:`, so the pod log can be grepped for the word
# by a reader that never saw the first failure.
#
# The three hooks below are functions so the self-test can replace
# them; the launcher uses the defaults.

# Publish the platform operator baseline, then the tenant. Both go
# through the public API. Non-zero means the tenant is NOT published
# and the baseline is untouched.
publish_tenant() {
    /opt/boss/infra/seed-operator-baseline.sh
    /opt/boss/infra/seed-brewery-tenant.sh
}

# The sim posts jobs the moment it starts, and their side effects fire
# only if the dispatcher's consumers are bound — gate on
# /api/dispatcher/readyz, bounded and loud (823fcb22 mechanism 1).
wait_for_dispatcher() {
    echo "    waiting for dispatcher readyz before the sim"
    local i
    for i in $(seq 1 60); do
        if curl -fsS -m 2 "http://127.0.0.1:7950/api/dispatcher/readyz" 2>/dev/null \
            | grep -q '"ready":[[:space:]]*true'; then
            echo "    dispatcher ready after ${i}s"
            return 0
        fi
        sleep 1
    done
    echo "    WARN: dispatcher not ready after 60s — starting the sim anyway (side effects may lag or dead-air; check /api/dispatcher/readyz)"
}

# Replace the current process with the sim. Called from a background
# subshell in both paths, so the subshell's PID becomes the sim's.
start_sim() {
    exec boss-brewery-sim 2>&1
}
# Whether the brewery tick daemon should run. Default: yes — the OSS
# quickstart and the public brewery demo want live data. The cluster
# sets BOSS_SIM_ENABLED=false to PARK the sim behind IT delivery
# reliability (2026-09-05, park packet 59f063de): the tenant is still
# published so the app keeps its brewery data, but no tick daemon spawns
# new packets onto the yard. A flag, not a code fork, so only the
# deployment that decided to pause it is affected.
sim_enabled() {
    case "${BOSS_SIM_ENABLED:-true}" in
        false|False|FALSE|0|no|No|NO|off|Off|OFF) return 1 ;;
        *) return 0 ;;
    esac
}
# When the sim is disabled AND this launch is a tracked background child
# (the degraded-then-cleared path), the child must NOT exit — the
# launcher's `wait -n` ends the pod on any child's departure. Hold the
# PID alive with no sim instead. A hook so the self-test can replace it.
hold_without_sim() {
    echo "    boss-brewery-sim DISABLED — tenant published, holding without a tick daemon (parked)"
    exec sleep infinity
}

# Publish the tenant, then start the sim in the background. Returns 0
# whether or not the publish succeeded — the caller keeps launching.
# Appends the sim's (or the retry loop's) PID to the array named by $1.
launch_tenant_and_sim() {
    local -n launched_pids=$1
    local retry="${BOSS_PREPARE_RETRY_SECONDS:-300}"
    if publish_tenant; then
        if sim_enabled; then
            ( wait_for_dispatcher; start_sim ) &
            launched_pids+=($!)
        else
            # Tenant published synchronously above; no tick daemon, so no
            # tracked child to add — the launcher simply runs one fewer
            # service and the pod's other long-lived children keep it up.
            echo "    boss-brewery-sim DISABLED (BOSS_SIM_ENABLED=${BOSS_SIM_ENABLED:-true}) — tenant published, tick daemon parked"
        fi
        return 0
    fi
    echo "DEGRADED: tenant prepare failed — every API stays up and the gateway starts; the sim stays DOWN until prepare succeeds; the reset baseline is untouched; retrying every ${retry}s" >&2
    (
        attempt=1
        until publish_tenant; do
            attempt=$((attempt + 1))
            echo "DEGRADED: tenant prepare still failing (attempt ${attempt} in ${retry}s)" >&2
            sleep "$retry"
        done
        echo "DEGRADED: cleared — tenant prepared on attempt ${attempt}" >&2
        if sim_enabled; then
            wait_for_dispatcher
            start_sim
        else
            hold_without_sim
        fi
    ) &
    launched_pids+=($!)
    return 0
}
