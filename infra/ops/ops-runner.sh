#!/bin/sh
# ops-runner — answer ops-request packets filed against this host.
#
# The PULL half of host observability (packet 729329c6). David: "I am
# really tired of the copy pasting after sshing." A debug read is a
# packet: somebody files an `ops-request` Job naming a host and an
# allowlisted read-only verb; this runner, on that host under a
# ~1-minute systemd timer, polls the system of record, executes the
# verb, and completes the packet's `execute` step with stdout/stderr
# and the exit code. The operator stops being the transport.
#
# ## Security posture (phase 1)
#
# - READ-ONLY. Every verb in the allowlist is a read; mutating verbs
#   are phase 2, behind per-verb policy, and are NOT in this script's
#   world at all.
# - THE ALLOWLIST IS THE AUTHORITY: infra/ops/verbs.json, in-tree,
#   reviewed, versioned. A packet carries only a verb NAME and args;
#   the command words come from the file. The runner never executes a
#   packet-supplied string.
# - NO SHELL INTERPOLATION OF ARGS, EVER. The runner builds an argv
#   ARRAY (`set -- word word ...`) and execs it directly — no sh -c,
#   no eval, nothing packet-supplied ever becomes program text. Args
#   are validated against strict per-param patterns first (no
#   whitespace, no leading '-'), which is also what makes the
#   newline-split of jq's argv output below exact rather than hopeful.
# - Anything else — unknown verb, wrong arg shape, pattern miss —
#   drives the packet to its `refused` terminal with the reason in
#   `output`. Refusing loudly in the SoR beats guessing.
#
# ## Behaviour
#
# - Polls open ops-request Jobs whose metadata.host equals HOST_ID,
#   exactly. A packet for a host with no runner is nobody's to guess
#   at; it sits open and visibly unanswered.
# - Executes with a wall-clock timeout (OPS_TIMEOUT, default 30s) and
#   an output cap (OPS_OUTPUT_CAP, default 100KB); both truncations
#   are LOUD — a marker line in the recorded output says what was cut.
# - Completes the `execute` step with metadata MERGED, never replaced:
#   `PUT .../steps/{id}` swaps `metadata` wholesale, so sending only
#   new keys silently wipes the rest, including `authority_role`
#   (the boss-step.sh lesson).
# - A per-packet problem (refusal, missing step) never kills the loop;
#   a transport failure to the SoR fails the unit loudly, systemd
#   records it red, and the same loud-local-failure posture as the
#   estate observers applies (3ddd8333: silent-on-curl-failure does
#   not get a second landing).
# - No maintenance-wrap packet pair, deliberately: this fires every
#   minute, and a packet per firing would drown the board. Its
#   product IS packets — the ops-requests it answers — and its
#   failure modes are a red systemd unit plus a filed packet aging
#   visibly unanswered.
#
# ## Env
#
#   HOST_ID        (required) estate node id this runner answers for
#   BOSS_JOBS_URL  (required, no default — see below) the SoR
#   OPS_VERBS_FILE (default: verbs.json beside this script)
#   OPS_TIMEOUT    (default 30) seconds before a verb is killed
#   OPS_OUTPUT_CAP (default 102400) bytes of output kept
#   BOSS_MACHINE_TOKEN (optional) forwarded as x-boss-machine-token
#
# All JSON parsing is jq with payloads on stdin or via --arg/--argjson/
# --rawfile, never spliced into the program text (the boss-step.sh /
# feedback-queue.sh rule).
set -u

: "${HOST_ID:?HOST_ID is required and must match the estate node id}"

# WHERE THE PACKET GOES IS NOT A DEFAULT, IT IS A DECISION — same
# refusal as boss-step.sh. Defaulting to 127.0.0.1 is how nightly
# maintenance packets spent weeks landing on a non-authoritative
# instance (2026-08-17). A runner with no system of record configured
# refuses, loudly, and systemd records a failed unit — which is a
# state somebody notices.
if [ -z "${BOSS_JOBS_URL:-}" ]; then
    echo "$(basename "$0"): BOSS_JOBS_URL is not set, and there is no safe default." >&2
    echo "    Defaulting to 127.0.0.1 is how nightly maintenance packets spent weeks" >&2
    echo "    landing on a non-authoritative instance (2026-08-17). Name the system of" >&2
    echo "    record explicitly:" >&2
    echo "        BOSS_JOBS_URL=http://<jobs-api-host>:<port> $(basename "$0")" >&2
    echo "    The installed unit pins it on the Exec line (see boss-ops-runner.service)." >&2
    exit 78   # EX_CONFIG — a configuration fault, not a run-time one.
