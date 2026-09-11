#!/usr/bin/env bash
# boss-gcp-converges-itself.sh — drive the boss-gcp self-converge loop
# into a scratch directory and assert what it would do to the host.
#
# WHY THIS EXISTS. On 2026-09-04 the conductor moved into the cluster
# (`feat/conductor-cutover`) and took the boss-gcp deploy hop with it.
# That hop — `sudo /opt/boss/infra/deploy-services.sh prod`, run by the
# train's `deployed` step — was the ONLY thing that installed systemd
# units on boss-gcp, and nothing replaced it. Measured 2026-09-10
# (backlog 408c81f6, and the landed fix 4ef79606 whose own summary says
# it changed nothing until someone deployed):
#
#   * maintenance-estate-observe-units — last packet 2026-09-04, the
#     cutover day, and that packet is still OPEN. A five-minute
#     observer.
#   * maintenance-conservation-invariants — last packet 2026-08-19,
#     while its timer fires hourly on the host.
#   * maintenance-ml-inference-batch — no packet has EVER been filed.
#
# Three urgent CADENCE SILENT alarms were auto-filed for exactly those.
# The forge host had the identical disease and `forge-converge` closed
# it; `boss-gcp-converge` is the same shape for this host, and this is
# the same lint one host over (infra/lint/forge-install-covers-the-ops-
# runner.sh is the sibling, and its idioms are reused verbatim).
#
# WHAT IT CHECKS
#   1. the converge script + unit pair exist, and the script is runnable
#   2. `boss-gcp-converge` is a TIMERS row — the loop that does the
#      installing is installed BY the thing it runs, so after the one
#      bootstrap no unit on this host ever needs a hand again
#   3. `deploy-services.sh units` installs every TIMERS unit pair into a
#      scratch root with a stub systemctl, enables every timer, reloads
#      — and stages no binary, converges no schema, restarts nothing
#   4. the loop converges from the FORGE, never from the GitHub mirror
#   5. a dirty checkout is refused with the tree untouched
#   6. a clean checkout fast-forwards to forge main and drives the
#      installer's `units` mode — the existing installer, not a second
#      deployment mechanism
#   7. a failed install prints the installer's COMPLETE captured output
#      and exits non-zero (CLAUDE.md §Diagnosis: a record that says THAT
#      it failed and not WHAT is the defect class that cost a day)
#   8. the converge brings up this host's JOURNAL READ DOOR
#      (systemd-journal-gatewayd on :19531) and stays non-fatal when the
#      distro package behind it is absent — backlog 68757702: boss-gcp
#      had no read path for logs OR unit state, so a timer there could
#      only be diagnosed by a human on the box, and the forge's door was
#      hand-installed and therefore one rebuild from gone
#   9. the converge installs this host's ops-request RUNNER — the third
#      loop (act), after converge and observe — from the same one
#      definition the forge installer uses, with this host's identity,
#      this checkout, and the CLUSTER as its system of record. Never
#      127.0.0.1, which here is the legacy second stack: a runner
#      pointed there answers nothing and looks healthy (c3d06016)
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
converge="$repo/infra/gcp/boss-gcp-converge.sh"
deploy="$repo/infra/deploy-services.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }

# 1. The pieces exist.
[ -x "$converge" ] || fail "$converge is missing or not executable"
for ext in service timer; do
    [ -f "$repo/infra/gcp/boss-gcp-converge.$ext" ] \
        || fail "infra/gcp/boss-gcp-converge.$ext is missing — the timer is the executor"
done

# 2. The loop installs itself.
rows=$(sed -n '/^TIMERS=(/,/^)/p' "$deploy" | grep -oE '"[a-z0-9-]+:[^"]+"' | tr -d '"')
printf '%s\n' "$rows" | grep -qx 'boss-gcp-converge:gcp' \
    || fail "boss-gcp-converge is not a TIMERS row in deploy-services.sh — the converge
    would install every OTHER unit and never itself, so the one hand-install would have
    to be repeated after every rebuild. That is the bootstrap treadmill this ends."

