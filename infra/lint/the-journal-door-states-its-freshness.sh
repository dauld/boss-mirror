#!/usr/bin/env bash
# the-journal-door-states-its-freshness — the forge's journal-over-HTTP
# door can never again answer confidently with stale data, and can never
# again fall out of the host's converge.
#
# 2026-09-10 (packet 8bea0c9c): `systemd-journal-gatewayd` on the forge
# served a journal whose newest entry was seven hours old while returning
# HTTP 200 to everything. A unit-filtered query for a unit that HAD run
# came back with zero rows — indistinguishable from "that unit never
# ran". It was one report away from becoming "the forge disk sweep is not
# running", a false finding filed against a healthy loop. CLAUDE.md
# §Doors names the rule: "a wrong target answers instead of erroring".
#
# Two halves, both pinned here because they fail separately:
#   1. THE REFUSAL. infra/forge/journal-read.sh must refuse a stale door
#      naming BOTH timestamps, pass a fresh one, and skip loudly — never
#      exit 0 — when the door does not answer. Exercised with fixtures
#      and a pinned `now`, so it needs no network and no forge.
#   2. THE CONVERGE. infra/forge/install.sh must still enable the gateway
#      socket. It was hand-enabled once on 2026-09-03 and in the tree
#      nowhere, so a rebuild lost the door; the same shape as the ops
#      runner's residue (4d5f158a). Exercised by running the installer
#      into a scratch root with a stub systemctl, exactly the way
#      forge-install-covers-the-ops-runner.sh does.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
door="$repo/infra/forge/journal-read.sh"
installer="$repo/infra/forge/install.sh"
for f in "$door" "$installer"; do
    [[ -f "$f" ]] || { echo "the-journal-door-states-its-freshness: missing $f" >&2; exit 1; }
