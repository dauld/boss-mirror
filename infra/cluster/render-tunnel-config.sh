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
#   render-tunnel-config.sh --summary  the ingress map as one line, for
#                                      the converge packet:
#                                      `<hostname> → <namespace>[ (<ns>
#                                      skipped: <reason>)]; …`
#
# BOSS_INSTANCES_SKIPPED="<ns> (<reason>: …)[; …]" — the converge's own
# `instances_skipped` field, verbatim — makes the render serve a SKIPPED
# instance's hostname from the SOURCE instance's gateway (see below).
# Refused with --check and --write: the committed file is the no-skip
# render, always.
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
#   * directly after an instance's rule, ITS SITE if it declares one
#     (`site` in instances.toml; design b64c4377): the site hostname ->
#     the SAME gateway Service, which answers it from the tenant's
#     site/ by Host (boss-gateway site.rs — the connector forwards the
#     visitor's Host, which is how the gateway tells the two apart).
#     The packet's summary names it `<site> → <ns> (site)`. A skipped
#     instance's site follows its hostname to the source's gateway.
#   * the catch-all `http_status:404` LAST — cloudflared refuses a
#     config without one, and a hostname the tunnel is not declared for
#     must answer 404 at the edge, never the first instance's gateway.
#
# A SKIPPED INSTANCE IS SERVED BY ITS SOURCE (backlog 40d46042, urgent,
# 2026-09-16). The converge applies an instance only when its Secrets
# are minted; one that is not is SKIPPED whole, with nothing running
# in its namespace. A route to that namespace's gateway is a route to
# nothing — measured 07:4xZ the same day: the rotation moved
# playground.algedonic.dev to this tunnel as designed, the ingress
# sent it to boss-gateway.boss-playground, the converge had skipped
# boss-playground (six Secrets absent), and visitors passed Cloudflare
# Access and reached nothing. So the converge re-renders this file AFTER its
# instance secret gate, handing over the packet's own
# `instances_skipped` string (BOSS_INSTANCES_SKIPPED; the parse is
# infra/cluster/instances-skipped.lib.sh, shared with the manifests
# check), and a skipped instance's hostname routes to the SOURCE
# instance's gateway — the one the files are written for, prod — under
# a comment naming why. Once the instance applies, the same call
# renders its own gateway again and the packet's `tunnel_ingress` line
# says so. The COMMITTED file stays the no-skip render: it is what the
# tree declares, held equal by test; the skip render is a converge-time
# state and is never written to it (--check and --write refuse the
# variable). The source itself cannot be skipped — nothing would be
# left to serve from — and is refused by name.
#
# REFUSALS (exit 2, nothing rendered, the reason on stderr):
#   * an instance without a hostname or a namespace;
#   * two instances on one hostname, or a site that is any other
#     route's hostname — a route that answers the wrong instance;
#   * a boss.yaml that no longer declares the gateway Service named
#     below on the port below — the routes would point at nothing, and
#     a render that answers instead of erroring is the class of failure
#     CLAUDE.md §Doors ends on;
#   * no `source = "<instance>"` line, or a skipped set naming the
#     source's namespace;
#   * BOSS_INSTANCES_SKIPPED with --check or --write;
#   * --check against a committed file that is not the render.
#
# BOSS_CLUSTER_TREE overrides the tree root (fixture trees in tests),
# exactly as render-instance.sh reads it.
set -euo pipefail

ME=render-tunnel-config
REFUSED=2
TREE="${BOSS_CLUSTER_TREE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
INSTANCES="$TREE/infra/cluster/instances.toml"
# The hostnames the tunnel routes that are NOT instances (the identity
# provider), declared beside the instances. Optional: a tree without
# the file routes instances only, as before 2026-09-16.
ORIGINS="$TREE/infra/cluster/tunnel-origins.toml"
BOSS_YAML="$TREE/infra/cluster/manifests/boss.yaml"
OUT="$TREE/infra/cluster/manifests/cloudflared-config.yaml"
# The parser of BOSS_INSTANCES_SKIPPED is code, not tree data: read from
# beside this script, never from $TREE.
# shellcheck source=infra/cluster/instances-skipped.lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/instances-skipped.lib.sh"

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

