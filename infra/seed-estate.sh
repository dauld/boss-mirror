#!/usr/bin/env bash
# seed-estate.sh — publish the tree's estate declaration onto a running
# BOSS stack: `boss estate declare <estate.toml>` through the estate
# door (POST /api/estate/nodes/batch, insert-if-absent), retried while
# the stack finishes binding (backlog ee368d0c, 2026-09-18).
#
# Run by tenant-launch.sh's publish_tenant BEFORE the operator baseline
# and the tenant: the estate is the instance's, not the tenant's, and
# the converges that read a host's roles off /api/estate/nodes
# (infra/estate/node-roles.sh) and the observer that compares declared
# against observed both need the declaration before anything else
# does. Until this script the estate reached a database only as a
# schema migration, so every fresh database — an adopter's included —
# declared this LAN's machines; now a fresh database declares nothing
# until this has run, and what it declares is infra/estate/estate.toml,
# the one file the estate lives in.
#
# Idempotent + retry-safe: the door keeps a node already there and adds
# only a role not yet on it, so an attempt that failed on an unreachable
# service resumes cleanly. A REFUSED declaration (the file fails the
# loader, or the door refuses a row by name) stops at once — waiting
# cannot change a file — and the launcher's DEGRADED loop retries on its
# own cadence. Output is captured and printed, success or failure
# (seed-tenant.sh's lesson: `>/dev/null` threw the reason away).
set -euo pipefail

SOURCE="${BOSS_ESTATE_SOURCE:-${BOSS_INFRA_DIR:-/opt/boss/infra}/estate/estate.toml}"
ATTEMPTS="${BOSS_PUBLISH_ATTEMPTS:-30}"
RETRY="${BOSS_PUBLISH_RETRY_SECONDS:-5}"

if ! command -v boss >/dev/null 2>&1; then
    echo "ERROR: boss not on PATH — cannot declare the estate from $SOURCE" >&2
    exit 1
fi
if [ ! -r "$SOURCE" ]; then
    echo "ERROR: no estate source at $SOURCE — the image carries infra/estate/estate.toml beside this script; nothing is declared" >&2
    exit 1
fi

echo "    declaring the estate from $SOURCE (boss estate declare)"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT
for attempt in $(seq 1 "$ATTEMPTS"); do
    if boss estate declare "$SOURCE" >"$log" 2>&1; then
        cat "$log"
        echo "    ✓ estate declared ($SOURCE)"
        exit 0
    fi
    if grep -Eq '→ 4(00|22) ' "$log"; then
        echo "ERROR: estate declaration REFUSED — the door refused a row; not retrying:" >&2
        cat "$log" >&2
        exit 1
    fi
    echo "    (estate attempt $attempt of $ATTEMPTS failed; retrying in ${RETRY}s)"
    sleep "$RETRY"
done
echo "ERROR: estate declaration failed after $ATTEMPTS attempts — last output:" >&2
cat "$log" >&2
exit 1
