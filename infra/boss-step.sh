#!/usr/bin/env bash
#
# boss-step — close a step on the open Job of a given Workflow.
#
#   ./infra/boss-step.sh <workflow> <step-slug-or-title> [key=value ...]
#   ./infra/boss-step.sh regenerate-deployment build source_ref=abc1234
#
# The point: work a machine did should be recorded by the machine that
# did it. A regen's `build` step closing because the build script
# finished is a fact; the same step closing because someone typed
# afterwards is a claim. The reason to model an operation as a Job is
# that its state stays true without anyone maintaining it.
#
# ## Behaviour
#
# Finds the single OPEN Job of `<workflow>`, finds the step by its
# spec slug (`steps.spec_slug`, the stable machine-facing identifier —
# falling back to the rendered title for steps materialized before the
# column existed), merges the key=value pairs into its metadata,
# completes it.
#
# - No open Job → NO-OP, exit 0. These scripts run outside regens all
#   the time; a build is not required to belong to one, and failing a
#   build because no Job is open would be the tail wagging the dog.
# - More than one open Job → exit 1. Guessing which to close is worse
#   than stopping.
# - Step already terminal → NO-OP, exit 0. Re-running a deploy inside
#   one regen must not fail: idempotence is the contract of a step's
#   status.
#
# Metadata is MERGED, never replaced. `PUT /api/jobs/{id}/steps/{id}`
# has PATCH semantics for top-level fields but swaps `metadata`
# wholesale, so sending only new keys silently wipes the rest —
# including `authority_role`, which is what keeps a gated step gated.
#
# Talks to jobs-api directly with an actor header, like
# feedback-queue.sh: the gateway is the browser edge and strips
# inbound `x-boss-*`, so terminal tooling cannot present itself there.
#
# All JSON parsing is jq with the payload on stdin and caller values
# passed via --arg, never spliced into the program text.
# feedback-queue.sh once embedded its parser in a double-quoted
# string and a literal double quote silently truncated the program.
set -euo pipefail

if [ $# -lt 2 ]; then
    sed -n '2,12p' "$0" >&2
    exit 2
fi

WORKFLOW="$1"; shift
STEP_TITLE="$1"; shift

# A VERDICT FOR THE RUN THAT JUST ENDED. systemd hands every
# ExecStopPost= process $SERVICE_RESULT (success / exit-code / timeout /
# signal / ...) and $EXIT_STATUS, and ExecStopPost runs whether ExecStart
# succeeded or not — unlike ExecStartPost, which systemd SKIPS when
# ExecStart fails. Until 2026-09-05 every timer completed its packet
# from ExecStartPost, so a failed run recorded nothing: the disk-floor
# sweep's 16:10 packet sat open through two FLOOR UNMET runs looking
# exactly like a run in progress, and forge-converge's 17:39 failure
# (exit 1) was closed "ok" by the next run's recovery. A packet that
# cannot say "failed" is not the record of the run.
#
# So when the caller passes no result of its own, the result IS the
# service result: `ok` for success (the word the outcome predicates
# route on), otherwise systemd's word for how it died, with the exit
# status beside it. An explicit result= pair still wins.
if [ -n "${SERVICE_RESULT:-}" ] && ! printf '%s\n' "$@" | grep -q '^result='; then
    if [ "$SERVICE_RESULT" = "success" ]; then
        set -- "$@" "result=ok"
    else
        set -- "$@" "result=$SERVICE_RESULT" "exit_status=${EXIT_STATUS:-unknown}"
    fi
fi
# The lint's self-test reads the pairs this run would record, and stops.
if [ -n "${BOSS_STEP_DRY_RUN:-}" ]; then
    printf '%s\n' "$@"
    exit 0
fi

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

# An automated close should read as automation in the audit trail, not
# as whichever human happened to be logged in.
ACTOR="${BOSS_STEP_ACTOR:-automation:boss-step}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

# Transport-retry curl (25e518c0) — same resolution as
# boss-maintenance-wrap.sh: next-to-self (repo checkout and image
# both keep the pair together), PATH as the fallback.
API_CURL="$(dirname "$0")/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh

if ! jobs_json=$("$API_CURL" -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=$WORKFLOW&status=open&limit=50" 2>/dev/null); then
    echo "boss-step: jobs-api unreachable at $BASE — '$STEP_TITLE' not recorded" >&2
    exit 1
fi

# The reply may be enveloped ({"data": [...]}) or a bare array; keep
# only the open rows either way.
open_jobs=$(printf '%s' "$jobs_json" | jq '
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open"))')
open_count=$(printf '%s' "$open_jobs" | jq 'length')

if [ "$open_count" -eq 0 ]; then
    echo "boss-step: no open $WORKFLOW Job — nothing to record" >&2
    exit 0
fi
if [ "$open_count" -gt 1 ]; then
    ids=$(printf '%s' "$open_jobs" | jq -r '[.[].id[:8]] | join(", ")')
    echo "boss-step: $open_count open $WORKFLOW Jobs ($ids) — refusing to guess" >&2
    exit 1
fi

job=$(printf '%s' "$open_jobs" | jq '.[0]')
job_id=$(printf '%s' "$job" | jq -r '.id')

# Slug first (the stable identifier), title as fallback for steps
# materialized before spec_slug existed. Never both vocabularies in
# one pass — an exact slug match must not lose to a title elsewhere.
step=$(printf '%s' "$job" | jq --arg t "$STEP_TITLE" '
    ((.steps // []) | map(select(.spec_slug == $t)) | .[0])
    // ((.steps // []) | map(select(.title == $t)) | .[0])')

if [ "$step" = "null" ]; then
    have=$(printf '%s' "$job" | jq -r '
        [(.steps // [])[] | "\(.spec_slug // "?") (\(.title // "?"))"]
        | join(", ")')
    echo "boss-step: no step '$STEP_TITLE' on ${job_id:0:8} (has slug (title): $have)" >&2
    exit 1
fi

step_status=$(printf '%s' "$step" | jq -r '.status // ""')
if [ "$step_status" = "completed" ] || [ "$step_status" = "skipped" ]; then
    echo "boss-step: $STEP_TITLE already $step_status — no-op" >&2
    exit 0
fi

merged=$(printf '%s' "$step" | jq '.metadata // {}')
for pair in "$@"; do
    case "$pair" in
        *=*) ;;
        *)
            echo "boss-step: '$pair' is not key=value" >&2
            exit 2
            ;;
    esac
    merged=$(printf '%s' "$merged" \
        | jq --arg k "${pair%%=*}" --arg v "${pair#*=}" '. + {($k): $v}')
done

payload=$(printf '%s' "$merged" | jq -c '{status: "completed", metadata: .}')
step_id=$(printf '%s' "$step" | jq -r '.id')
url="$BASE/api/jobs/$job_id/steps/$step_id"
if ! put_err=$("$API_CURL" -fsS -X PUT -H "content-type: application/json" \
        -H "x-boss-user: $BOSS_USER" \
        ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
        -d "$payload" "$url" 2>&1 >/dev/null); then
    echo "boss-step: PUT failed — $put_err" >&2
    exit 1
fi
echo "boss-step: closed $WORKFLOW/$STEP_TITLE on ${job_id:0:8}"
