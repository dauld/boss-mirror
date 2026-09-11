#!/usr/bin/env bash
#
# request-reclaim-disk — make the forge's disk reclaim follow the BUILD
# that fills the disk, instead of waiting for the next timer tick.
#
# WHY (backlog 0357e0eb, measured 2026-09-11). Six consecutive host
# observations fifteen minutes apart: 89, 100, 99, **60**, 92, 98 GB free
# of 227. The locomotive refuses to START a CI run below 70GB
# (locomotive.sh, BOSS_CI_MIN_FREE_GB), so the 04:03 trough sat ten
# gigabytes under the door a train boards through. A refusal there
# happens BEFORE any check runs — it says nothing about the branch and
# still strikes every car aboard, and two strikes hold a car out of the
# queue until a human looks. Four clean cars lost five departures that
# way on 2026-08-22 and a whole day went to holding on 2026-09-05.
#
# THE CAUSE IS A CADENCE MISMATCH, NOT A SHORTAGE, and both floors are
# individually correct:
#   * the FILL is EVENT-driven — every CI run builds and pulls a
#     per-train `boss-ci:<sha>` image into the system docker daemon;
#   * the RECLAIM is TIMER-driven — disk-floor-sweep.timer runs hourly,
#     and its unit defends 100GB, deliberately HIGHER than the
#     locomotive's 70 so a floor buys headroom above the one being
#     defended.
# A dip whose amplitude exceeds that 30GB gap, inside one timer
# interval, walks straight through it. Measured the same morning with
# `disk-report` (ops-request 28d3599c): 13 per-train images in the system
# daemon, 21.18GB total, 18.58GB reclaimable (87%) — and the OLDEST was
# five hours, so every one of them was still inside the routine pass's
# six-hour window. The cadence, not the windows, is what let the trough
# happen.
#
# SO ONLY THE TRIGGER WAS MISSING. The forge already runs the ops-runner
# on a ~1-minute poll, and `reclaim-disk` is an allowlisted bounded verb
# (infra/ops/verbs.json) whose argv is the SAME disk-floor-sweep.sh the
# hourly timer runs — regenerable docker caches only, in a fixed order,
# stopping at the floor, never volumes or non-docker paths. This script
# is the trigger, and .forgejo/workflows/ci.yml fires it from a job that
# needs the heavy jobs and runs under always(). Three properties come
# free from putting it there rather than in a new systemd unit:
#   * it needs no install step — the workflow IS the definition the
#     runner reads, so this cannot join the "landed but never installed"
#     pile this host has already produced;
#   * it fires on RED trains too, which built and pulled the same image;
#   * it fires after a LOCOMOTIVE REFUSAL, which is the self-healing
#     case — the door refused for want of disk, and this is what frees
#     it before the next boarding.
#
# THE SAME POSTURE AS THE `converge` VERB'S FAST PATH: an ACCELERATOR
# only. disk-floor-sweep.timer owes nothing to NATS or the system of
# record and remains the independent floor, so a request this script
# cannot file costs LATENCY, never a missed reclaim. That is why every
# failure here is one loud line and exit 0: this runs inside a CI job,
# and a non-zero exit would strike the cars aboard — causing the exact
# harm the whole packet is about (CLAUDE.md §Diagnosis, "an
# infrastructure refusal is not a consist failure").
#
# NO DEDUP GUARD, DELIBERATELY. The obvious one — do not file while an
# open reclaim packet exists — is the shape that silently retired a
# cadence for nineteen days: one packet left open at a non-terminal step
# kills the rule forever. The suppression here is the FLOOR instead: a
# host above it has nothing for the sweep to do (the sweep's own early
# return logs "nothing to do"), so a healthy host files nothing at all
# and cannot wedge. The ops-runner answers and closes each request in
# about ten seconds.
#
# Usage: request-reclaim-disk.sh [path]
#   path  the filesystem to measure, default `.` — the same measurement
#         the locomotive refuses on, so the two read one disk.
#
# Env:
#   BOSS_JOBS_URL          (required, no default) the system of record
#   BOSS_RECLAIM_HOST      (default forge) the estate node id to ask
#   BOSS_MACHINE_TOKEN     (optional) forwarded as x-boss-machine-token
#   BOSS_RECLAIM_DF_CMD    (default df)   the gate.sh BOSS_GATE_DF_CMD idiom
#   BOSS_RECLAIM_CURL_CMD  (default curl) so the request is testable
#
# NO `set -e`. Every path here must reach the exit 0 at the bottom.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="${1:-.}"
HOST="${BOSS_RECLAIM_HOST:-forge}"
DF_CMD="${BOSS_RECLAIM_DF_CMD:-df}"
CURL_CMD="${BOSS_RECLAIM_CURL_CMD:-curl}"

say()  { printf 'request-reclaim-disk: %s\n' "$*"; }
warn() { printf 'request-reclaim-disk: %s\n' "$*" >&2; }
floor_still_covered() {
    warn "the hourly disk-floor-sweep.timer on the forge host is unaffected and remains the independent floor — this run loses the accelerator, not the reclaim."
}

