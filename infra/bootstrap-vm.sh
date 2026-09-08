#!/usr/bin/env bash
# bootstrap-vm.sh — single-command BOSS install on a fresh Ubuntu 24.04 VM.
#
# Sequence:
#   0. apt packages (postgres, build deps, unzip)
#   1. Rust toolchain
#   2. Bun
#   3. Postgres role + DB bootstrap
#   4. NATS
#   5. cargo build --release --workspace (+ postgres-feature follow-ups)
#   6. Install binaries to /usr/local/bin
#   7. Re-seed registries (now that binaries are on PATH)
#   8. Deploy services
#   9. Tenant-specific seed (brewery OR used-device-shop)
#  10. Health-probe report
#
# Usage:
#   sudo TENANT=brewery        /opt/boss/infra/bootstrap-vm.sh
#   sudo TENANT=device-shop    /opt/boss/infra/bootstrap-vm.sh
#
# Designed to be idempotent — re-runs skip already-completed work
# where it's cheap to check. Intended for fresh-VM install
# verification ahead of a release cut.

set -euo pipefail

TENANT="${TENANT:-brewery}"
REPO_ROOT="${REPO_ROOT:-/opt/boss}"
DEV_USER="${DEV_USER:-boss}"

case "$TENANT" in
    brewery|device-shop) ;;
    *)
        echo "TENANT must be 'brewery' or 'device-shop'" >&2
        exit 1
        ;;
esac

log() { echo "[bootstrap-vm $(date +%H:%M:%S) $TENANT] $*"; }

log "== 0 — apt packages =="
DEBIAN_FRONTEND=noninteractive apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
    build-essential pkg-config libssl-dev curl ca-certificates \
    unzip jq git postgresql postgresql-contrib

log "== 1 — Rust toolchain =="
if ! sudo -u "$DEV_USER" -i bash -c 'command -v cargo' >/dev/null 2>&1; then
    sudo -u "$DEV_USER" -i bash -lc '
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --default-toolchain stable --profile minimal'
fi

log "== 2 — Bun =="
if ! sudo -u "$DEV_USER" -i bash -c 'command -v bun' >/dev/null 2>&1; then
    sudo -u "$DEV_USER" -i bash -lc 'curl -fsSL https://bun.sh/install | bash'
fi

log "== 3 — Postgres role + databases =="
if ! sudo -u postgres psql -d postgres -tc "SELECT 1 FROM pg_roles WHERE rolname='boss'" | grep -q 1; then
    sudo -u postgres psql -d postgres -c \
        "CREATE ROLE boss WITH LOGIN SUPERUSER PASSWORD 'boss'"
fi

# The ~24 API services each run an sqlx pool (default max 10 connections),
# so the cluster needs well above Postgres's default max_connections=100 —
# at 100 the pools saturate it ("FATAL: sorry, too many clients already")
# and burst load (a warp-2000 regen, JetStream redelivery) starves accepts
# until requests fail at the connection level. 400 matches bootstrap-local.sh,
# the docker quickstart, and the documented playground setting (see
# infra/deploy-services.sh). Needs a full restart, so it runs here — before
# any service connects.
CURRENT_MAXCONN=$(sudo -u postgres psql -tAc "SHOW max_connections" 2>/dev/null || echo 0)
if [[ "${CURRENT_MAXCONN:-0}" -lt 400 ]]; then
    log "raising Postgres max_connections ($CURRENT_MAXCONN -> 400) for the service stack"
    sudo -u postgres psql -c "ALTER SYSTEM SET max_connections = 400" >/dev/null
    systemctl restart postgresql
    for _ in $(seq 1 30); do pg_isready -h 127.0.0.1 -p 5432 -q && break; sleep 1; done
fi

"$REPO_ROOT/infra/postgres/bootstrap-boss.sh"
"$REPO_ROOT/infra/postgres/bootstrap-scratch.sh"

log "== 4 — NATS =="
if ! systemctl is-active --quiet nats-server; then
    "$REPO_ROOT/infra/nats/setup.sh"
fi

log "== 5 — cargo build (workspace + every bin that declares required-features) =="
# ONE call to the canonical builder, not a hand-listed second pass.
#
# build-release.sh reads each bin's `required-features` straight out of
# `cargo metadata`, so the set cannot drift from the Cargo.tomls. The
# list this script used to carry had already drifted, and not by a
# little: it named SEVEN gated bins while the workspace declares 44
# (measured 2026-08-29). The 37 it omitted were not merely built
# without their features — cargo SKIPS a bin whose required-features
# are not enabled, so a fresh VM never built them at all, and step 6
# below installed whatever happened to be in target/release.
#
# This is the drift build-release.sh's own header predicts, quoting the
# last instance of it: "Hand-maintaining a list of 'which crates need
# --features what' drifts instantly (deploy-services.sh's
# NEEDS_POSTGRES_FEATURE listed 7 of 17)." Same defect, same shape,
# bigger number — which is what CLAUDE.md §9a means by collapsing a
# fact that lives twice rather than pinning it with a comment.
sudo -u "$DEV_USER" -i bash -lc "cd $REPO_ROOT && ./infra/build-release.sh"

