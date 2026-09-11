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
# DERIVED from infra/deploy-services.sh's TIMERS list, the one place
# that knows what is installed, so the watch list cannot drift from the
# install list. A hand-written watch list holds no information its
# source does not.
#
# WHAT IT CHECKS
#   1. the observer answers `--roster` without touching systemd or the API
#   2. every installable TIMERS row contributes BOTH halves of its pair —
#      the .timer (is it installed and armed) and the .service (did it
#      run, and what did it exit with) — minus a named exclusion set
#   3. boss-ml-inference-batch.timer, the unit the measured failure was
#      about, is in the roster
#   4. the exclusion set is small and every entry is justified in the file
#   5. the unit file carries NO second copy of the list (the collapse)
#   6. an unreadable TIMERS source REFUSES (EX_CONFIG) rather than
#      falling back to a shorter list — a smaller roster would answer a
#      smaller question, confidently
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
observer="$repo/infra/estate/observe-units.sh"
deploy="$repo/infra/deploy-services.sh"
unit="$repo/infra/estate/boss-estate-observe-units.service"

fail() { echo "FAIL: $*" >&2; exit 1; }

[ -x "$observer" ] || fail "$observer is missing or not executable"
[ -f "$deploy" ] || fail "$deploy is missing — it is where the roster comes from"

# 1. --roster answers, with no HOST_ID, no JOBS_API, and no systemd.
roster="$(env -u UNITS -u HOST_ID -u JOBS_API bash "$observer" --roster 2>&1)" \
    || fail "observe-units.sh --roster exited non-zero. It must answer the one question
    worth asking a host without changing it — no systemctl, no POST, no required env:
$roster"
[ -n "$roster" ] || fail "observe-units.sh --roster printed nothing"

# 2. The roster IS the installable TIMERS rows, both halves, minus the
#    exclusions. Scraped independently here, the same way
#    infra/lint/boss-gcp-converges-itself.sh scrapes it.
rows=$(sed -n '/^TIMERS=(/,/^)/p' "$deploy" | grep -oE '"[a-z0-9-]+:[^"]+"' | tr -d '"')
[ -n "$rows" ] || fail "no TIMERS rows scraped from $deploy — the scrape broke, so a green
    result here would mean nothing"

excludes=$(sed -n 's/^ROSTER_EXCLUDE="\(.*\)"$/\1/p' "$observer")
[ -n "$excludes" ] || fail "observe-units.sh declares no ROSTER_EXCLUDE line. Even an empty
    exclusion set must be stated, so the next reader knows the roster is the whole list."

want=0
for row in $rows; do
    stem="${row%%:*}"; sub="${row##*:}"
    [ "$sub" = "." ] && src="$repo/infra" || src="$repo/infra/$sub"
    # A row whose source files are absent is SKIPped by the installer, so
    # it is not installed and watching it would report not-found forever.
    [ -f "$src/$stem.service" ] && [ -f "$src/$stem.timer" ] || continue
    for ext in timer service; do
        case " $excludes " in *" $stem.$ext "*) continue ;; esac
        printf '%s\n' "$roster" | grep -qx "$stem.$ext" \
            || fail "$stem.$ext installs on this host and the observer does not watch it.
    The roster must be DERIVED from deploy-services.sh's TIMERS (CLAUDE.md §9a), so a
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
    || fail "the roster carries $got units but $want install from TIMERS. An EXTRA unit is a
    hand-written entry that has outlived its source; a missing one is the drift this check
    exists to stop. Roster was:
$roster"

# 3. The unit the measured failure was about.
printf '%s\n' "$roster" | grep -qx 'boss-ml-inference-batch.timer' \
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
    deploy-services.sh's TIMERS; a host that runs a DIFFERENT set still overrides with a
    drop-in (the forge does), but boss-gcp's roster must be derived."
fi

# 6. AN UNREADABLE SOURCE REFUSES. A roster that silently fell back to a
#    short hand-written list would answer a smaller question and look
#    exactly as confident — the failure this whole car is about.
out=$(env -u UNITS OBSERVE_UNITS_DEPLOY="$repo/infra/does-not-exist.sh" \
    bash "$observer" --roster 2>&1)
rc=$?
[ "$rc" -ne 0 ] \
    || fail "the observer derived a roster from a TIMERS source that does not exist (exit $rc):
$out"
[ "$rc" -eq 78 ] \
    || fail "an unreadable TIMERS source exited $rc; EX_CONFIG (78) is what this script
    already uses for 'nothing to watch is a config fault':
$out"
printf '%s' "$out" | grep -q "does-not-exist.sh" \
    || fail "the refusal does not name the file it could not read:
$out"

echo "the-host-observer-watches-what-is-installed: ok — the host-units roster is derived from deploy-services.sh's TIMERS ($got units, both halves of $((got / 2)) pairs, $n_excl justified exclusions), boss-ml-inference-batch.timer among them, the unit file holds no second copy, and an unreadable source refuses with EX_CONFIG instead of answering a smaller question"
exit 0
