#!/usr/bin/env bash
# Every installed timer must leave a Job behind.
#
# WHY. boss-maintenance-wrap.sh states the contract: "the timer is the
# EXECUTOR, the Job is the VISIBILITY." ExecStartPre opens or reuses a
# Job and ExecStopPost records the verdict — boss-step.sh reads
# systemd's $SERVICE_RESULT, so a run that succeeded closes the packet
# "Maintenance completed" and a run that died closes it "Maintenance
# failed" with how it died. Until 2026-09-05 the completion sat on
# ExecStartPost, which systemd skips when ExecStart fails: a failed run
# recorded NOTHING, its packet stayed open looking like a run still in
# progress (disk-floor-sweep, 16:10, through two FLOOR UNMET runs) or
# was closed "ok" by the next run's recovery (forge-converge, 17:39,
# exit 1). Check 3b refuses that shape.
#
# Three of eleven timers were wired that way. Eight ran nightly with no
# packet, no findings, no event-log trace and nobody's queue, which
# means a silent failure in any of them was indistinguishable from a
# success. deploy-services.sh's own comment records that four of these
# units were "authored but never installed" and each was caught by
# hand; this is the same class one step later — installed, running, and
# unobservable.
#
# David, 2026-08-16: "Let's make sure we have a job to handle each" and
# "get as much maintenance and management into job protocols rather
# than floating around scripts or system timers elsewhere."
#
# WHAT IT CHECKS, for every row of deploy-services.sh's TIMERS array:
#   1. the .service unit exists where the array says it does
#   2. it calls boss-maintenance-wrap.sh with a kind (opens the Job)
#   3. it calls boss-step.sh with the SAME kind (records the verdict)
#   3b. that call is on ExecStopPost, the one phase that runs on failure
#   4. that kind is a real Workflow in the platform bundle
#
# (3) and (4) are the ones worth having. A unit that opens a Job and
# never completes it leaves an open packet every run — worse than no
# packet, because the fleet view fills with false failures. And a kind
# that no Workflow defines makes the wrapper's spawn fail at 03:00,
# where nobody is reading.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

DEPLOY="infra/deploy-services.sh"
# The FORGE host's own installer, added 2026-08-17. This lint read
# boss-gcp's TIMERS array and called itself complete, which was the
# same shape as the bug it was written for: the forge host runs two
# timers of its own and neither was covered. reap-dead-ci-jobs was
# committed and never installed anywhere as a result.
FORGE_INSTALL="infra/forge/install.sh"
BUNDLE="infra/platform/workflows.toml"
for f in "$DEPLOY" "$BUNDLE"; do
    [ -f "$f" ] || { echo "timers-leave-a-packet: $f not found" >&2; exit 1; }
done

# The kinds a Workflow actually defines: the bundle, plus the three
# still baked into platform_workflows() in registry.rs. Both are read,
# because a kind in either place is a real protocol — and the tree is
# mid-migration from the second to the first.
kinds=$(
    { grep -oE '^kind = "maintenance-[a-z-]+"' "$BUNDLE" | sed -E 's/kind = "(.*)"/\1/'
      grep -oE '"maintenance-[a-z-]+"' crates/core/boss-jobs/src/registry.rs | tr -d '"'
    } | sort -u
)

rows=$(sed -n '/^TIMERS=(/,/^)/p' "$DEPLOY" | grep -oE '"[a-z0-9-]+:[^"]+"' | tr -d '"')
# Forge-host units live beside their installer, so the "subdirectory"
# is always forge/. Same shape as a TIMERS row so the loop below is
# unchanged.
if [ -f "$FORGE_INSTALL" ]; then
    forge_rows=$(sed -n '/^UNITS=(/,/^)/p' "$FORGE_INSTALL" \
        | grep -oE '^\s+[a-z0-9-]+' | tr -d ' ' | sed 's|$|:forge|')
    rows="${rows}
${forge_rows}"
fi
count=$(printf '%s\n' "$rows" | grep -c . || true)
if [ "$count" -lt 5 ]; then
    echo "timers-leave-a-packet: only parsed $count timer rows from $DEPLOY —" >&2
    echo "  the scrape broke, so a green result would mean nothing." >&2
    exit 1
fi

