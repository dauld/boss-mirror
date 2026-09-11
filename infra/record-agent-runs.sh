#!/usr/bin/env bash
#
# record-agent-runs.sh — file measured agent runs through
# `POST /api/agent-runs`, from a data file of observations.
#
#   ./infra/record-agent-runs.sh infra/agent-runs/2026-09-10.tsv --dry-run
#   BOSS_JOBS_URL=http://10.20.0.34:7900 \
#     ./infra/record-agent-runs.sh infra/agent-runs/2026-09-10.tsv
#
# WHY A SCRIPT AND NOT FIFTEEN CALLS. The same reason every other
# observation in this tree arrives through one: a hand-typed call is a
# transcription of a measurement, and the night it is typed is the only
# night anyone checks it. The data file holds the observations, this
# holds the mechanism, and a second night is a second file.
#
# WHAT IT DOES NOT DO. It does not compute a cost. The server prices a
# run from `agent_rate_card` inside the transaction that writes it,
# because a caller that can assert its own `usd_micros` makes the rate
# card decorative. Runs reporting only a total are unpriced by design —
# the card charges input and output at different rates, so there is no
# arithmetic from one number to a price, and `unpriced_runs` /
# `total_only_runs` say so in the roll-up rather than a blended guess
# standing in for a measurement.
#
# IDEMPOTENT. `run_id` is minted in the data file, and the API collapses
# a repeat: a second run of this script reports `duplicate` per row and
# puts no second event on the log. Re-running after a partial failure is
# the supported recovery.
#
# Post-API by design, like infra/seed-operator-baseline.sh: the write
# goes through the service that owns the table, so it is policy-checked,
# evented and priced by the one code path. Nothing here touches
# Postgres.
#
# Data file: tab-separated, `#` comments and blank lines ignored.
#   run_id  actor_id  branch  total_tokens  tool_calls  duration_ms  finished_at  outcome  label
# See infra/agent-runs/2026-09-10.tsv for where each field comes from
# and which one is not measured.
set -euo pipefail

usage() {
    sed -n '2,12p' "$0" >&2
    exit 2
}

RUNS_FILE="${1:-}"
[ -n "$RUNS_FILE" ] || usage
[ -r "$RUNS_FILE" ] || { echo "record-agent-runs: cannot read $RUNS_FILE" >&2; exit 2; }
DRY_RUN=""
[ "${2:-}" = "--dry-run" ] && DRY_RUN=1

for tool in jq date; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "record-agent-runs: $tool is required" >&2; exit 2; }
done

# WHERE THE RECORD GOES IS NOT A DEFAULT, IT IS A DECISION — the same
# refusal boss-step.sh makes, for the same measured reason: a
# plausible 127.0.0.1 fallback spent weeks filing maintenance packets
# into a non-authoritative instance while every check passed
# (2026-08-17). A run filed into the wrong database is a cost figure
# that exists nowhere anyone will look.
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "record-agent-runs: BOSS_JOBS_URL is not set, and there is no safe default." >&2
    echo "    The system of record is the cluster's jobs API; a second, older stack" >&2
    echo "    answers on 127.0.0.1 with different data. Name it explicitly:" >&2
    echo "        BOSS_JOBS_URL=http://10.20.0.34:7900 $(basename "$0") $RUNS_FILE" >&2
    exit 78   # EX_CONFIG — a configuration fault, not a run-time one.
fi
BASE="${BOSS_JOBS_URL%/}"

# WHO FILES THE RECORD. Not the same actor as the run's own `actor_id`:
# that is the CPU that did the work, this is who reported it. An
# unnamed write is refused rather than attributed to a default, because
# the opposite cost a misattribution the audit log then held as fact
# (backlog 5083d6f5).
FILER="${BOSS_ACTOR:-}"
if [ -z "$FILER" ] && [ -r "$HOME/.config/boss/actor" ]; then
    FILER=$(head -1 "$HOME/.config/boss/actor")
fi
if [ -z "$FILER" ]; then
    echo "record-agent-runs: no actor — who is filing these records?" >&2
    echo "    Set BOSS_ACTOR, or write the id into $HOME/.config/boss/actor." >&2
    exit 78
fi
BOSS_USER="{\"id\":\"$FILER\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

# Transport-retry curl (25e518c0): a routine deploy roll takes the jobs
# API away for ~45s, which is a try-again and not a failure. Resolved
# next-to-self first, PATH as the fallback — the same two lines
# boss-step.sh and boss-maintenance-wrap.sh use.
API_CURL="$(dirname "$0")/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh

api() {
    local method="$1" path="$2" body="${3:-}"
    if [ -n "$body" ]; then
        "$API_CURL" -fsS -X "$method" -H "x-boss-user: $BOSS_USER" \
            -H 'content-type: application/json' --data-binary "$body" "$BASE$path"
    else
        "$API_CURL" -fsS -H "x-boss-user: $BOSS_USER" "$BASE$path"
    fi
}

# One read of the car packets, so a branch can carry the packet it was
# working. A branch with two packets (a re-rail) resolves to NOTHING
# rather than to a guess — `job_id` is nullable precisely so a run that
# cannot name its packet is still a record.
PACKETS='[]'
if [ -z "$DRY_RUN" ]; then
    if raw=$(api GET '/api/jobs?kind=ship-a-change&limit=500'); then
        PACKETS=$(printf '%s' "$raw" | jq 'if type == "object" and has("data") then .data else . end')
    else
        echo "record-agent-runs: could not read ship-a-change packets — every job_id will be null" >&2
    fi
