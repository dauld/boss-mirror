#!/usr/bin/env bash
#
# render-instance.sh — ONE manifest source, rendered per instance.
#
#   render-instance.sh <namespace> <tenant> <sim> <hostname>   the instance
#       manifests for that instance, on stdout, one YAML stream
#   render-instance.sh <namespace> <tenant> <sim> <hostname> --out-dir DIR
#       the same, one file per manifest (the source's basenames) in DIR
#   render-instance.sh --all DIR       every instance in instances.toml
#       into DIR/<namespace>/; the source instance's directory also gets
#       every `pipeline` manifest, copied as written
#   render-instance.sh --instances     name<TAB>namespace<TAB>tenant<TAB>sim<TAB>hostname
#       per instance, in file order — what the converge iterates
#   render-instance.sh --source        the source instance's namespace (the
#       one the files are written for; its render is the identity)
#   render-instance.sh --roster        file<TAB>set per manifest, or a refusal
#
# WHY (backlog 07d7549c car 1; design ffc83387, decided by David
# 2026-09-16). playground.algedonic.dev is a SECOND NAMESPACE on the
# current cluster, and both prod (namespace boss) and the playground
# converge on every train. Every manifest under infra/cluster/manifests/
# hard-codes `namespace: boss`, one tenant path, BOSS_SIM_ENABLED=false
# and one TLS-front hostname. A second namespace as a second copy of the
# YAML is the pair CLAUDE.md §9a bans — twenty-one files that would
# drift on the first car that edits one directory and not the other.
#
# So the directory stays the ONE source, written for exactly one
# instance (`source` in infra/cluster/instances.toml — prod), and any
# other instance is that directory with four values substituted for the
# source's — the idiom `manifests_with_image` in
# infra/forge/cluster-deploy-lib.sh already uses to put the converged
# image sha into the applied copy. Rendering the source instance is the
# identity by construction, and crates/core/boss-testing/tests/
# render_instance_sh.rs holds the prod render BYTE-IDENTICAL to the
# directory: landing this changed nothing live.
#
# WHAT IS SUBSTITUTED, for an instance other than the source:
#   * `namespace: <source>` on every object, and the Namespace object's
#     own name;
#   * `.<source>.svc.cluster.local` — the in-cluster DNS names by which
#     the chores reach the instance's jobs door and the TLS front reaches
#     the instance's gateway;
#   * the tenant manifest path under /opt/boss/ (the image ships the
#     tree there);
#   * the BOSS_SIM_ENABLED value;
#   * the TLS front's hostname, everywhere the source's appears;
#   * and the LoadBalancer IP pins (io.cilium/lb-ipam-ips,
#     metallb.universe.tf/loadBalancerIPs, spec.loadBalancerIP) are
#     COMMENTED OUT — only one Service on the LAN can hold an address,
#     the source's copy holds it, and the pool assigns the rest.
#     Commented rather than deleted so the rendered file still says what
#     prod holds.
#
# WHICH MANIFESTS. infra/cluster/instance-manifests.txt classifies every
# file in the directory as `instance` (rendered per instance) or
# `pipeline` (applied once, as written, in the source's namespace set:
# the conductor, the dev pod, the gate seed, and the instance-shaped
# objects pinned to prod for the reason on their line). A file the
# roster does not classify, a roster line with no file, or a file in
# both sets is REFUSED by name before anything is rendered: a partial
# render reads as a full one.
#
# REFUSALS (exit 2, nothing rendered, the reason on stderr):
#   * a namespace that is not `boss` or `boss-<name>` — or is boss-dev,
#     the pipeline's;
#   * a tenant path that is not examples/<name>/seeds/tenant.toml, or is
#     not in the tree;
#   * a sim value that is not true or false; a hostname that is not one;
#   * a roster that does not match the directory (above);
#   * a SOURCE value the instance manifests do not carry. The source's
#     values in instances.toml must be what the files say; a prod entry
#     that drifted (www while the files still say boss) would turn every
#     other instance's substitution into a no-op that answers instead of
#     erroring — the class of failure CLAUDE.md §Doors ends on.
#
# BOSS_CLUSTER_TREE overrides the tree root (fixture trees in tests),
# exactly as infra/cluster/undeclared-objects.sh reads it.
set -euo pipefail