# ---------------------------------------------------------------------
# (1) THE FLOOR, READ FROM THE UNIT THAT DEFENDS IT.
# ---------------------------------------------------------------------
# ONE number, not a copy beside it. The sweep's floor and the
# locomotive's were the §9a pair that already drifted: a 25GB sweep floor
# defended nothing in the 65-70GB band where CI actually refuses, and
# trains died there for a day. The reclaim-disk verb's own default is
# likewise 25, so the floor has to be passed EXPLICITLY — and the only
# honest source for it is the unit file in this same checkout.
SWEEP_UNIT="$HERE/disk-floor-sweep.service"
FLOOR_GB="$(sed -n 's/^[[:space:]]*Environment=BOSS_DISK_FLOOR_GB=\([0-9]\{1,3\}\)[[:space:]]*$/\1/p' \
    "$SWEEP_UNIT" 2>/dev/null | tail -1)"
if [ -z "${FLOOR_GB:-}" ]; then
    warn "cannot read Environment=BOSS_DISK_FLOOR_GB from $SWEEP_UNIT, so there is no floor to ask for — and guessing one is how the sweep came to defend 25GB while CI refused at 70."
    floor_still_covered
    exit 0
fi

# ---------------------------------------------------------------------
# (2) IS THERE ANYTHING TO DO? `df -P` for POSIX columns, on the
# workspace's own filesystem rather than `/` — in a container job those
# are frequently not the same one, which is the reason locomotive.sh
# reads it this way too.
# ---------------------------------------------------------------------
avail_kb="$($DF_CMD -Pk "$TARGET" 2>/dev/null | awk 'NR==2 {print $4}')"
case "${avail_kb:-}" in
    ''|*[!0-9]*)
        warn "could not read free space for $TARGET (\`$DF_CMD -Pk\` said '${avail_kb:-}'), so there is nothing to decide on."
        floor_still_covered
        exit 0
        ;;
esac
avail_gb=$((avail_kb / 1024 / 1024))

if [ "$avail_gb" -ge "$FLOOR_GB" ]; then
    say "${avail_gb}GB free >= the ${FLOOR_GB}GB floor $(basename "$SWEEP_UNIT") defends — not filing a reclaim, the sweep would log 'nothing to do'"
    exit 0
fi
say "${avail_gb}GB free < the ${FLOOR_GB}GB floor $(basename "$SWEEP_UNIT") defends — asking $HOST to reclaim now rather than at the next hourly tick"

# ---------------------------------------------------------------------
# (3) WHERE THE PACKET GOES IS NOT A DEFAULT, IT IS A DECISION.
# Same refusal as boss-step.sh and boss-maintenance-wrap.sh: defaulting
# to 127.0.0.1 is how weeks of maintenance packets landed on a
# non-authoritative instance (2026-08-17). The difference is the exit
# code — a systemd unit refuses loudly with EX_CONFIG because a failed
# unit is a state somebody notices; here a non-zero exit strikes the cars
# aboard, so the refusal is loud on stderr and the exit is 0.
# ---------------------------------------------------------------------
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    warn "BOSS_JOBS_URL is not set, and there is no safe default — a packet filed against the wrong instance is worse than no packet. Name the system of record on the job: BOSS_JOBS_URL=http://<jobs-api-host>:<port>."
    floor_still_covered
    exit 0
fi

# An automated request should read as automation in the audit trail, the
# same way the ops-runner's own answer does.
ACTOR="${BOSS_RECLAIM_ACTOR:-automation:ci-disk-reclaim}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

# The packet, built with jq so nothing is spliced into program text (the
# boss-step.sh / feedback-queue.sh rule). `args` is the verb's single
# positional param, floor_gb.
body="$(jq -nc \
    --arg host "$HOST" \
    --arg floor "$FLOOR_GB" \
    --arg free "$avail_gb" \
    --arg sha "${GITHUB_SHA:-unknown}" \
    --arg run "${GITHUB_RUN_ID:-unknown}" \
    '{
        kind: "ops-request",
        subject: { subject_kind: "custom", id: $host },
        title: ("reclaim-disk on " + $host + " — a CI run finished with " + $free + "GB free"),
        owner_id: "emp-bootstrap-admin",
        status: "open",
        priority: "standard",
        tags: ["maintenance"],
        metadata: {
            host: $host,
            verb: "reclaim-disk",
            args: [$floor],
            requested_by: "ci-run-completed",
            free_gb_at_request: $free,
            ci_sha: $sha,
            ci_run_id: $run
        }
    }')"
if [ -z "$body" ]; then
    warn "could not build the request body (jq failed)."
    floor_still_covered
    exit 0
fi

# --max-time, because an accelerator must never be the slowest thing in
# a CI run; no retry loop, because the hourly timer is the floor and a
# second attempt buys nothing a tick does not.
rc=0
reply="$("$CURL_CMD" -fsS --max-time 20 -X POST "$BOSS_JOBS_URL/api/jobs" \
    -H "x-boss-user: $BOSS_USER" \
    -H 'content-type: application/json' \
    ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
    -d "$body" 2>&1)" || rc=$?
if [ "$rc" -ne 0 ]; then
    warn "filing the reclaim request failed (curl exit $rc): ${reply:-no reply}"
    floor_still_covered
    exit 0
fi

# Say WHICH packet, so the answer can be read without re-deriving it —
# the ops-runner completes `execute` with the sweep's whole output.
id="$(printf '%s' "$reply" | jq -r '.id // .data.id // empty' 2>/dev/null)"
say "filed ops-request ${id:-(id not in the reply: $reply)} — verb reclaim-disk, floor ${FLOOR_GB}GB, host $HOST; the ops-runner answers within about a minute and the packet carries the sweep's output"
exit 0
