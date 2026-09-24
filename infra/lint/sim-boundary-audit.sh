#!/usr/bin/env bash
# Sim/system boundary audit.
#
# Rule: code that runs OUTSIDE the system (the simulator, tenant
# engine drivers, seed loaders) must interact with the system
# only through its public HTTP API. That means a sim-side crate's
# Cargo.toml may not declare a path dep on a system-side
# implementation crate (boss-{accounts,commerce,inventory,...})
# — those crates bundle the Postgres adapter, the HTTP handlers,
# and internal types in one binary surface. Importing them lets
# the simulator reach behind the API and write directly to the
# domain's state, defeating the whole point of running the
# simulator AS a pressure test of the API.
#
# What's allowed:
#   - boss-{domain}-client crates (HTTP contract types + thin client)
#   - boss-core, boss-ports, boss-nats, boss-jobs (shared primitives
#     + the Job/Step registry the sim necessarily speaks)
#   - boss-clock-client, boss-policy-client (cross-cutting client deps)
#   - any non-boss-* dep
#
# What's banned:
#   - boss-{accounts, assets, commerce, inventory, ledger, messages,
#           ml-plugins, people, products, shipping, catalog}
#     — the implementation crates in crates/modules/
#
# Allowlist below grandfathers in the 6 current violators discovered
# at v1.0.10 audit time. Migrating each off the allowlist is the
# follow-up slice — moves domain types the sim needs into the
# matching *-client crate (creating one when none exists yet) and
# switches the dep to *-client. The lint fails the build the moment
# a NEW sim-side dep on an impl crate lands, so we can't drift further
# while we're paying off the existing six.
#
# Wire into CI alongside tier-import-audit.sh. Exit 0 = clean,
# 1 = unexpected violation (allowlisted entries don't count).
#
# THIS LINT SCANNED NOTHING FROM 2026-06-26 TO 2026-09-18 (backlog
# cdf2d959, audit H2). The awk that lists a Cargo.toml's dependency
# names matched them with `\s*=`, and mawk — the awk on this pod and
# in the gate image — has no `\s`: it reads a literal `s`, so no line
# matched, "0 dep declarations scanned" was printed under `clean` on
# every run, and the six-entry allowlist below was never compared to
# anything. The class is `[[:space:]]`, which every awk has; the count
# is now a verdict (lib/scanned.sh) and the allowlist's two staleness
# checks are the shared ones (lib/allowlist.sh).

set -euo pipefail

LINT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$LINT_DIR/../.."
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3
# shellcheck source=infra/lint/lib/allowlist.sh
. "$LINT_DIR/lib/allowlist.sh" || exit 3

LINT=sim-boundary-audit

# Sim-side crates: anything that drives the system from outside.
# Find them by location convention:
#   - crates/orchestrators/boss-sim         (the simulator core)
#   - crates/tenants/*-engine               (every tenant engine)
# Seed loader binaries live inside their parent impl crate today
# (boss_operator_baseline_seed.rs is in boss-people, etc.) — those
# are tracked in the seed-loader-boundary follow-up; this lint covers
# the cleanly-separable sim-side crates only.
#
# The engines are the GLOB, not a list: the list named both tenants and
# outlived neither's rename, and when the used-device shop's engine was
# deleted (backlog a8991c86, car 7) its line would have been a path that
# silently matched nothing — the `[[ -f ]]` below skipped it on every run.
SIM_SIDE_CRATES=()
for path in \
    crates/orchestrators/boss-sim \
    crates/tenants/*-engine
do
    [[ -f "$path/Cargo.toml" ]] && SIM_SIDE_CRATES+=("$path")
done

# Banned impl crates. Mechanically: every directory under
# crates/modules/ that doesn't end in -client.
BANNED_CRATES=()
for path in crates/modules/*/; do
    name=$(basename "$path")
    [[ "$name" == *-client ]] && continue
    BANNED_CRATES+=("$name")
done

# Direct database drivers. The impl-crate ban above stops the Postgres
# ADAPTERS; this stops a sim-side crate from declaring a raw DB driver
# (e.g. sqlx) that would let it reach behind the public API straight
# into the database. No allowlist — direct DB access from the sim is
# never acceptable. (The simulator only ever touches the system through
# the public HTTP API.)
BANNED_DB_DRIVERS=("sqlx" "tokio-postgres" "deadpool-postgres" "diesel" "postgres" "rusqlite")