# The source instance: the one the manifests are written for, and the
# one a skipped instance's hostname is served by. Read the way
# render-instance.sh reads it (`param "" source` is the top-level key).
SRC=$(param "" source)
[ -n "$SRC" ] || refuse "${INSTANCES#"$TREE"/}: no \`source = \"<instance>\"\` line — which instance serves a skipped one's hostname?"
SRC_NS=$(param "$SRC" namespace)
[ -n "$SRC_NS" ] || refuse "${INSTANCES#"$TREE"/}: source instance [$SRC] declares no namespace"
if [ -n "$(skip_reason "$SRC_NS")" ]; then
    refuse "BOSS_INSTANCES_SKIPPED names the source instance's namespace \`$SRC_NS\` ([$SRC]) — nothing is left to serve the other hostnames from" \
        "the converge applies the source with no secret gate, so a skipped source is a caller's error, not a state"
fi

# One pass over the instances builds BOTH the ingress rules and the
# packet's one-line summary, so the two cannot disagree. RULES and
# SUMMARY are the outputs; a refusal exits from inside.
RULES=""
SUMMARY=""
routes() {
    local s ns host site seen="" reason origin_ns
    # Every hostname and site first, so a site that collides with a
    # LATER instance's hostname is refused too, not only an earlier one.
    for s in $(sections); do
        for host in $(param "$s" hostname) $(param "$s" site); do
            case "$seen" in *"|$host|"*) refuse "${INSTANCES#"$TREE"/}: \`$host\` is declared twice (a hostname or a site of two instances, or an instance's own site) — a route that answers the wrong instance" ;; esac
            seen="$seen|$host|"
        done
    done
    for s in $(sections); do
        ns=$(param "$s" namespace); host=$(param "$s" hostname); site=$(param "$s" site)
        [ -n "$ns" ] && [ -n "$host" ] \
            || refuse "${INSTANCES#"$TREE"/}: instance [$s] must declare namespace and hostname — a tunnel route needs both"
        reason=$(skip_reason "$ns")
        if [ -n "$reason" ]; then
            origin_ns="$SRC_NS"
            RULES="$RULES"$'\n'"      # [$s] skipped: $reason — served by $SRC_NS until provisioned"
            SUMMARY="${SUMMARY:+$SUMMARY; }$host → $SRC_NS ($ns skipped: $reason)"
        else
            origin_ns="$ns"
            RULES="$RULES"$'\n'"      # [$s] — its own gateway, in its own namespace"
            SUMMARY="${SUMMARY:+$SUMMARY; }$host → $ns"
        fi
        RULES="$RULES"$'\n'"      - hostname: $host"
        RULES="$RULES"$'\n'"        service: http://$GATEWAY_SVC.$origin_ns.svc.cluster.local:$GATEWAY_PORT"
        [ -n "$site" ] || continue
        # The instance's site: the same origin, told apart by Host.
        if [ -n "$reason" ]; then
            RULES="$RULES"$'\n'"      # [$s] site, skipped: $reason — served by $SRC_NS until provisioned"
            SUMMARY="${SUMMARY:+$SUMMARY; }$site → $SRC_NS ($ns skipped: $reason; site)"
        else
            RULES="$RULES"$'\n'"      # [$s] site — the same gateway, answered from the tenant's site/ by Host"
            SUMMARY="${SUMMARY:+$SUMMARY; }$site → $ns (site)"
        fi
        RULES="$RULES"$'\n'"      - hostname: $site"
        RULES="$RULES"$'\n'"        service: http://$GATEWAY_SVC.$origin_ns.svc.cluster.local:$GATEWAY_PORT"
    done
    [ -n "$RULES" ] || refuse "${INSTANCES#"$TREE"/} declares no instance — nothing for the tunnel to route"
    origins
}

