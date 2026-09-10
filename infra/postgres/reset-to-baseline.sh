#!/usr/bin/env bash
# Full reset of the brewery demo back to "seeded day 0".
#
# The demo builds itself live (see infra/seed-brewery-tenant.sh), so
# "reset to baseline" means: drop the live DB, re-apply schema +
# reference data + the brewery tenant seed, prime the clock to the demo
# epoch, and restart the sim. The sim then rebuilds the 12-month demo
# from scratch.
#
# This is the heavy, host-level reset (drop + reseed, ~1-2 min). The
# in-app "Reset" button (the restart-epoch endpoint) is the light path:
# it trims the audit_log back to epoch_baseline_audit_id — which the
# tenant seed stamps — without dropping the DB.
#
# Sequence:
#   1. Stop every boss service holding a DB connection.
#   2. Drop + recreate the live `boss` DB (re-applies schema).
#   3. Restart core services (jobs-api reconciles platform Workflows).
#   4. Apply brewery Class registry rows (POST /api/classes/batch from classes.json).
#   5. Seed operator-baseline hires + bootstrap-admin, project them
#      (FK targets for the tenant seed's account team members).
#   6. Prime sim_clock to the demo epoch (2025-04-01).
#   7. Seed the brewery tenant (Workflows + policy + accounts/vendors/
#      data) and stamp the reset baseline — infra/seed-brewery-tenant.sh.
#   8. Rebuild projections + GL from audit_log.
#   9. Start boss-brewery-sim — the live tick resumes from epoch_start.
#
# Usage:
#   sudo ./infra/postgres/reset-to-baseline.sh
#
# Optional env:
#   BOSS_BACKFILL_MONTHS=6             # how much synthetic past to lay down
#   BOSS_DEMO_EPOCH_START=YYYY-MM-DD   # override the computed start
#   BOSS_BOOTSTRAP_ADMIN_EMAIL=…       # platform-admin to re-seed

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SEEDS_DIR="$REPO_ROOT/examples/brewery/seeds"
DB_URL="postgres://boss:boss@127.0.0.1/boss"
# Six months of synthetic history ending TODAY. Relative to install,
# not a fixed date: the point is that the backfill abuts real now, so
# the live segment continues the same timeline instead of resuming
# some calendar the demo was cut against months ago.
BACKFILL_MONTHS="${BOSS_BACKFILL_MONTHS:-6}"
DEMO_EPOCH="${BOSS_DEMO_EPOCH_START:-$(date -u -d "$BACKFILL_MONTHS months ago" +%F)}"

echo "==> reset-to-baseline (live-sim demo → seeded day 0)"
echo "    repo:  $REPO_ROOT"
echo "    epoch: $DEMO_EPOCH"

echo "==> [1/9] stopping all boss services with DB connections"
# Every boss-*-api holds a Postgres pool; dropdb fails on "database is
# being accessed by other users" if any are still up. Stop them all +
# the dispatcher + sim; restart in step 3.
SERVICES_TO_STOP=(
    boss-brewery-sim
    boss-jobs-api boss-policy-api boss-people-api boss-commerce-api
    boss-inventory-api boss-shipping-api boss-assets-api boss-catalog-api
    boss-ledger-api boss-content-api boss-messages-api boss-ml-api
    boss-cybernetics boss-dispatcher boss-products-api boss-classes-api
    boss-locations-api boss-subject-kinds-api boss-calendar-api
    boss-events-api boss-accounts-api
    # clock-api + observability hold boss-DB pools, and the gateway proxies
    # them; all three must stop or dropdb fails on "database is being accessed".
    boss-clock-api boss-observability boss-gateway
)
for svc in "${SERVICES_TO_STOP[@]}"; do
    systemctl stop "$svc" 2>/dev/null || true
done
sleep 2