ME=render-instance
REFUSED=2
TREE="${BOSS_CLUSTER_TREE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
DIR="$TREE/infra/cluster/manifests"
ROSTER="$TREE/infra/cluster/instance-manifests.txt"
INSTANCES="$TREE/infra/cluster/instances.toml"

refuse() { # <headline> [detail lines...]
    printf '%s: REFUSED — %s\n' "$ME" "$1" >&2
    shift
    local l
    for l in "$@"; do printf '  %s\n' "$l" >&2; done
    exit "$REFUSED"
}

usage() {
    refuse "usage" \
        "$ME <namespace> <tenant> <sim: true|false> <hostname> [--out-dir DIR]" \
        "$ME --all DIR | --instances | --roster"
}

[ -d "$DIR" ] || refuse "$DIR does not exist"
[ -f "$ROSTER" ] || refuse "$ROSTER does not exist — every manifest must be classified instance or pipeline"
[ -f "$INSTANCES" ] || refuse "$INSTANCES does not exist — nothing says which instances to render"

# --- the roster ------------------------------------------------------------
# file<TAB>set, refusing every way the roster and the directory can
# disagree. Every entry of the directory except README.md must be a
# `.yaml` the roster names (a `.yml`, a dotfile, a subdirectory is a
# manifest the converge would never apply — the refusal
# manifests_the_converge_ignores makes, made here too because this is
# what stages the apply now).
roster() {
    local file set problems="" seen="" entry
    while read -r file set rest; do
        case "$file" in ''|'#'*) continue ;; esac
        [ -n "$rest" ] && problems="$problems"$'\n'"  $file: extra text on its line ($rest)"
        case "$set" in
            instance|pipeline) ;;
            *) problems="$problems"$'\n'"  $file: set is \`$set\` — the sets are instance and pipeline" ;;
        esac
        case "$seen" in *"|$file|"*) problems="$problems"$'\n'"  $file: named more than once" ;; esac
        seen="$seen|$file|"
        [ -f "$DIR/$file" ] || problems="$problems"$'\n'"  $file: named in the roster, not in $DIR"
        printf '%s\t%s\n' "$file" "$set"
    done < "$ROSTER" > "$TMP/roster"
    for entry in "$DIR"/* "$DIR"/.[!.]*; do
        [ -e "$entry" ] || continue
        file="${entry##*/}"
        [ "$file" = README.md ] && continue
        case "$seen" in
            *"|$file|"*) ;;
            *) problems="$problems"$'\n'"  $file: in $DIR, not classified in ${ROSTER#"$TREE"/} — add it as instance (rendered per instance) or pipeline (applied once, in prod)" ;;
        esac
    done
    if [ -n "$problems" ]; then
        refuse "the roster and the manifests directory disagree; nothing rendered" "${problems#$'\n'}"
    fi
    cat "$TMP/roster"
}
in_set() { # <set>  -> basenames, one per line, in roster order
    awk -F'\t' -v s="$1" '$2 == s { print $1 }' "$TMP/roster"
}

