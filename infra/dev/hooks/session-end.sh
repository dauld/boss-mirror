#!/usr/bin/env bash
# SessionEnd → the work-session ends cleanly (design 511fa7d4 car 2b,
# backlog da925366): its `active` step is completed with
# `ended = clean` and the packet's metadata takes `ended_at` and Claude
# Code's own `end_reason`. Through `boss-api` rather than the boss
# shim, because SessionEnd hooks share a 1.5 s budget and the shim
# weighs its candidate binary (git, --version) before every verb;
# two curls fit, a shim round trip may not. A session this hook does
# not reach — the budget, a dark system of record, a lid closed with
# no hook fired at all — is ended by the clock rule
# `work-session-ends-when-silent` six hours after its last heartbeat,
# which is why the rule exists. Exit 0 always; see lib.sh.
. "$(dirname "$(readlink -f "$0")")/lib.sh"
read_payload
session_dir
session_packet
[ -n "$packet" ] || bail "no work-session for session $(field .session_id) — nothing to end"
command -v boss-api >/dev/null 2>&1 || bail "boss-api is not on PATH — work-session $packet is left to the clock"
job=$(boss-api GET "/api/jobs/$packet" 2>/dev/null) \
  || bail "cannot read work-session $packet — left to the clock"
# The open `active` step, and its metadata: PATCH-on-PUT replaces
# `metadata` wholesale, so the step's own keys ride along.
step_id=$(printf '%s' "$job" | jq -r '[.steps[]? | select(.spec_slug == "active" and (.status == "ready" or .status == "active"))][0].id // empty' 2>/dev/null || true)
[ -n "$step_id" ] || bail "work-session $packet has no open active step — already ended"
printf '%s' "$job" | jq '
  ([.steps[] | select(.id == $id)][0].metadata // {}) as $md
  | {status: "completed", metadata: ($md + {ended: "clean"})}' --arg id "$step_id" \
  > "$dir/end.json" 2>/dev/null || bail "cannot write $dir/end.json"
out=$(boss-api PUT "/api/jobs/$packet/steps/$step_id" "$dir/end.json" 2>&1) \
  || bail "ending work-session $packet refused: ${out##*$'\n'}"
jq -n --arg at "$(now_utc)" --arg reason "$(field .reason)" \
  '{ended_at: $at, end_reason: $reason}' > "$dir/ended.json" 2>/dev/null
boss-api PATCH "/api/jobs/$packet/metadata" "$dir/ended.json" >/dev/null 2>&1 \
  || log "ended_at on work-session $packet not written"
log "work-session $packet ended clean ($(field .reason))"
rm -rf "$dir"
exit 0