# Clear the brewery-sim daemon's checkpointed counterparty queue. The daemon
# persists the ar-aging chain's scheduled paid/past-due/write-off emissions
# (keyed by billing step_id) to BOSS_SIM_STATE_DIR/counterparty-queue.json so
# they survive a plain daemon restart. But those step_ids reference invoices
# the DB drop below wipes — a surviving queue replays
# PUT /api/commerce/invoices/inv-step-<id>/paid against rows that no longer
# exist, a 404 flood that drains for sim-days. The queue is DB-derived state,
# so a full reset must clear it WITH the DB. (Only this host-level reset does;
# a plain `systemctl restart boss-brewery-sim` still keeps the queue.)
rm -f "${BOSS_SIM_STATE_DIR:-/var/lib/boss-sim}/counterparty-queue.json"

# Same principle for the JetStream delivery buffer: BOSS_EVENTS retains the
# old world's events (millions of them across epochs), and the dispatcher's
# durable consumers would replay them against the just-reset database —
# every old-world step effect 404s, churns the redelivery budget, and
# dead-letters as noise. The regen path (validate-brewery-sim.sh) already
# clears it; this host-level reset must too. Services are stopped, so the
# drop-and-recreate is race-free.
"$REPO_ROOT/target/release/boss-dispatcher" --reset-stream \
    || echo "    warning: BOSS_EVENTS stream reset failed (is NATS down?); old events may replay"

echo "==> [2/9] dropping + recreating boss DB"
# --force terminates any straggler connection (a timer mid-run, or a service
# missing from the stop-list) so the drop can't be blocked.
sudo -u postgres dropdb --if-exists --force boss
sudo -u postgres createdb -O boss boss
PGPASSWORD=boss "$REPO_ROOT/infra/postgres/migrate.sh" -- psql -U boss -d boss -h 127.0.0.1 >/dev/null

echo "==> [3/9] restarting core services"
for svc in boss-policy-api boss-classes-api boss-locations-api \
           boss-subject-kinds-api boss-people-api boss-accounts-api \
           boss-assets-api boss-catalog-api boss-products-api \
           boss-commerce-api boss-inventory-api boss-shipping-api \
           boss-messages-api boss-calendar-api boss-content-api \
           boss-events-api boss-ledger-api boss-ml-api \
           boss-cybernetics boss-clock-api boss-jobs-api boss-dispatcher; do
    systemctl restart "$svc" 2>/dev/null || echo "  (skipped $svc — not installed)"
done
# Give jobs-api a beat to reconcile the platform workflow-design.
sleep 3

echo "==> [4/9] priming the sim clock to $DEMO_EPOCH via clock-api"
# clock-api is the single writer for clock state — prime through its public
# /configure endpoint, never a direct sim_clock write (an end-around bypasses
# the clock's invariants + refresher). Prime FIRST, before the API seeds
# below: event time is clock-authoritative, so the operator + tenant seeds
# (which POST through the public API) inherit the epoch from the clock and
# land their events on day 0.
#
# epoch_end is TODAY, and it is not a loop boundary any more. Warp is a
# startup PHASE: the sim runs fast once to lay down the synthetic past,
# reaches today, and then hands the clock to real time (warp 1.0) and
# keeps going with real people writing alongside it. The log is
# append-only from the seed forward — reaching the end of the window
# used to trim it back and replay the year, which is what destroyed a
# posted year-end close and a day of real user feedback.
EPOCH_END="$(date -u +%F)"
# Backfill warp. ~21s of wall per sim-day, so six months lands in about
# an hour. Sustainable only because the sim drops to one tick per
# sim-day while warp > 1 (see boss_brewery_sim.rs); at hourly
# granularity anything past ~2000 fell behind its own clock.
clock_ok=
for attempt in 1 2 3 4 5 6; do
    code=$(curl -s -o /dev/null -w '%{http_code}' -m 5 -X POST \
        -H 'content-type: application/json' \
        -d "{\"epoch_start\":\"$DEMO_EPOCH\",\"epoch_end\":\"$EPOCH_END\",\"warp_factor\":4000}" \
        "http://127.0.0.1:7060/api/clock/configure" 2>/dev/null || echo 000)
    if [[ "$code" == "200" || "$code" == "201" ]]; then
        clock_ok=1
        echo "    clock primed to $DEMO_EPOCH..$EPOCH_END @ 1000x"
        break
    fi
    echo "    (clock-api not ready: HTTP $code; retry $attempt)"
    sleep 2