problems=0
for row in $rows; do
    name="${row%%:*}"; sub="${row##*:}"
    [ "$sub" = "." ] && unit="infra/$name.service" || unit="infra/$sub/$name.service"

    if [ ! -f "$unit" ]; then
        echo "timers-leave-a-packet: $name is installed by $DEPLOY but $unit does not exist" >&2
        problems=$((problems + 1)); continue
    fi

    open_kind=$(grep -oE 'boss-maintenance-wrap\.sh [a-z-]+' "$unit" | awk '{print $2}' | head -1)
    done_kind=$(grep -oE 'boss-step\.sh [a-z-]+' "$unit" | awk '{print $2}' | head -1)

    if [ -z "$open_kind" ]; then
        echo "timers-leave-a-packet: $name runs with no Job — add an ExecStartPre calling" >&2
        echo "    boss-maintenance-wrap.sh <kind> \"<label>\", and an ExecStopPost calling" >&2
        echo "    boss-step.sh <kind> run. A timer with no packet fails silently." >&2
        problems=$((problems + 1)); continue
    fi
    if [ -z "$done_kind" ]; then
        echo "timers-leave-a-packet: $name OPENS a Job ($open_kind) and never completes it." >&2
        echo "    Missing the ExecStopPost boss-step.sh call: every run would leave an open" >&2
        echo "    packet, so the fleet view fills with failures that did not happen." >&2
        problems=$((problems + 1)); continue
    fi
    # 3b. A completion that only runs on success records no failure.
    if grep -qE '^ExecStartPost=-?.*boss-step\.sh' "$unit" || ! grep -qE '^ExecStopPost=-?.*boss-step\.sh' "$unit"; then
        echo "timers-leave-a-packet: $name records its verdict from a phase systemd skips on" >&2
        echo "    failure. Put the boss-step.sh call on ExecStopPost=- (it runs whether ExecStart" >&2
        echo "    succeeded or not, with \$SERVICE_RESULT set) and pass no result= of your own:" >&2
        echo "    boss-step.sh reads the service result, so a run that died closes its packet" >&2
        echo "    'Maintenance failed' instead of sitting open as if it were still running." >&2
        problems=$((problems + 1)); continue
    fi
    if [ "$open_kind" != "$done_kind" ]; then
        echo "timers-leave-a-packet: $name opens '$open_kind' but completes '$done_kind'." >&2
        problems=$((problems + 1)); continue
    fi
    if ! printf '%s\n' "$kinds" | grep -qxF -- "$open_kind"; then
        echo "timers-leave-a-packet: $name uses kind '$open_kind', which no Workflow defines." >&2
        echo "    The wrapper's spawn would fail at run time, in the middle of the night." >&2
        problems=$((problems + 1))
    fi
done

# 5. AND THE DEPLOY MUST POINT THEM AT A JOBS API.
#
# The four checks above prove a timer opens and completes a packet of a
# real kind. They cannot see WHERE it writes it, and that turns out to
# be the difference between visibility and none.
#
# boss-maintenance-wrap.sh falls back to
# `BOSS_JOBS_URL:-http://127.0.0.1:7900`. On a box whose local instance
# is not the system of record, that default is a silent redirect:
# measured 2026-08-17, the backup / audit-integrity / ledger-replay
# timers had fired on schedule for weeks and left 7 packets EACH on
# boss-gcp's legacy instance and ZERO on the cluster SoR. Every check
# in this lint passed the whole time. The 2026-08-13 split-brain
# (incident c4b4a6b0) fixed the pipeline units by hand and missed
# these.
#
# So: the deploy that installs a timer must also write its
# BOSS_JOBS_URL, from the tree, where a reader can see it.
if ! grep -q 'BOSS_JOBS_URL=' "$DEPLOY" || ! grep -q 'jobs-url.conf' "$DEPLOY"; then
    echo "timers-leave-a-packet: deploy-services.sh installs timers but writes no" >&2
    echo "    BOSS_JOBS_URL drop-in for them, so boss-maintenance-wrap.sh falls back to" >&2
    echo "    127.0.0.1 — which on this deployment is not the system of record. Every" >&2
    echo "    nightly packet would open and close where nobody is looking." >&2
    problems=$((problems + 1))
