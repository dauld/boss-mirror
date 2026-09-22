# infra/dev/hooks/lib.sh — what the five Claude Code hooks share.
# Sourced, never run. bash: the hooks are `#!/usr/bin/env bash`.
#
# THE SHOP FLOOR (design 511fa7d4, decided 2026-09-18; car 2b of
# c87fb59b, backlog da925366). An operator's session and the runs it
# dispatches are packets; these hooks are what write them, from the
# events Claude Code fires (.claude/settings.json, project scope):
#
#   SessionStart      session-start.sh   files the work-session packet
#   UserPromptSubmit  prompt-submit.sh   heartbeat: last_active_at,
#                                        prompt_count — NEVER the prompt
#   PreToolUse Agent  agent-start.sh     boss dispatch <session> --from-hook
#   PostToolUse Agent agent-stop.sh      boss dispatch <run> --report
#   SessionEnd        session-end.sh     completes `active` (ended = clean)
#
# THE ONE RULE: every hook exits 0, and prints nothing on stdout that
# is not the door's own answer. Exit 2 blocks the operator's action;
# on SessionStart and UserPromptSubmit anything on stdout is added to
# the model's context. So an unreachable system of record, a missing
# `boss`, a payload that is not JSON — each is one line in the journal
# and exit 0. Visibility is best-effort; the executor never waits on it
# (CLAUDE.md §Diagnosis: an arm that needs the patient is not an arm).
# A session the hooks could not close is closed by the clock rule
# `work-session-ends-when-silent` six hours after its last heartbeat.
#
# WHO THE HOOKS SIGN AS: nothing here signs anything. `boss` and
# `boss-api` read BOSS_ACTOR, else the actor file, the way every verb
# does (identity.rs); the hooks only read the same to name the actor on
# the packet. Unnamed, the door refuses the write and the hook logs
# that refusal — never a fallback identity.
#
# STATE: one directory per Claude session id under BOSS_HOOK_STATE
# (default $XDG_STATE_HOME/boss/sessions, i.e. ~/.local/state/boss/
# sessions): `packet` holds the work-session id, `prompts` the count,
# `runs/<tool_use_id>` the run each Agent call opened. Cleared at
# SessionEnd. Pinned by crates/core/boss-testing/tests/dev_hooks_sh.rs
# with a stub `boss` on PATH (the shim/launcher tests' idiom).
set -u

# shellcheck source=infra/lib/jq.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../lib" && pwd)/jq.sh"

hook_name="${hook_name:-$(basename "$0" .sh)}"
state_root="${BOSS_HOOK_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/boss/sessions}"

# One line to the pod journal (logger, when there is one) and stderr —
# stderr is the debug log, never the model's context.
log() {
  logger -t "boss-hook" -- "$hook_name: $*" 2>/dev/null || true
  printf 'boss-hook %s: %s\n' "$hook_name" "$*" >&2
}

# Best-effort exit: log the reason, exit 0. Never exit 2.
bail() {
  log "$*"
  exit 0
}

# The payload, read once. A payload that is not JSON is a bail.
read_payload() {
  payload=$(cat 2>/dev/null || true)
  command -v jq >/dev/null 2>&1 || bail "jq is not on PATH — nothing recorded"
  # An EMPTY payload is what a closed stdin leaves, and `jq -e` reads
  # it as JSON (d96e38ab) — the hook would then record nothing and say
  # nothing, which is the failure bail() exists to make audible.
  jq_doc_text "$payload" \
    || bail "the hook payload is empty — nothing recorded"
  printf '%s' "$payload" | jq -e . >/dev/null 2>&1 || bail "the hook payload is not JSON — nothing recorded"
}

# A string field off the payload, empty when absent or null.
field() {
  printf '%s' "$payload" | jq -r "$1 // empty" 2>/dev/null || true
}

# Sets `dir`: the session's state directory, from the payload's
# session_id. A setter, not a printer, so its bail is the hook's exit
# and not a subshell's.
session_dir() {
  local sid
  sid=$(field .session_id)
  [ -n "$sid" ] || bail "the payload names no session_id"
  # A session id is a filename here: refuse anything that is not one.
  case "$sid" in */*|.|..) bail "session_id $sid is not a filename" ;; esac
  dir="$state_root/$sid"
}

# Sets `packet`: the work-session the session-start hook remembered
# under `dir`, or empty.
session_packet() {
  packet=""
  [ -s "$dir/packet" ] && packet=$(tr -d '[:space:]' < "$dir/packet")
  true
}

# Sets `actor`: the id the way identity.rs reads it — BOSS_ACTOR, else
# the actor file. Empty means unnamed.
actor_id() {
  actor=$(printf '%s' "${BOSS_ACTOR:-}" | tr -d '[:space:]')
  [ -n "$actor" ] || actor=$(head -n1 "${BOSS_ACTOR_FILE:-$HOME/.config/boss/actor}" 2>/dev/null | tr -d '[:space:]' || true)
  true
}

now_utc() { date -u +%Y-%m-%dT%H:%M:%SZ; }

need_boss() {
  command -v boss >/dev/null 2>&1 || bail "boss is not on PATH — nothing recorded"
}
