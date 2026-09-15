#!/usr/bin/env bash
# --------------------------------------------------------------------------
# Boss — boss-observability service setup
#
# Builds the boss-observability binary and web dashboard, installs the
# config + static assets, creates a systemd unit, and starts the service.
#
# Intended to run on os-manager-1 — the central aggregation point.
#
# Usage:
#   sudo ./setup.sh
#
# Environment:
#   BOSS_CONFIG_DIR — config install dir (default: /etc)
#   BOSS_STATIC_DIR — static assets dir  (default: /var/lib/boss-observability/web)
#   BOSS_HTTP_PORT  — dashboard port     (default: 7800) — must match config
# --------------------------------------------------------------------------
set -euo pipefail

BOSS_CONFIG_DIR="${BOSS_CONFIG_DIR:-/etc}"
BOSS_STATIC_DIR="${BOSS_STATIC_DIR:-/var/lib/boss-observability/web}"
BOSS_HTTP_PORT="${BOSS_HTTP_PORT:-7800}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

SOURCE_CONFIG="${REPO_ROOT}/infra/observability/config.toml"
INSTALL_CONFIG="${BOSS_CONFIG_DIR}/boss-observability.toml"
BINARY_SRC="${REPO_ROOT}/target/release/boss-observability"
BINARY_DST="/usr/local/bin/boss-observability"
WEB_SRC="${REPO_ROOT}/apps/web"
WEB_DIST="${WEB_SRC}/dist"

run_as_user() {
    if [[ -n "${SUDO_USER:-}" ]]; then
        sudo -u "${SUDO_USER}" bash -lc "$1"
    else
        bash -lc "$1"
    fi
}

# ---------- Rust binary ----------
if [[ ! -x "${BINARY_SRC}" ]]; then
    echo "==> Building boss-observability (release)"
    run_as_user "cargo build --release --manifest-path='${REPO_ROOT}/Cargo.toml' -p boss-observability"
else
    echo "==> Using existing release binary at ${BINARY_SRC}"
fi

echo "==> Installing binary to ${BINARY_DST}"
sudo install -m 0755 "${BINARY_SRC}" "${BINARY_DST}"

# ---------- Frontend ----------
if ! run_as_user "command -v bun >/dev/null 2>&1"; then
    echo "==> Installing bun for ${SUDO_USER:-$USER}"
    run_as_user "curl -fsSL https://bun.sh/install | bash"
fi

echo "==> Building web dashboard"
run_as_user "cd '${WEB_SRC}' && bun install && bun run build"

echo "==> Installing static assets to ${BOSS_STATIC_DIR}"
sudo mkdir -p "${BOSS_STATIC_DIR}"
sudo rm -rf "${BOSS_STATIC_DIR:?}"/*
sudo cp -r "${WEB_DIST}"/* "${BOSS_STATIC_DIR}/"

# ---------- Config ----------
echo "==> Installing config to ${INSTALL_CONFIG}"
sudo install -m 0644 "${SOURCE_CONFIG}" "${INSTALL_CONFIG}"

# ---------- User ----------
echo "==> Creating boss-observability user"
if ! id -u boss-observability >/dev/null 2>&1; then
    sudo useradd --system --no-create-home --shell /usr/sbin/nologin boss-observability
fi
sudo chown -R boss-observability:boss-observability "${BOSS_STATIC_DIR}"

# ---------- systemd ----------
echo "==> Installing systemd unit"
sudo tee /etc/systemd/system/boss-observability.service >/dev/null <<EOF
[Unit]
Description=Boss Observability (cross-VM Cybernetics dashboard)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=boss-observability
Environment=RUST_LOG=info
ExecStart=${BINARY_DST} --config ${INSTALL_CONFIG}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now boss-observability.service

echo "==> Status:"
sudo systemctl --no-pager status boss-observability.service | sed -n '1,10p' || true

echo
echo "Dashboard listening on :${BOSS_HTTP_PORT}"
echo "Local test: curl http://127.0.0.1:${BOSS_HTTP_PORT}/api/health"
echo
echo "For public access via the manager's public IP, ensure your cloud's"
echo "firewall / security group has ${BOSS_HTTP_PORT}/tcp open."
echo "WARNING: the dashboard is unauthenticated — restrict the rule by source IP"
echo "or use an SSH tunnel: ssh -L ${BOSS_HTTP_PORT}:127.0.0.1:${BOSS_HTTP_PORT} os-manager-1"