fi

# 5b. THE FORGE INSTALLER MUST DO THE SAME FOR ITS OWN UNITS.
#
# The check above reads deploy-services.sh (boss-gcp). The forge host is
# installed by install.sh, which had NO jobs-url drop-in — a blind spot
# that let reap-dead-ci-jobs run without BOSS_JOBS_URL and FAIL every
# time on 2026-09-03, with this lint green throughout. Same bug as the
# split-brain above, one installer over: a check that reads only one
# deploy manifest calls itself complete while the other host's units are
# uncovered — the exact shape the FORGE_INSTALL rows were added to close.
if [ -f "$FORGE_INSTALL" ] && ! grep -q 'BOSS_JOBS_URL=' "$FORGE_INSTALL"; then
    echo "timers-leave-a-packet: install.sh installs forge timers but writes no" >&2
    echo "    BOSS_JOBS_URL for them, so boss-maintenance-wrap.sh REFUSES (it has no" >&2
    echo "    localhost default) and every forge maintenance packet fails to open —" >&2
    echo "    which is how reap-dead-ci-jobs failed on 2026-09-03." >&2
    problems=$((problems + 1))
fi

# 6. AND NEITHER HELPER MAY CARRY A LOCALHOST DEFAULT.
#
# The drop-in above is the belt; this is the braces. A default of
# 127.0.0.1 in boss-maintenance-wrap.sh or boss-step.sh makes a missing
# BOSS_JOBS_URL look like a working configuration, which is exactly how
# 21 nightly packets landed on a non-authoritative instance without one
# check in this file noticing. If the wiring is ever dropped again, the
# helpers must FAIL rather than quietly pick somewhere plausible.
for helper in infra/boss-maintenance-wrap.sh infra/boss-step.sh; do
    # Comment lines are exempt: the refusal blocks quote the old
    # default to explain what they replaced, and a lint that cannot
    # tell code from prose would forbid documenting the fix.
    if grep -v '^[[:space:]]*#' "$helper" 2>/dev/null | grep -q 'BOSS_JOBS_URL:-http'; then
        echo "timers-leave-a-packet: $helper still defaults BOSS_JOBS_URL to a host." >&2
        echo "    A maintenance tool with no system of record configured must refuse," >&2
        echo "    not guess: a failed unit is noticed, a packet in the wrong database" >&2
        echo "    is not." >&2
        problems=$((problems + 1))
    fi
done

# 6. AND boss-step.sh MUST TURN THE SERVICE RESULT INTO A VERDICT.
#
# Check 3b proves the call sits where systemd will make it on failure;
# this proves what the call records. Driven through the real script in
# dry-run mode (it prints the pairs it would write and stops), because
# the behaviour under test is the derivation, not the arithmetic.
verdict() { # <service result> <exit status> [explicit pairs...]
    local sr="$1" es="$2"; shift 2
    SERVICE_RESULT="$sr" EXIT_STATUS="$es" BOSS_STEP_DRY_RUN=1 \
        BOSS_JOBS_URL=http://example.invalid \
        bash infra/boss-step.sh maintenance-selftest run "$@" 2>/dev/null | tr '\n' ' '
}
if ! verdict success 0 | grep -q 'result=ok'; then
    echo "timers-leave-a-packet: boss-step.sh does not record a successful run as result=ok" >&2
    problems=$((problems + 1))
fi
if ! verdict exit-code 1 | grep -q 'result=exit-code exit_status=1'; then
    echo "timers-leave-a-packet: boss-step.sh does not record a failed run's service result" >&2
    echo "    and exit status (SERVICE_RESULT=exit-code EXIT_STATUS=1 gave: $(verdict exit-code 1))" >&2
    problems=$((problems + 1))
fi
if ! verdict exit-code 1 result=floor-unmet | grep -q 'result=floor-unmet'; then
    echo "timers-leave-a-packet: boss-step.sh overrides a caller's explicit result=" >&2
    problems=$((problems + 1))
fi

if [ "$problems" -gt 0 ]; then
    echo "" >&2
    echo "  $problems timer(s) without working Job visibility." >&2
    exit 1
fi
echo "timers-leave-a-packet: $count timers, each opens and completes a defined Job"