# 2b. EVERY git CALL GOES THROUGH THE OWNER.
#
# The unit runs as root and /opt/boss belongs to a user, and git refuses
# to READ across that boundary ("dubious ownership", 2.35.2+) as firmly
# as a root WRITE would leave root-owned objects behind. A bare `git -C
# "$REPO"` anywhere in this script is therefore a command that fails
# every tick on the host while passing every test here, because the test
# fixtures are owned by whoever runs the gate. Structural, since the
# runuser branch itself cannot be exercised without a second account.
bare=$(grep -nE '(^|[^_"])git -C "\$REPO"' "$converge" || true)
[ -z "$bare" ] || fail "boss-gcp-converge.sh calls git outside as_owner:
$bare
    Root cannot even READ a checkout it does not own; wrap it:
      as_owner \"git -C '\$REPO' <args>\""

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# ---------------------------------------------------------------------
# 3. THE INSTALLER PATH: `deploy-services.sh units`.
# ---------------------------------------------------------------------
mkdir -p "$tmp/etc" "$tmp/bin"
cat >"$tmp/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
echo "systemctl $*" >>"$STUB_LOG"
[[ "${1:-}" == "is-active" ]] && echo active
exit 0
STUB
chmod +x "$tmp/bin/systemctl"
# The journal read door's package manager, stubbed for the same reason:
# the units mode reaches for it only when the distro unit is absent, and
# a lint must never run a real apt-get.
cat >"$tmp/bin/apt-get" <<'STUB'
#!/usr/bin/env bash
echo "apt-get $*" >>"$STUB_LOG"
exit 0
STUB
chmod +x "$tmp/bin/apt-get"
mkdir -p "$tmp/unitlib" "$tmp/empty-unitlib" "$tmp/etc-absent"
: >"$tmp/unitlib/systemd-journal-gatewayd.socket"

units_run() { # <systemctl-log> <etc> <unit-lib> <outfile>
    STUB_LOG="$1" INSTALL_ETC="$2" INSTALL_SYSTEMCTL="$tmp/bin/systemctl" \
        INSTALL_APT_GET="$tmp/bin/apt-get" INSTALL_UNIT_LIB="$3" \
        BOSS_REPO_ROOT="$repo" BOSS_DEPLOY_ENV=/dev/null \
        bash "$deploy" units >"$4" 2>&1
}

if ! units_run "$tmp/systemctl.log" "$tmp/etc" "$tmp/unitlib" "$tmp/units.out"; then
    echo "FAIL: deploy-services.sh units exited non-zero in the scratch run:" >&2
    cat "$tmp/units.out" >&2
    exit 1
fi
touch "$tmp/systemctl.log"

units_fail() { echo "FAIL: $*" >&2; echo "--- units-mode output:" >&2; cat "$tmp/units.out" >&2; exit 1; }

installed=0
for row in $rows; do
    stem="${row%%:*}"; sub="${row##*:}"
    [ "$sub" = "." ] && src="$repo/infra" || src="$repo/infra/$sub"
    # A row whose source files do not exist is check 1 of
    # timers-leave-a-packet's business, not this one's.
    [ -f "$src/$stem.service" ] && [ -f "$src/$stem.timer" ] || continue
    for ext in service timer; do
        [ -f "$tmp/etc/$stem.$ext" ] \
            || units_fail "$stem.$ext was not installed by the units mode"
    done
    grep -q "enable --now $stem.timer" "$tmp/systemctl.log" \
        || units_fail "$stem.timer was not enabled by the units mode"
    [ -f "$tmp/etc/$stem.service.d/jobs-url.conf" ] \
        || units_fail "$stem got no jobs-url drop-in — boss-maintenance-wrap.sh refuses
    without BOSS_JOBS_URL and every packet fails to open (timers-leave-a-packet check 5)"
    installed=$((installed + 1))
done
[ "$installed" -ge 10 ] \
    || units_fail "only $installed timer pairs landed — the scrape or the mode broke,
    so a green result here would mean nothing"
[ -f "$tmp/etc/boss-gcp-converge.service" ] && [ -f "$tmp/etc/boss-gcp-converge.timer" ] \
    || units_fail "the converge loop's own unit pair did not land"
grep -q "daemon-reload" "$tmp/systemctl.log" || units_fail "no daemon-reload"

