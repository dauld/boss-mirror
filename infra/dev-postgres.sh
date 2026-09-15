#!/usr/bin/env bash
# Start a throwaway Postgres for the DB-backed half of the test suite,
# apply the schema, and print the environment the tests read.
#
# WHY THIS EXISTS. `infra/gate.sh` runs `cargo test --all-features`,
# which includes every `#![cfg(feature = "postgres")]` test. On a dev
# box with no database those do not skip — they FAIL, with
# "Connection refused", one panic per test. The failure is
# indistinguishable at a glance from a real regression, so the habit it
# trains is to wave them off as "environmental". On 2026-08-13 that
# habit cost a red train: `station_event_kinds_are_registered` broke for
# a real reason (migration 120 added a fourth `jobs.station.*` kind and
# the roster assertion still expected three), was pushed anyway because
# the same four `job_edges_pg` cases had been failing on Connection
# refused all day, and CI found it seventeen minutes later. A red signal
# you have trained yourself to ignore is worse than no signal.
#
# There is a second, quieter trap this file is the answer to. Running
# `cargo test -p boss-jobs --test stations_pg` WITHOUT `--all-features`
# compiles the whole file away and reports "test result: ok. 0 passed" —
# which reads exactly like success. Always pass --all-features, as
# gate.sh does.
#
#   eval "$(infra/dev-postgres.sh)"      # start + schema + export
#   cargo test --all-features            # now actually covers them
#
# Prints only the export lines on stdout so it is eval-safe; progress
# goes to stderr.
set -euo pipefail

NAME="${BOSS_DEV_PG_NAME:-boss-dev-pg}"
PORT="${BOSS_DEV_PG_PORT:-5432}"
IMAGE="${BOSS_DEV_PG_IMAGE:-postgres:16}"
ADMIN_URL="postgres://boss:boss@127.0.0.1:${PORT}/postgres"
DB_URL="postgres://boss:boss@127.0.0.1:${PORT}/boss"

say() { echo "dev-postgres: $*" >&2; }

if ! docker info >/dev/null 2>&1; then
    say "no container runtime. Start one first (colima start / Docker Desktop)."
    exit 1
fi

if grep -qx "$NAME" <<<"$(docker ps --format '{{.Names}}')"; then
    say "$NAME already running"
else
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    say "starting $NAME ($IMAGE) on :$PORT"
    docker run -d --name "$NAME" \
        -e POSTGRES_USER=boss -e POSTGRES_PASSWORD=boss -e POSTGRES_DB=boss \
        -p "${PORT}:5432" "$IMAGE" >/dev/null
fi

for _ in $(seq 1 60); do
    if docker exec "$NAME" pg_isready -U boss >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
docker exec "$NAME" pg_isready -U boss >/dev/null 2>&1 || {
    say "postgres did not become ready"
    exit 1
}

# Idempotent: migrate.sh records what it applied, so re-running is a
# no-op on an already-migrated database.
say "applying schema"
"$(dirname "$0")/postgres/migrate.sh" -- psql "$DB_URL" >&2

say "ready — eval this script's stdout to export the test environment"
echo "export BOSS_TEST_POSTGRES_ADMIN_URL='${ADMIN_URL}'"
echo "export DATABASE_URL='${DB_URL}'"
echo "export BOSS_POSTGRES_URL='${DB_URL}'"
