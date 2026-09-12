#!/usr/bin/env bash
# Round-trip-write check.
#
# For each service that handles writes, POST a sentinel row, query
# Postgres directly to confirm the row landed, then DELETE the row.
# Catches the silent in-memory-fallback class of bug: the service
# returns 200 with a payload (because it wrote to a process-local
# map), but Postgres never sees the row.
#
# Why this exists: 2026-04-28 boss-docs-api was running in-memory
# only because it was built without `--features postgres`. POSTs
# returned 200; the design_pending_decisions table stayed empty. (That
# table, that POST and the whole boss-docs service are gone since
# 2026-09-10 — the crate that named this bug class no longer exists,
# but the class does; see below.)
# Option (1) of the fix was a boot-time guard; since 2026-09-12 the
# in-memory serving arms are DELETED (be793304), so a binary without
# the postgres feature does not compile and the two runtime choosers
# refuse to start without a postgres_url. This script is the
# defense-in-depth check that catches the same class of bug at
# *runtime* — useful in cron and after deploys.
#
# Usage:
#   ./infra/check-service-write-roundtrip.sh
#
# Exit codes:
#   0 — all round-trips succeeded
#   1 — at least one check failed (a write returned 200 but Postgres
#       has no row, a list disagreed with its table, or the call
#       itself failed)

set -euo pipefail

SENTINEL="heartbeat-$(date -u +%Y%m%d%H%M%S)-$$"

# psql against the local Postgres (peer auth as `postgres` user).
psql_run() {
    sudo -u postgres psql -d boss -tAc "$1"
}

declare -a FAILURES=()

# ---------- read-consistency check ----------
# For services whose HTTP surface is read-only at the platform level
# (classes, locations: registries seeded via SQL), the equivalent
# regression signal is "HTTP-list count != Postgres row count". A
# silent in-memory fallback would expose an empty vec![] over HTTP
# while the DB still has rows — diverges instantly.
check_read_consistency() {
    local service="$1"
    local url="$2"
    local table="$3"
    local where="${4:-1=1}"
    local http_count body
    # Capture the response first, then parse — keeps the fetch and the
    # parse as two visibly separate motions (this used to be a
    # `curl … | python3` pipe, which static scanners read as an
    # unpinned download-then-run).
    body=$(curl -sS "$url" 2>/dev/null) || body=""
    http_count=$(printf '%s' "$body" \
        | jq -e 'if type == "array" then length else error("expected a JSON array") end' \
        2>/dev/null || echo "?")
    if [[ "$http_count" == "?" ]]; then
        FAILURES+=("$service: GET $url failed or returned non-JSON")
        return
    fi
    local pg_count
    pg_count=$(psql_run "SELECT COUNT(*) FROM ${table} WHERE ${where}")
    if [[ "$http_count" != "$pg_count" ]]; then
        FAILURES+=("$service: HTTP returned ${http_count} rows, Postgres has ${pg_count} (silent in-memory fallback?)")
    fi
}

# boss-docs-api was the original subject of this whole script (the
# 2026-04-28 in-memory-fallback bug above) and it is no longer checked,
# because it no longer exists. Part 1 of the corpus deletion took its
# only write endpoint; part 2, the same day, took the service, its
# three tables and the parser behind them (backlog f5da586c) — the
# packet is the doc, so there is no corpus to keep consistent with
# anything. The two read-consistency checks below carry the signal it
# used to: a list endpoint that disagrees with its table is an
# in-memory fallback, and no fallback can fake agreement.
# `boss-classes-api` requires ?subject_kind=… — pick `employee`, the
# largest classes namespace today.
check_read_consistency "boss-classes-api" \
    "http://127.0.0.1:7800/api/classes?subject_kind=employee" \
    "classes" \
    "subject_kind='employee' AND retired_at IS NULL"
check_read_consistency "boss-locations-api" \
    "http://127.0.0.1:7820/api/locations" \
    "locations" \
    "retired_at IS NULL"

# ---------- capability handshake ----------
# Services that emit `/health.capabilities.storage` get a third
# layer: confirm the binary running self-reports `storage="postgres"`.
# This catches the boss-docs class of bug at the layer closest to
# truth — the binary itself tells you what it built with. (The crate
# that named the class was deleted on 2026-09-10; the class did not go
# with it, which is why these checks stay.)
check_capability() {
    local service="$1"
    local url="$2"
    local storage
    storage=$(curl -sS -m 3 "$url" 2>/dev/null \
        | jq -re '.capabilities.storage // "?"' 2>/dev/null \
        || echo "?")
    if [[ "$storage" != "postgres" ]]; then
        FAILURES+=("$service: /health.capabilities.storage is '$storage' (expected 'postgres')")
    fi
}

check_capability "boss-people-api"   "http://127.0.0.1:7500/api/people/health"
check_capability "boss-jobs-api"     "http://127.0.0.1:7900/api/jobs/health"
check_capability "boss-messages-api" "http://127.0.0.1:7200/api/messages/health"
check_capability "boss-assets-api"    "http://127.0.0.1:7600/api/assets/health"
check_capability "boss-calendar-api" "http://127.0.0.1:7860/api/calendar/health"

if [[ ${#FAILURES[@]} -eq 0 ]]; then
    echo "ok: 9 checks passed (3 read-consistency, 6 capability handshake)."
    exit 0
fi

echo "FAIL: ${#FAILURES[@]} service(s) failed the write-round-trip:"
printf '  - %s\n' "${FAILURES[@]}"
exit 1