# AND NOTHING ELSE. The units mode exists because the full deploy is not
# safe to run unattended every half hour on this host: it stages
# binaries, converges the schema and restarts ~24 services of the second
# (older) BOSS stack boss-gcp still carries. A converge that bounced
# those would be a worse defect than the one it fixes.
if grep -qE 'systemctl (restart|stop|disable)' "$tmp/systemctl.log"; then
    units_fail "the units mode restarts/stops units:
$(grep -E 'systemctl (restart|stop|disable)' "$tmp/systemctl.log")
    It must install unit FILES only — no service is bounced by a converge."
fi
for word in "stage" "converge schema" "health probes" "activate generation"; do
    grep -qi -- "$word" "$tmp/units.out" \
        && units_fail "the units mode ran '$word' — it must install unit files only"
done

# ---------------------------------------------------------------------
# 8. THE JOURNAL READ DOOR (:19531), CONVERGED.
#
# Backlog 68757702: boss-gcp had no read path from the pod for logs OR
# unit state. `http://10.99.0.1:19531/machine` did not answer while the
# forge's returned 200, so the only way to read a unit's journal on the
# WireGuard bastion was a human with ssh — and on 2026-09-10 that cost a
# WRONG conclusion about a nightly timer, reasoned from the tree because
# the host could not be read.
#
# The forge's door was hand-enabled on 2026-09-03 and in the tree
# nowhere, which is the class forge-converge closed: one rebuild away
# from losing the one door that still works when the API is dark. So
# boss-gcp's comes up through its CONVERGE, and the single definition of
# how (infra/journal-door-ensure.sh) is shared with the forge installer
# rather than copied beside it (CLAUDE.md §9a).
#
# WIDER THAN "UNIT FILES ONLY", DELIBERATELY AND BOUNDEDLY. The units
# mode's restraint is about BOSS's own fleet: no build, no schema, no
# service of the second (older) stack bounced. Enabling a distro socket
# unit — and, once, installing the distro package that provides it —
# bounces nothing of ours, and the alternative is a door that needs a
# human on the box, which is the defect. It must never be able to stop
# the converge, which is what the non-fatal path below asserts.
# ---------------------------------------------------------------------
grep -q 'enable --now systemd-journal-gatewayd.socket' "$tmp/systemctl.log" \
    || units_fail "the units mode did not enable systemd-journal-gatewayd.socket. Without it
    boss-gcp has no read path for any unit's journal and a failing timer there can only be
    diagnosed by a human on the box (backlog 68757702).
--- systemctl calls:
$(cat "$tmp/systemctl.log")"
grep -q 'apt-get' "$tmp/systemctl.log" "$tmp/units.out" \
    && units_fail "the units mode reached for apt with the gateway unit already present —
    that would run on every converge tick, every half hour, forever"

# The package-absent path: try the install, say what is down in a line an
# operator can act on, and CARRY ON. A visibility door that could abort
# the converge would be the 2026-09-05 shape — a loop that cannot act
# because of the thing it watches (CLAUDE.md §Diagnosis).
if ! units_run "$tmp/systemctl-absent.log" "$tmp/etc-absent" "$tmp/empty-unitlib" "$tmp/units-absent.out"; then
    echo "FAIL: the units mode ABORTED because the journal gateway unit was absent:" >&2
    cat "$tmp/units-absent.out" >&2
    echo "    The converge that keeps this host's units current must not hang on a read door." >&2
    exit 1
fi
grep -q 'apt-get install -y' "$tmp/systemctl-absent.log" \
    || { echo "FAIL: the units mode did not try to install systemd-journal-remote when the" >&2
         echo "    gateway unit was absent. Nothing else on boss-gcp installs it." >&2
         cat "$tmp/units-absent.out" >&2; exit 1; }
grep -q 'journal read door' "$tmp/units-absent.out" \
    || { echo "FAIL: the units mode was silent about the read door. Name what is down:" >&2
         cat "$tmp/units-absent.out" >&2; exit 1; }