done
[[ -n "$clock_ok" ]] || { echo "ERROR: clock-api /configure failed — aborting reset" >&2; exit 1; }

echo "==> [5/9] seeding brewery Class registry via /api/classes"
# Classes (roles, departments, account types) are the taxonomy employee +
# account writes validate against, so they must land before the operator +
# tenant seeds. Loaded through the public API (POST /api/classes/batch),
# never a direct psql load of classes.sql — see infra/lint/api-path-bypass-smell.sh.
class_code=$(curl -s -o /dev/null -w '%{http_code}' -m 10 -X POST \
    -H 'content-type: application/json' -H 'x-sim-origin: true' \
    -H 'x-boss-user: {"id":"automation:classes-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}' \
    --data-binary "@$SEEDS_DIR/classes.json" \
    "http://127.0.0.1:7800/api/classes/batch" 2>/dev/null || echo 000)
[[ "$class_code" == "200" || "$class_code" == "201" ]] \
    && echo "    classes seeded ($SEEDS_DIR/classes.json)" \
    || { echo "ERROR: POST /api/classes/batch failed (HTTP $class_code) — aborting reset" >&2; exit 1; }

echo "==> [6/9] seeding operator-baseline hires + bootstrap-admin via /api/people, projecting"
# operator-baseline-seed POSTs the founding operators (emp-cto / emp-ceo / …)
# and the bootstrap-admin through /api/people — clock-authoritative (the clock
# primed in step 4 stamps these at the epoch). The tenant seed (step 7)
# attaches them to brewery accounts as team members, so they must land + be
# projected first. BOSS_BOOTSTRAP_ADMIN_EMAIL re-mints the platform-admin the
# dropped DB lost.
BOSS_BOOTSTRAP_ADMIN_EMAIL="${BOSS_BOOTSTRAP_ADMIN_EMAIL:-}" \
    BOSS_AUTH_FILE="${BOSS_AUTH_FILE:-/var/lib/boss/auth/credentials.toml}" \
    "$REPO_ROOT/target/release/boss-operator-baseline-seed" \
    --seed-path "$REPO_ROOT/infra/operator-baseline/operator_hires.toml" || true
# Project the people read-model the tenant seed reads (the POSTs above emit
# events; rebuild ensures the projection is current before step 7 queries it).
"$REPO_ROOT/target/release/boss-rebuild-all" \
    --database-url "$DB_URL" --only people 2>&1 | tail -3

echo "==> [7/9] seeding brewery tenant + stamping reset baseline"
PATH="$REPO_ROOT/target/release:$PATH" \
    BOSS_DEMO_EPOCH_START="$DEMO_EPOCH" \
    BOSS_SIM_SEEDS_DIR="$SEEDS_DIR" \
    BOSS_POSTGRES_URL="$DB_URL" \
    "$REPO_ROOT/infra/seed-brewery-tenant.sh"

echo "==> [8/9] rebuilding projections + GL from audit_log"
"$REPO_ROOT/target/release/boss-rebuild-all" --database-url "$DB_URL" 2>&1 | tail -5
"$REPO_ROOT/target/release/boss" ledger rebuild --postgres-url "$DB_URL" 2>&1 | tail -3

echo "==> [9/9] starting boss-brewery-sim + bringing the edge back up"
systemctl start boss-brewery-sim
# clock-api came back in step 3; restore the observability aggregator + the
# gateway (the SPA's front door) that step 1 stopped.
systemctl restart boss-observability boss-gateway 2>/dev/null || true

echo "done. The demo rebuilds live from $DEMO_EPOCH."
