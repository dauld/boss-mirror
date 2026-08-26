#!/usr/bin/env bash
# boss-maintenance-wrap — ensure the open maintenance Job a timer run
# will complete (internal-forge.md Q6; the maintenance family).
#
#   ./infra/boss-maintenance-wrap.sh <kind> <chore-label>
#   e.g. ./infra/boss-maintenance-wrap.sh maintenance-backup "Nightly backup"
#
# Called from the timer service's ExecStartPre. If an open Job of the
# kind exists (yesterday's run FAILED and nobody closed it), reuse it:
# today's successful run completing it is the recovery, and the pile
# staying at one open Job per chore keeps boss-step's single-open
# contract intact. Otherwise spawn today's Job.
#
# The visibility contract, stated once: the timer is the EXECUTOR,
# the Job is the VISIBILITY. Success (ExecStartPost → boss-step.sh)
# completes `run` and the outcome marker closes the Job; failure
# completes nothing and the Job stays OPEN — on the fleet, the
# canvas, and the stage numbers — until a successful run or a human
# closes it.
#
# Deliberately NOT the dispatcher's schedule runner: that fires on
# SIM-day boundaries, and at warp a daily rule fires every couple of
# wall-minutes. Maintenance is wall-clock work.

set -euo pipefail

KIND="${1:?usage: boss-maintenance-wrap.sh <kind> <chore-label>}"
LABEL="${2:?chore label required}"
# WHERE THE PACKET GOES IS NOT A DEFAULT, IT IS A DECISION.
#
# This read `${BOSS_JOBS_URL:-http://127.0.0.1:7900}` until 2026-08-17.
# On a box whose local instance is not the system of record that
# fallback is a silent redirect, and it ran for weeks: the backup,
# audit-integrity and ledger-replay timers each left 7 packets on
# boss-gcp's demo instance and ZERO on the cluster SoR, while firing
# exactly on schedule. Every check in `timers-leave-a-packet` passed
# the whole time, because none of them can see WHERE a packet lands.
#
# The estate model already knows: `service_instances.authoritative` is
# true for boss-cluster and false for boss-gcp-local. It was inert
# metadata — nothing consulted it, and a plausible default beat it.
#
# So: no default. A maintenance tool with no system of record
# configured refuses, loudly, and systemd records a failed unit — which
# is a state somebody notices, unlike a packet filed in the wrong
# database. Set BOSS_JOBS_URL explicitly; deploy-services.sh writes it
# into a drop-in for every timer it installs.
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$(basename "$0"): BOSS_JOBS_URL is not set, and there is no safe default." >&2
    echo "    Defaulting to 127.0.0.1 is how nightly maintenance packets spent weeks" >&2
    echo "    landing on a non-authoritative instance (2026-08-17). Name the system of" >&2
    echo "    record explicitly:" >&2
    echo "        BOSS_JOBS_URL=http://<jobs-api-host>:<port> $(basename "$0") ..." >&2
    echo "    Installed timers get it from deploy-services.sh's jobs-url.conf drop-in." >&2
    exit 78   # EX_CONFIG — a configuration fault, not a run-time one.
fi
BASE="${BOSS_JOBS_URL}"
BOSS_USER='{"id":"automation:maintenance-timer","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'

# THE READ IS ALSO THE READINESS GATE, AND THAT IS WHY IT RETRIES.
#
# The jobs API is a single replica, so every deploy opens a window in
# which it does not exist. A chore firing into that window used to die
# here in two milliseconds — `curl: (7) ... Couldn't connect to server`
# — and take the whole run with it under `set -e`. boss-search-reindex
# did exactly that twice (2026-08-25 and 2026-08-26), and train #118's
# rollout reproduced it: about a minute with no ready pod while the new
# one waited on a Multi-Attach volume release.
#
# What makes that failure worse than an ordinary failed chore is the
# loop in it. This script's entire purpose is to leave a Job so the
# work is VISIBLE and goes overdue if it does not happen — and when the
# jobs API is the thing that is down, the record of the outage is the
# one write that cannot land. The outage erases its own evidence.
#
# So the read waits the window out. Nine retries at two seconds is
# comfortably longer than a rollout and far shorter than the gap to the
# next nightly run. `--retry-connrefused` is the flag that matters:
# plain `--retry` covers timeouts and 5xx but treats a REFUSED
# connection as a hard failure, which is the exact error seen.
#
# THE WRITE BELOW IS DELIBERATELY NOT RETRIED. `--retry` also retries
# 5xx, and a 5xx arriving after the server has already created the Job
# would leave two open packets for one chore — breaking the single-open
# contract this script exists to protect. It does not need retrying: a
# successful read here proves the API is up moments before, so the only
# uncovered case is the API dying between the two calls. That stays a
# loud failure, which is the right outcome for something this narrow.
#
# `.data` missing from the reply means the jobs API changed shape —
# error out (aborting the timer run) rather than reading it as zero
# open Jobs and spawning a duplicate.
open_count=$(curl -fsS --retry 9 --retry-delay 2 --retry-connrefused \
    -H "x-boss-user: $BOSS_USER" \
    "$BASE/api/jobs?kind=$KIND&status=open&limit=2" \
    | jq '.data | if . == null then error("jobs reply has no .data") else length end')

if [ "$open_count" != "0" ]; then
    echo "boss-maintenance-wrap: open $KIND Job exists — this run will complete it (recovery)"
    exit 0
fi

curl -fsS -X POST "$BASE/api/jobs" \
    -H "x-boss-user: $BOSS_USER" -H "content-type: application/json" \
    ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
    -d "$(jq -n --arg kind "$KIND" --arg title "$LABEL — $(date +%F)" '{
        kind: $kind,
        subject: {subject_kind: "custom", id: ("infra/" + $kind)},
        title: $title,
        owner_id: "emp-bootstrap-admin",
        priority: "standard",
        status: "open",
        metadata: {chore: $kind},
        tags: ["maintenance"]
    }')" >/dev/null
echo "boss-maintenance-wrap: spawned today's $KIND Job"
