#!/usr/bin/env bash
# forge-install-covers-the-ops-runner.sh — run the forge installer into
# a scratch directory and assert what it would put on the host.
#
# The forge host adopts its units from `infra/forge/install.sh` every
# ten minutes (forge-converge). A unit that script does not install is
# a unit a rebuild loses — on 2026-09-05 that was the ops-request
# runner, answering packets on the forge only because a hand had put
# it there (packet 4d5f158a). This exercises the installer with
# INSTALL_ETC pointing at a temp dir and a stub systemctl, and asserts:
# every listed unit pair lands; the ops runner lands from infra/ops
# with a drop-in carrying THIS host's identity and checkout; and the
# timers, the ops runner's included, are enabled.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
installer="$repo/infra/forge/install.sh"
[[ -f "$installer" ]] || { echo "forge-install-covers-the-ops-runner: missing $installer" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/etc" "$tmp/bin"
cat >"$tmp/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"$STUB_LOG"
[[ "${1:-}" == "is-active" ]] && echo active
exit 0
STUB
chmod +x "$tmp/bin/systemctl"

if ! STUB_LOG="$tmp/systemctl.log" INSTALL_ETC="$tmp/etc" INSTALL_SYSTEMCTL="$tmp/bin/systemctl" INSTALL_KUBECTL=0 \
    bash "$installer" >"$tmp/out" 2>&1; then
    echo "FAIL: install.sh exited non-zero in the scratch run:" >&2
    cat "$tmp/out" >&2
    exit 1
fi

fail() { echo "FAIL: $*" >&2; echo "--- installer output:" >&2; cat "$tmp/out" >&2; exit 1; }

for u in reap-dead-ci-jobs cluster-deploy-runner disk-floor-sweep forge-converge estate-observe-host boss-ops-runner; do
    for ext in service timer; do
        [[ -f "$tmp/etc/$u.$ext" ]] || fail "$u.$ext was not installed"
    done
    grep -q "enable --now $u.timer" "$tmp/systemctl.log" || fail "$u.timer was not enabled"
done
grep -q "daemon-reload" "$tmp/systemctl.log" || fail "no daemon-reload"

dropin="$tmp/etc/boss-ops-runner.service.d/forge.conf"
[[ -f "$dropin" ]] || fail "the ops runner has no forge drop-in"
grep -qx 'Environment=HOST_ID=forge' "$dropin" || fail "the drop-in does not name this host: $(cat "$dropin")"
grep -qx 'ExecStart=' "$dropin" || fail "the drop-in does not clear the unit's ExecStart before overriding it"
grep -qx "ExecStart=/usr/bin/env BOSS_JOBS_URL=http://10.20.0.34:7900 $repo/infra/ops/ops-runner.sh" "$dropin" \
    || fail "the drop-in does not run the runner from this checkout: $(cat "$dropin")"
# The unit file itself is boss-gcp's, byte for byte — one definition.
cmp -s "$repo/infra/ops/boss-ops-runner.service" "$tmp/etc/boss-ops-runner.service" \
    || fail "the installed ops unit differs from infra/ops/boss-ops-runner.service"

# ---------------------------------------------------------------------
# AND THE RUN SAYS WHAT IT DID, ON ITS PACKET.
#
# maintenance-forge-converge closed `result=ok` and carried nothing else,
# exactly like its boss-gcp sibling — whose identical silence on
# 2026-09-11 was enough to support a wrong conclusion about whether a
# converge had installed the ops runner (it had, twice). The installer now
# records what it installed in the run summary (infra/run-summary.sh),
# which boss-step.sh merges onto the step.
# ---------------------------------------------------------------------
command -v jq >/dev/null || fail "jq is required to check the run summary"
sum="$tmp/summary.json"
mkdir -p "$tmp/etc-sum"
if ! STUB_LOG="$tmp/systemctl-sum.log" INSTALL_ETC="$tmp/etc-sum" INSTALL_SYSTEMCTL="$tmp/bin/systemctl" \
    INSTALL_KUBECTL=0 BOSS_RUN_SUMMARY_FILE="$sum" bash "$installer" >"$tmp/out-sum" 2>&1; then
    echo "FAIL: install.sh exited non-zero with a summary file configured:" >&2
    cat "$tmp/out-sum" >&2
    exit 1
fi
sum_fail() { echo "FAIL: $*" >&2; echo "--- summary:" >&2; cat "$sum" 2>/dev/null >&2
             echo "--- installer output:" >&2; cat "$tmp/out-sum" >&2; exit 1; }
[ -f "$sum" ] || sum_fail "install.sh left no run summary, so forge-converge's packet can only
    say result=ok — the shape that cost a wrong diagnosis on 2026-09-11"
[ "$(jq -r '.units_installed // ""' "$sum")" -ge 6 ] 2>/dev/null \
    || sum_fail "the summary does not count the unit pairs it installed"
[ "$(jq -r '.ops_runner // ""' "$sum")" = "installed" ] \
    || sum_fail "the summary does not record the ops-request runner's own verdict"

# AND A RUNNER THAT DID NOT INSTALL MUST NOT REPORT THAT IT DID. With no
# `set -e`, install-ops-runner.sh used to print its success line and exit
# 0 after a refused `enable --now` — a claim made regardless of outcome,
# which is the defect one layer under the silent packet.
cat >"$tmp/bin/systemctl-refuses" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"$STUB_LOG"
[[ "${1:-}" == "enable" ]] && { echo "Failed to enable unit: stub refusal" >&2; exit 1; }
exit 0
STUB
chmod +x "$tmp/bin/systemctl-refuses"
sum_bad="$tmp/summary-refused.json"
mkdir -p "$tmp/etc-refused"
if STUB_LOG="$tmp/systemctl-refused.log" INSTALL_ETC="$tmp/etc-refused" \
    INSTALL_SYSTEMCTL="$tmp/bin/systemctl-refuses" BOSS_RUN_SUMMARY_FILE="$sum_bad" \
    bash "$repo/infra/ops/install-ops-runner.sh" forge >"$tmp/out-refused" 2>&1; then
    echo "FAIL: install-ops-runner.sh exited 0 after systemctl refused to enable its timer:" >&2
    cat "$tmp/out-refused" >&2
    echo "    Nothing fires the runner, so every ops-request for this host waits forever —" >&2
    echo "    and the caller, and the converge's packet, read that as an install." >&2
    exit 1
fi
case "$(jq -r '.ops_runner // ""' "$sum_bad" 2>/dev/null)" in
failed*) ;;
*) echo "FAIL: the failed ops-runner install is not recorded as failed on the run summary:" >&2
   cat "$sum_bad" 2>/dev/null >&2; cat "$tmp/out-refused" >&2; exit 1 ;;
esac

echo "forge-install-covers-the-ops-runner: self-test ok — 6 unit pairs installed into a scratch root, the ops runner from infra/ops with a forge drop-in (HOST_ID=forge, ExecStart from this checkout), 6 timers enabled, and the run's own summary carries the counts plus the ops runner's verdict — 'failed: …' and a non-zero exit when its timer will not enable, never an unconditional success line"
exit 0
