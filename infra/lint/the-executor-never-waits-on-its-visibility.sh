#!/usr/bin/env bash
# the-executor-never-waits-on-its-visibility.sh — boss-maintenance-wrap
# lets the chore run when the jobs API is unreachable, and still aborts
# it when the API answers an error.
#
# Runs a copy of the wrap beside a stub boss-api-curl.sh. 2026-09-05:
# the cluster converge could not start for four hours because its
# ExecStartPre (this wrap) failed on a dark system of record — the loop
# that would have restored the API was the one waiting on it.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wrap="$here/../boss-maintenance-wrap.sh"
[[ -f "$wrap" ]] || { echo "the-executor-never-waits-on-its-visibility: missing $wrap" >&2; exit 1; }
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
cp "$wrap" "$tmp/boss-maintenance-wrap.sh"; chmod +x "$tmp/boss-maintenance-wrap.sh"
stub() {  # $1 = exit code, $2 = stdout for GET
    printf '#!/usr/bin/env bash\necho "stub api-curl $*" >>"%s"\n[[ "$*" == *"-X POST"* ]] || printf %%s '"'"'%s'"'"'\nexit %s\n' "$tmp/calls" "$2" "$1" >"$tmp/boss-api-curl.sh"
    chmod +x "$tmp/boss-api-curl.sh"
}
run() { BOSS_JOBS_URL=http://stub bash "$tmp/boss-maintenance-wrap.sh" maintenance-cluster-converge "Cluster converge" >"$tmp/out" 2>&1; echo $?; }
fail() { echo "FAIL: $*" >&2; echo "--- wrap output:" >&2; cat "$tmp/out" >&2; exit 1; }

# 1. Unreachable (curl exit 7 past the deadline): the run proceeds, loudly.
stub 7 ""; rc=$(run)
[[ "$rc" -eq 0 ]] || fail "an unreachable API blocked the executor (rc $rc)"
grep -q "UNREACHABLE" "$tmp/out" || fail "no loud UNREACHABLE line"
# 2. The API answered an error (curl exit 22): the run still aborts.
stub 22 ""; rc=$(run)
[[ "$rc" -ne 0 ]] || fail "an API error did not abort the run"
grep -q "aborting the run" "$tmp/out" || fail "no aborting line on an API error"
# 3. A healthy API: the success paths are unchanged (needs jq, as the wrap does).
if command -v jq >/dev/null 2>&1; then
    stub 0 '{"data":[{"id":"x"}]}'; rc=$(run)
    [[ "$rc" -eq 0 ]] && grep -q "recovery" "$tmp/out" || fail "an open packet was not reused"
    stub 0 '{"data":[]}'; rc=$(run)
    [[ "$rc" -eq 0 ]] && grep -q "spawned" "$tmp/out" || fail "a fresh packet was not spawned"
    healthy="healthy paths unchanged (reuse and spawn)"
else
    healthy="healthy paths not exercised here (no jq on this box; the gate image has it)"
fi
echo "the-executor-never-waits-on-its-visibility: self-test ok — an unreachable API lets the chore run with a loud line, an API error still aborts it; $healthy"
exit 0
