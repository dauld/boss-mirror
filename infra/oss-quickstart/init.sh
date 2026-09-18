#!/usr/bin/env bash
# Init container — runs on EVERY start, converges the schema, then exits 0.
#
# init runs PRE-API: the boss-services container (which brings the API
# stack up) starts only after this exits. So init does only what can be
# done without the services:
#   1. Wait for Postgres.
#   2. Converge the per-module schema — on every start, whatever the
#      database already holds.
#   3. First start only: evict the example tenants' reference rows when
#      the declared tenant is not an example, provision the bootstrap-admin's local-auth
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

# THE DATABASE NAME HAS ONE SOURCE: the Secret's database-url, which
# every service reads as BOSS_POSTGRES_URL. Until 2026-09-16 this
# container's psql took PGDATABASE from a manifest literal (`boss`)
# beside DATABASE_URL from the Secret — two names for one fact, and the
# day they disagree (Option 3, design e652c7c6: the instance's database
# is repointed to a fresh one by switch-instance-database, 063dba4e)
# the schema would converge into one database while the services
# opened the other, and prod would be dark with every step green. So
# when DATABASE_URL is set its path names the database (the derivation
# is database-from-url.sh beside this file, tested) and wins over any
# PGDATABASE in the environment; the literal is gone from the manifest.
if [ -n "${DATABASE_URL:-}" ]; then
    PGDATABASE="$("$REPO/infra/oss-quickstart/database-from-url.sh" "$DATABASE_URL")" || exit 78
    export PGDATABASE
    echo "    database:        $PGDATABASE (from DATABASE_URL)"
fi

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
    echo "    boss-services publishes the tenant once per database (tenant_publishes"
    echo "    stamp; boss tenant publish <dir> lands a new repo row); clean restart:"
    echo "    docker compose down -v  &&  docker compose up"
fi

# ---- 3. converge schema ------------------------------------------------------
# Every start, empty database or not. migrate.sh prints the file it applied
# for each pending manifest entry and a `applied N, already recorded M, of K
# manifest entries` summary — the evidence that a converge happened. Not
# silenced: silence is what let four migrations accumulate unapplied.

echo "==> [1/5] converging per-module schema (migrate.sh, manifest order)"
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
echo "==> [2/5] seeding the platform Workflow bundle (insert-if-missing)"
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
# The remaining steps write state an operator or a running playground owns
# after the first start, so re-running them on an existing database would
# undo work rather than converge it: `boss-auth set` would reset a rotated
# bootstrap-admin password back to the default on every restart, and the
# sim_clock prime would drag a running epoch backwards. Schema convergence
# above is the part that must happen every time; this part must not.
if ! $FIRST_START; then
    echo "==> boss-init done (schema converged; first-start steps already done)."
    exit 0
fi

# ---- 3. a fresh instance carries only what its tenant declares ---------------
# 01-registries.sql and 40-ledger.sql seed the two worked examples'
# reference rows on every instance (the device shop's roles and
# departments, the brewery's location kinds, account types, equipment
# categories, two sites, its starter chart, a companies row each), and
# an applied migration is history — migrate.sh refuses a changed
# checksum — so the converge above has just put them into this fresh
# database too. Backlog 718ac982 (design e2580840 car 3): a real
# company's instance booted with a brewer's books and a refurb shop's
# org chart. This step evicts them BEFORE any service starts, on the
# first start only, and only when the tenant this instance is declared
# to run (BOSS_TENANT_DIR, the same directory the launcher publishes)
# is not one of the examples — an example tenant's rows are its own,
# and its engine's prepare expects them. What is a candidate is READ
# from the example tenants' seeds by infra/postgres/
# example-reference-rows.sh, the one derivation the forge verb
# retire-example-reference-rows also runs against an instance that has
# already booted; nothing here names a row. On a fresh database nothing
# references the rows, so every candidate goes; the SQL still judges
# each one, so a row something already points at is kept and named.
# THE TENANT'S OWN DECLARATIONS ARE NOT CANDIDATES (backlog 86835bf9):
# the same directory `boot` decided on is handed to `delete-sql`, which
# subtracts every id/code the tenant declares before judging. Measured
# 2026-09-18: without it a fresh instance whose tenant re-declares an
# example code (Algedonic's finance / marketing / sales / support
# departments, under the device shop's codes) lost those rows here and
# got them back only because the tenant publish runs after this step
# and inserts-if-absent — self-healing for classes, locations and the
# chart, but a record that said "evicted" about rows the tenant owns.
# Loud and non-fatal: a failure here leaves residue the forge verb can
# evict later, and a dead init would take the whole instance down
# (CLAUDE.md §Diagnosis: a boot guard that refuses to start takes the
# system of record with it).
echo "==> [3/5] example reference rows: keep or evict (first start, by tenant)"
DERIVE="$REPO/infra/postgres/example-reference-rows.sh"
if decision=$(BOSS_EXAMPLES_DIR="$REPO/examples" bash "$DERIVE" boot "${BOSS_TENANT_DIR:-}"); then
    echo "    $decision"
    if evicted=$(BOSS_EXAMPLES_DIR="$REPO/examples" bash "$DERIVE" delete-sql "$BOSS_TENANT_DIR" | psql -X -q -At -v ON_ERROR_STOP=1); then
        printf '%s\n' "$evicted" | sed 's/^/    /'
        echo "    ✓ example reference rows evicted — this instance carries only what its tenant declares plus what the platform needs"
    else
        echo "    WARN: eviction failed (see psql above) — the example rows are still in this database; retire them with: boss ops forge retire-example-reference-rows --dry-run <namespace>" >&2
    fi
else
    rc=$?
    if [ "$rc" = 3 ]; then
        echo "    $decision"
    else
        echo "    WARN: example-reference-rows.sh boot could not decide (exit $rc) — the example rows stay; see above" >&2
    fi
fi

# ---- 4. provision the bootstrap-admin credential -----------------------------
# The bootstrap-admin EMPLOYEE is seeded post-API by boss-services
# (services-launcher.sh → seed-operator-baseline.sh, which reads
# BOSS_BOOTSTRAP_ADMIN_EMAIL). Here we write only the matching local-auth
# credential — a file, no API needed. v1 uses a fixed default ("change-me")
# the operator MUST rotate via `boss-auth set $EMAIL` after first login. The
# file lives under /var/lib/boss/auth/credentials.toml, persisted via the
# docker volume so it survives container recreation.
echo "==> [4/5] provisioning bootstrap-admin credential"
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
echo "==> [5/5] priming sim_clock to $DEMO_EPOCH for the live playground"
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
