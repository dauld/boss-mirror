#!/usr/bin/env bash
#
# render-tunnel-config.sh — the Cloudflare Tunnel connector's config, as
# a ConfigMap, rendered from the instance list.
#
#   render-tunnel-config.sh            the ConfigMap manifest on stdout
#   render-tunnel-config.sh --check    exit 0 if infra/cluster/manifests/
#                                      cloudflared-config.yaml IS that
#                                      render, byte for byte; else a
#                                      refusal (exit 2) naming the fix
#   render-tunnel-config.sh --write    write the render over that file
#
# WHY (backlog 5a2bb0ce; design 4c565f8c, decided by David 2026-09-16).
# Every public hostname reaches the cluster through a Cloudflare Tunnel
# whose connector runs IN the cluster (infra/cluster/manifests/
# cloudflared.yaml), in config-file mode: the ingress map — which
# hostname goes to which in-cluster origin — is the tree's, not the
# dashboard's, so it is reviewed, versioned and converged like every
# other declaration. The connector on boss-gcp it replaces was a
# hand-written unit with its token inline and its routing nowhere.
#
# WHY THE ROUTES ARE RENDERED AND NOT WRITTEN. Every hostname the tunnel
# routes is an INSTANCE's hostname, and every origin is that instance's
# own gateway Service in its own namespace — both facts already declared,
# per instance, in infra/cluster/instances.toml (the 4th parameter and
# the 1st). A second file restating them as `hostname -> service` rows
# would be the pair CLAUDE.md §9a bans: car 2 renames prod's hostname
# to www.algedonic.dev in instances.toml, and the tunnel would keep
# routing the old name until someone remembered the other file. So the
# instance list is the ONE declaration; this script derives the rules
# from it; and the committed manifest is held equal to the render by
# crates/core/boss-testing/tests/the_tunnel_connector_runs_in_the_cluster.rs
# — the equality test §9a asks for when one copy must be a file the
# converge can apply as written (a pipeline manifest, byte for byte,
# exactly as render-instance.sh treats the directory).
#
# WHAT IS RENDERED, and the reason each line is what it is:
#   * `tunnel: <name>` — cloudflared 2026.9.1 resolves a non-UUID value
#     by reading TunnelID from the credentials file (cmd/cloudflared/
#     tunnel/subcommand_context.go findID), so the tunnel's UUID never
#     needs to live in the tree; the name is what `cloudflared tunnel
#     create` was given and is documentary.
#   * `credentials-file` — the Secret mount the Deployment declares
#     (cloudflare-tunnel-credentials, key credentials.json), read-only.
#   * `metrics: 0.0.0.0:2000` — the connector's own /ready answers 200
#     only with a live edge connection (metrics/readiness.go); that is
#     the readinessProbe, and what the converge reads as `connected`.
#   * one rule per instance, IN INSTANCE ORDER: `hostname` -> the
#     instance's gateway Service, plain HTTP. The edge terminates TLS
#     and the connector is in-cluster, so there is nothing to encrypt
#     between them; the gateway drops the Host header as hop-by-hop
#     (boss-gateway proxy.rs) and builds its URLs from BOSS_PUBLIC_URL,
#     so no originRequest override is needed. Its session cookie is
#     `Secure`, and the visitor's leg is HTTPS at the edge.
#   * the catch-all `http_status:404` LAST — cloudflared refuses a
#     config without one, and a hostname the tunnel is not declared for
#     must answer 404 at the edge, never the first instance's gateway.
#
# REFUSALS (exit 2, nothing rendered, the reason on stderr):
#   * an instance without a hostname or a namespace;
#   * two instances on one hostname — a route that answers the wrong
#     instance;
#   * a boss.yaml that no longer declares the gateway Service named
#     below on the port below — the routes would point at nothing, and
#     a render that answers instead of erroring is the class of failure
#     CLAUDE.md §Doors ends on;
#   * --check against a committed file that is not the render.
#
# BOSS_CLUSTER_TREE overrides the tree root (fixture trees in tests),
# exactly as render-instance.sh reads it.
set -euo pipefail

ME=render-tunnel-config
REFUSED=2
TREE="${BOSS_CLUSTER_TREE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
INSTANCES="$TREE/infra/cluster/instances.toml"
BOSS_YAML="$TREE/infra/cluster/manifests/boss.yaml"
OUT="$TREE/infra/cluster/manifests/cloudflared-config.yaml"

# The connector's facts. The Deployment (cloudflared.yaml) mounts the
# Secret at the directory of CREDS_FILE and the ConfigMap at
# /etc/cloudflared — siblings, never nested (a mountpoint inside a
# read-only mount is one the kubelet cannot create); a test holds the
# two files to these paths.
TUNNEL_NAME=boss
CREDS_FILE=/etc/cloudflared-creds/credentials.json
METRICS=0.0.0.0:2000
# The origin: the instance's gateway Service and its port, as boss.yaml
# declares them. Checked against the file below, never assumed.
GATEWAY_SVC=boss-gateway
GATEWAY_PORT=80