# --- instances.toml --------------------------------------------------------
# `key = "value"` / `key = true` under `[name]`; `source = "prod"` above
# the first header. Read with awk, not a TOML parser (roles.toml's rule).
param() { # <section or "" for top level> <key>
    awk -v s="$1" -v k="$2" '
        /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
        /^\[/ { cur = $0; sub(/^\[/, "", cur); sub(/\]$/, "", cur); next }
        cur == s && $1 == k { sub(/^[^=]*=[[:space:]]*/, ""); gsub(/"/, ""); print; exit }
    ' "$INSTANCES"
}
sections() { grep -oE '^\[[A-Za-z0-9_-]+\]$' "$INSTANCES" | tr -d '[]'; }

# --- parameter validation ---------------------------------------------------
check_namespace() {
    [[ "$1" =~ ^boss(-[a-z0-9]+)*$ ]] || refuse "namespace \`$1\`: an instance namespace is \`boss\` or \`boss-<name>\` (lowercase, digits, hyphens)"
    [ "$1" != boss-dev ] && return 0
    refuse "namespace \`boss-dev\` is the pipeline's namespace, not an instance"
}
check_tenant() {
    [[ "$1" =~ ^examples/[A-Za-z0-9_-]+/seeds/tenant\.toml$ ]] || refuse "tenant \`$1\`: a tenant manifest is examples/<name>/seeds/tenant.toml, repo-relative"
    [ -f "$TREE/$1" ] || refuse "tenant \`$1\`: not in the tree at $TREE/$1"
}
check_sim() {
    case "$1" in true|false) ;; *) refuse "sim \`$1\`: BOSS_SIM_ENABLED is true or false" ;; esac
}
check_hostname() {
    [[ "$1" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$ ]] || refuse "hostname \`$1\`: a TLS-front hostname is a lowercase DNS name with at least one dot"
}

# --- the source instance ----------------------------------------------------
# The values the files are WRITTEN with. Each is checked against the
# instance manifests, so the source entry cannot drift from the files.
load_source() {
    SRC=$(param "" source)
    [ -n "$SRC" ] || refuse "${INSTANCES#"$TREE"/}: no \`source = \"<instance>\"\` line — which instance are the files written for?"
    SRC_NS=$(param "$SRC" namespace); SRC_TENANT=$(param "$SRC" tenant)
    SRC_SIM=$(param "$SRC" sim);      SRC_HOST=$(param "$SRC" hostname)
    [ -n "$SRC_NS" ] && [ -n "$SRC_TENANT" ] && [ -n "$SRC_SIM" ] && [ -n "$SRC_HOST" ] \
        || refuse "${INSTANCES#"$TREE"/}: source instance [$SRC] must declare namespace, tenant, sim and hostname"
    check_namespace "$SRC_NS"; check_tenant "$SRC_TENANT"; check_sim "$SRC_SIM"; check_hostname "$SRC_HOST"
    local files=() f
    while read -r f; do files+=("$DIR/$f"); done < <(in_set instance)
    [ "${#files[@]}" -gt 0 ] || refuse "the roster names no instance manifest — nothing to render"
    grep -qE "^[[:space:]]*namespace: ${SRC_NS}\$" "${files[@]}" \
        || refuse "source namespace \`$SRC_NS\` appears on no instance manifest — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files"
    grep -qF -- "/opt/boss/$SRC_TENANT" "${files[@]}" \
        || refuse "source tenant \`$SRC_TENANT\` appears in no instance manifest — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files"
    grep -qF -- "BOSS_SIM_ENABLED, value: \"$SRC_SIM\"" "${files[@]}" \
        || refuse "source sim \`$SRC_SIM\` is not the BOSS_SIM_ENABLED value in the instance manifests — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files"
    grep -qF -- "$SRC_HOST" "${files[@]}" \
        || refuse "source hostname \`$SRC_HOST\` appears in no instance manifest — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files (the TLS front's vhost is what it must be)"
}

re_escape() { printf '%s' "$1" | sed -e 's,[][\.*^$/|],\\&,g'; }

# --- one file, rendered --------------------------------------------------
render_file() { # <src file> <ns> <tenant> <sim> <hostname>
    local f="$1" ns="$2" tenant="$3" sim="$4" host="$5"
    local ns_re tenant_re host_re
    ns_re=$(re_escape "$SRC_NS"); tenant_re=$(re_escape "$SRC_TENANT"); host_re=$(re_escape "$SRC_HOST")
    local pins=()
    if [ "$ns" != "$SRC_NS" ]; then
        pins=(
            -e "s~^([[:space:]]*)((io\.cilium/lb-ipam-ips|metallb\.universe\.tf/loadBalancerIPs|loadBalancerIP): .*)\$~\1# \2  (not pinned outside the source instance: the LB pool assigns)~"
        )
    fi
    # `${pins[@]+"${pins[@]}"}`: an empty array is unbound under set -u
    # on the bash this may run under (4.x on a host, 5.x on the pod).
    sed -E \
        -e "s|^([[:space:]]*)namespace: ${ns_re}\$|\1namespace: ${ns}|" \
        -e "/^kind: Namespace\$/,/^  name: /s|^  name: ${ns_re}\$|  name: ${ns}|" \
        -e "s|\.${ns_re}\.svc\.cluster\.local|.${ns}.svc.cluster.local|g" \
        -e "s|/opt/boss/${tenant_re}|/opt/boss/${tenant}|g" \
        -e "s|(BOSS_SIM_ENABLED, value: )\"${SRC_SIM}\"|\1\"${sim}\"|" \
        -e "s|${host_re}|${host}|g" \
        ${pins[@]+"${pins[@]}"} \
        "$f"
}

# --- one instance, into a directory or a stream ---------------------------
render_instance() { # <ns> <tenant> <sim> <hostname> <out-dir or "-">
    local ns="$1" tenant="$2" sim="$3" host="$4" out="$5" f first=1
    check_namespace "$ns"; check_tenant "$tenant"; check_sim "$sim"; check_hostname "$host"
    if [ "$out" != - ]; then
        mkdir -p "$out"
    fi
    while read -r f; do
        if [ "$out" = - ]; then
            [ "$first" = 1 ] || printf -- '---\n'
            first=0
            render_file "$DIR/$f" "$ns" "$tenant" "$sim" "$host"
        else
            render_file "$DIR/$f" "$ns" "$tenant" "$sim" "$host" > "$out/$f"
        fi
    done < <(in_set instance)
    # The pipeline set rides with the source instance, as written.
    if [ "$ns" = "$SRC_NS" ] && [ "$out" != - ]; then
        while read -r f; do cp "$DIR/$f" "$out/$f"; done < <(in_set pipeline)
    fi
}

TMP=$(mktemp -d) || exit 1
trap 'rm -rf "$TMP"' EXIT

case "${1:-}" in
    --roster)
        [ $# -eq 1 ] || usage
        roster
        ;;
    --source)
        [ $# -eq 1 ] || usage
        roster > /dev/null
        load_source
        printf '%s\n' "$SRC_NS"
        ;;
    --instances)
        [ $# -eq 1 ] || usage
        roster > /dev/null
        load_source
        for s in $(sections); do
            printf '%s\t%s\t%s\t%s\t%s\n' "$s" "$(param "$s" namespace)" "$(param "$s" tenant)" "$(param "$s" sim)" "$(param "$s" hostname)"
        done
        ;;
    --all)
        [ $# -eq 2 ] && [ -n "$2" ] || usage
        roster > /dev/null
        load_source
        seen_ns=""
        for s in $(sections); do
            ns=$(param "$s" namespace); tenant=$(param "$s" tenant); sim=$(param "$s" sim); host=$(param "$s" hostname)
            [ -n "$ns" ] && [ -n "$tenant" ] && [ -n "$sim" ] && [ -n "$host" ] \
                || refuse "${INSTANCES#"$TREE"/}: instance [$s] must declare namespace, tenant, sim and hostname"
            case "$seen_ns" in *"|$ns|"*) refuse "${INSTANCES#"$TREE"/}: two instances share namespace \`$ns\`" ;; esac
            seen_ns="$seen_ns|$ns|"
            render_instance "$ns" "$tenant" "$sim" "$host" "$2/$ns"
        done
        ;;
    --*|'')
        usage
        ;;
    *)
        out=-
        if [ $# -eq 6 ] && [ "$5" = --out-dir ] && [ -n "$6" ]; then
            out="$6"
        elif [ $# -ne 4 ]; then
            usage
        fi
        roster > /dev/null
        load_source
        render_instance "$1" "$2" "$3" "$4" "$out"
        ;;
esac
