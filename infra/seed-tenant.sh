#!/usr/bin/env bash
# seed-tenant.sh — publish a tenant onto a running BOSS stack: `boss
# tenant publish <dir>` through the shared doors (classes, the chart,
# locations, business calendars, the company Subject, policy grants,
# people, agents, Workflows, sensors, posting rules, dispatcher rules),
# retried while the stack finishes binding (backlog ee7b62bb,
# 2026-09-16).
#
# Run by tenant-launch.sh's publish_tenant for EVERY tenant, first
# (backlog b644d727, 2026-09-17): a tenant with an engine — the
# brewery — has its engine script run after this one, for what only
# the engine seeds (accounts, vendors, opening balances) and the sim's
# reset-baseline stamp. This script stamps NO baseline — a real
# company's log is never trimmed back to "seeded day 0" — and needs no
# engine binary, only `boss`, which the image ships in /usr/local/bin
# with every other workspace binary.
#
# Idempotent + retry-safe: every door is insert-if-absent / upsert /
# 409-swallowed, so an attempt that failed on the first unreachable
# service resumes cleanly on the next. The budget (30 x 5 s) matches
# the brewery script's 150 s stack wait; a cold stack takes 30-90 s to
# bind. A REFUSED plan (the directory fails `boss tenant check`) stops
# at once — waiting cannot change a file — and the launcher's DEGRADED
# loop retries it on its own cadence.
#
# Output is CAPTURED and printed: on success as the receipt of what
# was published, on failure as the only description of why (the
# brewery script's lesson — `>/dev/null` threw away the reason and it
# had to be recovered by re-running prepare by hand).
set -euo pipefail

TENANT_DIR="${BOSS_TENANT_DIR:?BOSS_TENANT_DIR (the tenant directory: tenant.toml + seeds/) is required}"
ATTEMPTS="${BOSS_PUBLISH_ATTEMPTS:-30}"
RETRY="${BOSS_PUBLISH_RETRY_SECONDS:-5}"

if ! command -v boss >/dev/null 2>&1; then
    echo "ERROR: boss not on PATH — cannot publish the tenant at $TENANT_DIR" >&2
    exit 1
fi

echo "    publishing tenant $TENANT_DIR (boss tenant publish)"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT
for attempt in $(seq 1 "$ATTEMPTS"); do
    if boss tenant publish "$TENANT_DIR" >"$log" 2>&1; then
        cat "$log"
        echo "    ✓ tenant published ($TENANT_DIR)"
        exit 0
    fi
    if grep -q 'REFUSED' "$log"; then
        echo "ERROR: tenant publish REFUSED — the directory fails boss tenant check; not retrying:" >&2
        cat "$log" >&2
        exit 1
    fi
    echo "    (publish attempt $attempt of $ATTEMPTS failed; retrying in ${RETRY}s)"
    sleep "$RETRY"
done
echo "ERROR: tenant publish failed after $ATTEMPTS attempts — last output:" >&2
cat "$log" >&2
exit 1
