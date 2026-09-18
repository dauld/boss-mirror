#!/usr/bin/env bash
# tenant-modules.sh — which of the launcher's services the TENANT asked
# for, read from its manifest's `[modules]`; and the sim's loopback pair,
# derived when the sim runs. Sourced by services-launcher.sh after
# tenant-launch.sh (it uses that file's sim_enabled); exercised by
# crates/core/boss-testing/tests/the_launcher_starts_what_the_tenant_declares.rs
# through the launcher's `--plan` door.
#
# WHY (backlog 18d6a6c9, design e2580840 car 5, 2026-09-17). Measured on
# prod: the launcher started the simulator UX, the catalog, assets,
# inventory and shipping APIs for a tenant whose manifest declares no
# sim, no equipment, no warehouse and no shipping — every one of them
# recorded 0 events — because the roster was the playground's and the
# only per-service switch was BOSS_SIM_ENABLED, set by hand per instance.
# The SPA reads the same manifest and, since ce68f137, shows a module
# only when it is listed true (a missing key is off). The launcher now
# reads the same keys the same way: a module's service starts only when
# the tenant asks. What the manifest does not own — the platform's
# services (clock, policy, registries, jobs, dispatcher, relay, gateway
# …) and the company-modeling services every tenant's `finance = true`
# and `people = true` ask for — starts as before. An UNDECLARED tenant
# (no BOSS_TENANT_DIR and no BOSS_TENANT_MANIFEST_TOML, the N-1 compose
# shape) keeps the full roster: nothing is derived from an absent file.
#
# THE SIM'S LOOPBACK PAIR. The dispatcher's webhook.notify forwards the
# events a tenant's `forward-*-to-webhook` rules name to
# BOSS_EVENT_WEBHOOK_URL, and the brewery engine RECEIVES them on
# BOSS_SIM_CALLBACK_BIND — one host:port, two spellings, both the sim
# engine's wiring. Until this car boss.yaml carried both on every
# instance, so a prod dispatcher with no engine behind it POSTed to a
# port nothing listened on (a debug line per event, never an error —
# the handler is lenient by design). They are deployment facts of the
# engine that listens, so the launcher derives them when the sim runs
# and leaves them unset when it does not; an explicit value from the
# deployment (the compose file sets both) always wins.

# tenant_manifest_path — the manifest the launcher resolved, or nothing.
tenant_manifest_path() {
    [[ -n "${BOSS_TENANT_MANIFEST_TOML:-}" && -f "${BOSS_TENANT_MANIFEST_TOML}" ]] || return 1
    echo "$BOSS_TENANT_MANIFEST_TOML"
}

# tenant_module_on <module> — 0 when the manifest's `[modules]` lists
# `<module> = true`; 1 for false, missing, or no manifest. The same
# grammar tenant_id_of reads with sed: one key per line, `key = value`,
# a trailing `# comment` allowed. Hyphenated keys (marketing-assets)
# read the same as any other.
tenant_module_on() {
    local f
    f="$(tenant_manifest_path)" || return 1
    awk -v key="$1" '
        /^[[:space:]]*\[/ { in_modules = ($0 ~ /^[[:space:]]*\[modules\][[:space:]]*(#.*)?$/); next }
        in_modules {
            line = $0; sub(/#.*/, "", line)
            if (match(line, /^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=[[:space:]]*true[[:space:]]*$/)) {
                k = line; sub(/[[:space:]]*=.*/, "", k); sub(/^[[:space:]]*/, "", k)
                if (k == key) { found = 1; exit }
            }
        }
        END { exit found ? 0 : 1 }
    ' "$f"
}

# service_module <service> — the manifest module a service belongs to,
# or nothing for a service the manifest does not own. The keys are the
# tenant contract's (docs/tenant-contract.md, the tenant.toml row):
#   sim        the /simulator UX (boss-simulator). The brewery tick
#              daemon (boss-brewery-sim) is NOT listed here: it is the
#              engine of exactly one tenant and its own switch,
#              BOSS_SIM_ENABLED, is derived from this same key below.
#   equipment  the Equipment KB — boss-catalog-api (models) and
#              boss-assets-api (units).
#   warehouse  boss-inventory-api.
#   shipping   boss-shipping-api.
# boss-commerce-api is the invoices + A/R surface behind /ux/finance,
# which every tenant's always-on `finance` asks for, so it is not
# listed; boss-ml-api serves the IT monitoring panel alone (Tier 1
# core, no module in the contract), so it is not listed either.
service_module() {
    case "$1" in
        boss-simulator) echo sim ;;
        boss-catalog-api|boss-assets-api) echo equipment ;;
        boss-inventory-api) echo warehouse ;;
        boss-shipping-api) echo shipping ;;
    esac
}

# service_wanted <service> — 0 to start it. Sets SERVICE_SKIP_REASON
# when it answers 1, for the launcher's SKIP line.
service_wanted() {
    local module
    SERVICE_SKIP_REASON=""
    module="$(service_module "$1")"
    [[ -n "$module" ]] || return 0
    tenant_manifest_path >/dev/null || return 0
    if tenant_module_on "$module"; then
        return 0
    fi
    SERVICE_SKIP_REASON="module $module is not on in the tenant manifest"
    return 1
}

# derive_sim_env — BOSS_SIM_ENABLED from the manifest's `sim` when the
# deployment did not set it, then the loopback pair when the sim runs.
# Prints one `sim …` line and one `webhook …` line (the launcher's log
# and the --plan door share them).
derive_sim_env() {
    if [[ -n "${BOSS_SIM_ENABLED:-}" ]]; then
        echo "sim BOSS_SIM_ENABLED=${BOSS_SIM_ENABLED} (set by the deployment)"
    elif tenant_manifest_path >/dev/null; then
        if tenant_module_on sim; then
            export BOSS_SIM_ENABLED=true
            echo "sim BOSS_SIM_ENABLED=true (derived: the tenant manifest lists sim = true)"
        else
            export BOSS_SIM_ENABLED=false
            echo "sim BOSS_SIM_ENABLED=false (derived: the tenant manifest does not list sim = true)"
        fi
    else
        echo "sim BOSS_SIM_ENABLED=unset (no tenant manifest to derive from)"
    fi
    if sim_enabled && tenant_manifest_path >/dev/null; then
        export BOSS_SIM_CALLBACK_BIND="${BOSS_SIM_CALLBACK_BIND:-127.0.0.1:7099}"
        export BOSS_EVENT_WEBHOOK_URL="${BOSS_EVENT_WEBHOOK_URL:-http://${BOSS_SIM_CALLBACK_BIND}/callback}"
    fi
    echo "webhook BOSS_EVENT_WEBHOOK_URL=${BOSS_EVENT_WEBHOOK_URL:-unset} BOSS_SIM_CALLBACK_BIND=${BOSS_SIM_CALLBACK_BIND:-unset}"
}
