#!/usr/bin/env bash
# Init container — runs on EVERY start, converges the schema, then exits 0.
#
# init runs PRE-API: the boss-services container (which brings the API
# stack up) starts only after this exits. So init does only what can be
# done without the services:
#   1. Wait for Postgres.
#   2. Converge the per-module schema — on every start, whatever the
#      database already holds.
#   3. First start only: provision the bootstrap-admin's local-auth
#      credential (a file write) and prime the formula clock.
#
# Everything that goes through the public API — the operator-baseline +
# bootstrap-admin EMPLOYEE, the brewery tenant (classes, Workflows, policy,
# accounts/vendors/data), and the sim that builds the demo live — is run by
# boss-services (services-launcher.sh) once the API is up. That's why
# operator/employee seeding can't live here: boss-operator-baseline-seed
# POSTs /api/people, which isn't listening during init.
#
# Why step 2 is unconditional
# ---------------------------
# This script used to ask "is the schema present?" and exit 0 if it was —
# so it initialized an EMPTY database and nothing else in the deploy path
# ever applied a NEW migration to an existing one. On 2026-08-13 that let
# four migrations (112, 113, 114, 116) accumulate unapplied on the cluster
# while the image, the code and the config all rolled forward: the station
# registry shipped, the deploy reported success, and `GET /api/stations`
# answered 500 `relation "stations" does not exist`. Schema is part of the
# tree and converges from the tree like the rest of it.
#
# Converging is safe to repeat: migrate.sh is idempotent by ledger — it
# applies only manifest entries not yet recorded in schema_migrations, each
# in one transaction with its bookkeeping row. A converge against an
# up-to-date database applies nothing and says so.
#
# A migration failure FAILS this container, and so the pod: a half-migrated
# database that keeps serving is worse than a visible failure.
#
# The first-start-only steps stay first-start-only on purpose. Re-running
# `boss-auth set` would reset an operator's rotated password to the default
# on every restart, and re-priming sim_clock would drag a running
# playground's epoch backwards. Clean restart is `docker compose down -v`
# then `up`.

set -euo pipefail

REPO=/opt/boss
EMAIL="${BOSS_BOOTSTRAP_ADMIN_EMAIL:?BOSS_BOOTSTRAP_ADMIN_EMAIL must be set}"
EMAIL="${EMAIL,,}"

echo "==> boss-init starting"
echo "    bootstrap-admin: $EMAIL"
echo "    mode:            converge schema from the tree, then live sim from empty"

# ---- 1. wait for Postgres ----------------------------------------------------

for i in $(seq 1 30); do
    if pg_isready -h "$PGHOST" -U "$PGUSER" -q; then
        break
    fi
    echo "    waiting for postgres ($i/30)..."
    sleep 2
done

# ---- 2. first start, or an existing database? --------------------------------
# "schema present" (subject_kinds exists) means a prior init ran. This no
# longer decides whether the schema converges — it always does, below —
# only whether the once-per-database steps [2/3] and [3/3] run.
SUBJECT_KINDS_EXISTS=$(psql -At -c "SELECT to_regclass('subject_kinds')" 2>/dev/null || echo "")
FIRST_START=true
if [[ -n "$SUBJECT_KINDS_EXISTS" ]]; then
    FIRST_START=false
    echo "==> existing database (schema present) — converging it, first-start seeds skipped"
    echo "    boss-services re-seeds the tenant on every up; clean restart:"
    echo "    docker compose down -v  &&  docker compose up"
fi

# ---- 3. converge schema ------------------------------------------------------
# Every start, empty database or not. migrate.sh prints the file it applied
# for each pending manifest entry and a `applied N, already recorded M, of K
# manifest entries` summary — the evidence that a converge happened. Not
# silenced: silence is what let four migrations accumulate unapplied.

echo "==> [1/4] converging per-module schema (migrate.sh, manifest order)"
if ! "$REPO/infra/postgres/migrate.sh"; then
    {
        echo
        echo "!! SCHEMA CONVERGE FAILED — boss-init is exiting nonzero."
        echo
        echo "   The services are NOT being started against a half-migrated"
        echo "   database. See migrate.sh's error above: it names the entry"
        echo "   that failed (its transaction rolled back, nothing from it"
        echo "   was kept) or the reason the run was refused."
        echo
        echo "   A database that predates the migration runner has to be"
        echo "   adopted once, by hand:  migrate.sh --baseline"
    } >&2
    exit 1
fi

