#!/usr/bin/env bash
# install-units.sh — install a managed host's systemd timer unit pairs
# from the checkout, by the roles the host declares. Unit FILES only:
# no build, no staging, no schema, no restart.
#
#   sudo ./infra/gcp/install-units.sh units
#       install + enable the pairs this host's roles name, report every
#       other row NOT IN ROLE by name, bring up the journal read door and
#       the ops-request runner. This is what infra/gcp/boss-gcp-converge.sh
#       runs every half hour, so a unit that lands on main reaches the
#       host without a human (408c81f6).
#   ./infra/gcp/install-units.sh roster
#       one line per row, `in-role <stem>` or `not-in-role <stem>` under
#       BOSS_NODE_ROLES, by the same derivation `units` installs by. Reads
#       only; the uninstall verb (infra/gcp/uninstall-not-in-role.sh) takes
#       its set from here and never computes its own.
#   ./infra/gcp/install-units.sh rows
#       every row as `<stem>:<dir>` — the directory under infra/ that
#       carries `<stem>.service` + `<stem>.timer` (`.` for infra/ itself,
#       `missing` when this checkout carries no pair). This is what the
#       lints and the estate unit observer read instead of scraping a
#       shell array.
#
# WHY THIS FILE EXISTS (2026-09-18, backlog e109bd71, design 42277636).
# These two modes were the ~310 live lines of infra/deploy-services.sh, a
# 2,081-line bare-metal deploy whose other 1,770 lines had no caller
# since the 2026-09-04 conductor cutover and the 2026-09-15 retirement of
# the second stack, and had drifted three ways from the container path
# (audit H7). The bare-metal path is deleted; the container launcher is
# the one way to run BOSS; this is the one thing a managed HOST still
# installs, moved beside the loop that runs it.
#
# THE ROSTER IS infra/estate/roles.toml, AND NOTHING ELSE. deploy-services
# carried a TIMERS array of `stem:dir` rows and roles.toml named every
# stem again under a role, pinned equal by a lint — a holding action
# (CLAUDE.md §9a). Now roles.toml is the one list: the set of rows is
# every stem it names, and where a stem's unit files live is found in
# the tree (`find infra -name <stem>.timer`), which is not a second list.
# Adding a timer = author the .service + .timer under infra/ and name the
# stem under the role that needs it.
set -euo pipefail

usage() {
    echo "usage: $0 units|roster|rows" >&2
    exit 2
}
MODE="${1:-}"
case "$MODE" in
    units|roster|rows) ;;
    *) usage ;;
esac

# The checkout this installs FROM. Overridable so the installer can be
# exercised against a scratch tree — infra/lint/boss-gcp-converges-
# itself.sh runs the `units` mode on every gate and asserts what it would
# put on the host. On boss-gcp it is the default.
REPO_ROOT="${BOSS_REPO_ROOT:-/opt/boss}"
ROLES_TOML="${BOSS_ROLES_TOML:-$REPO_ROOT/infra/estate/roles.toml}"
# What this run leaves for its own packet. A no-op unless the caller set
# BOSS_RUN_SUMMARY_FILE — boss-gcp-converge.service does, so its packet
# says what the `units` mode installed instead of only `result=ok`.
# shellcheck source=infra/run-summary.sh
. "$(dirname "$0")/../run-summary.sh"

# Where units land and who reloads them is overridable ONLY so the
# installer can be exercised into a scratch directory with a stub
# systemctl (the lint above, and infra/lint/forge-install-covers-the-
# ops-runner.sh for the forge installer). On the host both are the
# defaults.
TIMER_ETC="${INSTALL_ETC:-/etc/systemd/system}"
TIMER_SYSTEMCTL="${INSTALL_SYSTEMCTL:-systemctl}"

[ -f "$ROLES_TOML" ] || {
    echo "install-units: $ROLES_TOML is missing — the roster is that file, and" >&2
    echo "    without it there is nothing to derive. REFUSING rather than answering a" >&2
    echo "    smaller question." >&2
    exit 78   # EX_CONFIG
}