refuse() { # <headline> [detail lines...]
    printf '%s: REFUSED — %s\n' "$ME" "$1" >&2
    shift
    local l
    for l in "$@"; do printf '  %s\n' "$l" >&2; done
    exit "$REFUSED"
}

[ -f "$INSTANCES" ] || refuse "$INSTANCES does not exist — nothing says which hostnames the tunnel routes"
[ -f "$BOSS_YAML" ] || refuse "$BOSS_YAML does not exist — nothing declares the gateway Service the routes point at"

# `key = "value"` under `[name]` — read with awk, not a TOML parser
# (render-instance.sh's rule, and the same subset).
param() { # <section> <key>
    awk -v s="$1" -v k="$2" '
        /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
        /^\[/ { cur = $0; sub(/^\[/, "", cur); sub(/\]$/, "", cur); next }
        cur == s && $1 == k { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); print; exit }
    ' "$INSTANCES"
}
sections() { grep -oE '^\[[A-Za-z0-9_-]+\]$' "$INSTANCES" | tr -d '[]'; }

# The gateway Service, as boss.yaml declares it: a `name: boss-gateway`
# metadata line followed, within its object, by the port. A rename or
# a port move in boss.yaml refuses the render here instead of routing
# every hostname to nothing.
awk -v svc="$GATEWAY_SVC" -v port="$GATEWAY_PORT" '
    /^---$/ { in_svc = 0 }
    $0 ~ "^  name: " svc "$" { in_svc = 1 }
    in_svc && $0 ~ "\\{port: " port "," { found = 1 }
    END { exit found ? 0 : 1 }
' "$BOSS_YAML" \
    || refuse "boss.yaml declares no Service \`$GATEWAY_SVC\` with port $GATEWAY_PORT — the tunnel's origins would point at nothing" \
        "the connector proxies every hostname to http://$GATEWAY_SVC.<namespace>.svc.cluster.local:$GATEWAY_PORT;" \
        "if the gateway Service moved, move GATEWAY_SVC / GATEWAY_PORT in ${BASH_SOURCE[0]##*/} with it"

render() {
    local s ns host seen="" rules=""
    for s in $(sections); do
        ns=$(param "$s" namespace); host=$(param "$s" hostname)
        [ -n "$ns" ] && [ -n "$host" ] \
            || refuse "${INSTANCES#"$TREE"/}: instance [$s] must declare namespace and hostname — a tunnel route needs both"
        case "$seen" in *"|$host|"*) refuse "${INSTANCES#"$TREE"/}: two instances declare hostname \`$host\` — a route that answers the wrong instance" ;; esac
        seen="$seen|$host|"
        rules="$rules"$'\n'"      # [$s] — its own gateway, in its own namespace"
        rules="$rules"$'\n'"      - hostname: $host"
        rules="$rules"$'\n'"        service: http://$GATEWAY_SVC.$ns.svc.cluster.local:$GATEWAY_PORT"
    done
    [ -n "$rules" ] || refuse "${INSTANCES#"$TREE"/} declares no instance — nothing for the tunnel to route"
    cat <<EOF
# GENERATED by infra/cluster/render-tunnel-config.sh from
# infra/cluster/instances.toml — do not edit; edit the instance list and
# run \`infra/cluster/render-tunnel-config.sh --write\`. A test holds this
# file equal to the render (backlog 5a2bb0ce; the reasons are in the
# script's header). The connector that mounts it is cloudflared.yaml.
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: cloudflared-config
  namespace: boss
  labels: {app.kubernetes.io/part-of: boss, app: cloudflared}
data:
  config.yaml: |
    tunnel: $TUNNEL_NAME
    credentials-file: $CREDS_FILE
    metrics: $METRICS
    no-autoupdate: true
    ingress:${rules}
      # the mandatory catch-all, last
      - service: http_status:404
EOF
}

case "${1:-}" in
    '')
        render
        ;;
    --check)
        [ $# -eq 1 ] || refuse "usage" "$ME [--check | --write]"
        [ -f "$OUT" ] || refuse "${OUT#"$TREE"/} does not exist — run \`${BASH_SOURCE[0]##*/} --write\`"
        # Rendered to a file first: a refusal inside render() must exit
        # as itself, not as a diff against half a stream.
        tmp=$(mktemp) || exit 1
        trap 'rm -f "$tmp"' EXIT
        render > "$tmp"
        if ! diff -u "$OUT" "$tmp" >&2; then
            refuse "${OUT#"$TREE"/} is not what ${INSTANCES#"$TREE"/} renders (diff above: committed vs render)" \
                "run \`infra/cluster/render-tunnel-config.sh --write\` and commit the result"
        fi
        ;;
    --write)
        [ $# -eq 1 ] || refuse "usage" "$ME [--check | --write]"
        tmp=$(mktemp) || exit 1
        trap 'rm -f "$tmp"' EXIT
        render > "$tmp"
        mv "$tmp" "$OUT"
        printf '%s: wrote %s\n' "$ME" "${OUT#"$TREE"/}"
        ;;
    *)
        refuse "usage" "$ME [--check | --write]"
        ;;
esac