fi
BASE="$BOSS_JOBS_URL"

VERBS_FILE="${OPS_VERBS_FILE:-$(dirname "$0")/verbs.json}"
OPS_TIMEOUT="${OPS_TIMEOUT:-30}"
OPS_OUTPUT_CAP="${OPS_OUTPUT_CAP:-102400}"

if [ ! -r "$VERBS_FILE" ]; then
    echo "ops-runner: allowlist $VERBS_FILE is missing or unreadable — refusing to run" >&2
    exit 78
fi

# An automated answer should read as automation in the audit trail.
ACTOR="${BOSS_OPS_ACTOR:-automation:ops-runner}"
BOSS_USER="{\"id\":\"$ACTOR\",\"role\":\"platform-admin\",\"access_tier\":\"operator\",\"territory_account_ids\":[],\"direct_report_ids\":[],\"department\":\"platform\"}"

workdir=$(mktemp -d) || exit 1
trap 'rm -rf "$workdir"' EXIT

if ! jobs_json=$(curl -fsS -H "x-boss-user: $BOSS_USER" \
        "$BASE/api/jobs?kind=ops-request&status=open&limit=100" 2>&1); then
    echo "ops-runner: jobs-api unreachable at $BASE — $jobs_json" >&2
    exit 1
fi

# Envelope ({"data": [...]}) or bare array; keep open rows for THIS
# host only.
mine=$(printf '%s' "$jobs_json" | jq -c --arg h "$HOST_ID" '
    (if type == "object" and has("data") then .data else . end)
    | map(select(.status == "open" and (.metadata.host // "") == $h))')
n=$(printf '%s' "$mine" | jq 'length')

if [ "$n" -eq 0 ]; then
    echo "ops-runner: no open ops-request for $HOST_ID"
    exit 0
fi

answered=0; refused=0; skipped=0; failed=0
i=0
while [ "$i" -lt "$n" ]; do
    job=$(printf '%s' "$mine" | jq -c ".[$i]")
    i=$((i + 1))
    job_id=$(printf '%s' "$job" | jq -r '.id')
    short=$(printf '%s' "$job_id" | cut -c1-8)

    # Slug first, title as fallback — the boss-step.sh idiom.
    step=$(printf '%s' "$job" | jq -c '
        ((.steps // []) | map(select(.spec_slug == "execute")) | .[0])
        // ((.steps // []) | map(select(.title == "execute")) | .[0])
        // empty')
    if [ -z "$step" ]; then
        echo "ops-runner: $short has no execute step — skipping" >&2
        skipped=$((skipped + 1))
        continue
    fi
    step_status=$(printf '%s' "$step" | jq -r '.status // ""')
    case "$step_status" in
        ready|active) ;;
        *)
            # pending (predicate not yet true) waits for the next
            # poll; completed/skipped is a race with the outcome step.
            echo "ops-runner: $short execute is '$step_status' — skipping this cycle" >&2
            skipped=$((skipped + 1))
            continue
            ;;
    esac
    step_id=$(printf '%s' "$step" | jq -r '.id')

    verb=$(printf '%s' "$job" | jq -r '.metadata.verb // ""')
    args=$(printf '%s' "$job" | jq -c '.metadata.args // []')

    # One jq pass over the ALLOWLIST decides: either a refusal reason
    # or a fully resolved argv. The packet's verb and args enter only
    # as --arg/--argjson values — data, never program text.
    decision=$(jq -c --arg verb "$verb" --argjson args "$args" '
        def refuse(msg): {refuse: msg};
        .verbs[$verb] as $spec
        | if $verb == "" then refuse("metadata.verb is missing")
          elif $spec == null then
            refuse("verb \($verb) is not in the allowlist (infra/ops/verbs.json); phase-1 verbs: "
                   + (.verbs | keys | join(", ")))
          elif ($args | type) != "array" or any($args[]; type != "string") then
            refuse("metadata.args must be a JSON array of strings")
          elif ($args | length) > ($spec.params | length) then
            refuse("verb \($verb) takes at most \($spec.params | length) arg(s), got \($args | length)")
          else
            [ $spec.params | to_entries[] | .key as $i | .value as $p
              | $args[$i] as $raw
              | if $raw == null then
                  (if $p | has("default") then {ok: $p.default}
                   else {err: "missing required arg \($p.name)"} end)
                elif ($raw | test($p.pattern)) | not then
                  {err: "arg \($p.name) value \($raw) does not match \($p.pattern)"}
                elif ($p | has("max")) and (($raw | tonumber) > $p.max) then
                  {err: "arg \($p.name) value \($raw) exceeds max \($p.max)"}
                else {ok: $raw} end
            ] as $vals
            | [ $vals[] | select(has("err")) | .err ] as $errs
            | if ($errs | length) > 0 then refuse($errs | join("; "))
              else {argv: [ $spec.argv[]
                            | if test("^\\{[0-9]+\\}$")
                              then . as $ph
                                   | $vals[($ph | ltrimstr("{") | rtrimstr("}") | tonumber) - 1].ok
                              else . end ]}
              end
          end' "$VERBS_FILE")

    outf="$workdir/out"
    reason=$(printf '%s' "$decision" | jq -r '.refuse // empty')
    if [ -n "$reason" ]; then
        disp="refused"; rc_str=""
        printf '%s' "$reason" > "$outf"
    else
        # Build the argv as positional parameters. Newline-split is
        # EXACT here, not hopeful: fixed argv words are reviewed file
        # content and substituted args have passed patterns that admit
        # no whitespace.
        set --
        while IFS= read -r w; do
            set -- "$@" "$w"
        done <<ARGV
$(printf '%s' "$decision" | jq -r '.argv[]')
ARGV
        rawf="$workdir/raw"
        timeout "$OPS_TIMEOUT" "$@" > "$rawf" 2>&1 < /dev/null
        rc=$?
        size=$(wc -c < "$rawf")
        if [ "$size" -gt "$OPS_OUTPUT_CAP" ]; then
            head -c "$OPS_OUTPUT_CAP" "$rawf" > "$outf"
            printf '\n[ops-runner: output truncated — kept %s of %s bytes]\n' \
                "$OPS_OUTPUT_CAP" "$size" >> "$outf"
        else
            cat "$rawf" > "$outf"
        fi
        if [ "$rc" -eq 124 ]; then
            printf '\n[ops-runner: command killed at %ss timeout]\n' "$OPS_TIMEOUT" >> "$outf"
        fi
        disp="answered"; rc_str="$rc"
    fi

    # Merge, never replace (see header). The output rides --rawfile so
    # arbitrary command output stays data.
    merged=$(printf '%s' "$step" | jq -c --rawfile out "$outf" \
        --arg d "$disp" --arg rc "$rc_str" --arg h "$HOST_ID" '
        (.metadata // {})
        + {disposition: $d, output: $out, runner_host: $h}
        + (if $rc == "" then {} else {exit_code: $rc} end)')
    payloadf="$workdir/payload"
    printf '%s' "$merged" | jq -c '{status: "completed", metadata: .}' > "$payloadf"

    if ! put_err=$(curl -fsS -X PUT -H "content-type: application/json" \
            -H "x-boss-user: $BOSS_USER" \
            ${BOSS_MACHINE_TOKEN:+-H "x-boss-machine-token: $BOSS_MACHINE_TOKEN"} \
            --data-binary @"$payloadf" \
            "$BASE/api/jobs/$job_id/steps/$step_id" 2>&1 >/dev/null); then
        echo "ops-runner: PUT failed on $short — $put_err" >&2
        failed=$((failed + 1))
        continue
    fi

    if [ "$disp" = "answered" ]; then
        echo "ops-runner: answered $verb on $short (exit $rc_str, ${size}B)"
        answered=$((answered + 1))
    else
        echo "ops-runner: refused $short — $reason"
        refused=$((refused + 1))
    fi
done

echo "ops-runner: $HOST_ID answered=$answered refused=$refused skipped=$skipped failed=$failed"
[ "$failed" -eq 0 ] || exit 1
