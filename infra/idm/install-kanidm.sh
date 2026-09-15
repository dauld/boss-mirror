#!/usr/bin/env bash
# Kanidm install on the GCP box — the steps that CAN'T run until DNS
# exists are checked loudly instead of guessed at. Re-runnable.
#
# Install-day order (docs/design/idm-kanidm.md Q5):
#   1. David: Cloudflare -> A record id.algedonic.dev -> this box,
#      GREY CLOUD (DNS-only; Kanidm terminates its own TLS).
#   2. sudo ./install-kanidm.sh --check   # verifies DNS + prereqs
#   3. Install kanidmd per the current upstream channel (their Ubuntu
#      repo or release binary — deliberately not pinned here; verify
#      against https://kanidm.github.io/kanidm/ at install time).
#   4. certbot certonly --standalone -d id.algedonic.dev
#   5. sudo ./install-kanidm.sh          # installs config + unit, starts
#   6. kanidmd recover-account admin     # first credential
#
# GCP firewall needs tcp:8443 (and tcp:80 briefly for certbot):
#   gcloud compute firewall-rules create allow-kanidm --allow tcp:8443 --direction INGRESS

set -euo pipefail

DOMAIN="id.algedonic.dev"
HERE="$(cd "$(dirname "$0")" && pwd)"

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }

check() {
    local ok=true
    if ip=$(dig +short "$DOMAIN" 2>/dev/null | sed -n 1p) && [ -n "$ip" ]; then
        echo "  DNS: $DOMAIN -> $ip"
    else
        echo "  DNS: $DOMAIN does not resolve yet (David: grey-cloud A record)"; ok=false
    fi
    if command -v kanidmd >/dev/null; then
        echo "  kanidmd: $(kanidmd version 2>/dev/null | sed -n 1p || echo present)"
    else
        echo "  kanidmd: not installed (step 3)"; ok=false
    fi
    if [ -f "/etc/letsencrypt/live/$DOMAIN/fullchain.pem" ]; then
        echo "  TLS: cert present"
    else
        echo "  TLS: no cert yet (step 4, after DNS)"; ok=false
    fi
    $ok && echo "OK — ready for full install" || echo "not ready — see above"
    $ok
}

if [[ "${1:-}" == "--check" ]]; then check; exit $?; fi

check || { echo "preconditions missing — fix the lines above first" >&2; exit 1; }

install -d -m 750 /etc/kanidm /var/lib/kanidm /var/lib/kanidm/backups
install -m 640 "$HERE/kanidm-server.toml" /etc/kanidm/server.toml
install -m 644 "$HERE/kanidmd.service" /etc/systemd/system/kanidmd.service
systemctl daemon-reload
systemctl enable --now kanidmd
sleep 2
systemctl is-active kanidmd && echo "==> kanidmd up at https://$DOMAIN:8443"
echo "==> next: kanidmd recover-account admin, then the gateway OIDC work (Q1-Q3)"
