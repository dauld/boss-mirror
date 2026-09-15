#!/usr/bin/env bash
# --------------------------------------------------------------------------
# Boss — Caddy front door setup
#
# Installs Caddy, drops in the Caddyfile, wires up the hostname, and starts
# the service. Caddy handles Let's Encrypt issuance + renewal automatically
# as long as DNS resolves and ports 80/443 are reachable.
#
# Usage:
#   sudo BOSS_HOSTNAME=example.com ./setup.sh [--force]
#
# Environment:
#   BOSS_HOSTNAME — public hostname that resolves to this VM (required)
#
# Flags:
#   --force — install over an /etc/caddy/Caddyfile that differs from this
#             quickstart's, backing the old one up first. Without it the
#             script refuses: that path also carries real front doors
#             (boss-gcp serves playground.algedonic.dev + id.algedonic.dev
#             from it, tracked at infra/gcp/caddy/Caddyfile), and a
#             quickstart run must not silently replace one.
# --------------------------------------------------------------------------
set -euo pipefail

BOSS_HOSTNAME="${BOSS_HOSTNAME:?BOSS_HOSTNAME is required (e.g. example.com)}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

CADDYFILE_SRC="${REPO_ROOT}/infra/caddy/Caddyfile"
CADDYFILE_DST="/etc/caddy/Caddyfile"

FORCE=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        *)
            echo "unknown argument: ${arg}" >&2
            echo "usage: sudo BOSS_HOSTNAME=example.com $0 [--force]" >&2
            exit 2
            ;;
    esac
done

# True when something else already lives at the destination. Byte
# equality is the test, so re-running the quickstart on a host it
# already configured is still a silent no-op.
caddyfile_is_foreign() {
    [ -f "$CADDYFILE_DST" ] && ! sudo cmp -s "$CADDYFILE_SRC" "$CADDYFILE_DST"
}

# ---------- guard: don't replace a front door we didn't install ----------
# Checked before anything is installed or written, so a refusal leaves
# the host exactly as it was found.
if caddyfile_is_foreign && [ "$FORCE" -eq 0 ]; then
    echo "==> REFUSING: ${CADDYFILE_DST} exists and is not this quickstart's Caddyfile" >&2
    echo >&2
    echo "    It currently serves:" >&2
    sudo grep -E '^[^[:space:]#].*\{[[:space:]]*$' "$CADDYFILE_DST" | sed 's/^/        /' >&2 \
        || echo "        (no site blocks matched — read the file yourself)" >&2
    echo >&2
    echo "    ${CADDYFILE_SRC} would replace all of that with one" >&2
    echo "    virtual host for ${BOSS_HOSTNAME}. If this is boss-gcp, the real" >&2
    echo "    front door is tracked in-tree at infra/gcp/caddy/Caddyfile —" >&2
    echo "    deploy that instead of running the quickstart here." >&2
    echo >&2
    echo "    To install anyway (the existing file is backed up first):" >&2
    echo "        sudo BOSS_HOSTNAME=${BOSS_HOSTNAME} $0 --force" >&2
    exit 1
fi

# ---------- install caddy via official apt repo ----------
if ! command -v caddy >/dev/null 2>&1; then
    echo "==> Installing Caddy"
    sudo apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/gpg.key \
        | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt \
        | sudo tee /etc/apt/sources.list.d/caddy-stable.list
    sudo apt-get update -qq
    sudo apt-get install -y caddy
else
    echo "==> Using existing caddy ($(caddy version))"
fi

# ---------- config ----------
echo "==> Installing Caddyfile for ${BOSS_HOSTNAME}"
# Reached with a foreign Caddyfile in place only under --force; keep a
# timestamped copy so the front door being replaced is recoverable.
if caddyfile_is_foreign; then
    CADDYFILE_BACKUP="${CADDYFILE_DST}.$(date +%Y%m%d-%H%M%S).bak"
    sudo cp -p "$CADDYFILE_DST" "$CADDYFILE_BACKUP"
    echo "    --force: backed up the existing Caddyfile to ${CADDYFILE_BACKUP}"
fi
sudo install -m 0644 "$CADDYFILE_SRC" "$CADDYFILE_DST"

# ---------- environment (hostname) ----------
sudo mkdir -p /etc/systemd/system/caddy.service.d
sudo tee /etc/systemd/system/caddy.service.d/hostname.conf >/dev/null <<EOF
[Service]
Environment=BOSS_HOSTNAME=${BOSS_HOSTNAME}
EOF

# ---------- start ----------
sudo systemctl daemon-reload
sudo systemctl enable --now caddy
sudo systemctl restart caddy

echo "==> Status:"
sudo systemctl --no-pager status caddy | sed -n '1,10p' || true

echo
echo "Caddy listening on :80, :443"
echo "Certificate issuance logs: journalctl -u caddy -f"
echo "Test (after cert issues): curl -v https://${BOSS_HOSTNAME}/health"
echo
echo "If LE issuance fails (rate limit or DNS not resolving yet), the"
echo "Caddyfile can be switched to 'tls internal' for self-signed certs"
echo "— browser warning only, but useful for bring-up."