# `[[origin]]` blocks of tunnel-origins.toml as `hostname|service|sni|verify`
# lines, one per block, in file order — the same awk-not-a-parser rule
# as `param`. A block missing hostname or service refuses by name: a
# route to nothing is the failure this file exists to end.
origin_rows() {
    [ -f "$ORIGINS" ] || return 0
    awk '
        function flush() {
            if (n) { print h "|" svc "|" sni "|" v }
            h = ""; svc = ""; sni = ""; v = "false"
        }
        /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
        /^\[\[origin\]\]/ { flush(); n++; next }
        n && $1 == "hostname"           { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); h = $0; next }
        n && $1 == "service"            { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); svc = $0; next }
        n && $1 == "origin_server_name" { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); sni = $0; next }
        n && $1 == "no_tls_verify"      { sub(/^[^=]*=[[:space:]]*/, ""); v = $0; next }
        END { flush() }
    ' "$ORIGINS"
}

# The declared non-instance routes, after the instances and before the
# catch-all. 2026-09-16: the IdP's hostname pointed at a deleted tunnel
# for a day because nothing in the tree routed it (see the file).
origins() {
    local row host svc sni verify
    while IFS='|' read -r host svc sni verify; do
        # No file, or an empty one, is one empty line here: nothing declared.
        [ -z "$host$svc$sni" ] && continue
        [ -n "$host" ] && [ -n "$svc" ] \
            || refuse "${ORIGINS#"$TREE"/}: an [[origin]] must declare hostname and service — a route to nothing"
        case "$RULES" in *"- hostname: $host"*) refuse "${ORIGINS#"$TREE"/}: \`$host\` is already an instance's hostname — one route per hostname" ;; esac
        RULES="$RULES"$'\n'"      # [origin] $host — ${ORIGINS#"$TREE"/}"
        RULES="$RULES"$'\n'"      - hostname: $host"
        RULES="$RULES"$'\n'"        service: $svc"
        if [ -n "$sni" ] || [ "$verify" = "true" ]; then
            RULES="$RULES"$'\n'"        originRequest:"
            [ -n "$sni" ] && RULES="$RULES"$'\n'"          originServerName: $sni"
            [ "$verify" = "true" ] && RULES="$RULES"$'\n'"          noTLSVerify: true"
        fi
        SUMMARY="${SUMMARY:+$SUMMARY; }$host → $svc (origin)"
    done <<EOF_ROWS
$(origin_rows)
EOF_ROWS
}

render() {
    local rules
    routes
    rules="$RULES"
    cat <<EOF
# GENERATED by infra/cluster/render-tunnel-config.sh from
# infra/cluster/instances.toml and tunnel-origins.toml — do not edit; edit those and
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

# The committed file is the no-skip render, always: a skipped set is a
# converge-time state, and writing it into the tree — or judging the
# tree against it — would make the file say what one converge saw.
committed_only() {
    [ -z "${BOSS_INSTANCES_SKIPPED:-}" ] \
        || refuse "$1 renders the COMMITTED file, which is the no-skip render; BOSS_INSTANCES_SKIPPED is for the converge's re-render only" \
            "unset it, or call with no mode (the render on stdout) or --summary"
}

case "${1:-}" in
    '')
        render
        ;;
    --summary)
        [ $# -eq 1 ] || refuse "usage" "$ME [--check | --write | --summary]"
        routes
        printf '%s\n' "$SUMMARY"
        ;;
    --check)
        [ $# -eq 1 ] || refuse "usage" "$ME [--check | --write | --summary]"
        committed_only --check
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
        [ $# -eq 1 ] || refuse "usage" "$ME [--check | --write | --summary]"
        committed_only --write
        tmp=$(mktemp) || exit 1
        trap 'rm -f "$tmp"' EXIT
        render > "$tmp"
        mv "$tmp" "$OUT"
        printf '%s: wrote %s\n' "$ME" "${OUT#"$TREE"/}"
        ;;
    *)
        refuse "usage" "$ME [--check | --write | --summary]"
        ;;
esac
