#!/usr/bin/env bash
# Validate the brewery sim: run a full 12-month sim against the live
# API and assert it reconstructs + reconciles cleanly. This is the CI
# correctness gate the release leans on — the demo itself builds live
# (see infra/seed-brewery-tenant.sh + the quickstart launchers).
#
# Sequence (per docs/design/projection-rebuilders.md §E):
#
#   1. Drop + recreate the live `boss` DB.
#   2. Bring services up (jobs-api reconciles `workflow-design`
#      via platform_kinds()).
#   3. Run `boss-brewery-sim prepare` → the converged prepare_model:
#      publishes the brewery Workflows (real `workflow-design` Jobs,
#      full audit_log provenance), seeds tenant policy grants, and
#      POSTs accounts / vendors / employees / messages / FG + raw +
#      asset opening balances. The SAME seed code the live demo runs
#      (seed-brewery-tenant.sh → boss-brewery-sim prepare) — so CI
#      can't drift from the demo. (Replaces the old separate
#      brewery-bootstrap + policy-bootstrap + data-seed trio.)
#   5. Pre-seed check: raw inventory_items come from step 3's prepare
#      (parts.toml); warn if the SKU file is missing.
#   6. Run `boss-brewery-sim run` for 365 days from 2025-04-01,
#      against the live API, hard-fail. The SAME run_brewery_live
#      driver the live daemon's per-tick loop is built on. Any
#      non-2xx aborts immediately so we fix the bug instead of
#      letting drift accumulate.
#   7. Wait for dispatcher rules + downstream services to drain
#      their NATS queues.
#   8. Run boss-rebuild-all to verify every projection
#      reconstructs cleanly from audit_log alone.
#   9. Run boss-audit-integrity-check for dangling-FK violations.
#
# Hard requirement: zero errors at every step. Any failure aborts
# the script with a non-zero exit code.
#
# Usage:
#   sudo ./infra/postgres/validate-brewery-sim.sh
#
# Optional env:
#   BOSS_REGEN_DAYS=14         # short dry run for sanity (default 365)
#   BOSS_REGEN_START=2025-04-01  # sim start date (default matches tenant.toml)

set -euo pipefail

DAYS="${BOSS_REGEN_DAYS:-365}"
START="${BOSS_REGEN_START:-2025-04-01}"
# Sim warp (sim-seconds per wall-second). Default 2000, NOT the sim `run`
# default of 8640: the brewery's write path is effectively serial (every step write
# does a synchronous policy round-trip; audit_log is a hash chain) and
# sustains only ~10 writes/wall-second. At 8640 that's ~105 completions per
# sim-day — far below the ~400+ steps generated/day — so work-in-flight
# grows unboundedly. The compressed burst load also stresses Postgres: the
# ~24 API pools now run against max_connections=400 (raised from 100 — see
# deploy-services.sh), which keeps connection contention in the transient
# regime the JetStream redelivery layer can self-heal rather than the
# sustained saturation that dead-letters assignments. 2000 still gives the
# serial path ~43 wall-s per sim-day to keep pace. Raising serial write
# throughput (to allow a higher warp) is a future-release optimization.
# Override with BOSS_REGEN_WARP for tuning.
WARP="${BOSS_REGEN_WARP:-2000}"
# Workforce check-in cadence (ms between passes in --live-api mode). The
# completion pipeline is latency-bound at high warp: each Job advances ~one
# step per workforce check-in (claim next step → wait its duration →
# complete), so a faster cadence walks each Job's DAG through in less
# wall-time and lets completions keep pace with generation. Lower it in step
# with a higher warp; the `run` default is 200. Override with
# BOSS_REGEN_POLL_SLEEP_MS for tuning.
POLL_SLEEP_MS="${BOSS_REGEN_POLL_SLEEP_MS:-200}"
# Wall-clock when this run began — the dispatcher health gate
# (step 7a) scopes its journal scan to this run's failures only.
RUN_STARTED="$(date -u +"%Y-%m-%d %H:%M:%S")"

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SEEDS_DIR="$REPO_ROOT/examples/brewery/seeds"

echo "==> validating brewery sim: $DAYS days from $START"
echo "    repo:  $REPO_ROOT"
echo

