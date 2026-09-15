#!/usr/bin/env bash
# --------------------------------------------------------------------------
# Boss — nats-server setup
#
# Installs nats-server on a VM (typically os-manager-1) and exposes it to
# other VMs on port 4222. Also opens 8222 for the monitoring endpoint.
#
# Prerequisites:
#   - Ubuntu host with sudo
#   - Outbound internet access for the release download
#
# Environment:
#   NATS_VERSION    — release to install (default: 2.11.0)
#   NATS_PORT       — listen port (default: 4222)
#   JETSTREAM_STORE — JetStream file-store dir (default: /var/lib/nats/jetstream)
# --------------------------------------------------------------------------
set -euo pipefail

NATS_VERSION="${NATS_VERSION:-2.11.0}"
NATS_PORT="${NATS_PORT:-4222}"
MONITOR_PORT="8222"
# JetStream backs the durable event-delivery layer (the dispatcher's durable
# consumers redeliver transient handler failures instead of dropping them).
# The audit_log (Postgres) stays the system of record; this is a bounded,
# file-backed delivery buffer.
JETSTREAM_STORE="${JETSTREAM_STORE:-/var/lib/nats/jetstream}"
ARCH="$(dpkg --print-architecture)"   # amd64, arm64
TARBALL="nats-server-v${NATS_VERSION}-linux-${ARCH}.tar.gz"
URL="https://github.com/nats-io/nats-server/releases/download/v${NATS_VERSION}/${TARBALL}"
INSTALL_PATH="/usr/local/bin/nats-server"

echo "==> Downloading nats-server ${NATS_VERSION} (${ARCH})"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT
curl -sSL "${URL}" -o "${TMP}/${TARBALL}"
tar -xzf "${TMP}/${TARBALL}" -C "${TMP}"
EXTRACTED="${TMP}/nats-server-v${NATS_VERSION}-linux-${ARCH}/nats-server"

echo "==> Installing to ${INSTALL_PATH}"
sudo install -m 0755 "${EXTRACTED}" "${INSTALL_PATH}"

echo "==> Creating nats user"
if ! id -u nats >/dev/null 2>&1; then
    sudo useradd --system --no-create-home --shell /usr/sbin/nologin nats
fi

echo "==> Creating JetStream store dir ${JETSTREAM_STORE}"
sudo install -d -o nats -g nats -m 0750 "${JETSTREAM_STORE}"

echo "==> Writing config to /etc/nats-server.conf"
sudo tee /etc/nats-server.conf >/dev/null <<EOF
# Boss nats-server config
port: ${NATS_PORT}
http_port: ${MONITOR_PORT}
max_payload: 4MB
# TODO: enable TLS and auth before production use

# JetStream — durable event delivery for the dispatcher's consumers.
# Transient handler failures self-heal via NAK-redelivery instead of
# silently orphaning Jobs. The Postgres audit_log remains authoritative.
jetstream {
  store_dir: "${JETSTREAM_STORE}"
  max_memory_store: 256MB
  max_file_store: 8GB
}
EOF

echo "==> Installing systemd unit"
sudo tee /etc/systemd/system/nats-server.service >/dev/null <<EOF
[Unit]
Description=NATS Server (Boss)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=nats
ExecStart=${INSTALL_PATH} -c /etc/nats-server.conf
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now nats-server.service

echo "==> Status:"
sudo systemctl --no-pager status nats-server.service | sed -n '1,8p' || true

echo
echo "nats-server listening on :${NATS_PORT} (monitoring :${MONITOR_PORT})"
echo "Clients reach it at: nats://\$(hostname -I | awk '{print \$1}'):${NATS_PORT}"
echo
echo "Remember: open ${NATS_PORT}/tcp on your cloud's firewall / security group so other VMs can connect."