# ---------------------------------------------------------------------
# 9. THE THIRD LOOP: THE ops-request RUNNER, CONVERGED.
#
# Backlog c3d06016. BOSS runs three loops on a managed host — converge,
# observe, and ACT. boss-gcp had the first two and not the third: the
# ops-runner had never been installed here, so an ops-request filed
# against `host: boss-gcp` sat at `ready` with nothing behind it and
# every operational read on the WireGuard bastion went back through a
# human. It rides this mode, from the same ONE definition the forge
# installer uses (infra/ops/install-ops-runner.sh).
#
# THE FAILURE THIS SECTION EXISTS FOR is the invisible one. This mode
# writes every TIMERS row a `jobs-url.conf` drop-in naming
# `127.0.0.1:<jobs port>` — on THIS host the legacy second stack, not
# the system of record. A runner pointed there finds no ops-request
# packets, exits 0 every minute and looks healthy forever: a wrong
# target answers instead of erroring (CLAUDE.md §Doors). So the
# assertion that matters most below is the negative one.
# ---------------------------------------------------------------------
for ext in service timer; do
    [ -f "$tmp/etc/boss-ops-runner.$ext" ] \
        || units_fail "boss-ops-runner.$ext did not land — this host cannot answer an
    ops-request, so every read on the bastion is a human with ssh again (c3d06016)"
done
cmp -s "$repo/infra/ops/boss-ops-runner.service" "$tmp/etc/boss-ops-runner.service" \
    || units_fail "the installed ops unit differs from infra/ops/boss-ops-runner.service —
    the unit file is ONE definition for every host; what differs is the drop-in"
grep -q 'enable --now boss-ops-runner.timer' "$tmp/systemctl.log" \
    || units_fail "boss-ops-runner.timer was not enabled:
$(cat "$tmp/systemctl.log")"
dropin="$tmp/etc/boss-ops-runner.service.d/boss-gcp.conf"
[ -f "$dropin" ] || units_fail "the ops runner has no boss-gcp drop-in. The unit file carries
    no HOST_ID, deliberately — a runner that guessed its host would answer another host's
    packets — so with no drop-in it refuses on every tick."
grep -qx 'Environment=HOST_ID=boss-gcp' "$dropin" \
    || units_fail "the drop-in does not name this host: $(cat "$dropin")"
grep -qx 'ExecStart=' "$dropin" \
    || units_fail "the drop-in does not clear the unit's ExecStart before overriding it"
grep -qx "ExecStart=/usr/bin/env BOSS_JOBS_URL=http://10.20.0.34:7900 $repo/infra/ops/ops-runner.sh" "$dropin" \
    || units_fail "the drop-in does not run the runner from THIS checkout with the cluster
    pinned inline by env(1): $(cat "$dropin")"
# THE NEGATIVE. `127.0.0.1` anywhere in what configures this runner is
# the legacy stack, and the failure it causes is silent.
# Directive lines only: the unit's own comments explain this very trap
# by naming the address.
ops_conf=$(cat "$tmp/etc/boss-ops-runner.service" "$tmp/etc/boss-ops-runner.service.d/"*.conf 2>/dev/null \
    | grep -vE '^[[:space:]]*[#;]')
if printf '%s\n' "$ops_conf" | grep -n '127\.0\.0\.1'; then
    units_fail "the ops runner is configured against 127.0.0.1 — boss-gcp's localhost jobs API
    is the LEGACY second stack (91ddebfb), not the system of record. The runner would poll it,
    find no ops-request packets, exit 0 every minute and look healthy forever."
fi
# AND IT IS NOT A TIMERS ROW, deliberately. A row would earn it that
# 127.0.0.1 drop-in and would owe timers-leave-a-packet.sh a
# maintenance-wrap packet pair — which this unit must not have: it
# fires every minute, its product IS packets, and a packet per firing
# would drown the board (infra/ops/ops-runner.sh's header).
printf '%s\n' "$rows" | grep -q 'boss-ops-runner' \
    && units_fail "boss-ops-runner is a TIMERS row. It must be installed from its own block:
    as a row it would get the legacy 127.0.0.1 jobs-url drop-in and would owe a
    maintenance-wrap packet pair it deliberately does not have."