# -- Step 0: clock-api must be up in sim mode ------------------
# Every write in the run is stamped through clock-api, so the whole
# script depends on a sim-mode clock (`POST /configure` is sim-only —
# a wall-mode clock 405s it). deploy-services.sh installs the clock
# with BOSS_CLOCK_MODE=wall (the prod default), so a fresh
# bootstrap-vm.sh box fails here until the mode is flipped; the
# playground carries the sim-mode drop-in already. Probe /configure
# up front — before the DB drop — so a wall-mode box fails in
# seconds with the remediation, not minutes in with a half-reset DB.
# In sim mode the probe doubles as an early epoch prime; step 2.4
# re-primes after the DB recreate (idempotent rebase, same epoch).
echo "==> [0/10] checking clock-api is up in sim mode"
PROBE_CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 3 -X POST \
    -H "Content-Type: application/json" \
    -d "{\"epoch_start\": \"${START}\"}" \
    "http://127.0.0.1:7060/api/clock/configure" || echo 000)
if [[ "$PROBE_CODE" != "200" && "$PROBE_CODE" != "201" ]]; then
    cat >&2 <<EOF
ERROR: clock-api /configure returned HTTP $PROBE_CODE — the regen needs a
sim-mode clock (405 = wall mode; 000 = clock-api not running). Flip the
deployed unit to sim mode and re-run:

  sudo mkdir -p /etc/systemd/system/boss-clock-api.service.d
  printf '[Service]\nEnvironment=BOSS_CLOCK_MODE=sim\n' |
      sudo tee /etc/systemd/system/boss-clock-api.service.d/override.conf
  sudo systemctl daemon-reload && sudo systemctl restart boss-clock-api

(The drop-in survives deploy-services.sh re-runs; see the clock section
there for why wall is the install default.)
EOF
    exit 1
fi
echo "    clock-api sim mode confirmed (epoch primed to ${START})"

# -- Step 1: drop + recreate live boss DB ----------------------
echo "==> [1/10] dropping + recreating boss DB"
# Stop every boss service that holds a connection to the boss DB.
# `|| true` so a missing-on-this-box unit doesn't abort the script
# (different fleets carry different subsets — boss-cybernetics is
# optional, the brewery deploy currently doesn't run it).
SERVICES=(
    boss-jobs-api
    boss-policy-api
    boss-people-api
    boss-commerce-api
    boss-inventory-api
    boss-shipping-api
    boss-assets-api
    boss-catalog-api
    boss-ledger-api
    boss-content-api
    boss-messages-api
    boss-ml-api
    boss-cybernetics
    boss-dispatcher
    # 2026-05-27: products / classes / locations / subject-kinds /
    # calendar were missing from this list, so the
    # brewery-data-seed step ran against unreachable downstream
    # services and silently skipped its finished-product-inventory
    # seed — leaving 0 FP rows, 0 COGS, and 0 production-side ledger
    # activity, which surfaced as "the sim looks broken". Match the
    # list to reset-to-baseline.sh so this always restarts everything
    # it needs.
    boss-products-api
    boss-classes-api
    boss-locations-api
    boss-subject-kinds-api
    boss-calendar-api
)
for svc in "${SERVICES[@]}"; do
    sudo systemctl stop "$svc" 2>/dev/null || true
done
# Quiesce the maintenance timers for the run's duration. The nightly
# deep replay-check runs its reprojection inside a transaction that
# TRUNCATEs financial_facts — an ACCESS EXCLUSIVE lock held for the
# check's full runtime, which stalls every fact write behind it. In
# steady-state prod that's a ~2-minute window the JetStream NAK layer
# rides out; mid-regen it times out the sim's synchronous ledger calls
# and hard-fails the run (2026-07-10: the 03:00 timer killed a year
# run at sim-day 133 — a 29.99s INSERT wait, a 30s client timeout).
# The validate pipeline runs its own integrity checks at steps 8-9;
# the timers resume at the end of this script.
MAINT_TIMERS=(
    boss-ledger-replay-check.timer
    boss-audit-integrity-check.timer
    boss-conservation-invariants.timer
    boss-ledger-recognize.timer
    boss-backup.timer
    boss-files-gc.timer
    boss-messages-events-purge.timer
    boss-ml-inference-batch.timer
)
for tmr in "${MAINT_TIMERS[@]}"; do
    sudo systemctl stop "$tmr" 2>/dev/null || true
done
# Resume them on ANY exit — a failed run must not leave the box
# without its nightly integrity net.
resume_timers() {
    for tmr in "${MAINT_TIMERS[@]}"; do
        sudo systemctl start "$tmr" 2>/dev/null || true
    done
}
trap resume_timers EXIT
# bootstrap-db.sh without --init does the idempotent drop+recreate
# (--init mode is the inverse: skip if DB already exists). --no-seed
# skips the embedded data dump; we'll repopulate via the meta-Job
# path below.
sudo "$REPO_ROOT/infra/postgres/bootstrap-db.sh" boss --no-seed

