#!/usr/bin/env bash
# bootstrap-local.sh — get a local BOSS playground running.
#
# Wraps the four steps in the README's "Run BOSS locally" section
# into one command:
#   1. cargo build --release --workspace
#   2. sudo ./infra/postgres/bootstrap-boss.sh   (empty boss DB)
#   3. cd apps/web && bun install && bun run build
#   4. (optional) start the services + live brewery sim
#
# Idempotent — safe to re-run. The cargo + bun builds skip work
# they've already done; the DB bootstrap drops + recreates an empty
# boss DB, and the live sim builds the demo from there.
#
# Prerequisites checked on entry:
#   - Rust stable toolchain      (cargo on PATH)
#   - Bun 1.1+                   (bun on PATH)
#   - Postgres 16+ on :5432      (peer-auth `boss` superuser)
#   - NATS on :4222              (any version)
#
# Usage:
#   ./infra/bootstrap-local.sh                # build + bootstrap DB + build SPA
#   ./infra/bootstrap-local.sh --skip-build   # only DB + SPA (binaries cached)
#   ./infra/bootstrap-local.sh --skip-spa     # only build + DB
#   ./infra/bootstrap-local.sh --start        # also start the services (background)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SKIP_BUILD=0
SKIP_SPA=0
SKIP_DB=0
START=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1 ;;
        --skip-spa)   SKIP_SPA=1 ;;
        --skip-db)    SKIP_DB=1 ;;
        --start)      START=1 ;;
        --help|-h)    sed -n '2,30p' "$0"; exit 0 ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
    shift
done

# ---- prereqs ------------------------------------------------------------------

missing=()
command -v cargo >/dev/null 2>&1 || missing+=("cargo (install via rustup)")
command -v bun   >/dev/null 2>&1 || missing+=("bun (curl -fsSL https://bun.sh/install | bash)")
command -v psql  >/dev/null 2>&1 || missing+=("psql (postgresql client)")
command -v nats-server >/dev/null 2>&1 || true  # optional — can run via brew/apt service

if ! pg_isready -h 127.0.0.1 -p 5432 -q; then
    missing+=("postgres listening on 127.0.0.1:5432")
fi
if ! (echo > /dev/tcp/127.0.0.1/4222) >/dev/null 2>&1; then
    missing+=("nats listening on 127.0.0.1:4222")
fi