# ---------------------------------------------------------------------
# 4. WHERE IT CONVERGES FROM. GitHub is the mirror, never the source
#    (27ab7680). boss-gcp's /opt/boss carries BOTH remotes, so the loop
#    has to choose, and choosing wrong converges the host on a mirror
#    that lags the forge by a publish.
# ---------------------------------------------------------------------
export HOME="$tmp/home"   # no global gitconfig in the way
mkdir -p "$HOME"
export GIT_AUTHOR_NAME=lint GIT_AUTHOR_EMAIL=lint@example.invalid
export GIT_COMMITTER_NAME=lint GIT_COMMITTER_EMAIL=lint@example.invalid
git_q() { git "$@" >/dev/null 2>&1 || fail "git $* failed in the scratch fixture"; }

forge_bare="$tmp/forge.git"
git_q init --quiet --bare --initial-branch=main "$forge_bare"
seed="$tmp/seed"
git_q clone --quiet "$forge_bare" "$seed"
echo one >"$seed/file"
git_q -C "$seed" add file
git_q -C "$seed" commit --quiet -m "one"
git_q -C "$seed" push --quiet origin main
first=$(git -C "$seed" rev-parse HEAD)

resolve() { # <dir> -> prints the chosen remote, or fails
    BOSS_GCP_REPO_DIR="$1" bash "$converge" --resolve-remote 2>&1
}

# 4a. forge + the public mirror: the forge wins.
both="$tmp/both"
git_q clone --quiet --origin forge "$forge_bare" "$both"
git_q -C "$both" remote add origin https://github.com/algedonic-dev/boss.git
got=$(resolve "$both") || fail "--resolve-remote refused a checkout that has a forge remote: $got"
[ "$got" = "forge" ] || fail "with remotes forge + a github.com origin, the loop chose '$got'"

# 4b. the mirror alone is a refusal, not a fallback.
mirror="$tmp/mirror-only"
git_q clone --quiet "$forge_bare" "$mirror"
git_q -C "$mirror" remote set-url origin https://github.com/algedonic-dev/boss.git
if out=$(resolve "$mirror"); then
    fail "--resolve-remote chose '$out' from a checkout whose only remote is the GitHub
    mirror. The mirror lags the forge by a publish and is never the source of truth; a
    converge that reads it installs yesterday's units and says it converged."
fi
printf '%s' "$out" | grep -qi "github" \
    || fail "the refusal does not name the mirror it refused: $out"

# 4c. an explicit override is honoured; a bogus one is refused.
got=$(BOSS_GCP_CONVERGE_REMOTE=forge resolve "$both") \
    || fail "an explicit BOSS_GCP_CONVERGE_REMOTE=forge was refused: $got"
[ "$got" = "forge" ] || fail "the override was not honoured (got '$got')"
if out=$(BOSS_GCP_CONVERGE_REMOTE=nope resolve "$both"); then
    fail "BOSS_GCP_CONVERGE_REMOTE named a remote that does not exist and was accepted: $out"
fi

# ---------------------------------------------------------------------
# 5/6/7. THE LOOP ITSELF, with the installer stubbed so the assertions
#        are about the converge and not about systemd.
# ---------------------------------------------------------------------
cat >"$tmp/bin/installer-ok" <<'STUB'
#!/usr/bin/env bash
echo "stub installer: args=$*" >>"$STUB_CALLS"
echo "units: 14 timer unit pair(s) installed and enabled"
exit 0
STUB
cat >"$tmp/bin/installer-bad" <<'STUB'
#!/usr/bin/env bash
echo "stub installer: args=$*" >>"$STUB_CALLS"
echo "LINE-ONE-OF-MANY"
echo "DISTINCTIVE-FAILURE-DETAIL: migration 202609 refused"
exit 3
STUB
chmod +x "$tmp/bin/installer-ok" "$tmp/bin/installer-bad"

run_converge() { # <dir> <installer> -> output; returns the script's status
    BOSS_GCP_REPO_DIR="$1" BOSS_GCP_CONVERGE_INSTALLER="$2" \
        STUB_CALLS="$tmp/calls.log" bash "$converge" 2>&1
}

