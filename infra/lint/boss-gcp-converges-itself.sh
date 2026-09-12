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
#  10. the run SAYS WHAT IT INSTALLED on its own packet — counts, each
#      sub-installer's verdict, every skip named verbatim, the sha and the
#      remote, and the installer's exit status when it fails; and a
#      summary from an earlier run is cleared before this one can refuse
#      anything, so a stale record can never read as this run's. Measured
#      2026-09-11: `result=ok` was the whole record, and answering "did it
#      install?" took an ops-request plus 200 journal lines off the host
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
command -v jq >/dev/null || fail "jq is required to check the run summary and the roles read"
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

# UNITS_REPO_ROOT and UNITS_SUMMARY are read from the environment rather
# than taken as arguments so the three original call sites read as they
# did; check 10 sets them.
units_run() { # <systemctl-log> <etc> <unit-lib> <outfile>
    STUB_LOG="$1" INSTALL_ETC="$2" INSTALL_SYSTEMCTL="$tmp/bin/systemctl" \
        INSTALL_APT_GET="$tmp/bin/apt-get" INSTALL_UNIT_LIB="$3" \
        BOSS_REPO_ROOT="${UNITS_REPO_ROOT:-$repo}" BOSS_DEPLOY_ENV=/dev/null \
        BOSS_RUN_SUMMARY_FILE="${UNITS_SUMMARY:-}" \
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
# 3b. A HOST INSTALLS THE ROWS ITS ROLES NAME, AND REPORTS THE REST.
#
# Design 9e3e093f: a node declares its roles as registry data and
# infra/estate/roles.toml maps each role to TIMERS stems. Two things are
# pinned. First, the two rosters are one roster (CLAUDE.md §9a): every
# TIMERS stem is named by exactly one role (or `always`) in roles.toml,
# and roles.toml names nothing the array does not have. Second, with
# BOSS_NODE_ROLES set, the units mode installs ONLY the named rows plus
# `always`, prints NOT IN ROLE for each of the others by name, enables
# none of those, and counts them on the packet — while with no roles at
# all it installs every row (check 3 above ran exactly that).
# ---------------------------------------------------------------------
roles_toml="$repo/infra/estate/roles.toml"
[ -f "$roles_toml" ] || fail "infra/estate/roles.toml is missing — the role -> units map"
role_stems=$(grep -oE '"boss-[a-z0-9-]+"' "$roles_toml" | tr -d '"' | sort)
timer_stems=$(printf '%s\n' $rows | cut -d: -f1 | sort)
[ "$role_stems" = "$timer_stems" ] || fail "roles.toml and the TIMERS array disagree —
    named by roles.toml but not a TIMERS row: $(comm -23 <(echo "$role_stems") <(echo "$timer_stems") | tr '\n' ' ')
    a TIMERS row no role names: $(comm -13 <(echo "$role_stems") <(echo "$timer_stems") | tr '\n' ' ')
    Every unit a host runs is derived from a role; a row outside every role is a row no host is for."
dup=$(printf '%s\n' $role_stems | uniq -d)
[ -z "$dup" ] || fail "roles.toml names a unit under two roles: $dup — one role owns each row"

# The installer under roles: legacy-stack only, so the observer and the
# ML batch (other roles) must be reported, not installed.
mkdir -p "$tmp/etc-roles"
: >"$tmp/systemctl-roles.log"
# THE HOST'S OWN ROLE SET FIRST — including one that maps to NO units
# (wireguard-bastion: `units = []`). Measured 2026-09-12 18:55Z on
# boss-gcp, three converges in a row: the roles read worked, the
# installer printed its header and died with exit 1 and not one more
# line, because `role_units` piped awk into `grep -oE '"[^"]+"'`, an
# empty units list gave grep nothing to match, and `set -euo pipefail`
# ended the script on that exit 1. The lint below had only ever run
# with BOSS_NODE_ROLES=legacy-stack, a role with units.
mkdir -p "$tmp/etc-all"
: >"$tmp/systemctl-all.log"
sum_all="$tmp/summary-all-roles.json"
BOSS_NODE_ROLES=legacy-stack,ml-batch-host,off-cluster-observer,wireguard-bastion UNITS_SUMMARY="$sum_all" \
    units_run "$tmp/systemctl-all.log" "$tmp/etc-all" "$tmp/unitlib" "$tmp/units-all.out" \
    || { cat "$tmp/units-all.out" >&2; fail "units mode with the host's four roles (one with units = []) exited non-zero — an empty role must install nothing, not kill the converge"; }
# Those four roles plus [always] name the whole TIMERS roster, so every
# stem must land — the empty role adds nothing and removes nothing.
for stem in $timer_stems; do
    [ -f "$tmp/etc-all/$stem.service" ] || fail "$stem was not installed under boss-gcp's own four roles (one of them empty)"
done

sum_roles="$tmp/summary-roles.json"
BOSS_NODE_ROLES=legacy-stack UNITS_SUMMARY="$sum_roles" \
    units_run "$tmp/systemctl-roles.log" "$tmp/etc-roles" "$tmp/unitlib" "$tmp/units-roles.out" \
    || { cat "$tmp/units-roles.out" >&2; fail "units mode with BOSS_NODE_ROLES=legacy-stack exited non-zero"; }
touch "$tmp/systemctl-roles.log"
in_role=$(awk '$0=="[always]"||$0=="[roles.legacy-stack]"{on=1;next} /^\[/{on=0} on&&/^units/' "$roles_toml" | grep -oE '"[^"]+"' | tr -d '"')
for stem in $timer_stems; do
    if grep -qxF "$stem" <<<"$in_role"; then
        [ -f "$tmp/etc-roles/$stem.service" ] || fail "$stem is in the legacy-stack/always roster and was NOT installed under BOSS_NODE_ROLES=legacy-stack"
    else
        [ -f "$tmp/etc-roles/$stem.service" ] && fail "$stem is outside the legacy-stack roster and was installed anyway under BOSS_NODE_ROLES=legacy-stack"
        grep -q "NOT IN ROLE $stem" "$tmp/units-roles.out" \
            || fail "$stem is outside the roster and the units mode did not REPORT it by name (NOT IN ROLE $stem)"
        grep -q "enable --now $stem.timer" "$tmp/systemctl-roles.log" \
            && fail "$stem is outside the roster and was still ENABLED"
    fi
done
not_in_role=$(grep -c 'NOT IN ROLE ' "$tmp/units-roles.out" || true)
[ "$not_in_role" -ge 1 ] || fail "BOSS_NODE_ROLES=legacy-stack reported nothing as NOT IN ROLE — the observer and the ML batch are outside it"
[ "$(jq -r '.units_not_in_role // ""' "$sum_roles")" = "$not_in_role" ] \
    || fail "the summary says units_not_in_role='$(jq -r '.units_not_in_role // ""' "$sum_roles")'; the run reported $not_in_role — the packet must carry the count a reader with no host access needs"
[ "$(jq -r '.node_roles // ""' "$sum_roles")" = "legacy-stack" ] \
    || fail "the summary does not record which roles the roster was derived from"
grep -q 'NOT IN ROLE boss-estate-observe-host' "$sum_roles" \
    || fail "the summary's anomalies do not name the observer as NOT IN ROLE — a report that reaches only the journal is a report a reader without host access never sees"

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
# 10. THE PACKET SAYS WHAT IT INSTALLED.
#
# WHY, measured 2026-09-11. Car e09e30bd landed the ops-runner block
# above at 15:40 UTC; the converges at 15:52 and 16:22 both closed their
# packet `result=ok` and that is ALL either packet held. Asking the only
# question that mattered next — did it actually install? — took filing
# an ops-request, reading 200 journal lines off the host and parsing
# them by hand, and in the meantime the estate unit observer (whose
# roster is the TIMERS array, which this unit deliberately is not in)
# was read as saying the unit was absent. It was not absent. A reader
# with no host access could not tell "installed 14 pairs" from
# "installed 12 and skipped 2", so the wrong conclusion was available
# and got drawn.
#
# A single boolean IS the defect (CLAUDE.md §Diagnosis: a verdict must
# name what failed; a record reduced before it is stored throws away the
# only copy). So the run leaves a structured summary — counts, each
# sub-installer's own verdict, and every anomaly VERBATIM — which
# boss-step.sh merges onto the `run` step. The full log stays in the
# journal; the packet carries the counted shape and the anomalies.
#
# A SKIP IS LOUD, NOT FATAL: a TIMERS row whose unit files this commit
# does not carry is a legitimate state (observe-units.sh derives its
# roster by skipping exactly those), and failing the converge over one
# would stop the other thirteen pairs converging. It must be IN THE
# RECORD, with the name, which is what this asserts.
# ---------------------------------------------------------------------
sum="$tmp/summary.json"
mkdir -p "$tmp/etc-sum"
UNITS_SUMMARY="$sum" units_run "$tmp/systemctl-sum.log" "$tmp/etc-sum" "$tmp/unitlib" "$tmp/units-sum.out" \
    || { echo "FAIL: the units mode failed with a summary file configured:" >&2
         cat "$tmp/units-sum.out" >&2; exit 1; }
sum_out="$tmp/units-sum.out"
sum_fail() { echo "FAIL: $*" >&2; echo "--- summary ($sum):" >&2; cat "$sum" 2>/dev/null >&2
             echo "--- units-mode output:" >&2; cat "$sum_out" >&2; exit 1; }
[ -f "$sum" ] || sum_fail "the units mode wrote no run summary to BOSS_RUN_SUMMARY_FILE.
    Without it the converge's packet carries result=ok and nothing else, and 'did it
    install?' is a question only a human on the host can answer (2026-09-11)."
jq -e . "$sum" >/dev/null 2>&1 || sum_fail "the run summary is not valid JSON"
got=$(jq -r '.units_installed // ""' "$sum")
[ "$got" = "$installed" ] \
    || sum_fail "the summary says units_installed='$got'; this lint installed $installed pairs.
    A count that is the ROSTER LENGTH rather than what was installed is the false claim
    this check exists for."
[ "$(jq -r '.units_skipped // ""' "$sum")" = "0" ] \
    || sum_fail "nothing was skipped, but the summary does not say units_skipped=0"
[ "$(jq -r '.ops_runner // ""' "$sum")" = "installed" ] \
    || sum_fail "the summary does not record that the ops-request runner installed —
    the exact fact that took a journal read to establish on 2026-09-11"
[ "$(jq -r '.journal_door // ""' "$sum")" = "active" ] \
    || sum_fail "the summary does not record the journal read door's verdict"
[ -n "$(jq -r '.summary // ""' "$sum")" ] \
    || sum_fail "the summary carries no one-line summary for a reader of the packet"
[ "$(jq -r '.anomalies // "none"' "$sum")" = "none" ] \
    || sum_fail "a clean run reported anomalies: $(jq -r .anomalies "$sum")"

# 10b. A SKIPPED PAIR IS COUNTED AND NAMED. The unit files are read from
# BOSS_REPO_ROOT, so a copy of the tree missing one pair drives the real
# SKIP branch of install_timer_units.
skip_row=$(printf '%s\n' "$rows" | tail -n 1)
skip_stem="${skip_row%%:*}"; skip_sub="${skip_row##*:}"
partial="$tmp/partial"
mkdir -p "$partial"
cp -r "$repo/infra" "$partial/infra"
[ "$skip_sub" = "." ] && skip_dir="$partial/infra" || skip_dir="$partial/infra/$skip_sub"
rm -f "$skip_dir/$skip_stem.service" "$skip_dir/$skip_stem.timer"
sum_skip="$tmp/summary-skip.json"
mkdir -p "$tmp/etc-skip"
if ! UNITS_SUMMARY="$sum_skip" UNITS_REPO_ROOT="$partial" \
    units_run "$tmp/systemctl-skip.log" "$tmp/etc-skip" "$tmp/unitlib" "$tmp/units-skip.out"; then
    echo "FAIL: the units mode FAILED because one TIMERS row's files were absent:" >&2
    cat "$tmp/units-skip.out" >&2
    echo "    A skip must be loud in the record, not fatal — failing here would stop the" >&2
    echo "    other $((installed - 1)) pairs converging over a row this commit does not carry." >&2
    exit 1
fi
sum="$sum_skip"; sum_out="$tmp/units-skip.out"
[ "$(jq -r '.units_skipped // ""' "$sum_skip")" = "1" ] \
    || sum_fail "one pair was missing and the summary does not say units_skipped=1"
[ "$(jq -r '.units_installed // ""' "$sum_skip")" = "$((installed - 1))" ] \
    || sum_fail "units_installed did not drop by one when a pair was skipped"
jq -r '.anomalies // ""' "$sum_skip" | grep -q "$skip_stem" \
    || sum_fail "the skipped unit ($skip_stem) is not NAMED in the summary's anomalies.
    'something was skipped' sends the reader back to the host, which is the cost this
    whole check removes."

# 10c. THE DOOR'S VERDICT, when the distro package behind it is absent.
# It still may not fail the converge (check 8) — but a reader must be
# able to tell an active door from a down one WITHOUT the host.
sum_door="$tmp/summary-door.json"
mkdir -p "$tmp/etc-door"
UNITS_SUMMARY="$sum_door" units_run "$tmp/systemctl-door.log" "$tmp/etc-door" "$tmp/empty-unitlib" "$tmp/units-door.out" \
    || { echo "FAIL: the units mode aborted with the gateway package absent:" >&2
         cat "$tmp/units-door.out" >&2; exit 1; }
sum="$sum_door"; sum_out="$tmp/units-door.out"
case "$(jq -r '.journal_door // ""' "$sum_door")" in
down*) ;;
*) sum_fail "the gateway unit was absent and the summary does not say the door is down" ;;
esac

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
echo "stub installer: args=$* roles=${BOSS_NODE_ROLES-unset}" >>"$STUB_CALLS"
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