if [[ ${#missing[@]} -gt 0 ]]; then
    echo "==> missing prerequisites:" >&2
    for m in "${missing[@]}"; do echo "    - $m" >&2; done
    exit 1
fi

# ---- 1. cargo build -----------------------------------------------------------

if [[ "$SKIP_BUILD" -eq 0 ]]; then
    # boss-brewery-sim builds as a normal workspace bin now — it's a
    # pure public-API client with no sqlx/DB feature to gate.
    echo "==> [1/4] cargo build --release --workspace --features postgres"
    cd "$REPO_ROOT"
    cargo build --release --workspace --features postgres
    # boss-accounts-api + boss-events-api gate on umbrella features, not
    # `postgres`, so the workspace build skips them. Build explicitly —
    # else nothing listens on 7550/7150 and accounts never seed.
    cargo build --release -p boss-accounts --bin boss-accounts-api --features accounts-api
    cargo build --release -p boss-events --bin boss-events-api --features events-api
fi

# ---- 2. bootstrap DB ----------------------------------------------------------

if [[ "$SKIP_DB" -eq 1 ]]; then
    echo "==> [2/4] --skip-db set; leaving existing DB untouched"
else
    echo "==> [2/4] bootstrapping boss DB (empty — the live sim builds the demo)"
    sudo "$REPO_ROOT/infra/postgres/bootstrap-boss.sh"
fi

# ---- 2b. tune Postgres for the full service stack ----------------------------
# The ~24 API services each run an sqlx pool (default max 10 connections), so
# the cluster needs well above Postgres's default max_connections=100 — at 100
# the pools saturate it ("FATAL: sorry, too many clients already") and services
# fail-closed mid-startup. 400 matches the documented playground setting (see
# infra/deploy-services.sh). Idempotent: only raises + restarts when the cluster
# is below 400. max_connections needs a full restart (not a reload) to apply,
# so this runs here — before any service connects — not after --start.
CURRENT_MAXCONN=$(sudo -n -u postgres psql -tAc "SHOW max_connections" 2>/dev/null || echo 0)
if [[ "${CURRENT_MAXCONN:-0}" -lt 400 ]]; then
    echo "==> raising Postgres max_connections ($CURRENT_MAXCONN -> 400) for the service stack"
    sudo -n -u postgres psql -c "ALTER SYSTEM SET max_connections = 400" >/dev/null
    if command -v systemctl >/dev/null 2>&1; then
        sudo systemctl restart postgresql
        for _ in $(seq 1 30); do pg_isready -h 127.0.0.1 -p 5432 -q && break; sleep 1; done
    else
        echo "    WARN: restart Postgres yourself for max_connections=400 to take effect"
    fi
else
    echo "==> Postgres max_connections already $CURRENT_MAXCONN (>=400) — no change"
fi

# ---- 3. SPA build -------------------------------------------------------------

if [[ "$SKIP_SPA" -eq 0 ]]; then
    echo "==> [3/4] bun install + bun run build (apps/web)"
    cd "$REPO_ROOT/apps/web"
    bun install
    bun run build
    cd "$REPO_ROOT"
fi

# ---- 4. (optional) start services in the background --------------------------

if [[ "$START" -eq 1 ]]; then
    # API binaries default --config to /etc/<name>.toml. Generate
    # the per-service configs into a user-owned dir so we don't need
    # sudo to write to /etc. Same heredoc bodies as the docker-side
    # generate-configs.sh; this reuses that script with ETC_DIR
    # pointed at $HOME/.config/boss.
    CONFIG_DIR="$HOME/.config/boss"
    mkdir -p "$CONFIG_DIR"
    # generate-configs.sh requires boss-ports-list on PATH; bare-metal
    # builds put it at target/release/, not /usr/local/bin like the
    # docker image. Prepend so the script finds it.
    PATH="$REPO_ROOT/target/release:$PATH" \
        DATABASE_URL="postgres://boss:boss@127.0.0.1/boss" \
        BOSS_NATS_URL="nats://127.0.0.1:4222" \
        ETC_DIR="$CONFIG_DIR" \
        "$REPO_ROOT/infra/oss-quickstart/generate-configs.sh"

    # Start every service as a plain background process — no
    # multiplexer dependency. Each logs to ~/.boss-logs/<name>.log and its
    # PID lands in ~/.boss-pids, so the whole stack stops with one kill.
    LOG_DIR="$HOME/.boss-logs"; mkdir -p "$LOG_DIR"
    PID_FILE="$HOME/.boss-pids"; : > "$PID_FILE"
    echo "==> [4/4] starting services in the background (logs -> $LOG_DIR/)"

    start_svc() {  # start_svc <log-name> <env-string> <cmd...>
        local name="$1"; shift; local env="$1"; shift
        nohup env $env "$@" > "$LOG_DIR/$name.log" 2>&1 &
        echo "$!" >> "$PID_FILE"
    }

    # Service list mirrors infra/oss-quickstart/services-launcher.sh
    # (the docker-side launcher) — keep them in sync as services come
    # and go.
    SERVICES=(
        policy classes locations subject-kinds people accounts assets catalog
        products commerce inventory shipping messages calendar
        content ledger docs events jobs
    )
    SVC_ENV="BOSS_POSTGRES_URL=postgres://boss:boss@127.0.0.1/boss BOSS_NATS_URL=nats://127.0.0.1:4222"
    # The jobs-api's demo-loop Reset shells out to boss-rebuild-all to
    # re-derive projections after trimming the audit_log. Bare-metal builds
    # it into target/release, not /usr/local/bin (the reset's default), so
    # point the jobs-api at the in-tree binary — else Reset 500s on a spawn
    # error. Harmless for the other services (only jobs-api reads it).
    SVC_ENV="$SVC_ENV BOSS_REBUILD_ALL_BIN=$REPO_ROOT/target/release/boss-rebuild-all"

    # Prime the formula clock to the demo epoch (fixed 2025-04-01,
    # override via BOSS_DEMO_EPOCH_START), then start boss-clock-api in
    # sim mode FIRST — the clock-authoritative brewery-sim reads
    # /api/clock/now at startup, and without a warped clock the playground
    # sits frozen at wall-time. The tenant seed below runs against this
    # clock so its events land on day 0, not wall-time.
    #
    # epoch_end = epoch_start + 365 gives the playground a 12-month range to
    # tick through. Without an epoch_end past epoch_start the loop is
    # zero-length: the sim hits 'epoch complete' on the first tick and
    # auto-pauses, so the demo never moves.
    DEMO_EPOCH="${BOSS_DEMO_EPOCH_START:-2025-04-01}"
    # clock-api is the single writer for clock state. Start it first, then
    # prime through its public /configure endpoint — never a direct
    # sim_clock write (an end-around bypasses the clock's invariants). The
    # clock-authoritative tenant seed below then lands its events at the
    # epoch. epoch_end = epoch_start + 365 → a 12-month loop.
    start_svc boss-clock-api "$SVC_ENV BOSS_CLOCK_MODE=sim" \
        "$REPO_ROOT/target/release/boss-clock-api"
    EPOCH_END="$(date -u -d "$DEMO_EPOCH + 365 days" +%F)"
    clock_ok=
    for attempt in 1 2 3 4 5 6; do
        code=$(curl -s -o /dev/null -w '%{http_code}' -m 5 -X POST \
            -H 'content-type: application/json' \
            -d "{\"epoch_start\":\"$DEMO_EPOCH\",\"epoch_end\":\"$EPOCH_END\",\"warp_factor\":1000}" \
            "http://127.0.0.1:7060/api/clock/configure" 2>/dev/null || echo 000)
        [[ "$code" == "200" || "$code" == "201" ]] && { clock_ok=1; echo "    clock primed to $DEMO_EPOCH..$EPOCH_END @ 1000x warp"; break; }
        sleep 2
    done
    [[ -n "$clock_ok" ]] || echo "    WARN: clock-api /configure failed; playground may sit at wall-time"

    for svc in "${SERVICES[@]}"; do
        start_svc "boss-$svc-api" "$SVC_ENV" \
            "$REPO_ROOT/target/release/boss-$svc-api" --config "$CONFIG_DIR/boss-$svc-api.toml"
    done
    start_svc boss-observability "$SVC_ENV" "$REPO_ROOT/target/release/boss-observability"
    # The brewery sim reads its tenant seed files (tenant.toml + workflows
    # + rates) at runtime; without BOSS_SIM_SEEDS_DIR it falls back to the
    # /opt/boss dev-box path and crashes on any other checkout. Point it at
    # this repo, plus a user-writable state dir (the default /var/lib/boss-sim
    # needs root to create).
    SIM_STATE_DIR="$HOME/.local/state/boss-sim"; mkdir -p "$SIM_STATE_DIR"
    # BOSS_SIM_CALLBACK_BIND: receive the dispatcher's pushed step.done.<kind>
    # events on :7099 (matches the dispatcher's BOSS_EVENT_WEBHOOK_URL above) so
    # the counterparty engine drives the collections loop; unset → AR balloons.
    SIM_ENV="$SVC_ENV BOSS_SIM_SEEDS_DIR=$REPO_ROOT/examples/brewery/seeds BOSS_SIM_STATE_DIR=$SIM_STATE_DIR BOSS_SIM_CALLBACK_BIND=127.0.0.1:7099"

    # Platform operator-baseline (emp-audit + the bootstrap-admin) —
    # post-API, before the brewery tenant. boss-operator-baseline-seed
    # POSTs /api/people (the policy-checked path), so it runs now that
    # the services above are up. BOSS_BOOTSTRAP_ADMIN_EMAIL (exported by
    # quickstart.sh) injects the platform-admin login; absent, the binary
    # falls back to the first BOSS_AUTH_FILE credential. The clock primed
    # above stamps these at the epoch.
    PATH="$REPO_ROOT/target/release:$PATH" \
        BOSS_OPERATOR_BASELINE_TOML="$REPO_ROOT/infra/operator-baseline/operator_hires.toml" \
        "$REPO_ROOT/infra/seed-operator-baseline.sh"

    # Publish the brewery tenant (Workflows + policy + accounts/vendors/data)
    # before the sim. None of it is event-sourced — it's published through
    # the API — so the live-from-empty demo seeds it here or the sim's job
    # posts 400 ("unknown or inactive job kind") and the playground stays
    # idle. Shared with docker services-launcher.sh; target/release is on
    # PATH so the script resolves the bins by bare name.
    PATH="$REPO_ROOT/target/release:$PATH" \
        BOSS_DEMO_EPOCH_START="$DEMO_EPOCH" \
        BOSS_SIM_SEEDS_DIR="$REPO_ROOT/examples/brewery/seeds" \
        BOSS_POSTGRES_URL="postgres://boss:boss@127.0.0.1/boss" \
        "$REPO_ROOT/infra/seed-brewery-tenant.sh"

    # Dispatcher BEFORE the sim. It runs step-completion side-effect rules
    # off step.done.<kind> NATS topics — and, via BOSS_EVENT_WEBHOOK_URL,
    # forwards billing/courier events to the sim's callback receiver
    # (BOSS_SIM_CALLBACK_BIND below), driving the collections loop (AR-aging
    # payments, courier scans). Env-var driven (no --config like the *-api
    # services); BOSS_DISPATCHER_RULES points at the in-tree rule registry —
    # without it the rules runner never starts and zero side effects fire.
    # Downstream API URLs default to 127.0.0.1:<port> via boss-ports, which
    # is correct for this single-box layout.
    DISPATCHER_ENV="$SVC_ENV BOSS_DISPATCHER_RULES=$REPO_ROOT/infra/dispatcher/rules"
    # Step-assignment distribution strategy named as data (default spread:
    # hash-spread each ready step across its role's active holders). Unknown
    # value → dispatcher warns + falls back to spread.
    DISPATCHER_ENV="$DISPATCHER_ENV BOSS_DISPATCH_STRATEGY=spread"
    DISPATCHER_ENV="$DISPATCHER_ENV BOSS_EVENT_WEBHOOK_URL=http://127.0.0.1:7099/callback"
    # boss-event-relay: outbox → audit_log + NATS (outbox phase 2).
    # The dispatcher's step.done/step.ready signals ride this path —
    # without the relay no side-effect rules ever fire. Borrows the
    # jobs config (generated above) for postgres_url + nats_url,
    # matching the systemd unit.
    start_svc boss-event-relay   "" "$REPO_ROOT/target/release/boss-event-relay" --config /etc/boss-jobs-api.toml
    start_svc boss-dispatcher    "$DISPATCHER_ENV" "$REPO_ROOT/target/release/boss-dispatcher"

    start_svc boss-brewery-sim   "$SIM_ENV" "$REPO_ROOT/target/release/boss-brewery-sim"

    # Gateway last — depends on every other service being reachable.
    # Anonymous visitors get 401 and are sent to /login.
    #
    # BOSS_GUEST_ACCESS answers one question: does /login offer a
    # read-only guest session? A local playground says yes.
    #
    # This was BOSS_DEMO_MODE, which also decided whether the brewery
    # simulator ran — one variable for two unrelated questions, so
    # turning off synthetic activity silently withdrew guest access.
    # Whether the sim runs is now answered by whether its unit is
    # enabled.
    #
    # Earlier still, the same flag made the gateway MINT an
    # audit-readonly session for anyone arriving without a cookie.
    # That made an expired login indistinguishable from a permissions
    # problem: the mint replaced the real cookie, so an operator
    # stayed apparently signed in while silently downgraded, and every
    # write came back 403.
    GW_ENV="$SVC_ENV"
    GW_ENV="$GW_ENV BOSS_GUEST_ACCESS=1"
    GW_ENV="$GW_ENV BOSS_LISTEN=127.0.0.1:4443"
    GW_ENV="$GW_ENV BOSS_STATIC_DIR=$REPO_ROOT/apps/web/dist"
    GW_ENV="$GW_ENV BOSS_AUTH_PROVIDER=local-auth"
    GW_ENV="$GW_ENV BOSS_AUTH_FILE=${BOSS_AUTH_FILE:-/var/lib/boss/auth/credentials.toml}"
    # Tenant manifest path. Core code falls back to /etc/boss-gateway/tenant.toml
    # if this env is unset; bare-metal points at the in-tree brewery seed.
    GW_ENV="$GW_ENV BOSS_TENANT_MANIFEST_TOML=$REPO_ROOT/examples/brewery/seeds/tenant.toml"
    GW_ENV="$GW_ENV BOSS_SESSION_KEY=${BOSS_SESSION_KEY:-please-rotate-me-in-prod-do-not-leak}"
    start_svc boss-gateway "$GW_ENV" "$REPO_ROOT/target/release/boss-gateway"

    echo "==> $(wc -l < "$PID_FILE") services started in the background."
    echo "==> SPA at http://127.0.0.1:4443 (gateway + demo-mode)"
    echo "==> Logs:    $LOG_DIR/"
    echo "==> Configs: $CONFIG_DIR/"
    echo "==> Stop:    kill \$(cat $PID_FILE)"
fi

echo "==> done."
