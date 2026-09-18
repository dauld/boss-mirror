#!/usr/bin/env bash
# the-host-observer-watches-what-is-installed.sh — the host-units
# observer's watch roster must BE the set of timer units the installer
# installs on that host, derived, not a list somebody typed twice.
#
# WHY THIS EXISTS. On 2026-09-10 the question "is
# boss-ml-inference-batch.timer installed on boss-gcp?" had no read path
# (backlog 68757702). The observer answered — `GET
# /api/estate/observations?scope=host-units` returned a clean, confident
# observation — and it watched TWO units, so every other unit on that
# host was outside what it watched BY CONSTRUCTION and its absence from
# the answer said nothing at all. Reasoning from the tree instead
# produced a WRONG conclusion ("the unit has never run") against a
# reading taken over ssh that showed it firing and FAILING nightly since
# 2026-08-17, 23 consecutive status=22. A correction had to be written
# onto the alarm it was filed against.
#
# That is CLAUDE.md §Doors' rule in its purest form — "a wrong target
# answers instead of erroring" — and the repair is §9a's: the roster is
# DERIVED from the installer's rows (infra/gcp/install-units.sh, whose
# rows are infra/estate/roles.toml), the one place that knows what is
# installed, so the watch list cannot drift from the install list. A
# hand-written watch list holds no information its source does not.
# (Until 2026-09-18 the rows were the TIMERS array of the bare-metal
# deploy script, scraped as shell; that script is deleted, e109bd71.)
#
# WHAT IT CHECKS
#   1. the observer answers `--roster` without touching systemd or the API
#   2. every installable roles.toml row contributes BOTH halves of its pair —
#      the .timer (is it installed and armed) and the .service (did it
#      run, and what did it exit with) — minus a named exclusion set
#   3. boss-ml-inference-batch.timer, the unit the measured failure was
#      about, is in the roster
#   4. the exclusion set is small and every entry is justified in the file
#   5. the unit file carries NO second copy of the list (the collapse)
#   6. an unreadable installer REFUSES (EX_CONFIG) rather than
#      falling back to a shorter list — a smaller roster would answer a
#      smaller question, confidently
#   7. under a host's ROLES the roster is exactly the installer's
#      `in-role` rows, both halves — a row the host's roles do not name
#      is not watched. The measured cost of the roster ignoring roles
#      (2026-09-15): the legacy-stack role left boss-gcp, its ten chores
#      were uninstalled, and the observer filed twelve urgent
#      `unit_unhealthy` alarms for units read `not-found` every five
#      minutes — a broken watch list reported as a broken host
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$here/lib/scanned.sh"
observer="$repo/infra/estate/observe-units.sh"
installer="$repo/infra/gcp/install-units.sh"
unit="$repo/infra/estate/boss-estate-observe-units.service"

fail() { echo "FAIL: $*" >&2; exit 1; }

[ -x "$observer" ] || fail "$observer is missing or not executable"
[ -f "$installer" ] || fail "$installer is missing — it is where the roster comes from"

# 1. --roster answers, with no HOST_ID, no JOBS_API, and no systemd.
roster="$(env -u UNITS -u HOST_ID -u JOBS_API bash "$observer" --roster 2>&1)" \
    || fail "observe-units.sh --roster exited non-zero. It must answer the one question
    worth asking a host without changing it — no systemctl, no POST, no required env:
$roster"
[ -n "$roster" ] || fail "observe-units.sh --roster printed nothing"

# 2. The roster IS the installable roles.toml rows, both halves, minus
#    the exclusions. Read independently here off the installer's `rows`
#    mode, the same way infra/lint/boss-gcp-converges-itself.sh reads it.
rows=$(BOSS_REPO_ROOT="$repo" bash "$installer" rows 2>/dev/null | grep -E '^[a-z0-9-]+:[^:]+$')
[ -n "$rows" ] || fail "no rows came out of $installer rows — the mode broke, so a green
    result here would mean nothing"

excludes=$(sed -n 's/^ROSTER_EXCLUDE="\(.*\)"$/\1/p' "$observer")
[ -n "$excludes" ] || fail "observe-units.sh declares no ROSTER_EXCLUDE line. Even an empty
    exclusion set must be stated, so the next reader knows the roster is the whole list."