# The registry the converge reads its roles from is a FILE here — the
# harness never touches the network — shaped like /api/estate/nodes.
nodes_json="$tmp/nodes.json"
cat >"$nodes_json" <<'JSON'
{"data":[{"id":"boss-gcp","role":"bastion","roles":["legacy-stack","ml-batch-host","off-cluster-observer","wireguard-bastion"]},
         {"id":"w-1","role":"talos-worker","roles":[]}]}
JSON
run_converge() { # <dir> <installer> -> output; returns the script's status
    BOSS_GCP_REPO_DIR="$1" BOSS_GCP_CONVERGE_INSTALLER="$2" \
        BOSS_NODE_ID="${CONVERGE_NODE_ID:-boss-gcp}" \
        BOSS_ESTATE_NODES_URL="${CONVERGE_NODES_URL:-file://$nodes_json}" \
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

# 6b. THE HOST'S ROLES REACH THE INSTALLER, read off the registry — and
#     a registry that does not answer leaves them empty, installs every
#     row, and says so, instead of stopping the converge.
grep -q "roles=legacy-stack,ml-batch-host,off-cluster-observer,wireguard-bastion" "$tmp/calls.log" \
    || fail "the converge did not hand boss-gcp's declared roles to the installer as BOSS_NODE_ROLES (calls: $(cat "$tmp/calls.log"))"
printf '%s' "$out" | grep -q "declares roles: legacy-stack" \
    || fail "the converge does not say which roles it read:
$out"
: >"$tmp/calls.log"
out=$(CONVERGE_NODE_ID=w-1 run_converge "$clean" "$tmp/bin/installer-ok") \
    || fail "the converge failed for a node that declares no roles:
$out"
grep -q "roles=$" "$tmp/calls.log" \
    || fail "a node with no declared roles must reach the installer with BOSS_NODE_ROLES empty, so every row installs (calls: $(cat "$tmp/calls.log"))"
: >"$tmp/calls.log"
out=$(CONVERGE_NODES_URL="file://$tmp/no-such-registry.json" run_converge "$clean" "$tmp/bin/installer-ok") \
    || fail "an unreachable registry STOPPED the converge — an arm that needs the patient is not an arm:
$out"
grep -q "roles=$" "$tmp/calls.log" \
    || fail "an unreachable registry must still drive the installer, with no roles (calls: $(cat "$tmp/calls.log"))"
printf '%s' "$out" | grep -q "did not answer" \
    || fail "the converge does not say the registry did not answer:
$out"

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

# ---------------------------------------------------------------------
# 10d. THE CONVERGE'S OWN FACTS RIDE THE SAME SUMMARY — and a summary
#      from a PREVIOUS run must never be mistaken for this one's.
#
# The summary is read by boss-step.sh in ExecStopPost, a separate
# process; the only thing linking the two is the file. So a run that
# dies before writing must leave NOTHING, or the packet reports the last
# run's success over this run's failure — "a wrong target answers
# instead of erroring", one layer in. The converge therefore clears the
# file before it does anything at all, INCLUDING before the refusals.
# ---------------------------------------------------------------------
sum_conv="$tmp/summary-converge.json"
run_converge_sum() { # <dir> <installer>
    BOSS_GCP_REPO_DIR="$1" BOSS_GCP_CONVERGE_INSTALLER="$2" \
        BOSS_RUN_SUMMARY_FILE="$sum_conv" \
        STUB_CALLS="$tmp/calls.log" bash "$converge" 2>&1
}
conv_fail() { echo "FAIL: $*" >&2; echo "--- summary ($sum_conv):" >&2
              cat "$sum_conv" 2>/dev/null >&2; exit 1; }

echo '{"converge_sha":"STALE-FROM-A-PREVIOUS-RUN"}' >"$sum_conv"
if out=$(run_converge_sum "$dirty" "$tmp/bin/installer-ok"); then
    fail "the converge ran against a dirty checkout: $out"
fi
if [ -f "$sum_conv" ] && grep -q STALE-FROM-A-PREVIOUS-RUN "$sum_conv"; then
    conv_fail "the converge REFUSED and left the previous run's summary in place. Its
    ExecStopPost would then stamp that run's facts onto this run's packet — the refusal
    would read as a successful converge."
fi

# The clean path records which remote, which sha, and what it moved from.
: >"$tmp/calls.log"
rm -f "$sum_conv"
out=$(run_converge_sum "$clean" "$tmp/bin/installer-ok") \
    || fail "the converge failed on the clean checkout with a summary file set:
$out"
[ -f "$sum_conv" ] || conv_fail "the converge left no summary for its packet"
[ "$(jq -r '.converge_sha // ""' "$sum_conv")" = "$want" ] \
    || conv_fail "the summary does not carry the sha converged on ($want)"
[ "$(jq -r '.converge_remote // ""' "$sum_conv")" = "forge" ] \
    || conv_fail "the summary does not name the remote it converged from"

# 10e. A FAILED INSTALL SAYS SO ON THE PACKET, not only in the journal
# the host may be unreadable from. Check 7 proves the journal keeps every
# line; this proves the packet keeps the verdict.
: >"$tmp/calls.log"
rm -f "$sum_conv"
if out=$(run_converge_sum "$clean" "$tmp/bin/installer-bad"); then
    fail "the converge reported success over an installer that exited 3: $out"
fi
[ -f "$sum_conv" ] || conv_fail "a FAILED converge left no summary at all — the packet gets
    systemd's exit code and nothing about where it died"
[ "$(jq -r '.installer_exit // ""' "$sum_conv")" = "3" ] \
    || conv_fail "the summary does not carry the installer's exit status"
[ "$(jq -r '.converge_sha // ""' "$sum_conv")" = "$want" ] \
    || conv_fail "a failed converge lost the sha it was installing from — record each fact
    as soon as it is known, not at the end of a run that may not get there"

echo "boss-gcp-converges-itself: ok — $installed timer pairs install from \`deploy-services.sh units\` (the converge's own among them, nothing restarted), the journal read door on :19531 comes up with it and cannot abort it, the ops-request runner lands with HOST_ID=boss-gcp, this checkout and the CLUSTER as its system of record (never the legacy 127.0.0.1 stack) without being a TIMERS row, the loop converges from the forge and refuses the mirror, refuses a dirty tree, fast-forwards and drives the installer idempotently, prints every line of a failed install, and leaves its packet a counted summary — units installed/skipped with every skip named, the ops runner's and the read door's own verdicts, the sha and remote it converged from, the installer's exit on failure, and nothing at all from a previous run"
exit 0