done
[[ -x "$door" ]] || { echo "the-journal-door-states-its-freshness: $door is not executable" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

# The incident's own numbers, so the fixture is the failure and not a
# story about it: newest entry 2026-09-09 20:40:53 UTC, read at
# 2026-09-10 03:42:00 UTC, seven hours and one minute apart.
STALE_US=1788986453000000     # 2026-09-09T20:40:53Z
FRESH_US=1789011700000000     # 2026-09-10T03:41:40Z, 20s before the read
NOW_S=1789011720              # 2026-09-10T03:42:00Z
FROM_US=1775930329839130      # the retention floor the live door reported

machine() { printf '{ "machine_id" : "a14291dd", "hostname" : "david-asus-minipc", "cutoff_from_realtime" : "%s", "cutoff_to_realtime" : "%s" }\n' "$FROM_US" "$1"; }
entry()   { printf '{"__REALTIME_TIMESTAMP":"%s","MESSAGE":"boss-ops-runner.service: Deactivated successfully."}\n' "$1"; }

machine "$STALE_US" >"$tmp/machine-stale.json"
machine "$FRESH_US" >"$tmp/machine-fresh.json"
entry   "$STALE_US" >"$tmp/tail-stale.json"
entry   "$FRESH_US" >"$tmp/tail-fresh.json"

run() { "$door" --now "$NOW_S" "$@" >"$tmp/out" 2>&1; echo $?; }

# 1a. STALE REFUSES, and the refusal carries both timestamps.
rc=$(run --machine-file "$tmp/machine-stale.json" --tail-file "$tmp/tail-stale.json")
[[ "$rc" == "4" ]] || fail "a 7-hour-stale door exited $rc, not 4 (refuse):
$(cat "$tmp/out")"
grep -q 'STALE — REFUSING TO READ' "$tmp/out" || fail "the refusal does not say it is refusing: $(cat "$tmp/out")"
# Both timestamps, so nobody re-derives them from a bare "stale".
grep -q '2026-09-09T20:40:53Z' "$tmp/out" || fail "the refusal does not name the newest entry it holds: $(cat "$tmp/out")"
grep -q '2026-09-10T03:42:00Z' "$tmp/out" || fail "the refusal does not name the time now: $(cat "$tmp/out")"
grep -q '7h01m' "$tmp/out" || fail "the refusal does not name the gap in human units: $(cat "$tmp/out")"
grep -q 'NOT evidence that a' "$tmp/out" || fail "the refusal does not say an empty result is not evidence: $(cat "$tmp/out")"
grep -q 'journal-tail' "$tmp/out" || fail "the refusal does not name the independent door: $(cat "$tmp/out")"

# 1b. FRESH PASSES.
rc=$(run --machine-file "$tmp/machine-fresh.json" --tail-file "$tmp/tail-fresh.json")
[[ "$rc" == "0" ]] || fail "a 20-second-behind door exited $rc, not 0:
$(cat "$tmp/out")"
grep -q 'FRESH' "$tmp/out" || fail "a fresh door is not reported fresh: $(cat "$tmp/out")"
grep -q '2026-09-10T03:41:40Z' "$tmp/out" || fail "the pass does not name the newest entry: $(cat "$tmp/out")"

# 1c. ONE FIELD MAY NOT VOUCH FOR ITSELF. /machine claiming freshness
# while a real entry is seven hours old must still refuse — we never
# established which internal view went stale, so the older reading wins.
rc=$(run --machine-file "$tmp/machine-fresh.json" --tail-file "$tmp/tail-stale.json")
[[ "$rc" == "4" ]] || fail "a door whose /machine claims fresh but whose newest entry is 7h old exited $rc, not 4:
$(cat "$tmp/out")"
grep -q 'DISAGREE' "$tmp/out" || fail "the two readings disagreed and the refusal did not say so: $(cat "$tmp/out")"

# 1d. UNREACHABLE SKIPS LOUDLY — exit 3, never 0. Port 1 on loopback is
# refused by the kernel with no network of any kind.
rc=$(run --host 127.0.0.1:1 --check)
[[ "$rc" == "3" ]] || fail "an unreachable door exited $rc; 0 would be a silent pass and 4 a false accusation:
$(cat "$tmp/out")"
grep -q 'SKIPPED, the question is NOT answered' "$tmp/out" || fail "the skip is not loud: $(cat "$tmp/out")"

# 1e. A THRESHOLD WITH A STATED REASON, in the file, not in someone's head.
grep -q 'THE THRESHOLD: 30 minutes' "$door" || fail "$door no longer states its threshold and why"

# 1f. A SECOND HOST IS A NAMED TARGET, AND A REFUSAL ABOUT IT NAMES IT.
#
# backlog 68757702 adds boss-gcp's door (the WireGuard bastion, 10.99.0.1),
# so this script now reads two hosts. `--host` carried the read fine, but
# every line of guidance it printed named the FORGE regardless: a refusal
# about boss-gcp told an operator to repair a gateway on another machine,
# and to reach for an ops-request against a host whose runner is not
# installed. That is CLAUDE.md §Diagnosis one notch down — a verdict
# somebody must go re-derive — so the advice is derived from the target.
rc=$(run --host boss-gcp --machine-file "$tmp/machine-stale.json" --tail-file "$tmp/tail-stale.json")
[[ "$rc" == "4" ]] || fail "a stale door on a named second host exited $rc, not 4:
$(cat "$tmp/out")"
grep -q '10.99.0.1:19531' "$tmp/out" \
    || fail "--host boss-gcp did not resolve to that host's door (10.99.0.1:19531). A named
    target keeps the address in the tree once, the way 'forge' already does:
$(cat "$tmp/out")"
grep -q 'host=boss-gcp' "$tmp/out" \
    || fail "the refusal about boss-gcp advises an ops-request with host=forge. It must name
    the host it refused about — advice pointing at another machine is worse than none:
$(cat "$tmp/out")"
grep -qi 'boss-gcp' <(grep -i 'repair' -A2 "$tmp/out") \
    || fail "the repair command in a boss-gcp refusal does not say it runs on boss-gcp:
$(cat "$tmp/out")"
# And the forge's own advice is unchanged — the default target still
# reads as the forge, with the runbook that covers it.
rc=$(run --machine-file "$tmp/machine-stale.json" --tail-file "$tmp/tail-stale.json")
grep -q 'host=forge' "$tmp/out" \
    || fail "the default target's refusal no longer advises host=forge:
$(cat "$tmp/out")"

# 2. The installer still enables the socket.
mkdir -p "$tmp/etc" "$tmp/bin" "$tmp/unitlib"
cat >"$tmp/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"$STUB_LOG"
[[ "${1:-}" == "is-active" ]] && echo active
exit 0
STUB
chmod +x "$tmp/bin/systemctl"
cat >"$tmp/bin/apt-get" <<'STUB'
#!/usr/bin/env bash
echo "apt-get $*" >>"$STUB_LOG"
exit 0
STUB
chmod +x "$tmp/bin/apt-get"
: >"$tmp/unitlib/systemd-journal-gatewayd.socket"

install_run() {
    STUB_LOG="$1" INSTALL_ETC="$tmp/etc" INSTALL_SYSTEMCTL="$tmp/bin/systemctl" \
        INSTALL_APT_GET="$tmp/bin/apt-get" INSTALL_UNIT_LIB="$2" INSTALL_KUBECTL=0 \
        bash "$installer" >"$tmp/install-out" 2>&1
}

if ! install_run "$tmp/log-present" "$tmp/unitlib"; then
    fail "install.sh exited non-zero with the gateway unit present:
$(cat "$tmp/install-out")"
fi
grep -q 'enable --now systemd-journal-gatewayd.socket' "$tmp/log-present" \
    || fail "install.sh did not enable systemd-journal-gatewayd.socket — a rebuild loses the read door again:
$(cat "$tmp/log-present")"
grep -q 'apt-get' "$tmp/log-present" \
    && fail "install.sh reached for apt with the gateway unit already present; that runs on every converge tick"

# The package-absent path: warn with the command a human can run, install
# the package — and DO NOT abort. A visibility door must not be able to
# stop the converge that keeps this host alive (CLAUDE.md §Diagnosis).
if ! install_run "$tmp/log-absent" "$tmp/empty-unitlib"; then
    fail "install.sh aborted because the gateway unit was absent; the whole converge must not hang on a read door:
$(cat "$tmp/install-out")"
fi
grep -q 'apt-get install -y' "$tmp/log-absent" \
    || fail "install.sh did not try to install systemd-journal-remote when the unit was absent"
grep -q 'the journal read door' "$tmp/install-out" \
    || fail "install.sh was silent about the read door being down: $(cat "$tmp/install-out")"

echo "the-journal-door-states-its-freshness: ok — journal-read.sh refuses a 7h-stale door naming both timestamps, refuses when its two readings disagree, passes a 20s-behind one, skips loudly (exit 3) on an unreachable target, and resolves forge + boss-gcp as named targets whose advice names the host it refused about; install.sh enables the gateway socket and stays non-fatal when the package is absent"
exit 0
