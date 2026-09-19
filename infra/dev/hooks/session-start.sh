#!/usr/bin/env bash
# SessionStart → a work-session packet (design 511fa7d4 car 2b,
# backlog da925366). Files it through `boss job file` with the actor
# the session signs as, the host, the cwd, the instant and the source
# Claude Code names (startup / resume / clear / compact), and
# remembers the packet's id under the session's own id so the other
# four hooks find it. A resumed session already has one: nothing is
# filed twice. Exit 0 always; see lib.sh.
. "$(dirname "$(readlink -f "$0")")/lib.sh"
read_payload
session_dir
mkdir -p "$dir" 2>/dev/null || bail "cannot create $dir"
session_packet
if [ -n "$packet" ]; then
  log "session $(field .session_id) resumed ($(field .source)) — work-session $packet kept"
  exit 0
fi
need_boss
actor_id
[ -n "$actor" ] || bail "nothing names the actor (BOSS_ACTOR or the actor file) — the door would refuse the write, so no packet is filed"
host=$(hostname 2>/dev/null || echo unknown)
jq -n \
  --arg actor "$actor" --arg host "$host" --arg cwd "$(field .cwd)" \
  --arg started_at "$(now_utc)" --arg session_id "$(field .session_id)" \
  --arg source "$(field .source)" \
  '{actor: $actor, host: $host, cwd: $cwd, started_at: $started_at,
    session_id: $session_id, source: $source, prompt_count: 0}' \
  > "$dir/metadata.json" 2>/dev/null || bail "cannot write $dir/metadata.json"
out=$(boss job file --kind work-session --title "Session: $actor on $host" \
        --metadata "$dir/metadata.json" 2>&1) \
  || bail "boss job file refused: ${out##*$'\n'}"
# The verb's own confirmation line carries the id — copied, not retyped.
id=$(printf '%s\n' "$out" | sed -n 's/^boss job: filed \([0-9a-f-]\{36\}\).*/\1/p')
[ -n "$id" ] || bail "boss job file answered without a packet id: ${out##*$'\n'}"
printf '%s\n' "$id" > "$dir/packet"
printf '0\n' > "$dir/prompts"
log "work-session $id opened for $actor on $host ($(field .source))"
exit 0