# The stems roles.toml names under one section header ("always" or
# "roles.<name>"), one per line — or under EVERY section when $1 is
# `*`. The file keeps each `units = [...]` on one line so this needs no
# TOML parser. awk does the extraction on its own: a role whose list is
# empty (`units = []`) yields no lines and exit 0. The earlier shape
# piped into `grep -oE '"[^"]+"'`, which exits 1 when it matches
# nothing, and under `set -o pipefail` that killed the converge of every
# host declaring such a role — boss-gcp, three times running from
# 2026-09-12 18:55Z, one header line and no more (estate alarm a1b4f3fa).
role_units() { # <section>|*
    awk -v want="$1" '
        /^\[/ { on = ($0 == "[" want "]" || want == "*"); next }
        on && /^units[[:space:]]*=/ {
            while (match($0, /"[^"]+"/)) {
                print substr($0, RSTART + 1, RLENGTH - 2)
                $0 = substr($0, RSTART + RLENGTH)
            }
        }
    ' "$ROLES_TOML"
}

# Every row, in file order — THE roster.
ALL_STEMS="$(role_units '*')"
if [ -z "$ALL_STEMS" ]; then
    echo "install-units: $ROLES_TOML names no units at all — the roster is empty, which" >&2
    echo "    is a config fault, not a host with nothing to run." >&2
    exit 78
fi
dup="$(printf '%s\n' "$ALL_STEMS" | sort | uniq -d)"
if [ -n "$dup" ]; then
    echo "install-units: $ROLES_TOML names a unit under two sections: $dup — one role" >&2
    echo "    owns each row, or 'not in role' has two answers." >&2
    exit 78
fi

# Where one stem's unit pair lives: the directory under $REPO_ROOT/infra
# carrying both files, printed relative to infra/ (`.` for infra/
# itself); `missing` when the checkout carries no pair. A stem carried
# in two places is refused — two files with one name is a fact that
# lives twice, and a converge that picked one would be guessing.
unit_dir() { # <stem>
    local hits
    hits="$(find "$REPO_ROOT/infra" -name "$1.timer" -printf '%h\n' 2>/dev/null | sort)"
    case "$(printf '%s\n' "$hits" | grep -c .)" in
        0) echo missing; return 0 ;;
        1) ;;
        *) echo "install-units: $1.timer is carried in more than one place under infra/:" >&2
           printf '    %s\n' $hits >&2
           exit 78 ;;
    esac
    [ -f "$hits/$1.service" ] || { echo missing; return 0; }
    hits="${hits#"$REPO_ROOT/infra"}"
    hits="${hits#/}"
    echo "${hits:-.}"
}

# The stems this host installs, or nothing when no roles are declared
# (the caller reads "nothing" as "every row").
roster_for_roles() { # <comma-separated roles>
    local roles="$1" role
    [[ -z "$roles" ]] && return 0
    role_units always
    IFS=, read -ra _roles <<<"$roles"
    for role in "${_roles[@]}"; do
        role_units "roles.${role}"
    done
}

# Is one stem in the roster? ONE predicate for the three readers —
# install, enable, and the `roster` mode the uninstall verb reads — so
# what a host installs and what it may remove cannot be two different
# questions. An empty roster (no roles declared) is "every row", as
# before roles existed.
stem_in_role() { # <stem> <roster>
    [[ -z "$2" ]] && return 0
    grep -qxF "$1" <<<"$2"
}

case "$MODE" in
    rows)
        for stem in $ALL_STEMS; do
            echo "$stem:$(unit_dir "$stem")"
        done
        exit 0
        ;;
    roster)
        # THE ROSTER AS A LIST, ONE LINE PER ROW: `in-role <stem>` or
        # `not-in-role <stem>` under BOSS_NODE_ROLES — the same
        # roster_for_roles / stem_in_role the `units` mode installs and
        # enables by. This is the ONE derivation of "what this host is
        # for"; infra/gcp/uninstall-not-in-role.sh (d5941ef3 car 4)
        # takes the set it may remove from these lines, so the installed
        # set and the removable set are complements by construction.
        # Reads only: no root, no systemctl, nothing written.
        roster="$(roster_for_roles "${BOSS_NODE_ROLES:-}")"
        for stem in $ALL_STEMS; do
            if stem_in_role "$stem" "$roster"; then
                echo "in-role $stem"
            else
                echo "not-in-role $stem"
            fi
        done
        exit 0
        ;;
