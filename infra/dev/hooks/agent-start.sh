#!/usr/bin/env bash
# PreToolUse on the Agent tool → `boss dispatch <session> --from-hook`
# (design 511fa7d4 car 2b, backlog da925366). The payload is piped to
# the verb verbatim; the verb reads the prompt, dispatches the packet
# it names (the prompt as the brief, the session on the run), links a
# run it already carries, or counts an untracked run — and never
# refuses (crates/orchestrators/boss-cli/src/dispatch_hook.rs).
#
# Two things come back. The verb's STDOUT is the hook's answer — the
# updatedInput JSON that puts the run section on the agent's prompt
# when a packet was dispatched, empty otherwise — and is printed here
# and nowhere else. The verb's STDERR carries one `agent_run=<id>`
# line, stored under the call's tool_use_id so agent-stop.sh can report
# the run when the agent returns; every other stderr line goes to the
# journal. Exit 0 always; see lib.sh.
. "$(dirname "$(readlink -f "$0")")/lib.sh"
read_payload
[ "$(field .tool_name)" = Agent ] || exit 0
session_dir
session_packet
need_boss
mkdir -p "$dir/runs" 2>/dev/null || bail "cannot create $dir/runs"
err="$dir/dispatch.$$.err"
answer=$(printf '%s' "$payload" | boss dispatch "${packet:--}" --from-hook 2>"$err")
status=$?
run=$(sed -n 's/^agent_run=//p' "$err" | tr -d '[:space:]')
while IFS= read -r line; do
  case "$line" in agent_run=*|'') ;; *) log "$line" ;; esac
done < "$err"
rm -f "$err"
[ "$status" -eq 0 ] || log "boss dispatch --from-hook exited $status — the Agent call proceeds unrecorded"
tool_use=$(field .tool_use_id)
if [ -n "$run" ] && [ -n "$tool_use" ]; then
  case "$tool_use" in */*|.|..) log "tool_use_id $tool_use is not a filename — run $run not remembered" ;;
    *) printf '%s\n' "$run" > "$dir/runs/$tool_use" ;;
  esac
fi
[ -n "$answer" ] && printf '%s\n' "$answer"
exit 0
