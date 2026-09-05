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

# Transport-retry curl (25e518c0): a deploy roll takes the API away
# for ~45s and that must not kill the chore. Resolved next-to-self so
# the same line works from the repo checkout (boss-gcp timers) and
# /usr/local/bin (the image).
API_CURL="$(dirname "$0")/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh

# THE EXECUTOR NEVER WAITS ON ITS VISIBILITY. If the jobs API cannot
# be reached at all past the transport deadline, this run has no
# packet — and it still RUNS. On 2026-09-05 the cluster's system of
# record was dark for four hours because the one loop that could have
# restored it, cluster-deploy-runner, has this script as its
# ExecStartPre: the wrap failed on the unreachable API, systemd never
# started the converge, and the fix on main sat unbuilt until a human
# ran the script by hand. A chore that cannot record itself must say
# so and go; a chore that refuses to run because it cannot be seen is
# the outage. An API that ANSWERS with an error is a different thing —
# a contract or configuration fault — and still aborts the run.
transport_unreachable() {  # $1 = curl exit code
    case "$1" in 6|7|28|35|52|55|56) return 0 ;; *) return 1 ;; esac
}
reply=""; rc=0
reply=$("$API_CURL" -fsS -H "x-boss-user: $BOSS_USER" \
    "$BASE/api/jobs?kind=$KIND&status=open&limit=2") || rc=$?
if [ "$rc" -ne 0 ]; then
    if transport_unreachable "$rc"; then
        echo "boss-maintenance-wrap: the jobs API at $BASE is UNREACHABLE (curl exit $rc, past the transport deadline) — running $KIND WITHOUT its packet; the work goes on, only this run's visibility is lost" >&2
        exit 0
    fi
    echo "boss-maintenance-wrap: the jobs API answered an error for $KIND (curl exit $rc) — a contract or configuration fault, aborting the run" >&2
    exit "$rc"
fi
# `.data` missing from the reply means the jobs API changed shape —
# error out (aborting the timer run) rather than reading it as zero
# open Jobs and spawning a duplicate.
open_count=$(printf '%s' "$reply" \
    | jq '.data | if . == null then error("jobs reply has no .data") else length end')

if [ "$open_count" != "0" ]; then
    echo "boss-maintenance-wrap: open $KIND Job exists — this run will complete it (recovery)"
    exit 0
fi

rc=0
"$API_CURL" -fsS -X POST "$BASE/api/jobs" \
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
    }')" >/dev/null || rc=$?
if [ "$rc" -ne 0 ]; then
    if transport_unreachable "$rc"; then
        echo "boss-maintenance-wrap: the jobs API at $BASE went UNREACHABLE while spawning $KIND (curl exit $rc) — running WITHOUT its packet" >&2
        exit 0
    fi
    echo "boss-maintenance-wrap: spawning today's $KIND Job failed (curl exit $rc) — aborting the run" >&2
    exit "$rc"
fi
echo "boss-maintenance-wrap: spawned today's $KIND Job"