esac

# ---------------------------------------------------------------------
# units
# ---------------------------------------------------------------------
if [[ "$TIMER_ETC" == "/etc/systemd/system" && "$(id -u)" != "0" ]]; then
    echo "error: units mode needs root to write $TIMER_ETC — re-run with sudo" >&2
    exit 1
fi
echo "==> install timer units from $REPO_ROOT (files only: no build, no schema, no restart)"

# WHAT IT INSTALLED IS COUNTED, NOT ASSUMED. The tail line used to print
# the ROSTER LENGTH, so a run that skipped two pairs still claimed it had
# installed every one of them. A count that cannot disagree with the
# roster is not a count.
TIMER_UNITS_INSTALLED=0
TIMER_UNITS_SKIPPED=0
TIMER_UNITS_NOT_IN_ROLE=0
ROSTER_LEN="$(printf '%s\n' "$ALL_STEMS" | grep -c .)"

# WHICH ROWS THIS HOST IS FOR. A host declares its roles in the estate
# registry (Classes of `node`, migration 202609120300) and roles.toml maps
# each role to the stems it needs; the converge passes the host's roles
# in BOSS_NODE_ROLES (comma-separated, read off /api/estate/nodes). With
# roles declared, only their rows and the `always` set are installed and
# every other row is REPORTED as NOT IN ROLE — reported, never
# uninstalled here: taking a unit off a host is the uninstall-not-in-role
# verb's job, which reads the SAME set off the `roster` mode above. With
# NO roles (undeclared, or the registry unreachable), every row installs
# exactly as it did before roles existed, and the run says so.
roster="$(roster_for_roles "${BOSS_NODE_ROLES:-}")"
if [[ -n "${BOSS_NODE_ROLES:-}" ]]; then
    echo "  roles: ${BOSS_NODE_ROLES} — installing only the rows they name (infra/estate/roles.toml)"
else
    echo "  roles: none declared — installing every row"
fi
for stem in $ALL_STEMS; do
    if ! stem_in_role "$stem" "$roster"; then
        echo "  NOT IN ROLE $stem — this host's roles do not name it; the uninstall-not-in-role verb removes it"
        TIMER_UNITS_NOT_IN_ROLE=$((TIMER_UNITS_NOT_IN_ROLE + 1))
        run_summary_note "NOT IN ROLE $stem"
        continue
    fi
    sub="$(unit_dir "$stem")"
    src_dir="$REPO_ROOT/infra"
    [[ "$sub" != "." ]] && src_dir="$src_dir/$sub"
    svc_src="$src_dir/${stem}.service"
    tmr_src="$src_dir/${stem}.timer"
    if [[ "$sub" == "missing" ]]; then
        # LOUD IN THE RECORD, NOT FATAL. A row whose unit files this
        # commit does not carry is a legitimate state (observe-units.sh
        # derives its watch roster by skipping exactly these), and
        # failing here would stop every other pair converging. But it
        # must reach the packet by NAME: "something was skipped" sends
        # the reader back to the host, which is the cost this records
        # away (2026-09-11).
        echo "  SKIP $stem (no ${stem}.service + ${stem}.timer pair under $REPO_ROOT/infra)"
        TIMER_UNITS_SKIPPED=$((TIMER_UNITS_SKIPPED + 1))
        run_summary_note "SKIP $stem — no ${stem}.service + ${stem}.timer pair under $REPO_ROOT/infra"
        continue
    fi
    install -m 0644 "$svc_src" "${TIMER_ETC}/${stem}.service"
    install -m 0644 "$tmr_src" "${TIMER_ETC}/${stem}.timer"
    # NO jobs-url DROP-IN. deploy-services wrote every row an
    # `Environment=BOSS_JOBS_URL=http://127.0.0.1:<jobs port>` drop-in —
    # the second, older stack this host carried. That stack was retired
    # on 2026-09-15 (ops-request 7912c9ae) and its chores uninstalled;
    # every unit still in a role pins the system of record INLINE with
    # env(1) on both Exec lines, which timers-leave-a-packet.sh check 7
    # makes the rule. A drop-in naming a port nothing listens on would
    # be a wrong target one layer down.
    echo "  installed $stem unit + timer"
    TIMER_UNITS_INSTALLED=$((TIMER_UNITS_INSTALLED + 1))
