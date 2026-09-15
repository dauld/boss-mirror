#!/usr/bin/env bash
# --------------------------------------------------------------------------
# Boss — boss-cybernetics service setup
#
# Builds the boss-cybernetics binary, installs the appropriate config for
# this VM's role, creates a systemd unit, and starts the service.
#
# Usage:
#   sudo BOSS_ROLE=worker ./setup.sh
#   sudo BOSS_ROLE=manager ./setup.sh
#
# Environment:
#   BOSS_ROLE       — "manager" or "worker" (required)
#   BOSS_CONFIG_DIR — config install dir (default: /etc)
#   BOSS_HTTP_PORT  — dashboard port (default: 7700) — opens in firewall
# --------------------------------------------------------------------------
set -euo pipefail

BOSS_ROLE="${BOSS_ROLE:?BOSS_ROLE must be 'manager' or 'worker'}"
BOSS_CONFIG_DIR="${BOSS_CONFIG_DIR:-/etc}"
BOSS_HTTP_PORT="${BOSS_HTTP_PORT:-7700}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

case "${BOSS_ROLE}" in
    manager|worker) ;;
    *) echo "ERROR: BOSS_ROLE must be 'manager' or 'worker'" >&2; exit 1 ;;
esac

SOURCE_CONFIG="${REPO_ROOT}/infra/cybernetics/config.${BOSS_ROLE}.toml"
INSTALL_CONFIG="${BOSS_CONFIG_DIR}/boss-cybernetics.toml"
BINARY_SRC="${REPO_ROOT}/target/release/boss-cybernetics"
BINARY_DST="/usr/local/bin/boss-cybernetics"

if [[ ! -x "${BINARY_SRC}" ]]; then
    echo "==> Building boss-cybernetics (release) as ${SUDO_USER:-$USER}"
    # Build as the invoking user so we pick up their rustup toolchain
    # (cargo/rustc are often not on root's PATH).
    if [[ -n "${SUDO_USER:-}" ]]; then
        sudo -u "${SUDO_USER}" bash -lc "cargo build --release --manifest-path='${REPO_ROOT}/Cargo.toml' -p boss-cybernetics"
    else
        cargo build --release --manifest-path="${REPO_ROOT}/Cargo.toml" -p boss-cybernetics
    fi
else
    echo "==> Using existing release binary at ${BINARY_SRC}"
fi

echo "==> Installing binary to ${BINARY_DST}"
sudo install -m 0755 "${BINARY_SRC}" "${BINARY_DST}"

echo "==> Installing config to ${INSTALL_CONFIG}"
sudo install -m 0644 "${SOURCE_CONFIG}" "${INSTALL_CONFIG}"

echo "==> Creating boss-cybernetics user"
if ! id -u boss-cybernetics >/dev/null 2>&1; then
    sudo useradd --system --no-create-home --shell /usr/sbin/nologin boss-cybernetics
fi

echo "==> Installing systemd unit"
sudo tee /etc/systemd/system/boss-cybernetics.service >/dev/null <<EOF
[Unit]
Description=Boss Cybernetics (per-VM agent runtime)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=boss-cybernetics
Environment=RUST_LOG=info
ExecStart=${BINARY_DST} --config ${INSTALL_CONFIG}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now boss-cybernetics.service

echo "==> Status:"
sudo systemctl --no-pager status boss-cybernetics.service | sed -n '1,10p' || true

echo
echo "Introspection API listening on :${BOSS_HTTP_PORT}"
echo "Test: curl http://127.0.0.1:${BOSS_HTTP_PORT}/health"
echo
echo "Open ${BOSS_HTTP_PORT}/tcp on your cloud's firewall / security group if you want to reach it from elsewhere."