# Direct message-bus / event-store crates. Same rule as the DB drivers:
# a sim-side crate pulling boss-nats (the bus) or boss-events (the event
# store) could publish or persist behind the public API. The sim emits
# only through the public HTTP API (LiveApiOutput -> POST /api/...).
BANNED_BUS_CRATES=("boss-nats" "boss-events")

# Allowlist of CURRENT violators. Each entry is
# `<sim-crate-path>::<banned-dep-name>`. The lint succeeds when a
# violation appears here, fails when it doesn't. Empty the list
# once the migration is complete.
ALLOWLIST=(
    # boss-sim — the simulator core.
    "crates/orchestrators/boss-sim::boss-assets"
    "crates/orchestrators/boss-sim::boss-catalog"
    "crates/orchestrators/boss-sim::boss-commerce"
    "crates/orchestrators/boss-sim::boss-shipping"
    # boss-brewery-engine — Algedonic Ales tenant engine.
    "crates/tenants/boss-brewery-engine::boss-accounts"
    "crates/tenants/boss-brewery-engine::boss-inventory"
)

is_allowlisted() {
    local key="$1"
    for entry in "${ALLOWLIST[@]}"; do
        [[ "$entry" == "$key" ]] && return 0
    done
    return 1
}

violations=0
unexpected_violations=()
allowlisted_used=""
total_deps_audited=0

for crate_path in "${SIM_SIDE_CRATES[@]}"; do
    toml="$crate_path/Cargo.toml"
    crate_name=$(basename "$crate_path")
    while IFS= read -r dep; do
        [[ -z "$dep" ]] && continue
        total_deps_audited=$((total_deps_audited + 1))
        for banned in "${BANNED_CRATES[@]}"; do
            if [[ "$dep" == "$banned" ]]; then
                key="$crate_path::$banned"
                if is_allowlisted "$key"; then
                    allowlisted_used="$allowlisted_used"$'\n'"$key"
                else
                    unexpected_violations+=("$crate_name → $banned (in $toml)")
                    violations=$((violations + 1))
                fi
                break
            fi
        done
        for db in "${BANNED_DB_DRIVERS[@]}"; do
            if [[ "$dep" == "$db" ]]; then
                unexpected_violations+=("$crate_name → $dep (direct DB driver — the sim must use the public API)")
                violations=$((violations + 1))
                break
            fi
        done
        for bus in "${BANNED_BUS_CRATES[@]}"; do
            if [[ "$dep" == "$bus" ]]; then
                unexpected_violations+=("$crate_name → $dep (direct bus/event-store — the sim must use the public API)")
                violations=$((violations + 1))
                break
            fi
        done
    done < <(awk -F= '
        /^\[/  { in_deps = ($0 ~ /^\[(dependencies|dev-dependencies|build-dependencies)/); next }
        in_deps && /^[a-zA-Z0-9_-]+[[:space:]]*=/ {
            name=$1
            gsub(/[[:space:]]/, "", name)
            print name
        }
    ' "$toml")
done

# The scan is the evidence: three crates and their dependency lines.
# Zero lines is the defect this lint carried for three months, and it
# is refused here before any verdict is printed.
lint_scanned "$LINT" "$total_deps_audited" "dep declaration(s) across ${#SIM_SIDE_CRATES[@]} sim-side crate(s)"

# Stale allowlist entries: each names a crate that must exist, and each
# must have excused a dependency THIS run — an entry whose dep was
# migrated to *-client, or whose crate was renamed, is removed in the
# same change. Both checks are lib/allowlist.sh's.
allowlist_paths=()
for entry in "${ALLOWLIST[@]}"; do
    allowlist_paths+=("${entry%::*}/Cargo.toml")
done
allowlist_paths_exist "$LINT" "${allowlist_paths[@]}"

# Report.
if [[ ${#unexpected_violations[@]} -gt 0 ]]; then
    echo "sim-boundary-audit: ${#unexpected_violations[@]} unexpected violation(s)"
    echo
    for v in "${unexpected_violations[@]}"; do
        echo "  $v"
    done
    echo
    echo "Sim-side crates must depend on boss-{domain}-client for HTTP contracts,"
    echo "not on the impl crate. If the contract you need isn't in the client crate"
    echo "yet, move it there first; if the client crate doesn't exist, create one."
    echo
    exit 1
fi

allowlist_entries_used "$LINT" "$allowlisted_used" "${ALLOWLIST[@]}"

echo "sim-boundary-audit: clean"
echo "  ${#ALLOWLIST[@]} allowlisted violator(s) (migrate to *-client to retire)"
exit 0
