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
#   7. a boss-gcp unit whose kind only the bundle defines pins the
#      system of record inline and opens its packet best-effort (`-`)
#   8. and every CLUSTER CRONJOB does (2)–(4) too, because that is where
#      the chores live since the 2026-09-04 cutover — the nightly backup
#      ran unrecorded for twenty days inside this lint's blind spot

#   9. its cadence is declared, with the same number, to the
#      cadence-silence sweep that notices when its packets stop
#      arriving (CLAUDE.md 9a — the interval lives twice)
#
# (9) is the one that closes the loop. Checks 1-7 all describe a unit;
# none of them can tell whether its packet ever ARRIVES, so a unit that
# is uninstalled, masked, or failing before ExecStart leaves this file
# green while filing nothing — 23 nights of it, measured (e109f57e).
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
BUNDLE="infra/platform/workflows"   # a directory: one <kind>.toml per protocol
[ -f "$DEPLOY" ] || { echo "timers-leave-a-packet: $DEPLOY not found" >&2; exit 1; }
[ -d "$BUNDLE" ] || { echo "timers-leave-a-packet: $BUNDLE not found" >&2; exit 1; }

# The kinds a Workflow actually defines: the bundle, plus the three
# still baked into platform_workflows() in registry.rs. Both are read,
# because a kind in either place is a real protocol — and the tree is
# mid-migration from the second to the first.
kinds=$(
    { cat "$BUNDLE"/*.toml | grep -oE '^kind = "maintenance-[a-z-]+"' | sed -E 's/kind = "(.*)"/\1/'
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

# 7. A CHORE WHOSE KIND ONLY THE BUNDLE DEFINES FILES WHERE THE BUNDLE
#    IS SEEDED — and its packet never blocks its run.
#
# Measured 2026-09-08 on boss-gcp (backlog e109f57e). deploy-services'
# jobs-url.conf drop-in points every timer at the LOCAL instance
# (127.0.0.1:7900, "a chore reports to the instance whose data it
# maintains", 2026-08-19). That instance carries only the three kinds
# baked into registry.rs — the platform bundle was never seeded there
# and never will be (the-cluster-is-the-system retires it). So a
# boss-gcp timer whose kind exists ONLY in the bundle gets
# `400 unknown or inactive job kind` from its own ExecStartPre, and a
# hard ExecStartPre turns that into a run that never starts:
# boss-ml-inference-batch failed 23 nights in a row and the ML
# predictions went three weeks stale, while boss-conservation-
# invariants and boss-deploy-confirm (the deploy dead-man) did the
# same. The estate observers had already met this bug and pinned the
# system of record INLINE with env(1) on the Exec line, which outranks
# the drop-in (91ddebfb); this check makes that the rule.
#
# WHICH UNITS. Kinds the bundle defines minus the baked-in three,
# minus kinds a cluster CronJob already opens (a boss-gcp copy of
# those is the vestige the-cluster-is-the-system retires: pinning it
# to the SoR would file a second packet of a kind the cluster already
# runs, and the wrap's reuse-the-open-packet recovery would then
# "recover" the cluster's. Retirement is their fix, not a pin.)
#
# WHAT THEY MUST DO. Name the system of record on BOTH the wrap and
# the boss-step lines (a packet opened on one instance and closed on
# another is the split-brain) — the URL infra/deploy.env.example names
# for boss-gcp, so the tree states it once — and prefix the packet-
# opening ExecStartPre with `-`: the packet is visibility, never a
# precondition (CLAUDE.md §Diagnosis, "an arm that needs the patient
# is not an arm"; cluster-watchdog.service is the precedent).
sor=$(grep -oE '^BOSS_JOBS_URL=http://[^ ]+' infra/deploy.env.example | head -1 | cut -d= -f2)
if [ -z "$sor" ]; then
    echo "timers-leave-a-packet: infra/deploy.env.example names no BOSS_JOBS_URL — check 7 cannot know the system of record" >&2
    problems=$((problems + 1))
fi
baked=$(grep -oE '"maintenance-[a-z-]+"' crates/core/boss-jobs/src/registry.rs | tr -d '"' | sort -u)
cluster_kinds=$(grep -ohE 'boss-maintenance-wrap\.sh maintenance-[a-z-]+' infra/cluster/manifests/*.yaml 2>/dev/null \
    | awk '{print $2}' | sort -u)
gcp_rows=$(sed -n '/^TIMERS=(/,/^)/p' "$DEPLOY" | grep -oE '"[a-z0-9-]+:[^"]+"' | tr -d '"')
for row in $gcp_rows; do
    name="${row%%:*}"; sub="${row##*:}"
    [ "$sub" = "." ] && unit="infra/$name.service" || unit="infra/$sub/$name.service"
    [ -f "$unit" ] || continue   # check 1 already named it
    kind=$(grep -oE 'boss-maintenance-wrap\.sh [a-z-]+' "$unit" | awk '{print $2}' | head -1)
    [ -n "$kind" ] || continue   # check 2 already named it
    printf '%s\n' "$baked" | grep -qxF -- "$kind" && continue          # local instance knows it
    printf '%s\n' "$cluster_kinds" | grep -qxF -- "$kind" && continue  # the cluster runs it; this copy is a vestige
    pre=$(grep -E '^ExecStartPre=' "$unit" | grep 'boss-maintenance-wrap' | head -1)
    post=$(grep -E '^ExecStopPost=' "$unit" | grep 'boss-step\.sh' | head -1)
    if ! printf '%s' "$pre" | grep -qF -- "BOSS_JOBS_URL=$sor " \
        || ! printf '%s' "$post" | grep -qF -- "BOSS_JOBS_URL=$sor "; then
        echo "timers-leave-a-packet: $name opens '$kind', a kind only the platform bundle defines," >&2
        echo "    but does not pin the system of record on both its Exec lines. deploy-services'" >&2
        echo "    drop-in points it at the local instance, which has never heard of that kind:" >&2
        echo "    every run dies 400 in ExecStartPre. Pin it inline, where env(1) outranks the" >&2
        echo "    drop-in, on the wrap AND the boss-step call:" >&2
        echo "      ExecStartPre=-/usr/bin/env BOSS_JOBS_URL=$sor /opt/boss/infra/boss-maintenance-wrap.sh $kind \"<label>\"" >&2
        echo "      ExecStopPost=-/usr/bin/env BOSS_JOBS_URL=$sor /opt/boss/infra/boss-step.sh $kind run" >&2
        problems=$((problems + 1)); continue
    fi
    if ! printf '%s' "$pre" | grep -q '^ExecStartPre=-'; then
        echo "timers-leave-a-packet: $name opens its packet from a HARD ExecStartPre. The packet is" >&2
        echo "    visibility, not a precondition: an API that answers 400 must not stop the" >&2
        echo "    chore (boss-ml-inference-batch lost 23 nights to exactly that). Prefix it:" >&2
        echo "      ExecStartPre=-/usr/bin/env BOSS_JOBS_URL=$sor ..." >&2
        problems=$((problems + 1)); continue
    fi
    if printf '%s\n%s' "$pre" "$post" | grep -qE '127\.0\.0\.1|localhost'; then
        echo "timers-leave-a-packet: $name names a localhost jobs API on an Exec line — that is" >&2
        echo "    boss-gcp's non-authoritative instance, never the system of record." >&2
        problems=$((problems + 1))
    fi
done

# 8. AND SO MUST EVERY CLUSTER CRONJOB.
#
# Checks 1–7 read systemd units, because that is where the maintenance
# chores lived when this lint was written. The 2026-09-04 cutover moved
# most of them into cluster CronJobs, and this file could not see one of
# them: boss-pg-backup replaced boss-backup.service and did NOT take the
# service's ExecStartPre with it. The backups kept running and kept
# succeeding — verified artefact, both offsite legs — while the system
# of record's last backup packet stayed at 2026-08-19 for twenty days.
# Nothing here noticed, because a CronJob is not a TIMERS row. Found by
# hand (backlog 60095754), which is the definition of a gap in this
# lint.
#
# The consequence is not tidiness. Once the silent-cadence sweep alarms
# on `maintenance-backup`, its first alarm on a REAL stopped backup is
# indistinguishable from this one — CLAUDE.md §Diagnosis, a check nobody
# can read is a check that is not running.
#
# WHAT IT CHECKS, per manifest holding a CronJob: it calls
# boss-maintenance-wrap.sh with a kind, calls boss-step.sh with the SAME
# kind, and that kind is a real Workflow — or its name is listed below
# with the reason it files its visibility some other way.
#
# WHAT IT DELIBERATELY DOES NOT CHECK YET: that the packet-opening call
# cannot fail the chore. boss-backup.yaml swallows it (the packet is
# visibility, never a precondition — check 7 states the same rule for
# boss-gcp units), but the seven siblings open theirs inside
# `set -euo pipefail`, so an API answering 400 would stop them. That is
# the boss-ml-inference-batch shape one layer over, and fixing seven
# working chores belongs in its own change rather than riding this one.
CLUSTER_MANIFESTS="infra/cluster/manifests"
# A CronJob whose visibility is its own, with the reason. Adding a name
# here is a decision; the default is that a scheduled run leaves a
# packet.
CRONJOB_OWN_VISIBILITY=(
    # Posts every run straight to /api/estate/observation — the
    # observation IS the record, and a maintenance packet beside it
    # would say less than the thing it wrapped.
    "boss-estate-observe"
)

if [ -d "$CLUSTER_MANIFESTS" ]; then
    cron_files=$(grep -lE '^kind: CronJob' "$CLUSTER_MANIFESTS"/*.yaml 2>/dev/null)
    cron_count=$(printf '%s\n' "$cron_files" | grep -c . || true)
    if [ "$cron_count" -lt 5 ]; then
        echo "timers-leave-a-packet: only found $cron_count CronJob manifests under" >&2
        echo "    $CLUSTER_MANIFESTS — the scrape broke, so a green result would mean nothing." >&2
        problems=$((problems + 1))
    fi
    seen_exempt=""
    for file in $cron_files; do
        # The CronJob's own metadata.name, not the file's: the message
        # has to name the thing an operator sees in `kubectl get cronjob`.
        cname=$(awk '/^kind: CronJob/{f=1} f && /^  name: /{print $2; exit}' "$file")
        [ -n "$cname" ] || cname="$(basename "$file" .yaml)"

        case " ${CRONJOB_OWN_VISIBILITY[*]} " in
            *" $cname "*)
                seen_exempt="$seen_exempt $cname"
                continue
                ;;
        esac

        open_kind=$(grep -oE 'boss-maintenance-wrap\.sh [a-z-]+' "$file" | awk '{print $2}' | head -1)
        done_kind=$(grep -oE 'boss-step\.sh [a-z-]+' "$file" | awk '{print $2}' | head -1)

        if [ -z "$open_kind" ]; then
            echo "timers-leave-a-packet: CronJob $cname ($file) runs with no packet." >&2
            echo "    Add a boss-image container that calls" >&2
            echo "      boss-maintenance-wrap.sh <kind> \"<label>\"   before the work, and" >&2
            echo "      boss-step.sh <kind> run result=ok            after it," >&2
            echo "    with BOSS_JOBS_URL pointing at the in-cluster jobs API. Seven siblings" >&2
            echo "    under $CLUSTER_MANIFESTS show the shape; boss-backup.yaml shows it for a" >&2
            echo "    multi-image Pod. A scheduled run the system of record cannot see is a" >&2
            echo "    run whose silence means nothing." >&2
            problems=$((problems + 1)); continue
        fi
        if [ -z "$done_kind" ]; then
            echo "timers-leave-a-packet: CronJob $cname OPENS a packet ($open_kind) and never" >&2
            echo "    completes it. Every run would leave an open packet, which is worse than" >&2
            echo "    no packet: the fleet view fills with failures that did not happen." >&2
            problems=$((problems + 1)); continue
        fi
        if [ "$open_kind" != "$done_kind" ]; then
            echo "timers-leave-a-packet: CronJob $cname opens '$open_kind' but completes" >&2
            echo "    '$done_kind' — one packet is left open and another is closed blind." >&2
            problems=$((problems + 1)); continue
        fi
        if ! printf '%s\n' "$kinds" | grep -qxF -- "$open_kind"; then
            echo "timers-leave-a-packet: CronJob $cname uses kind '$open_kind', which no" >&2
            echo "    Workflow defines. The wrapper's spawn would fail at run time, on a" >&2
            echo "    schedule nobody is watching." >&2
            problems=$((problems + 1))
        fi
    done

    # A stale exemption is not free: left standing, it holds a future
    # CronJob of that name out of this check without anyone deciding so.
    # Same refusal gate.sh applies to its own pre-flight roster.
    for exempt in "${CRONJOB_OWN_VISIBILITY[@]}"; do
        case " $seen_exempt " in
            *" $exempt "*) ;;
            *)
                echo "timers-leave-a-packet: CRONJOB_OWN_VISIBILITY names '$exempt', which is" >&2
                echo "    not a CronJob under $CLUSTER_MANIFESTS any more. Remove the entry —" >&2
                echo "    a dead exemption silently covers the next CronJob to take that name." >&2
                problems=$((problems + 1))
                ;;
        esac
    done
fi



# 9. AND A TIMER'S CADENCE MUST BE DECLARED WHERE THE SILENCE SWEEP
#    READS IT — with the same number.
#
# Checks 1-7 prove a timer opens and completes a packet of a real kind
# on the right instance. None of them can tell whether the packet ever
# ARRIVES: a unit that is uninstalled, masked, failing before ExecStart,
# or pointed at a host nobody converges leaves this whole file green
# while filing nothing. Measured 2026-09-08, both by a human looking:
# boss-ml-inference-batch had been dead 23 nights (e109f57e — check 7
# was written for the cause; this is the missing detector for the
# effect) and the five-minute estate-observe-units observer had been
# quiet four days (408c81f6).
#
# The detector is the `cadence-silence-sweep-daily` dispatcher rule
# (migration 202609082300): it compares each DECLARED cadence against
# the newest ACTUAL packet of that kind and files one alarm per silent
# kind. Its declarations ride its own rule row's args, which is registry
# data an operator can retune without a deploy — and which means the
# interval now lives twice, here and in the .timer file that executes
# the chore. CLAUDE.md §9a: a fact that lives twice gets an equality
# test, and this is it.
#
# WHAT IT ASSERTS, for every rostered timer that has a SCHEDULE:
#   * its kind appears in the sweep's roster, and
#   * the declared minutes equal the LOOSEST rostered timer for that
#     kind (maintenance-estate-observe-host is opened by a 15-minute
#     forge timer AND a daily boss-gcp one; the SoR can only honestly
#     expect the daily).
# A timer whose schedule shape this cannot read FAILS rather than being
# skipped — the same fail-closed posture the sweep itself takes for a
# declaration it cannot parse. A timer with no schedule directive at
# all (boss-deploy-confirm is armed by `systemctl restart`, not by the
# clock) is not a cadence and is not checked. A DECLARED kind with no
# rostered timer is left alone: it may be a cluster CronJob or a
# dispatcher-driven kind, neither of which this file can see.
# The cadence roster is the args of ONE rule, and since 2026-09-09 each
# rule is its own file (infra/dispatcher/rules/README.md).
#
# THIS CHECK COVERS ONE OF THE ROSTER'S TWO SOURCES, and since
# 2026-09-10 it says so. The args above are the TIMER half. A daily
# dispatcher CLOCK RULE that spawns a chore packet is equally a declared
# cadence and has no timer file to pin against, so it was outside this
# check — and outside the sweep — by construction: eight such cadences
# existed, three families of them had been silent for nine to nineteen
# days, and the public mirror drifted 238 commits behind with nothing
# announcing it (backlog cf0f5e2d). That half is now DERIVED from the
# rule registry at runtime (`cadence_roster::clock_cadences`), so the
# cadence, the kind, the target and the dedup guard have exactly one
# definition — the rule row — and there is nothing here for bash to keep
# in sync. What can still go wrong is a new rule dropping out of that
# derivation silently, and the pin for that lives with the derivation:
# `boss-dispatcher-handlers/tests/the_silence_roster_covers_the_clock_rules.rs`
# fails CI naming the rule. Re-implementing "which rules declare a
# cadence" in bash would be the second definition §9a warns about, which
# is why it is deliberately not here.
RULES_TOML="infra/dispatcher/rules/cadence-silence-sweep-daily.toml"

# The schedule of one .timer, in minutes. Echoes NONE for a unit with no
# clock schedule and UNPARSED:<text> for a shape this cannot read.
timer_interval_minutes() {
    local unit="$1" oua oc n
    oua=$(grep -oE '^OnUnitActiveSec=[^ ]+' "$unit" 2>/dev/null | head -1 | cut -d= -f2)
    if [ -n "$oua" ]; then
        case "$oua" in
            *min) n="${oua%min}"; [ "$n" -gt 0 ] 2>/dev/null && { echo "$n"; return; } ;;
            *h)   n="${oua%h}";   [ "$n" -gt 0 ] 2>/dev/null && { echo $((n * 60)); return; } ;;
            *d)   n="${oua%d}";   [ "$n" -gt 0 ] 2>/dev/null && { echo $((n * 1440)); return; } ;;
        esac
        echo "UNPARSED:OnUnitActiveSec=$oua"; return
    fi
    oc=$(grep -E '^OnCalendar=' "$unit" 2>/dev/null | head -1 | cut -d= -f2-)
    if [ -z "$oc" ]; then echo NONE; return; fi
    case "$oc" in
        hourly)  echo 60 ;;
        daily)   echo 1440 ;;
        weekly)  echo 10080 ;;
        # `*-*-* HH:MM:SS [TZ]` — every day at a fixed time.
        '*-*-* '*) echo 1440 ;;
        # `Mon *-*-* HH:MM:SS` — one named weekday.
        [A-Z][a-z][a-z]' *-*-* '*) echo 10080 ;;
        *) echo "UNPARSED:OnCalendar=$oc" ;;
    esac
}

# The sweep's declared roster, as `<kind> <minutes>` lines. Read off the
# rule row mirrored in its own file under infra/dispatcher/rules/ — the
# same text the migration seeds and dispatcher_rules_seed_matches_toml
# pins to the live table.
declared=$(grep -oE '"interval_minutes\.[a-z0-9-]+" = "[0-9]+"' "$RULES_TOML" \
    | sed -E 's/"interval_minutes\.([a-z0-9-]+)" = "([0-9]+)"/\1 \2/')
if [ -z "$declared" ]; then
    echo "timers-leave-a-packet: $RULES_TOML declares no cadence intervals for the" >&2
    echo "    cadence-silence-sweep-daily rule. A silence sweep with an empty roster" >&2
    echo "    watches nothing, which is the defect it was built to end (ecca2f43)." >&2
    problems=$((problems + 1))
fi

# The loosest rostered timer per kind — the number the sweep must carry.
# Same row shapes checks 1-7 already walk: boss-gcp's TIMERS plus the
# forge installer's UNITS.
expected=""
for row in $rows; do
    name="${row%%:*}"; sub="${row##*:}"
    if [ "$sub" = "." ]; then unit="infra/$name.service"; timer="infra/$name.timer"
    else unit="infra/$sub/$name.service"; timer="infra/$sub/$name.timer"; fi
    [ -f "$unit" ] || continue
    [ -f "$timer" ] || continue
    kind=$(grep -oE 'boss-maintenance-wrap\.sh [a-z-]+' "$unit" | awk '{print $2}' | head -1)
    [ -n "$kind" ] || continue
    mins=$(timer_interval_minutes "$timer")
    case "$mins" in
        NONE) continue ;;
        UNPARSED:*)
            echo "timers-leave-a-packet: $name's schedule (${mins#UNPARSED:}) is a shape this" >&2
            echo "    check cannot read, so it cannot prove the cadence-silence sweep expects" >&2
            echo "    the right interval for '$kind'. Teach timer_interval_minutes the shape" >&2
            echo "    rather than leaving the cadence unpinned — an interval nobody checks is" >&2
            echo "    how a retuned timer starts a daily false alarm." >&2
            problems=$((problems + 1)); continue ;;
    esac
    expected="${expected}${kind} ${mins}
"
done

# Reduce to the maximum per kind, then compare with the declaration.
for kind in $(printf '%s' "$expected" | awk '{print $1}' | sort -u); do
    want=$(printf '%s' "$expected" | awk -v k="$kind" '$1 == k {print $2}' | sort -n | tail -1)
    got=$(printf '%s\n' "$declared" | awk -v k="$kind" '$1 == k {print $2}' | head -1)
    if [ -z "$got" ]; then
        echo "timers-leave-a-packet: a timer opens '$kind' every $want minutes, but the" >&2
        echo "    cadence-silence-sweep-daily rule does not declare it — so nothing notices" >&2
        echo "    when its packets stop arriving, which is exactly how the ML inference batch" >&2
        echo "    went 23 nights unmissed (e109f57e). Add to the rule's args in" >&2
        echo "    $RULES_TOML AND to its seeding migration:" >&2
        echo "      \"interval_minutes.$kind\" = \"$want\"" >&2
        problems=$((problems + 1)); continue
    fi
    if [ "$got" != "$want" ]; then
        echo "timers-leave-a-packet: '$kind' is executed every $want minutes by its timer but" >&2
        echo "    declared every $got to the cadence-silence sweep. The two must agree or the" >&2
        echo "    sweep alarms on a healthy chore (declared too tight) or stays quiet on a" >&2
        echo "    dead one (too loose). Fix $RULES_TOML and its seeding migration, or the" >&2
        echo "    timer — whichever states the wrong number." >&2
        problems=$((problems + 1))
    fi
done

if [ "$problems" -gt 0 ]; then
    echo "" >&2
    echo "  $problems scheduled run(s) without working Job visibility." >&2
    exit 1
fi
echo "timers-leave-a-packet: $count timers and ${cron_count:-0} cluster CronJobs, each opens and completes a defined Job"