# Reset the durable JetStream delivery buffer alongside Postgres. The
# dispatcher's durable consumers replay the stream from its start, so a
# stale BOSS_EVENTS (from a prior regen) would replay last run's events
# against the freshly-reset DB — every replayed step-completion would 404
# and fail the health gate. `--reset-stream` drops + recreates it empty,
# then exits; the services started below recreate consumers on the clean
# stream. No-op (logs a warning) if JetStream is somehow unavailable.
echo "    resetting BOSS_EVENTS JetStream buffer"
"$REPO_ROOT/target/release/boss-dispatcher" --reset-stream || \
    echo "    WARN: stream reset failed (continuing; dispatcher will ensure it on start)"

# -- Step 2: bring services back up ----------------------------
echo "==> [2/10] starting services"
# Start order matters: boss-policy-api must be up before
# boss-jobs-api so the policy round-trip works on the very
# first request. Other services are independent.
START_ORDER=(
    # Class / locations / subject-kinds registries first — boss-people-api
    # validates employee writes against the Class registry on creation,
    # so a missing boss-classes-api leaves brewery-data-seed unable to
    # post any employees (the symptom that 789 failures surfaced post-D6
    # before this fix paired up with the SERVICES-list expansion above).
    boss-classes-api
    boss-locations-api
    boss-subject-kinds-api
    boss-calendar-api
    boss-policy-api
    boss-people-api
    boss-products-api
    boss-commerce-api
    boss-inventory-api
    boss-shipping-api
    boss-assets-api
    boss-catalog-api
    boss-ledger-api
    boss-content-api
    boss-messages-api
    boss-ml-api
    boss-cybernetics
    boss-jobs-api
    boss-dispatcher
)
for svc in "${START_ORDER[@]}"; do
    sudo systemctl start "$svc" 2>/dev/null || true
done
echo "    waiting up to 30s for services to bind ports + reconcile defaults"

