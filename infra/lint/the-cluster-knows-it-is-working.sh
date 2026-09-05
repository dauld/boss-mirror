#!/usr/bin/env bash
# the-cluster-knows-it-is-working.sh — the forge watchdog's decision
# table, exercised as a pure function, plus the unit's one rule: it
# carries no maintenance wrap.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="$here/../forge/cluster-watchdog-lib.sh"; unit="$here/../forge/cluster-watchdog.service"
[[ -f "$lib" && -f "$unit" ]] || { echo "the-cluster-knows-it-is-working: missing $lib or $unit" >&2; exit 1; }
# shellcheck source=/dev/null
. "$lib"
fail() { echo "FAIL: $*" >&2; exit 1; }
expect() { local got; got=$(watchdog_decision "$1" "$2" "$3" "$4" 3); [[ "$got" == "$5" ]] || fail "live=$1 image=$2 stamp=$3 dark=$4 -> $got, expected $5"; }
expect up    2de17dd 2683908 0 ok              # the API answers: nothing to do
expect down  2de17dd 2683908 1 wait            # one dark check is a deploy roll, not an outage
expect down  2de17dd 2683908 2 wait
expect down  2de17dd 2683908 3 roll-to-stamp   # past the threshold, serving a non-converged image: the named lever
expect down  b2814ef 2683908 5 roll-to-stamp   # the placeholder image too
expect down  2683908 2683908 3 hands           # dark on the last converged build itself: nothing safe to roll to
expect down  2de17dd none    3 hands           # no stamp yet: nothing to roll to
# Its packet is visibility, never a precondition: both hooks must be
# prefixed `-` so systemd runs the watchdog whatever the API says.
grep -qE '^ExecStartPre=-/home/david/boss/infra/boss-maintenance-wrap.sh maintenance-cluster-watchdog' "$unit" || fail "the watchdog's packet hook is missing or is not best-effort (needs the - prefix)"
grep -qE '^ExecStartPost=-/home/david/boss/infra/boss-step.sh maintenance-cluster-watchdog' "$unit" || fail "the watchdog's completion hook is missing or is not best-effort (needs the - prefix)"
grep -qE '^ExecStart(Pre|Post)?=/home/david/boss/infra/(boss-maintenance-wrap|boss-step)' "$unit" && fail "a packet hook on the watchdog is a hard precondition — it would deadlock on the API it watches"
grep -q '^ExecStart=/home/david/boss/infra/forge/cluster-watchdog.sh' "$unit" || fail "the unit does not run the watchdog"
echo "the-cluster-knows-it-is-working: self-test ok — ok/wait/roll-to-stamp/hands decided as designed (7 cases); its packet hooks are best-effort, never a precondition"
exit 0
