#!/usr/bin/env bash
# PostToolUse on the Agent tool → `boss dispatch <run> --report`
# (design 511fa7d4 car 2b, backlog da925366). The run is the one
# agent-start.sh remembered under this call's tool_use_id; a call it
# never recorded (no packet in the prompt, or the door was unreachable
# at start) is not reported — there is no run to report on.
#
# The summary is the Agent tool's own result — `tool_response.content`
# text blocks, joined — copied, not retyped; the tokens are the
# response's `usage` split when it carries one (the only shape the rate
# card prices). `--report` is car 3's half of the run
# (feat/agent-controls-station-per-role-model-and-budgeted-claim); on a
# CLI without it the verb refuses the flag and this hook logs that,
# which is the record saying the report did not land. Exit 0 always;
# see lib.sh.
. "$(dirname "$(readlink -f "$0")")/lib.sh"
read_payload
[ "$(field .tool_name)" = Agent ] || exit 0
session_dir
tool_use=$(field .tool_use_id)
[ -n "$tool_use" ] || bail "the payload names no tool_use_id"
case "$tool_use" in */*|.|..) bail "tool_use_id $tool_use is not a filename" ;; esac
[ -s "$dir/runs/$tool_use" ] || bail "call $tool_use opened no run — nothing to report"
run=$(tr -d '[:space:]' < "$dir/runs/$tool_use")
need_boss
summary=$(printf '%s' "$payload" | jq -r '
  .tool_response
  | if type == "string" then .
    elif type == "object" then ([.content[]? | select(.type == "text") | .text] | join("\n"))
    else "" end' 2>/dev/null || true)
[ -n "$summary" ] || summary="(the Agent tool returned no text)"
tokens=$(printf '%s' "$payload" | jq -r '
  .tool_response.usage
  | select(type == "object")
  | select((.input_tokens | type) == "number" and (.output_tokens | type) == "number")
  | "\(.input_tokens),\(.output_tokens)"' 2>/dev/null || true)
if [ -n "$tokens" ]; then
  out=$(boss dispatch "$run" --report --summary "$summary" --tokens "$tokens" 2>&1)
else
  out=$(boss dispatch "$run" --report --summary "$summary" 2>&1)
fi
status=$?
[ "$status" -eq 0 ] || bail "boss dispatch --report on run $run refused: ${out##*$'\n'}"
rm -f "$dir/runs/$tool_use"
log "run $run reported (${#summary} bytes of summary${tokens:+, tokens $tokens})"
exit 0