# Sanity check — both APIs must be reachable + have completed
# their startup reconcile before step 3 fires.
#
# Pre-2026-05-23 this polled `journalctl` for log strings ("reconciled
# platform Workflows" / "boss-policy-api listening"). That was racy:
# journalctl's `--since "60 seconds ago"` window depends on the
# script's wall-clock start vs the service's logging timestamp, and
# we hit "didn't log within 30s" failures three times in a single
# session even though the service WAS up and the log line WAS in
# the journal. Replaced with HTTP health-check polling — same
# guarantee (API is reachable), no journal-window timing dependency.
#
# /api/workflows returns 200 only after reconcile completes (the
# handler reads from the freshly-reconciled workflows table); a
# 200 with a non-empty array proves reconcile ran.
RECONCILED=0
for i in $(seq 1 60); do
    # Fetch first, then parse from a variable — keeps the fetch and the
    # parse as two visibly separate motions for static scanners.
    KINDS=$(curl -s -m 2 http://127.0.0.1:7900/api/workflows 2>/dev/null || true)
    if curl -s -f -m 2 http://127.0.0.1:7900/api/jobs/health >/dev/null 2>&1 \
        && printf '%s' "$KINDS" \
        | jq -e '(type == "array") and (length > 0)' >/dev/null 2>&1; then
        RECONCILED=1
        echo "    boss-jobs-api ready + reconciled (after ${i}s)"
        break
    fi
    sleep 1
done
if [ "$RECONCILED" -ne 1 ]; then
    echo "ERROR: boss-jobs-api health-check didn't reach 200+reconciled within 60s" >&2
    exit 1
fi

# boss-policy-api must also be ready before step 3 — the brewery
# bootstrap calls /api/workflows which round-trips through
# boss-policy-api for the Read check.
POLICY_READY=0
for i in $(seq 1 60); do
    if curl -s -f -m 2 http://127.0.0.1:7250/api/policy/health >/dev/null 2>&1; then
        POLICY_READY=1
        echo "    boss-policy-api ready (after ${i}s)"
        break
    fi
    sleep 1
done
if [ "$POLICY_READY" -ne 1 ]; then
    echo "ERROR: boss-policy-api health-check didn't reach 200 within 60s" >&2
    exit 1
fi

# -- Step 2.4: prime clock-api with the sim epoch --------------
# Every service uses clock-api as its authoritative `now`. In sim
# mode, until /advance is called clock-api falls back to wallclock
# — and any audit_log row stamped during that window inherits
# wallclock. Brewery-bootstrap (step 3) and operator-baseline-seed
# (step 2.6) both write through the live API, so they need clock-api
# primed BEFORE they start. Pre-2026-05-30 this only happened in
# step 4 (brewery-data-seed), and steps 2.6 + 3 leaked ~250
# wallclock-stamped rows into every regen.
echo "==> [2.4/10] priming clock-api with sim epoch ($START)"
# Formula clock: /configure rebases epoch_start; /advance is gone.
ADVANCE_RESP=$(curl -sS -w '\n%{http_code}' -X POST \
    -H "Content-Type: application/json" \
    -d "{\"epoch_start\": \"${START}\"}" \
    "http://127.0.0.1:7060/api/clock/configure" || echo $'\n000')
ADV_CODE=$(echo "$ADVANCE_RESP" | tail -n1)
if [[ "$ADV_CODE" != "200" && "$ADV_CODE" != "201" ]]; then
    echo "ERROR: clock-api /configure failed (HTTP $ADV_CODE) — sim mode + bootstrap writes would land on wallclock" >&2
    echo "$ADVANCE_RESP" | sed '$d' >&2
    exit 1
fi
echo "    clock-api primed to ${START}T00:00:00Z"

# -- Step 2.5: tenant-side Class registry rows -----------------
# Employee writes validate role + department against the class
# registry, so the brewery's specialised roles + departments must
# land BEFORE the operator-baseline hires (2.6) and the prepare step
# (3). Seed via the public API (POST /api/classes/batch) like the demo
# path — not a `psql -f classes.sql` end-around. The prepare step
# re-seeds classes idempotently (the batch endpoint is an upsert);
# this earlier pass is what lets 2.6's operator hires validate.
echo "==> [2.5/10] applying brewery Class registry rows (POST /api/classes/batch)"
class_code=$(curl -s -o /dev/null -w '%{http_code}' -m 15 -X POST \
    -H 'content-type: application/json' -H 'x-sim-origin: true' \
    -H 'x-boss-user: {"id":"automation:classes-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}' \
    --data-binary "@$SEEDS_DIR/classes.json" \
    "http://127.0.0.1:7800/api/classes/batch" 2>/dev/null || echo 000)
[[ "$class_code" == "200" || "$class_code" == "201" ]] \
    || { echo "ERROR: POST /api/classes/batch failed (HTTP $class_code) — aborting" >&2; exit 1; }

# -- Step 2.6: operator-baseline hires -------------------------
# brewery-data-seed (step 4) attaches operators (emp-cto, emp-coo,
# etc.) to brewery accounts as account_team_members; without their
# operator-hire events in audit_log first, the rebuild (step 8) hits
# forward-references it can't resolve. Seed the hires here, before the
# data seed, so the references resolve cleanly. (The install launchers
# do the same via bootstrap-db.sh's operator-baseline step.)
echo "==> [2.6/10] seeding operator-baseline hires into audit_log"
# BOSS_BOOTSTRAP_ADMIN_EMAIL (v1.0.10 audit F2): when set, the seed
# binary injects an emp-bootstrap-admin Employee at the head of
# the hire list with role=platform-admin and email = this var. The
# seed binary also falls back to the first credentials.toml email
# when this var is unset. Either path provisions the platform-admin
# as part of system init; the gateway login no longer auto-creates.
#
# Since Q7 (every Job names a human owner), the platform-admin is
# STRUCTURALLY required from empty: publish_workflows opens
# `workflow-design` Jobs whose owner resolves via role
# `platform-admin`, and the brewery roster seeds no holder of it —
# emp-bootstrap-admin IS the holder. The install launchers hard-fail
# without an admin email (quickstart `:?`); this harness instead
# provisions a deterministic validation identity when neither the
# env var nor a readable credentials file supplies one (pristine
# CI runners / regen VMs), so from-empty validation stays viable.
BOSS_AUTH_FILE="${BOSS_AUTH_FILE:-/var/lib/boss/auth/credentials.toml}"
if [[ -z "${BOSS_BOOTSTRAP_ADMIN_EMAIL:-}" && ! -r "$BOSS_AUTH_FILE" ]]; then
    BOSS_BOOTSTRAP_ADMIN_EMAIL="validation-admin@boss.local"
    echo "    (no admin email/credentials — provisioning ${BOSS_BOOTSTRAP_ADMIN_EMAIL})"
fi
DATABASE_URL="postgres://boss:boss@127.0.0.1/boss" \
BOSS_EPOCH_START="$START" \
BOSS_BOOTSTRAP_ADMIN_EMAIL="${BOSS_BOOTSTRAP_ADMIN_EMAIL:-}" \
BOSS_AUTH_FILE="$BOSS_AUTH_FILE" \
    "$REPO_ROOT/target/release/boss-operator-baseline-seed" \
    --seed-path "$REPO_ROOT/infra/operator-baseline/operator_hires.toml" \
    | grep -E "operator hired|seed complete|skipping|bootstrap-admin" || true

# -- Step 3: prepare the brewery tenant (Workflows + policy + data) --
# One converged call (the prepare_model lib fn) publishes the brewery
# Workflows via real `workflow-design` Jobs, seeds tenant policy grants
# (core ships only platform rules), and POSTs accounts / vendors /
# employees / messages / bulletins / calendar + FG + raw + asset
# opening balances. Replaces the old bootstrap + policy-bootstrap +
# data-seed trio so CI drives identical seed code to the live demo
# (seed-brewery-tenant.sh runs this same `prepare`). Per-service
# routing (no gateway in CI); classes were seeded in 2.5 and prepare
# re-seeds them idempotently. Idempotent + retry-safe throughout.
echo "==> [3/10] preparing brewery tenant (boss-brewery-sim prepare)"
BOSS_SIM_SEEDS_DIR="$SEEDS_DIR" BOSS_EPOCH_START="$START" \
    "$REPO_ROOT/target/release/boss-brewery-sim" prepare

# -- Step 5: pre-seed inventory_items --------------------------
# Step 3's `prepare` seeds raw inventory_items + their opening JEs
# from `parts.toml` (ensure_raw_inventory_opening_balances). Check
# the SKU file exists so a missing one surfaces now, not at sim time
# when consume calls would 404.
echo "==> [5/10] verifying raw inventory_items seed (parts.toml)"
if [ ! -f "$SEEDS_DIR/parts.toml" ]; then
    echo "WARNING: $SEEDS_DIR/parts.toml not found — inventory consume calls will 404" >&2
    echo "         (the brewery seed needs every SKU referenced by inventory.parts.consume)" >&2
fi

# -- Step 5.5: barrier — wait for the people projection to warm -
# Seeding is not a natural event path: the workforce is bulk-loaded in
# step 4, but the people projection the dispatcher reads (/api/people)
# drains asynchronously. The sim opens day-1 Jobs the instant it starts;
# if the roster is not queryable yet, their role-bearing tier-0 steps
# find no eligible employee. The dispatcher NAKs + redelivers (correct),
# but a cold roster that outlasts the redelivery budget dead-letters the
# step. Barrier here so the workforce is fully present before any Job
# opens — the sim's premise is an existing brewery's existing workforce.
# Query the live model for readiness rather than guessing a sleep: poll
# until /api/people plateaus (the hire backlog has drained).
echo "==> [5.5/10] waiting for people projection to warm (roster readiness)"
ppl_prev=-1; ppl_stable=0
for _i in $(seq 1 90); do
    ppl_n=$(curl -s "http://127.0.0.1:7500/api/people" 2>/dev/null \
        | jq -e 'length' 2>/dev/null || echo 0)
    if [ "$ppl_n" -gt 100 ] && [ "$ppl_n" = "$ppl_prev" ]; then
        ppl_stable=$((ppl_stable + 1))
        if [ "$ppl_stable" -ge 3 ]; then
            echo "    roster ready: ${ppl_n} employees (stable, after ${_i}s)"
            break
        fi
    else
        ppl_stable=0
    fi
    ppl_prev=$ppl_n
    sleep 1
done
if [ "$ppl_stable" -lt 3 ]; then
    echo "WARNING: people projection did not stabilize in 90s (last count: ${ppl_prev})" >&2
fi

# -- Step 6: run the canonical sim in --hard-fail mode ---------
# Stop the async runner FIRST. The regen runs side effects in-
# process via `--local-side-effects` so every dispatch is
# synchronous — no NATS-queue race that surfaces as 30-sim-days-
# later 404s on PUT /paid (the original audit failure). Restart
# the runner in step 7 once the engine is done.
#
# v1.0.10 F15: boss-step-effects-runner retired. Step-completion
# side effects route through the dispatcher's rule registry now
# (infra/dispatcher/rules/). The brewery-engine runs WITHOUT
# --local-side-effects so the in-process bridges stay silent —
# every domain-write side effect flows engine → jobs-api PUT
# step → jobs-api `step.done.<kind>` NATS event → dispatcher
# rule → domain HTTP API. Matches the production wiring exactly
# and stress-tests the new dispatcher pipeline end-to-end.
echo "==> [6/10] running the brewery regen for $DAYS days from $START (boss-brewery-sim run)"
# `boss-brewery-sim run` drives the bounded regen via run_brewery_live —
# the SAME driver the live daemon's per-tick loop is built on, so CI and
# the demo can't diverge. Hard-fail by default (any non-2xx aborts).
# Env-driven (the same BOSS_REGEN_* knobs as before, now read by the
# subcommand):
#  - BOSS_SIM_CALLBACK_BIND wires the external-party callback loop that
#    drives the counterparties (AR-aging collections, courier scans, …).
#    The system stays sim-agnostic: the dispatcher PUSHES live events to
#    its webhook (BOSS_EVENT_WEBHOOK_URL=http://127.0.0.1:7099/callback,
#    set on the boss-dispatcher unit) and the regen RECEIVES them on
#    127.0.0.1:7099, drains them onto its bus, and ticks the
#    counterparties. Unset → the receiver stays dark and ZERO collections
#    fire (every invoice sits unpaid), so the canonical regen MUST set it.
#  - BOSS_REGEN_DRAIN_PAUSE pauses after each sim-day's flush so the async
#    dispatcher finishes creating that day's invoices before the regen
#    advances (keeps the collection-PUT-before-invoice 404 window small).
env BOSS_SIM_CALLBACK_BIND="${BOSS_SIM_CALLBACK_BIND:-127.0.0.1:7099}" \
    BOSS_SIM_API_BASE="direct://127.0.0.1" \
    BOSS_SIM_SEEDS_DIR="$SEEDS_DIR" \
    BOSS_REGEN_DAYS="$DAYS" \
    BOSS_REGEN_START="$START" \
    BOSS_REGEN_WARP="$WARP" \
    BOSS_REGEN_POLL_SLEEP_MS="$POLL_SLEEP_MS" \
    BOSS_REGEN_DRAIN_PAUSE="${BOSS_REGEN_DRAIN_PAUSE:-3000}" \
    "$REPO_ROOT/target/release/boss-brewery-sim" run

# -- Step 7: drain dispatcher + step-effects NATS queues -------
echo "==> [7/10] waiting for dispatcher rules + downstream services to drain"
# The dispatcher subscribes to NATS topics declared in its rules
# registry. Give it time to dispatch any backlog before we capture
# audit_log.
sleep 10

# -- Step 7a: dispatcher side-effect health gate ---------------
# Surface side-effect failures the dispatcher could not recover from.
# Handlers now run on durable JetStream consumers: a transient failure
# (policy-api blip, DB saturation) is NAK'd and redelivered on a backoff
# schedule, so it self-heals and is NOT a defect. Only a DEAD-LETTER —
# an event that exhausted its redelivery budget — is a genuinely-stuck
# side effect, the modern equivalent of the old silent-drop. Fail loudly
# on those. (Transient NAKs are logged at warn but deliberately not in
# this pattern; grep the journal for "NAK for redelivery" to see how many
# transients were ridden out.)
echo "==> [7a/10] dispatcher side-effect health gate"
DEAD_LETTERS=$(journalctl -u boss-dispatcher --since "$RUN_STARTED" --no-pager 2>/dev/null \
    | grep -cE "DEAD-LETTER" || true)
if [[ "${DEAD_LETTERS:-0}" -gt 0 ]]; then
    echo "ERROR: $DEAD_LETTERS dispatcher dead-letter(s) this run — a side effect exhausted redelivery and is permanently stuck:" >&2
    journalctl -u boss-dispatcher --since "$RUN_STARTED" --no-pager 2>/dev/null \
        | grep -E "DEAD-LETTER" | grep -oE "subject=[a-z._*-]+ .*error=[^\"]*" | sort | uniq -c | sort -rn | head -10 >&2
    exit 1
fi
# For visibility: how many transient failures self-healed via redelivery.
REDELIVERED=$(journalctl -u boss-dispatcher --since "$RUN_STARTED" --no-pager 2>/dev/null \
    | grep -cE "NAK for redelivery" || true)
echo "    clean — no dead-letters (${REDELIVERED:-0} transient failure(s) self-healed via redelivery)"

# -- Step 7b: fiscal-year close --------------------------------
# Roll revenue + expense balances into retained earnings so the
# balance sheet ties out. Without this the 4xxx/5xxx/6xxx
# accounts accumulate against $0 in 3000 Retained Earnings and
# the playground BS perpetually shows "net income hasn't been
# closed yet". One yearly period per calendar year the sim
# covered; close only the years that are strictly in the past
# (the in-flight year stays open so December activity isn't
# truncated). The close endpoint is idempotent — re-running
# the regen script over a fresh DB still works.
echo "==> [7b/10] creating + closing fiscal-year periods"
START_YEAR=$(date -u -d "$START" +%Y)
END_YEAR=$(date -u -d "$START + $DAYS days" +%Y)
CURRENT_YEAR=$(date -u +%Y)
for YEAR in $(seq "$START_YEAR" "$END_YEAR"); do
    # Create the yearly period.
    CREATE_RESP=$(curl -sS -w '\n%{http_code}' -X POST \
        -H "Content-Type: application/json" \
        -H "X-Boss-Actor: ledger-regen" \
        -d "{\"year\": ${YEAR}}" \
        "http://127.0.0.1:7080/api/ledger/periods" || echo $'\n000')
    HTTP_CODE=$(echo "$CREATE_RESP" | tail -n1)
    BODY=$(echo "$CREATE_RESP" | sed '$d')
    if [[ "$HTTP_CODE" != "200" && "$HTTP_CODE" != "201" && "$HTTP_CODE" != "409" ]]; then
        echo "ERROR: failed to create FY${YEAR} period (HTTP $HTTP_CODE): $BODY" >&2
        exit 1
    fi
    PERIOD_ID=$(echo "$BODY" | jq -r '.id // .period_id // ""' 2>/dev/null || true)
    if [[ -z "$PERIOD_ID" ]]; then
        # Fall back to lookup if the response didn't include the id
        # (e.g., 409 idempotent return).
        PERIODS_JSON=$(curl -sS "http://127.0.0.1:7080/api/ledger/periods?kind=year")
        PERIOD_ID=$(printf '%s' "$PERIODS_JSON" | jq -r --arg y "$YEAR" '
            (if type == "array" then . else (.data // .rows // []) end)
            | map(select((.starts_on // "") | startswith($y + "-")))
            | .[0].id // ""' 2>/dev/null || true)
    fi
    if [[ -z "$PERIOD_ID" ]]; then
        echo "ERROR: couldn't resolve FY${YEAR} period id" >&2
        exit 1
    fi
    # Close only past years; the in-flight year stays open so
    # current-quarter activity isn't sealed off.
    if (( YEAR < CURRENT_YEAR )); then
        echo "    closing FY${YEAR} (period ${PERIOD_ID:0:8})"
        CLOSE_RESP=$(curl -sS -w '\n%{http_code}' -X POST \
            -H "Content-Type: application/json" \
            -H "X-Boss-Actor: ledger-regen" \
            -d '{"closed_by":"ledger-regen","retained_earnings_account":"3000"}' \
            "http://127.0.0.1:7080/api/ledger/periods/${PERIOD_ID}/close" || echo $'\n000')
        CLOSE_CODE=$(echo "$CLOSE_RESP" | tail -n1)
        CLOSE_BODY=$(echo "$CLOSE_RESP" | sed '$d')
        if [[ "$CLOSE_CODE" != "200" && "$CLOSE_CODE" != "201" ]]; then
            echo "ERROR: failed to close FY${YEAR} (HTTP $CLOSE_CODE): $CLOSE_BODY" >&2
            exit 1
        fi
        echo "      $CLOSE_BODY"
    else
        echo "    leaving FY${YEAR} open (in-flight or future year)"
    fi
done

# -- Step 8: rebuild verification ------------------------------
echo "==> [8/10] verifying rebuilders reconstruct from audit_log"
# Determinism guard: snapshot the LIVE ledger per-account balances BEFORE
# boss-rebuild-all overwrites the projections in place. The rebuild must
# reproduce them exactly from the audit_log — a divergence means a fact
# written live isn't reconstructable from the log, which is precisely the
# class of bug the 2026-06-14 tax-rebuild gap was (sales-tax + excise
# accruals posted live but the rebuild silently dropped them, and the
# conservation/dead-letter checks below couldn't see it because each is
# internally balanced). The snapshot is a separate table, so the rebuild
# leaves it intact.
PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss -q \
    -c "DROP TABLE IF EXISTS _det_ledger_snapshot;
        CREATE TABLE _det_ledger_snapshot AS
        SELECT account_id, SUM(debit_cents) AS dr, SUM(credit_cents) AS cr
        FROM gl_journal_lines GROUP BY account_id;"

"$REPO_ROOT/target/release/boss-rebuild-all" \
    --database-url "postgres://boss:boss@127.0.0.1/boss"

# Compare the rebuilt ledger to the live snapshot. Any per-account
# (debit, credit) divergence is a non-deterministic rebuild — hard-fail
# (a wrong seed must never ship). A psql error leaves DET_DIVERGENT empty,
# which defaults to "diverged" so the failure is loud, never silent.
DET_DIVERGENT=$(PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss -tA -c "
    WITH rebuilt AS (
        SELECT account_id, SUM(debit_cents) AS dr, SUM(credit_cents) AS cr
        FROM gl_journal_lines GROUP BY account_id)
    SELECT COUNT(*) FROM _det_ledger_snapshot s
    FULL OUTER JOIN rebuilt r USING (account_id)
    WHERE COALESCE(s.dr,0) <> COALESCE(r.dr,0)
       OR COALESCE(s.cr,0) <> COALESCE(r.cr,0);")
if [ "${DET_DIVERGENT:-1}" -ne 0 ]; then
    echo "ERROR: rebuilt ledger diverges from live on ${DET_DIVERGENT:-?} account(s) — a live fact is not reconstructable from audit_log (non-deterministic rebuild):" >&2
    PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss -c "
        WITH rebuilt AS (
            SELECT account_id, SUM(debit_cents) AS dr, SUM(credit_cents) AS cr
            FROM gl_journal_lines GROUP BY account_id)
        SELECT a.code, a.name,
               COALESCE(s.dr,0) AS live_dr, COALESCE(r.dr,0) AS rebuilt_dr,
               COALESCE(s.cr,0) AS live_cr, COALESCE(r.cr,0) AS rebuilt_cr
        FROM _det_ledger_snapshot s
        FULL OUTER JOIN rebuilt r USING (account_id)
        JOIN gl_accounts a ON a.id = COALESCE(s.account_id, r.account_id)
        WHERE COALESCE(s.dr,0) <> COALESCE(r.dr,0)
           OR COALESCE(s.cr,0) <> COALESCE(r.cr,0)
        ORDER BY a.code;" >&2
    PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss -q -c "DROP TABLE IF EXISTS _det_ledger_snapshot;"
    exit 1
fi
PGPASSWORD=boss psql -h 127.0.0.1 -U boss -d boss -q -c "DROP TABLE IF EXISTS _det_ledger_snapshot;"
echo "    determinism OK — rebuilt ledger matches live across all accounts"

# -- Step 8.5: conservation invariants (incl. exact GL ≡ value) --
echo "==> [8.5/10] running conservation-invariant sweep"
# The full lettered sweep the nightly timer runs, promoted to a
# regen gate. Invariants N + P are EXACT under value-primary rows
# (PR 6a): balance(1300) == Σ inventory_items.value_cents and
# balance(1320) == Σ finished_product_inventory.value_cents, to the
# cent, after a full year — the class of leak the old ±$50k / $100
# tolerances papered over no longer exists, so any hit here is a
# write path that moved stock without its JE (or vice versa).
if ! PGHOST=127.0.0.1 PGUSER=boss PGDATABASE=boss PGPASSWORD=boss     "$REPO_ROOT/infra/lint/conservation-invariants.sh"; then
    echo "ERROR: conservation invariants failed — see the lettered failures above" >&2
    exit 1
fi
echo "    conservation invariants green (N + P exact)"

# -- Step 9: dangling-FK lint ----------------------------------
echo "==> [9/10] running audit_log integrity check"
# `boss-audit-integrity-check` walks audit_log and reports
# dangling cross-event refs + chain-hash anomalies + created_at
# regressions. The integrity-check anomalies (chain-hash) are
# NOT made hard-fail because the chain-hash verification
# surfaces a known race in the trigger (BIGSERIAL id is allocated
# before the advisory_xact_lock is acquired, so concurrent
# multi-service inserts can produce id-order ≠ lock-order, which
# the verifier reads as a chain break even though the row data is
# correct). Tracked under "Deployment / infra → audit_log
# id-ordering race" in TODO.md. The rebuilders (step 8) are the
# canonical correctness check; they walk by id and reconstruct
# projections cleanly even with hash-chain anomalies present.
#
# But the binary itself MUST exist — pre-2026-05-05 the `bin`
# feature was opt-in, so `cargo build --release --workspace`
# silently produced no binary and this step turned into a no-op.
# Hard-fail on missing binary so the silent-skip class of bug
# can't recur.
INTEGRITY_BIN="$REPO_ROOT/target/release/boss-audit-integrity-check"
if [ ! -x "$INTEGRITY_BIN" ]; then
    echo "ERROR: $INTEGRITY_BIN missing or not executable — rebuild with 'cargo build --release --workspace' (boss-events has 'bin' in default features as of 2026-05-05)" >&2
    exit 1
fi
"$INTEGRITY_BIN" \
    --database-url "postgres://boss:boss@127.0.0.1/boss" || \
    echo "    (integrity check reported anomalies — informational only; see TODO)"

echo
echo "==> validation complete — all gates green."
echo "    Sim ran $DAYS days from $START, rebuilt deterministically, and"
echo "    passed conservation + dangling-FK integrity checks."
