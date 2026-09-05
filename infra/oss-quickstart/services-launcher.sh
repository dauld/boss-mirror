#!/usr/bin/env bash
# Tiny supervisor — starts every BOSS service binary in the
# background, captures their PIDs, and waits. SIGTERM forwards to
# every child; if any child exits, the launcher exits too so
# Docker's restart policy can kick in.
#
# This isn't a real init system (no log rotation, no restart-on-
# crash, no dependency ordering beyond start-everything-in-parallel).
# It's the simplest thing that lets `docker compose up boss-services`
# bring the full stack up. Production deploys use systemd directly;
# tenants who outgrow the docker quickstart have the runbook for
# that path.

set -euo pipefail

# Each entry: "<binary> <port-env-var>=<port> [extra env]"
#
# All binaries read BOSS_POSTGRES_URL + BOSS_NATS_URL from the env;
# the per-service URLs that consumers use are also driven from
# boss-ports defaults, so we don't need to set BOSS_*_URL here as
# long as the hostnames resolve in-container (they do — every
# service runs in this same container, so localhost works).
SERVICES=(
    # Clock first — the authoritative "what time is it" service
    # (boss-clock + boss-clock-client). The brewery-sim probes
    # /api/clock/now at startup and event time is clock-authoritative,
    # so the clock must be listening before the sim (and before any
    # event-emitting service) comes up.
    "boss-clock-api"
    "boss-policy-api"
    "boss-classes-api"
    "boss-locations-api"
    "boss-subject-kinds-api"
    "boss-people-api"
    "boss-accounts-api"
    "boss-assets-api"
    "boss-catalog-api"
    "boss-campaigns-api"
    "boss-customers-api"
    "boss-products-api"
    "boss-commerce-api"
    "boss-inventory-api"
    "boss-shipping-api"
    "boss-messages-api"
    "boss-calendar-api"
    "boss-content-api"
    "boss-ledger-api"
    "boss-docs-api"
    "boss-events-api"
    # boss-event-relay: the single mover from the transactional
    # event_outbox into audit_log + NATS (outbox phase 2). Every
    # migrated domain's events — including the dispatcher's
    # step.done/step.ready signals from boss-jobs — stage on the
    # outbox and reach NATS only through this relay. Without it the
    # dispatcher never hears a step complete and NO side effects
    # fire (2026-07-28 smoke failure: invoices=0 forever). Borrows
    # the jobs config for postgres_url + nats_url, same as the
    # systemd unit.
    "boss-event-relay"
    "boss-jobs-api"
    # boss-observability is a non-`-api` service: NATS aggregator +
    # /api/snapshot for the /ops dashboard. Required for /ops to
    # render anything — without it the gateway's /api/snapshot proxy
    # returns 502 and the page reads as broken. Brewery deploys
    # configure [demo_agents] so the snapshot ships synthetic agent
    # telemetry (with the SPA-side "demo mode" banner on /ops).
    "boss-observability"
    # The views tier + search + ML + the simulator UX. These four
    # were absent from this roster while present in boss-ports and
    # deploy-services.sh — the fact-lives-thrice drift (CLAUDE.md
    # §9a) surfacing as 502s on /system/os-map, /api/search/*,
    # /api/ml/* and /simulator in every quickstart/container deploy
    # (aab30bbf). The binaries were always in the image; only this
    # list forgot them. A pin tying this roster to boss-ports (the
    # deploy-services.sh treatment) is proposed on the feedback item.
    "boss-views-api"
    "boss-search-api"
    "boss-ml-api"
    "boss-simulator"
    "boss-dispatcher"
    "boss-brewery-sim"
    # Gateway last — depends on every other service being reachable.
    "boss-gateway"
)

PIDS=()

# Generate /etc/boss-*.toml configs at container start. The API
# binaries default --config to /etc/<name>.toml; bare-metal installs
# get these via infra/deploy-services.sh, the docker image via this
# generator. Single-container assumption: every cross-service URL
# is 127.0.0.1:<port>.
if command -v boss-generate-configs >/dev/null 2>&1; then
    boss-generate-configs
else
    echo "WARN: boss-generate-configs not on PATH — /etc/boss-*.toml configs missing; many services will fail" >&2
fi

cleanup() {
    echo "==> launcher SIGTERM — stopping ${#PIDS[@]} children"
    for pid in "${PIDS[@]}"; do
        kill -TERM "$pid" 2>/dev/null || true
    done
    wait
    exit 0
}
trap cleanup TERM INT

# Seed the platform operator-baseline (emp-audit + the bootstrap-admin)
# and then the brewery tenant (Workflows + policy + accounts/vendors/data)
# before the sim starts. Both go through the public API — and boss-init
# can't do them (the API isn't up during init), so they run here, just
# before the sim opens its first Job. operator-baseline FIRST so the
# platform-admin login + emp-audit exist before the brewery seed + sim.
# Shared with bare-metal bootstrap-local.sh.
#
# A FAILED PREPARE DEGRADES THE POD, IT DOES NOT END IT. The publish and
# the sim start live in tenant-launch.sh: on failure the APIs stay up,
# the gateway (later in this list) still starts, the sim stays down and
# prepare retries in a background child that becomes the sim when it
# succeeds. On 2026-09-02 the seed's exit 1 met this file's `set -e`
# and took the jobs API down for 65 minutes over a one-field 400.
# shellcheck source=/dev/null
. "$(dirname "${BASH_SOURCE[0]}")/tenant-launch.sh"

echo "==> boss-launch starting ${#SERVICES[@]} services"
for svc in "${SERVICES[@]}"; do
    # Just before the sim — which posts jobs immediately — make sure the
    # brewery Workflows + policy grants exist (the jobs-api it needs is up
    # by now, having been started earlier in this loop).
    if [[ "$svc" == "boss-brewery-sim" ]]; then
        if ! command -v "$svc" >/dev/null 2>&1; then
            echo "    SKIP: $svc (binary not in image)"
            continue
        fi
        # Publishes the tenant, gates on the dispatcher's readyz, and
        # starts the sim as a background child — or degrades and
        # retries. Never returns non-zero; the launch goes on.
        echo "    starting $svc (after the tenant publish)"
        launch_tenant_and_sim PIDS
        sleep 0.1
        continue
    fi
    if ! command -v "$svc" >/dev/null 2>&1; then
        echo "    SKIP: $svc (binary not in image)"
        continue
    fi
    echo "    starting $svc"
    if [[ "$svc" == "boss-event-relay" ]]; then
        # Shares the jobs config for postgres_url + nats_url (see the
        # SERVICES comment) — the relay's env fallbacks differ from
        # the BOSS_POSTGRES_URL convention the other services use.
        "$svc" --config /etc/boss-jobs-api.toml 2>&1 &
    else
        "$svc" 2>&1 &
    fi
    PIDS+=($!)
    # Tiny stagger so log lines from different services don't
    # interleave during the first second.
    sleep 0.1
done

echo "==> all services up — pid count: ${#PIDS[@]}"
echo "==> SPA: http://localhost:4443"

# Wait for any child to exit. If one dies, exit so docker compose
# restart policy decides what to do.
wait -n
exit_code=$?
echo "==> a child exited with code $exit_code — shutting down"
cleanup