# 5. A dirty checkout is refused, and nothing is discarded or installed.
dirty="$tmp/dirty"
git_q clone --quiet --origin forge "$forge_bare" "$dirty"
echo "local edit nobody committed" >>"$dirty/file"
: >"$tmp/calls.log"
if out=$(run_converge "$dirty" "$tmp/bin/installer-ok"); then
    fail "the converge ran against a DIRTY checkout instead of refusing:
$out"
fi
grep -q "local edit nobody committed" "$dirty/file" \
    || fail "the converge DISCARDED an uncommitted local change — it must refuse, never clobber"
[ ! -s "$tmp/calls.log" ] \
    || fail "the converge installed units from a dirty tree: $(cat "$tmp/calls.log")"
printf '%s' "$out" | grep -q "$dirty" \
    || fail "the refusal does not name the checkout an operator has to look at:
$out"

# 6. A clean checkout fast-forwards to forge main and drives the real
#    installer path, in `units` mode.
echo two >>"$seed/file"
git_q -C "$seed" commit --quiet -am "two"
git_q -C "$seed" push --quiet origin main
want=$(git -C "$seed" rev-parse HEAD)

clean="$tmp/clean"
git_q clone --quiet --origin forge "$forge_bare" "$clean"
# BEHIND forge main by one commit, which is the state this whole car is
# about. (The clone is made after the push, so it has to be moved back
# deliberately — a fixture already at the target proved nothing, and the
# mutation test that checked this lint caught exactly that.)
git_q -C "$clean" reset --hard --quiet "$first"
: >"$tmp/calls.log"
out=$(run_converge "$clean" "$tmp/bin/installer-ok") \
    || fail "the converge failed on a clean checkout:
$out"
got=$(git -C "$clean" rev-parse HEAD)
[ "$got" = "$want" ] \
    || fail "the checkout is at $got, not forge main ($want) — the converge did not move the tree:
$out"
[ "$(git -C "$clean" rev-parse --abbrev-ref HEAD)" = "main" ] \
    || fail "the converge left the checkout off main. The retired deploy hop it replaces
    (boss-cli train.rs) refused to deploy unless the tree was on main and clean; a
    detached HEAD on a hand-operated host is a surprise nobody asked for."
grep -q "args=units" "$tmp/calls.log" \
    || fail "the converge did not drive the installer's units mode (calls: $(cat "$tmp/calls.log"))"
printf '%s' "$out" | grep -q "${want:0:8}" \
    || fail "the converge does not say which commit it converged on:
$out"
printf '%s' "$out" | grep -q "14 timer unit pair" \
    || fail "the installer's own summary is not in the converge's output — print what it did:
$out"

# Idempotent: a second pass moves nothing, still installs, still says so.
: >"$tmp/calls.log"
out=$(run_converge "$clean" "$tmp/bin/installer-ok") \
    || fail "the second converge failed — the loop is not idempotent:
$out"
grep -q "args=units" "$tmp/calls.log" \
    || fail "the second pass skipped the installer. A unit removed by hand must come back
    on the next tick even when main has not moved: that is what converge means."

# 7. A failed install prints EVERY line the installer wrote.
: >"$tmp/calls.log"
if out=$(run_converge "$clean" "$tmp/bin/installer-bad"); then
    fail "the converge reported success over an installer that exited 3:
$out"
fi
printf '%s' "$out" | grep -q "DISTINCTIVE-FAILURE-DETAIL" \
    || fail "the failure output does not carry what the installer said. A tail, a -q or a
    digest suppresses OUTPUT, not work: capture to a file and print it ALL on failure.
$out"
printf '%s' "$out" | grep -q "LINE-ONE-OF-MANY" \
    || fail "only part of the installer's output survived — no tails (CLAUDE.md §Diagnosis)
$out"

echo "boss-gcp-converges-itself: ok — $installed timer pairs install from \`deploy-services.sh units\` (the converge's own among them, nothing restarted), the journal read door on :19531 comes up with it and cannot abort it, the ops-request runner lands with HOST_ID=boss-gcp, this checkout and the CLUSTER as its system of record (never the legacy 127.0.0.1 stack) without being a TIMERS row, the loop converges from the forge and refuses the mirror, refuses a dirty tree, fast-forwards and drives the installer idempotently, and prints every line of a failed install"
exit 0