want=0
for row in $rows; do
    stem="${row%%:*}"; sub="${row##*:}"
    # A row whose source files are absent is SKIPped by the installer, so
    # it is not installed and watching it would report not-found forever.
    [ "$sub" = "missing" ] && continue
    [ "$sub" = "." ] && src="$repo/infra" || src="$repo/infra/$sub"
    [ -f "$src/$stem.service" ] && [ -f "$src/$stem.timer" ] || continue
    for ext in timer service; do
        case " $excludes " in *" $stem.$ext "*) continue ;; esac
        grep -qx "$stem.$ext" <<< "$roster" \
            || fail "$stem.$ext installs on this host and the observer does not watch it.
    The roster must be DERIVED from the installer's rows (CLAUDE.md §9a), so a
    timer that lands cannot be outside what the observer watches. Roster was:
$roster"
        want=$((want + 1))
    done
done
[ "$want" -ge 20 ] \
    || fail "only $want units were expected of the roster — the scrape or the derivation
    broke, and a green result here would mean nothing"

got=$(printf '%s\n' "$roster" | sed '/^$/d' | wc -l | tr -d ' ')
[ "$got" -eq "$want" ] \
    || fail "the roster carries $got units but $want install from roles.toml. An EXTRA unit is a
    hand-written entry that has outlived its source; a missing one is the drift this check
    exists to stop. Roster was:
$roster"

# 3. The unit the measured failure was about.
grep -qx 'boss-ml-inference-batch.timer' <<< "$roster" \
    || fail "boss-ml-inference-batch.timer is not in the roster. That is the exact unit whose
    installed-ness could not be established on 2026-09-10, and the wrong answer reasoned
    from the tree instead cost a correction on a live alarm (backlog 68757702)."

# 4. EVERY EXCLUSION IS JUSTIFIED IN THE FILE. The gate's own roster is
#    "the directory minus a four-entry exclusion set" with each reason
#    written down once (gate.sh PREFLIGHT_EXCLUDES); an unexplained
#    exclusion is how a unit stops being watched without anybody
#    deciding so.
n_excl=0
for ex in $excludes; do
    n_excl=$((n_excl + 1))
    grep -q "^#.*$ex" "$observer" \
        || fail "$ex is excluded from the roster with no reason written beside it in
    $observer. Write the why in a comment line naming the unit, the way gate.sh writes
    down what its pre-flight does not run."
done
[ "$n_excl" -le 3 ] \
    || fail "$n_excl units are excluded from the derived roster. An exclusion set that grows
    is a hand-written roster wearing a disguise — if this many units cannot be watched, the
    health rule is what needs changing, not the list."

# 5. THE COLLAPSE: no second copy of the list. Until this car the unit
#    file carried `Environment="UNITS=boss-estate-observe-host.timer
#    boss-gcp-converge.timer"` — a roster that could not learn about a
#    timer the installer installs.
if grep -qE '^Environment="?UNITS=' "$unit"; then
    fail "$unit still hardcodes a UNITS list:
$(grep -nE '^Environment="?UNITS=' "$unit")
    That is the second copy this check exists to delete. The roster comes from
    the installer's rows; a host that runs a DIFFERENT set still overrides with a
    drop-in (the forge does), but boss-gcp's roster must be derived."
fi

# 6. AN UNREADABLE SOURCE REFUSES. A roster that silently fell back to a
#    short hand-written list would answer a smaller question and look
#    exactly as confident — the failure this whole car is about.
out=$(env -u UNITS OBSERVE_UNITS_INSTALLER="$repo/infra/does-not-exist.sh" \
    bash "$observer" --roster 2>&1)
rc=$?
[ "$rc" -ne 0 ] \
    || fail "the observer derived a roster from an installer that does not exist (exit $rc):
$out"
[ "$rc" -eq 78 ] \
    || fail "an unreadable installer exited $rc; EX_CONFIG (78) is what this script
    already uses for 'nothing to watch is a config fault':
$out"
grep -q "does-not-exist.sh" <<<"$out" \
    || fail "the refusal does not name the file it could not read:
$out"

