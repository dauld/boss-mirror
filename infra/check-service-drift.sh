#!/usr/bin/env bash
# Drift check for Boss service units.
#
# Compares /etc/systemd/system/boss-*.service against the services
# declared in infra/deploy-services.sh plus the list of bespoke
# services tracked elsewhere in infra/. Exits non-zero on drift.
#
# Catches:
#   1. Units on disk that nothing in git installs (the boss-docs bug).
#   2. Services in infra/deploy-services.sh whose unit is disabled or
#      inactive (would die on next VM restart).
#   3. Bespoke services in infra/ whose unit is missing or inactive.
#
# Does NOT catch config-file drift — use `deploy-services.sh check`
# for that.
#
# Usage: ./infra/check-service-drift.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Services managed by deploy-services.sh (paired = both prod+scratch).
PAIRED_SERVICES=(shipping messages inventory commerce people assets kb)
SOLO_SERVICES=(sim ml)

# Services installed by bespoke infra/<name>/setup.sh or raw unit files.
# If you add a new one, put the unit name here.
BESPOKE_UNITS=(
    boss-backup.service
    boss-backup.timer
    boss-cybernetics.service
    boss-gateway.service
    boss-observability.service
    nats-server.service
)

# Build the expected-unit set from the declarations above.
declare -A EXPECTED=()
for name in "${PAIRED_SERVICES[@]}"; do
    EXPECTED["boss-${name}-api.service"]=1
    EXPECTED["boss-${name}-api-scratch.service"]=1
done
for name in "${SOLO_SERVICES[@]}"; do
    EXPECTED["boss-${name}-api.service"]=1
done
for unit in "${BESPOKE_UNITS[@]}"; do
    EXPECTED["$unit"]=1
done

# Walk units on disk and classify.
declare -a UNKNOWN=() DISABLED=() INACTIVE=()
shopt -s nullglob
for u in /etc/systemd/system/boss-*.service /etc/systemd/system/boss-*.timer /etc/systemd/system/nats-server.service; do
    name=$(basename "$u")
    if [[ -z "${EXPECTED[$name]:-}" ]]; then
        UNKNOWN+=("$name")
        continue
    fi
    # Skip oneshot units from the active/enabled checks (boss-backup.service
    # is triggered by its timer and sits inactive between runs).
    if [[ "$name" == "boss-backup.service" ]]; then
        continue
    fi
    enabled=$(systemctl is-enabled "$name" 2>&1)
    active=$(systemctl is-active "$name" 2>&1)
    if [[ "$enabled" != "enabled" && "$enabled" != "static" && "$enabled" != "enabled-runtime" ]]; then
        DISABLED+=("$name ($enabled)")
    fi
    if [[ "$active" != "active" ]]; then
        INACTIVE+=("$name ($active)")
    fi
done

# Every expected unit should actually exist on disk.
declare -a MISSING=()
for name in "${!EXPECTED[@]}"; do
    if [[ ! -f "/etc/systemd/system/$name" ]]; then
        MISSING+=("$name")
    fi
done

ok=true
if [[ ${#UNKNOWN[@]} -gt 0 ]]; then
    ok=false
    echo "DRIFT: units on disk not declared in infra/:"
    printf '  %s\n' "${UNKNOWN[@]}"
fi
if [[ ${#MISSING[@]} -gt 0 ]]; then
    ok=false
    echo "DRIFT: units declared in infra/ but missing on disk:"
    printf '  %s\n' "${MISSING[@]}"
fi
if [[ ${#DISABLED[@]} -gt 0 ]]; then
    ok=false
    echo "DRIFT: units that are not enabled (will not come back on reboot):"
    printf '  %s\n' "${DISABLED[@]}"
fi
if [[ ${#INACTIVE[@]} -gt 0 ]]; then
    ok=false
    echo "DRIFT: units that are not running:"
    printf '  %s\n' "${INACTIVE[@]}"
fi

if $ok; then
    echo "ok: all declared units are present, enabled, and active."
    # Companion check: writes round-trip to Postgres. Catches the
    # silent in-memory-fallback class of bug (boss-docs 2026-04-28).
    if [[ -x "${SCRIPT_DIR}/check-service-write-roundtrip.sh" ]]; then
        echo
        "${SCRIPT_DIR}/check-service-write-roundtrip.sh" || exit 1
    fi
    # Companion check: every workspace [[bin]] with required-features
    # has those features activated by either its crate's default or
    # by a workspace dep. Catches the silent-skip class of bug at
    # build time (boss-docs 2026-04-28; boss-audit-integrity-check
    # 2026-05-05).
    if [[ -x "${SCRIPT_DIR}/check-binary-build-coverage.sh" ]]; then
        echo
        "${SCRIPT_DIR}/check-binary-build-coverage.sh" || exit 1
    fi
    exit 0
fi
exit 1
