#!/usr/bin/env bash
# a-failed-prepare-degrades-the-pod.sh — the launcher's tenant publish
# degrades on failure and retries; it never ends the launch.
#
# A self-test of infra/oss-quickstart/tenant-launch.sh under stubs: the
# publish fails twice and then succeeds; the sim must start exactly
# once, only after the successful publish, and the function must
# return 0 on the first (failing) call with a DEGRADED line on stderr.
# This is the 2026-09-02 boot-brick (packets 88a07cc4, 089a99bc) as a
# check that runs on every gate, so the `set -e` that turned a
# one-field 400 into 65 minutes of dead jobs API cannot come back.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lib="$here/../oss-quickstart/tenant-launch.sh"
[[ -f "$lib" ]] || { echo "a-failed-prepare-degrades-the-pod: missing $lib" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export STUB_DIR="$tmp"
export BOSS_PREPARE_RETRY_SECONDS=0

# shellcheck source=/dev/null
. "$lib"

# Stubs replace the three hooks. State lives in files because the
# retry loop runs in a subshell.
publish_tenant() {
    local n=0
    [[ -f "$STUB_DIR/attempts" ]] && n=$(<"$STUB_DIR/attempts")
    n=$((n + 1)); echo "$n" >"$STUB_DIR/attempts"
    [[ "$n" -ge 3 ]]
}
wait_for_dispatcher() { echo "ready" >>"$STUB_DIR/readyz"; }
start_sim() { echo "started after $(<"$STUB_DIR/attempts") attempts" >>"$STUB_DIR/sim"; exit 0; }

PIDS=()
err="$tmp/stderr"
launch_tenant_and_sim PIDS 2>"$err"; rc=$?
[[ "$rc" -eq 0 ]] || { echo "FAIL: launch_tenant_and_sim returned $rc on a failed publish — that is the launcher's own exit, the boot-brick" >&2; exit 1; }
grep -q '^DEGRADED: tenant prepare failed' "$err" || { echo "FAIL: no DEGRADED line on the first failure:"; cat "$err"; exit 1; } >&2
[[ "${#PIDS[@]}" -eq 1 ]] || { echo "FAIL: expected one retry child in PIDS, got ${#PIDS[@]}" >&2; exit 1; }
wait "${PIDS[0]}"
attempts=$(<"$tmp/attempts")
[[ "$attempts" -eq 3 ]] || { echo "FAIL: expected 3 publish attempts (2 failures, 1 success), got $attempts" >&2; exit 1; }
[[ -f "$tmp/sim" && "$(wc -l <"$tmp/sim")" -eq 1 ]] || { echo "FAIL: the sim did not start exactly once after the successful publish" >&2; exit 1; }
[[ -f "$tmp/readyz" ]] || { echo "FAIL: the sim started without the dispatcher readyz wait" >&2; exit 1; }
grep -q '^DEGRADED: cleared' "$err" || { echo "FAIL: the cleared line is missing:"; cat "$err"; exit 1; } >&2

# The happy path: a publish that succeeds first time starts the sim
# once, with no DEGRADED line at all.
rm -f "$tmp/attempts" "$tmp/sim" "$tmp/readyz"
publish_tenant() { echo 1 >"$STUB_DIR/attempts"; return 0; }
PIDS=()
launch_tenant_and_sim PIDS 2>"$err"
wait "${PIDS[0]}"
[[ -f "$tmp/sim" ]] || { echo "FAIL: a successful publish did not start the sim" >&2; exit 1; }
! grep -q DEGRADED "$err" || { echo "FAIL: a successful publish printed DEGRADED" >&2; exit 1; }

echo "a-failed-prepare-degrades-the-pod: self-test ok — a failed publish degrades and retries (3 attempts, sim started once, after readyz); a clean publish starts the sim with no DEGRADED line"
exit 0