# 7. UNDER ROLES, THE ROSTER IS THE INSTALLER'S. boss-gcp's declared
#    roles as of 2026-09-16 (ml-batch-host, off-cluster-observer,
#    wireguard-bastion; the legacy-stack role gone the day before): the
#    installer's `roster` mode says which rows are in role, and the
#    observer must watch those and only those.
roles="ml-batch-host,off-cluster-observer,wireguard-bastion"
in_role=$(BOSS_REPO_ROOT="$repo" BOSS_NODE_ROLES="$roles" bash "$installer" roster 2>/dev/null \
    | sed -n 's/^in-role //p')
not_in_role=$(BOSS_REPO_ROOT="$repo" BOSS_NODE_ROLES="$roles" bash "$installer" roster 2>/dev/null \
    | sed -n 's/^not-in-role //p')
[ -n "$in_role" ] && [ -n "$not_in_role" ] \
    || fail "install-units.sh roster under roles '$roles' named no in-role or no not-in-role
    row — the derivation this check compares against broke, so a green here would mean nothing"
role_roster="$(env -u UNITS -u HOST_ID -u JOBS_API BOSS_NODE_ROLES="$roles" bash "$observer" --roster 2>&1)" \
    || fail "observe-units.sh --roster under BOSS_NODE_ROLES='$roles' exited non-zero:
$role_roster"
for stem in $not_in_role; do
    for ext in timer service; do
        ! grep -qx "$stem.$ext" <<< "$role_roster" \
            || fail "$stem.$ext is NOT IN ROLE for '$roles' and the observer watches it anyway.
    The installer does not install it there and the uninstall verb removes it, so the
    reading is not-found forever: a broken watch list filed as a broken host — twelve
    urgent unit_unhealthy alarms on 2026-09-15. Roster under roles was:
$role_roster"
    done
done
role_want=0
for stem in $in_role; do
    src="$repo/infra"
    row=$(grep -m1 "^$stem:" <<<"$rows") || continue
    sub="${row##*:}"
    [ "$sub" = "missing" ] && continue
    [ "$sub" = "." ] || src="$repo/infra/$sub"
    [ -f "$src/$stem.service" ] && [ -f "$src/$stem.timer" ] || continue
    for ext in timer service; do
        case " $excludes " in *" $stem.$ext "*) continue ;; esac
        grep -qx "$stem.$ext" <<< "$role_roster" \
            || fail "$stem.$ext is IN ROLE for '$roles' and the observer does not watch it:
$role_roster"
        role_want=$((role_want + 1))
    done
done
role_got=$(printf '%s\n' "$role_roster" | sed '/^$/d' | wc -l | tr -d ' ')
[ "$role_got" -eq "$role_want" ] \
    || fail "under roles '$roles' the roster carries $role_got units but $role_want are in role:
$role_roster"
[ "$role_got" -lt "$got" ] \
    || fail "the roster under roles '$roles' ($role_got) is not smaller than the every-row roster
    ($got) — the roles did not narrow it, which is the 2026-09-15 defect"
# The converge's dark-registry sentinel maps to [always] only — the
# observer narrows the same way, never widening to every row.
sentinel_roster="$(env -u UNITS -u HOST_ID -u JOBS_API BOSS_NODE_ROLES=registry-unread bash "$observer" --roster 2>&1)" \
    || fail "--roster under the registry-unread sentinel exited non-zero:
$sentinel_roster"
! grep -qx 'boss-ml-inference-batch.timer' <<< "$sentinel_roster" \
    || fail "under the registry-unread sentinel the roster still carries a role's unit;
    a dark registry must narrow the watch to [always], never widen it:
$sentinel_roster"

lint_scanned the-host-observer-watches-what-is-installed "$got" "unit(s) derived from the installer's roles.toml rows"
echo "the-host-observer-watches-what-is-installed: ok — the host-units roster is derived from the installer's roles.toml rows ($got units, both halves of $((got / 2)) pairs, $n_excl justified exclusions), boss-ml-inference-batch.timer among them, the unit file holds no second copy, an unreadable source refuses with EX_CONFIG instead of answering a smaller question, and under boss-gcp's roles the roster is the installer's in-role set ($role_got units)"
exit 0