log "== 6 — install binaries =="
cd "$REPO_ROOT/target/release"
# Stamp every built bin as current — same reasoning as build-release.sh:
# after a clean build every binary IS up to date (cargo rebuilt or
# verified it), but cargo leaves a skipped bin's mtime untouched, and a
# re-checkout refreshes every SOURCE mtime — so the deploy freshness
# guard false-flags untouched binaries as stale on the second run of
# this script (bit the 2026-07-08 regen-VM rerun).
find . -maxdepth 1 -type f -executable -name 'boss-*' -exec touch {} +
install -m 755 -t /usr/local/bin/ $(ls boss-* | grep -v '\.d$')

log "== 7 — re-seed registries =="
"$REPO_ROOT/infra/postgres/bootstrap-boss.sh"

log "== 8 — deploy services =="
"$REPO_ROOT/infra/deploy-services.sh" prod

log "== 8b — build + install the browser bundles =="
# The other half of "this is the from-scratch installer": without this
# a bootstrapped VM served APIs and no UI. verify-replay.sh:191 already
# said so out loud — "API stack only" — and nothing acted on it, so the
# one script that stands a production-shaped box up from nothing
# produced a box with no dashboard (feedback fca10483).
#
# AFTER deploy-services.sh, not before: that stages the generation
# directory for this checkout's HEAD, and deploy-web.sh rsyncs
# web-dist/, simulator-dist/ and step-plugins/ INTO that same
# generation. Run first, it would build into a generation that does not
# exist yet.
#
# Called AS ROOT, like deploy-services.sh above and unlike the cargo
# build in step 5: it installs into /usr/local/boss/releases/<sha>/,
# which the dev user cannot write, and it already de-escalates the bun
# build to BUILD_AS itself (deploy-web.sh:97,150). Wrapping it in
# `sudo -u "$DEV_USER"` would fail on the install, after doing the
# expensive part.
#
# BUILD_AS is passed so the bun build runs as the same user the cargo
# build did. Left to its default it would take SUDO_USER — the human
# who ran this script — and split node_modules ownership in a checkout
# $DEV_USER owns.
BUILD_AS="$DEV_USER" "$REPO_ROOT/infra/deploy-web.sh"

log "== 9 — tenant seed =="
case "$TENANT" in
    brewery)
        # Converged tenant prepare: classes + Workflows + policy + data
        # (operators / employees / accounts / vendors / opening
        # balances) in one call — the same prepare_model the live demo
        # (seed-brewery-tenant.sh) and CI (validate-brewery-sim.sh)
        # run. Binaries were installed to /usr/local/bin in step 6, so
        # resolve boss-brewery-sim by bare name.
        if command -v boss-brewery-sim >/dev/null; then
            BOSS_SIM_SEEDS_DIR="$REPO_ROOT/examples/brewery/seeds" \
                boss-brewery-sim prepare || log "WARN: brewery prepare exit non-zero"
        else
            log "WARN: boss-brewery-sim not on PATH — brewery tenant not seeded"
        fi
        ;;
    device-shop)
        # Converged tenant prepare, mirroring the brewery branch:
        # classes + company identity + policy + roster + device
        # catalog + Workflows in one idempotent call
        # (boss_used_device_shop_engine::prepare::prepare_model).
        # Unlike the brewery there is no sim daemon to start
        # afterwards — once seeded, work on this tenant is driven by
        # human/agent actors through the normal surfaces.
        if command -v boss-used-device-shop-engine >/dev/null; then
            BOSS_SIM_SEEDS_DIR="$REPO_ROOT/examples/used-device-shop/seeds" \
                boss-used-device-shop-engine prepare || log "WARN: device-shop prepare exit non-zero"
        else
            log "WARN: boss-used-device-shop-engine not on PATH — device-shop tenant not seeded"
        fi
        ;;
esac

log "== 10 — health probes =="
sleep 3
for svc_port in \
    jobs:7900 commerce:7400 inventory:7300 assets:7600 \
    shipping:7100 messages:7200 people:7500 catalog:7750 \
    calendar:7860 events:7150 accounts:7550 \
    clock:7060 ml:7070 ledger:7080 content:7090 \
    docs:7050 policy:7250 classes:7800 locations:7820 \
    subject-kinds:7830 products:7840 observability:7880; do
    IFS=: read -r name port <<<"$svc_port"
    case "$name" in
        observability) path="/api/health" ;;
        classes|locations|subject-kinds|events|accounts)
            path="/api/$name/health"
            ;;
        docs)          path="/api/design/health" ;;
        *)             path="/api/$name/health" ;;
    esac
    code=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$port$path" || echo "000")
    printf "  %-18s %-4s %s\n" "$name" "$code" "$path"
done

log "== done — tenant=$TENANT =="
