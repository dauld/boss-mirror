#!/usr/bin/env bash
# seed-operator-baseline.sh — POST the platform operator-baseline
# (emp-audit + the bootstrap-admin, injected from
# BOSS_BOOTSTRAP_ADMIN_EMAIL or the first BOSS_AUTH_FILE credential)
# to a RUNNING people-api.
#
# Post-API by design: boss-operator-baseline-seed POSTs /api/people —
# the policy-checked, service-mediated path — so the API stack must be
# up. (It used to write audit_log directly via sqlx; that end-around
# was removed, which is why the old pre-API call sites in the docker
# init container + the bare-metal quickstart broke.) This script is the
# converged post-API seed step, called by the quickstart launchers
# (tenant-launch.sh: before the brewery tenant seed, AFTER the generic
# `boss tenant publish` of a tenant with no engine — backlog 0d2d7daa,
# 2026-09-16) — the platform-baseline sibling of
# infra/seed-brewery-tenant.sh, and the same thing reset-to-baseline
# does inline at its step 6.
#
# Binaries are PATH-resolved: docker ships them in /usr/local/bin;
# bare-metal callers prepend target/release. Idempotent (409 on a
# duplicate id = skip; the bootstrap-admin injection asks the people
# API who holds the email before injecting); retries while the
# people-api finishes binding.

set -euo pipefail

SEEDS_TOML="${BOSS_OPERATOR_BASELINE_TOML:-/opt/boss/infra/operator-baseline/operator_hires.toml}"

if ! command -v boss-operator-baseline-seed >/dev/null 2>&1; then
    echo "    WARN: boss-operator-baseline-seed not on PATH — admin login won't work" >&2
    exit 0
fi

echo "    seeding operator-baseline + bootstrap-admin via /api/people"
# Q7 made this row LOAD-BEARING: owner resolution requires a human
# platform-admin, so a fresh install without the bootstrap-admin cannot
# open ANY platform Job - brewery prepare 400s on every kind and the
# whole install is dead on arrival (found by PR #233's install smoke).
# The old shape here failed exactly that way on a cold runner: an
# 18-second budget (6x3s) for a whole stack's readiness, the binary's
# output discarded, then warn-and-continue. Now: a readiness budget
# sized like the pg wait (a 2-vCPU CI runner takes minutes, not
# seconds), and on failure everything the binary said is SHOWN - a
# seed that fails silently is how the last three fresh-install defects
# stayed invisible.
ok=
for attempt in $(seq 1 40); do
    # BOSS_BOOTSTRAP_ADMIN_EMAIL / BOSS_AUTH_FILE are read from the
    # environment by the binary to inject the platform-admin row.
    if SEED_OUT="$(boss-operator-baseline-seed --seed-path "$SEEDS_TOML" 2>&1)"; then
        ok=1
        echo "    ✓ operator-baseline seeded (attempt $attempt)"
        break
    fi
    echo "    (people-api not ready; retry $attempt/40)"
    sleep 4
done
if [[ -z "$ok" ]]; then
    {
        echo
        echo "!! OPERATOR-BASELINE SEED FAILED after 40 attempts (~160s)."
        echo "   No bootstrap-admin means owner resolution cannot name a"
        echo "   human for ANY platform Job (Q7) - this install is not"
        echo "   usable until seeded by hand. Last attempt output:"
        printf '%s\n' "$SEED_OUT"
        echo
    } >&2
fi