done
"$TIMER_SYSTEMCTL" daemon-reload
for stem in $ALL_STEMS; do
    # A row outside this host's roles is neither installed nor enabled
    # here, even if an earlier converge left its files.
    stem_in_role "$stem" "$roster" || continue
    if [[ -f "${TIMER_ETC}/${stem}.timer" ]]; then
        "$TIMER_SYSTEMCTL" enable --now "${stem}.timer" >/dev/null 2>&1 || true
        echo "  enabled ${stem}.timer"
    fi
done

# RECORDED THE MOMENT IT IS KNOWN, never at the end. What follows (the
# read door, the ops runner) can fail, and on this host the whole run's
# only off-host record is the packet: a summary assembled at the end is
# a summary a failure takes with it.
run_summary_field units_installed "$TIMER_UNITS_INSTALLED"
run_summary_field units_skipped "$TIMER_UNITS_SKIPPED"
run_summary_field units_not_in_role "$TIMER_UNITS_NOT_IN_ROLE"
run_summary_field node_roles "${BOSS_NODE_ROLES:-}"
run_summary_field summary \
    "installed $TIMER_UNITS_INSTALLED of $ROSTER_LEN timer unit pair(s), skipped $TIMER_UNITS_SKIPPED, not in role $TIMER_UNITS_NOT_IN_ROLE"

# THE JOURNAL READ DOOR, converged with the units (backlog 68757702:
# boss-gcp had no read path from the pod for logs OR unit state, and on
# 2026-09-10 that cost a wrong conclusion about a nightly unit). Wider
# than "unit files only", deliberately and boundedly: enabling a DISTRO
# socket unit bounces nothing of ours and stages nothing, and it cannot
# fail this mode — journal-door-ensure.sh always exits 0 and warns
# instead, because a visibility door must never stop the converge that
# keeps this host's units current.
"$REPO_ROOT/infra/journal-door-ensure.sh"

# THE THIRD LOOP: THIS HOST ANSWERS ops-request PACKETS. Converge adopts
# the units, observe reports them, ACT answers packets (c3d06016; David,
# design 9e3e093f: boss-gcp "isn't part of the kubernetes cluster... I
# still want it fully managed and maintained by BOSS and protocol"). Not
# a roles.toml row on purpose: it fires every minute, its product IS
# packets, and timers-leave-a-packet.sh rightly demands a packet pair of
# every row — so it is installed from its own ONE definition, the same
# one the forge installer uses: infra/ops/install-ops-runner.sh. Its
# failure is loud but late — a red unit and a failed packet, carried to
# the end rather than abandoning the rest of this mode.
#
# NOT A ROSTER ROW, BUT STILL BY ROLE. Since 2026-09-22 (backlog
# cb9eb0f2) that script reads BOSS_NODE_ROLES itself and installs a
# runner only where `ops-runner` is declared — the same declaration the
# rows above are selected by, so this host's third loop answers to the
# registry like its first two. The predicate is not repeated here: it
# lives in the one definition, which is why both callers still call it
# unconditionally. A host outside the role gets exit 0 and a `not in
# role` verdict on the packet, never the refusal this block exits with.
ops_runner_rc=0
INSTALL_ETC="$TIMER_ETC" INSTALL_SYSTEMCTL="$TIMER_SYSTEMCTL" \
    bash "$REPO_ROOT/infra/ops/install-ops-runner.sh" boss-gcp || ops_runner_rc=$?
# THE TAIL LINE SAYS WHAT HAPPENED, not what the roster holds.
echo "units: installed $TIMER_UNITS_INSTALLED of $ROSTER_LEN timer unit pair(s) from $REPO_ROOT ($TIMER_UNITS_SKIPPED skipped)"
if [[ "$ops_runner_rc" -ne 0 ]]; then
    echo "units: the ops-request runner did NOT install (exit $ops_runner_rc) — it named" >&2
    echo "    what failed above, and the run summary carries it. Every unit file above" >&2
    echo "    converged; this host cannot answer an ops-request until that is fixed." >&2
    exit "$ops_runner_rc"
fi
exit 0