# ---- platform Workflow bundle, every start ----------------------------------
# Protocols shipped as DATA (infra/platform/workflows/) reach the
# registry through boss-platform-workflow-seed — insert-if-missing, so
# re-running converges rather than overwrites, and a bundle kind that
# arrives with an image update seeds on the restart that delivers it.
# THIS PATH WAS MISSING ENTIRELY: bootstrap-db.sh (the cluster/gcp boot)
# had the seed, the quickstart's init never did, and a fresh install came
# up without workflow-design/backlog-item/regenerate-deployment — brewery
# prepare then died on a 400 four steps of silence later. Found by the
# public mirror's install smoke, twice (2026-08-20/21). Output is shown
# UNFILTERED and a failure warns loudly but does not kill init: the
# downstream prepare error now has its cause printed directly above it.
echo "==> [2/4] seeding the platform Workflow bundle (insert-if-missing)"
SEED_URL="postgres://${PGUSER}:${PGPASSWORD:-}@${PGHOST}:${PGPORT:-5432}/${PGDATABASE:-$PGUSER}"
if ! boss-platform-workflow-seed \
    --database-url "$SEED_URL" \
    --seed-path "$REPO/infra/platform/workflows"; then
    {
        echo
        echo "!! PLATFORM BUNDLE SEED FAILED — bundle-supplied Workflow kinds"
        echo "   are missing or stale in this deployment. The seed's own error"
        echo "   is printed above. Services still start; tenant prepare will"
        echo "   name the first missing kind it hits."
        echo
    } >&2
fi

# The demo builds itself live: boss-services seeds the operator-baseline +
# brewery tenant through the public API and starts the sim, which grows the
# audit_log from empty. There's no bulk seed load and no pre-API rebuild —
# audit_log is empty until the services run (see services-launcher.sh).

# ---- everything below is FIRST START ONLY ------------------------------------
# Both remaining steps write state an operator or a running playground owns
# after the first start, so re-running them on an existing database would
# undo work rather than converge it: `boss-auth set` would reset a rotated
# bootstrap-admin password back to the default on every restart, and the
# sim_clock prime would drag a running epoch backwards. Schema convergence
# above is the part that must happen every time; this part must not.
if ! $FIRST_START; then
    echo "==> boss-init done (schema converged; first-start steps already done)."
    exit 0
fi

# ---- 4. provision the bootstrap-admin credential -----------------------------
# The bootstrap-admin EMPLOYEE is seeded post-API by boss-services
# (services-launcher.sh → seed-operator-baseline.sh, which reads
# BOSS_BOOTSTRAP_ADMIN_EMAIL). Here we write only the matching local-auth
# credential — a file, no API needed. v1 uses a fixed default ("change-me")
# the operator MUST rotate via `boss-auth set $EMAIL` after first login. The
# file lives under /var/lib/boss/auth/credentials.toml, persisted via the
# docker volume so it survives container recreation.
echo "==> [3/4] provisioning bootstrap-admin credential"
DEFAULT_PASSWORD="${BOSS_BOOTSTRAP_ADMIN_PASSWORD:-change-me}"
export BOSS_AUTH_FILE="${BOSS_AUTH_FILE:-/var/lib/boss/auth/credentials.toml}"
mkdir -p "$(dirname "$BOSS_AUTH_FILE")"
# `boss-auth set` is a no-flag CLI: piped stdin is the new password.
# Don't suppress stderr — when this fails, the actual error is the
# whole story (missing dir perms, tty detection, etc.).
if echo "$DEFAULT_PASSWORD" | boss-auth set "$EMAIL"; then
    echo "    ✓ Credential set for $EMAIL (password: $DEFAULT_PASSWORD)"
    echo "    ⚠  Rotate it with: docker compose exec boss-services boss-auth set $EMAIL"
else
    echo "    WARN: failed to provision credential for $EMAIL — see stderr above"
fi

# ---- 5. prime the formula clock for the live playground ----------------------
# The brewery-sim is clock-authoritative: it reads /api/clock/now to pick the
# sim-day to advance, and boss-clock-api runs in sim mode (BOSS_CLOCK_MODE=sim
# in compose), reading sim_clock at startup. Prime the row to the demo epoch
# (fixed 2025-04-01, override via BOSS_DEMO_EPOCH_START) so the playground
# ticks forward at 1000x instead of sitting frozen at wall-time. The post-API
# seeds (in services-launcher.sh) run against this clock so their events land
# on day 0. This is a direct sim_clock write because clock-api isn't up yet.
DEMO_EPOCH="${BOSS_DEMO_EPOCH_START:-2025-04-01}"
echo "==> [4/4] priming sim_clock to $DEMO_EPOCH for the live playground"
# epoch_end = epoch_start + 365 gives the playground a 12-month range; without
# an epoch_end past epoch_start the loop is zero-length and the sim auto-pauses
# on the first tick ('epoch complete'), leaving the demo frozen.
if psql -v ON_ERROR_STOP=1 -c "
    INSERT INTO sim_clock
        (id, epoch_start_date, epoch_end_date, warp_factor, wall_anchor,
         paused, paused_offset_seconds, restart_in_progress)
    VALUES
        (1, DATE '$DEMO_EPOCH', DATE '$DEMO_EPOCH' + 365, 1000, NOW(),
         false, 0, false)
    ON CONFLICT (id) DO UPDATE SET
        epoch_start_date = EXCLUDED.epoch_start_date,
        epoch_end_date   = EXCLUDED.epoch_end_date,
        warp_factor      = EXCLUDED.warp_factor,
        wall_anchor      = EXCLUDED.wall_anchor;" >/dev/null; then
    echo "    ✓ formula clock primed to $DEMO_EPOCH @ 1000x warp"
else
    echo "    WARN: sim_clock prime failed; playground will sit at wall-time" >&2
fi

echo "==> boss-init done."
