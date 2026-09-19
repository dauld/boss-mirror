#!/usr/bin/env bash
# UserPromptSubmit → the heartbeat (design 511fa7d4 car 2b, backlog
# da925366): `last_active_at` and `prompt_count` merged onto the
# work-session packet through `boss job patch`. NEVER the prompt text —
# `.prompt` is not read here at all: the session's existence and rhythm
# are the record; what was said is the transcript's. The clock rule
# `work-session-ends-when-silent` reads `last_active_at` as movement.
# Nothing on stdout: on this event stdout is added to the model's
# context. Exit 0 always; see lib.sh.
. "$(dirname "$(readlink -f "$0")")/lib.sh"
read_payload
session_dir
session_packet
[ -n "$packet" ] || bail "no work-session for session $(field .session_id) — SessionStart filed none"
need_boss
n=$(tr -d '[:space:]' < "$dir/prompts" 2>/dev/null || true)
case "${n:-empty}" in empty|*[!0-9]*) n=0 ;; esac
n=$((n + 1))
printf '%s\n' "$n" > "$dir/prompts"
jq -n --arg at "$(now_utc)" --argjson n "$n" \
  '{last_active_at: $at, prompt_count: $n}' > "$dir/heartbeat.json" 2>/dev/null \
  || bail "cannot write $dir/heartbeat.json"
out=$(boss job patch "$packet" "$dir/heartbeat.json" 2>&1) \
  || bail "heartbeat $n on $packet refused: ${out##*$'\n'}"
log "heartbeat $n on work-session $packet"
exit 0