fi

job_id_for() {
    printf '%s' "$PACKETS" | jq -r --arg b "$1" '
        [ .[] | select(.metadata.branch == $b) | .id ]
        | if length == 1 then .[0] else "" end'
}

filed=0; duplicate=0; failed=0; first_finish=""
while IFS=$'\t' read -r run_id actor_id branch total_tokens tool_calls duration_ms finished_at outcome label; do
    case "${run_id:-}" in ''|'#'*) continue ;; esac
    if [ -z "${outcome:-}" ]; then
        echo "record-agent-runs: $run_id — short row (expected 9 tab-separated fields)" >&2
        failed=$((failed+1)); continue
    fi

    # started_at = finished_at - the MEASURED duration, to the
    # millisecond. Both instants are bound here, by the caller, from the
    # observation: the API never substitutes NOW(), because a write-time
    # reading would record when the report arrived rather than when the
    # run happened. The duration itself is stored NOWHERE — the server
    # derives it from the pair, and a stored copy could disagree.
    if ! fin_s=$(date -u -d "$finished_at" +%s 2>/dev/null); then
        echo "record-agent-runs: $run_id — unparseable finished_at '$finished_at'" >&2
        failed=$((failed+1)); continue
    fi
    start_total_ms=$(( fin_s * 1000 - duration_ms ))
    started_at="$(date -u -d "@$(( start_total_ms / 1000 ))" +%Y-%m-%dT%H:%M:%S).$(printf '%03d' $(( start_total_ms % 1000 )))Z"
    [ -z "$first_finish" ] && first_finish="$started_at"

    job_id=""
    [ -n "$branch" ] && job_id=$(job_id_for "$branch")

    # total_tokens ALONE: no input/output keys at all, because no split
    # was measured. The server reads that as a total-only run and leaves
    # usd_micros NULL.
    body=$(jq -n \
        --arg run_id "$run_id" \
        --arg actor_id "$actor_id" \
        --arg started_at "$started_at" \
        --arg finished_at "$finished_at" \
        --arg outcome "$outcome" \
        --arg branch "$branch" \
        --arg job_id "$job_id" \
        --arg run_label "$label" \
        --arg source "$(basename "$RUNS_FILE")" \
        --argjson total_tokens "$total_tokens" \
        --argjson tool_calls "$tool_calls" \
        '{
            run_id: $run_id,
            actor_id: $actor_id,
            started_at: $started_at,
            finished_at: $finished_at,
            outcome: $outcome,
            total_tokens: $total_tokens,
            tool_calls: $tool_calls,
            branch: (if $branch == "" then null else $branch end),
            job_id: (if $job_id == "" then null else $job_id end),
            detail: {
                "label": $run_label,
                "host": "boss-dev pod",
                "source": $source,
                "tokens_reported_as": "a single total — the harness reports subagent_tokens with no input/output split",
                "finished_at_basis": "the instant the gate-run packet for this branch was opened; started_at is that minus the measured duration"
            }
        }')

    if [ -n "$DRY_RUN" ]; then
        printf '%s\n' "$body"
        filed=$((filed+1))
        continue
    fi

    out=$(mktemp)
    if api POST /api/agent-runs "$body" >"$out" 2>&1; then
        # Read the verdict back off the ANSWER, not off the request: the
        # row the server holds is the record, and `usd_micros: null` is
        # the point being proved.
        priced=$(jq -r 'if .run.usd_micros == null then "unpriced" else "$" + (.run.usd_micros / 1000000 | tostring) end' <"$out")
        if [ "$(jq -r '.recorded' <"$out")" = "true" ]; then
            filed=$((filed+1))
            printf 'recorded  %-34s %9s tok  %4s calls  %5ss  %-9s %s\n' \
                "$run_id" "$total_tokens" "$tool_calls" "$(( duration_ms / 1000 ))" \
                "$priced" "${branch:-(no branch)}"
        else
            duplicate=$((duplicate+1))
            printf 'duplicate %-34s already on the log — nothing written\n' "$run_id"
        fi
    else
        failed=$((failed+1))
        # The whole answer, not a tail of it: a refusal names its fix and
        # the fix is the only thing worth keeping from a failed write.
        echo "FAILED    $run_id" >&2
        sed 's/^/          /' "$out" >&2
    fi
    rm -f "$out"
done < "$RUNS_FILE"

echo
echo "recorded $filed, duplicate $duplicate, failed $failed"

if [ -z "$DRY_RUN" ] && [ -n "$first_finish" ]; then
    # Read the cost back, over the window these runs sit in, so the run
    # ends with what the system now holds rather than with what this
    # script believes it sent.
    echo
    echo "GET /api/agent-runs/cost?since=$first_finish"
    api GET "/api/agent-runs/cost?since=$first_finish" | jq '{
        runs: .summary.runs,
        total_tokens: .summary.total_tokens,
        tool_calls: .summary.tool_calls,
        wall_secs: .summary.wall_secs,
        usd_micros: .summary.usd_micros,
        unpriced_runs: .summary.unpriced_runs,
        total_only_runs: .summary.total_only_runs,
        since: .since
    }'
fi

[ "$failed" -eq 0 ]
