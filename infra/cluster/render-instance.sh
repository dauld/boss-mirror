#!/usr/bin/env bash
#
# render-instance.sh — ONE manifest source, rendered per instance.
#
#   render-instance.sh <namespace> <tenant-dir> <sim> <hostname> <guest>
#       the instance manifests for that instance, on stdout, one YAML
#       stream
#   render-instance.sh <namespace> <tenant-dir> <sim> <hostname> <guest> --out-dir DIR
#       the same, one file per manifest (the source's basenames) in DIR
#   render-instance.sh --all DIR       every instance in instances.toml
#       into DIR/<namespace>/; the source instance's directory also gets
#       every `pipeline` manifest, copied as written
#   render-instance.sh --instances     name<TAB>namespace<TAB>tenant<TAB>sim<TAB>hostname<TAB>shares-with<TAB>tenant-repo<TAB>tenant-ref
#       per instance, in file order — what the converge iterates. The
#       sixth column is the NAMESPACE of the instance this one copies
#       its shared Secrets from (`shares_with` in instances.toml; backlog
#       dc1bc724), empty when it declares none. The third is the
#       DIRECTORY the pod reads under /opt/boss (`tenant_dir`, or the
#       literal `tenant` for a repo-sourced instance); the seventh and
#       eighth are `tenant_repo` and `tenant_ref`, empty for an
#       image-sourced one (backlog f4f5c387)
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
#   * the tenant directory under /opt/boss/ — BOSS_TENANT_DIR on the boss
#     container (backlog f4f5c387): /opt/boss/<tenant_dir> for a
#     directory the image ships, /opt/boss/tenant for a `tenant_repo`
#     the converge delivers as a ConfigMap (instances.toml says how);
#   * the BOSS_SIM_ENABLED value;
#   * the BOSS_GUEST_ACCESS value — `guest = true|false` in
#     instances.toml, rendered "1"|"0", the two spellings the gateway
#     reads (backlog 0d2d7daa, 2026-09-16: anonymous read-only sessions
#     are right for the public example and wrong for the operating
#     company's site, so each instance says);
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
#   * a tenant directory that is not examples/<name> (or `tenant`, the
#     delivered mount), or holds no tenant.toml / seeds/tenant.toml; an
#     instance declaring both `tenant_dir` and `tenant_repo`, neither,
#     a repo without a ref or a ref without a repo, a repo that is not
#     `owner/name` — or the RETIRED `tenant = "<manifest path>"` key,
#     which must not read as "no source";
#   * a sim value that is not true or false; a guest value that is not
#     true or false (or is missing — silence must not read as "guests
#     may read"); a hostname that is not one;
#   * a `shares_with` that names no instance in the file, or the
#     instance itself — the converge copies shared Secrets from the
#     namespace this resolves to, and an unknown source must not read
#     as "shares with nobody";
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
        "$ME <namespace> <tenant> <sim: true|false> <hostname> <guest: true|false> [--out-dir DIR]" \
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
# check_tenant <dir> [<where>] — the directory the pod reads under
# /opt/boss: examples/<name> in the tree, holding tenant.toml or
# seeds/tenant.toml (docs/tenant-contract.md accepts both spellings),
# or the literal `tenant` — the mount a `tenant_repo` is delivered at,
# which is nowhere in the tree by design. <where> names the instance in
# a refusal when the value came from instances.toml.
check_tenant() {
    local where="${2:+ ($2)}"
    [ "$1" = tenant ] && return 0
    [[ "$1" =~ ^examples/[A-Za-z0-9_-]+$ ]] || refuse "tenant_dir \`$1\`$where: a tenant directory is examples/<name>, repo-relative (or a tenant_repo, delivered at /opt/boss/tenant)"
    [ -f "$TREE/$1/tenant.toml" ] || [ -f "$TREE/$1/seeds/tenant.toml" ] \
        || refuse "tenant_dir \`$1\`$where: no tenant.toml or seeds/tenant.toml under $TREE/$1 — not a tenant directory"
}
# tenant_of <section> — the directory the instance's pod reads under
# /opt/boss, from ONE of two sources in instances.toml (backlog
# f4f5c387): `tenant_dir` (a directory the image ships) or `tenant_repo`
# + `tenant_ref` (a forge repo the converge delivers at /opt/boss/tenant).
# Every other combination is refused by name, including the RETIRED
# `tenant = "<manifest path>"` key — a stale line must not read as
# "no source" any more than an unknown shares_with reads as "nobody".
tenant_of() {
    local s="$1" d r ref where
    where="${INSTANCES#"$TREE"/}: instance [$s]"
    [ -z "$(param "$s" tenant)" ] || refuse "$where declares \`tenant = …\`, the manifest-path key retired by f4f5c387 — declare tenant_dir = \"examples/<name>\" (the directory) or tenant_repo + tenant_ref"
    d=$(param "$s" tenant_dir); r=$(param "$s" tenant_repo); ref=$(param "$s" tenant_ref)
    if [ -n "$d" ] && { [ -n "$r" ] || [ -n "$ref" ]; }; then
        refuse "$where declares tenant_dir AND tenant_repo/tenant_ref — a tenant has one source"
    fi
    if [ -n "$r" ] || [ -n "$ref" ]; then
        [ -n "$r" ] || refuse "$where declares tenant_ref = \"$ref\" with no tenant_repo"
        [ -n "$ref" ] || refuse "$where declares tenant_repo = \"$r\" with no tenant_ref — which branch or tag is delivered?"
        [[ "$r" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || refuse "$where: tenant_repo \`$r\` is not owner/name on the forge"
        [[ "$ref" =~ ^[A-Za-z0-9_./-]+$ ]] && [[ "$ref" != *..* ]] || refuse "$where: tenant_ref \`$ref\` is not a branch or tag name"
        printf 'tenant\n'
        return 0
    fi
    [ -n "$d" ] || refuse "$where must declare its tenant source: tenant_dir = \"examples/<name>\", or tenant_repo + tenant_ref"
    check_tenant "$d" "[$s]"
    printf '%s\n' "$d"
}
check_sim() {
    case "$1" in true|false) ;; *) refuse "sim \`$1\`: BOSS_SIM_ENABLED is true or false" ;; esac
}
check_guest() {
    case "$1" in true|false) ;; *) refuse "guest \`$1\`: BOSS_GUEST_ACCESS is true or false (rendered \"1\" or \"0\")" ;; esac
}
# guest_value <true|false> — the string the manifest carries: the
# gateway reads BOSS_GUEST_ACCESS == "1" as on and anything else as off.
guest_value() { [ "$1" = true ] && printf '1\n' || printf '0\n'; }
# guest_of <section> — the instance's `guest`, refused by name when the
# line is missing: an instance that inherited the source's "1" by
# silence would hand anonymous visitors the company's read-only view.
guest_of() {
    local v
    v=$(param "$1" guest)
    [ -n "$v" ] || refuse "${INSTANCES#"$TREE"/}: instance [$1] must declare guest = true|false (BOSS_GUEST_ACCESS — may an anonymous visitor read?)"
    check_guest "$v"
    printf '%s\n' "$v"
}
check_hostname() {
    [[ "$1" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$ ]] || refuse "hostname \`$1\`: a TLS-front hostname is a lowercase DNS name with at least one dot"
}
# shares_with_ns <section> — the namespace of the instance <section>
# declares `shares_with`, empty when it declares none. The converge
# copies the instance's shared Secrets (forgejo-registry, resend —
# cluster-deploy-lib.sh provision_instance_secrets) from that namespace,
# so a value that names no instance is refused rather than read as
# "nobody": the instance would then wait on a person for Secrets it
# was declared to inherit.
shares_with_ns() {
    local s="$1" v
    v=$(param "$s" shares_with)
    [ -n "$v" ] || return 0
    [ "$v" != "$s" ] || refuse "${INSTANCES#"$TREE"/}: instance [$s] declares shares_with = \"$v\" — itself"
    # A here-string, not a pipe: `grep -q` exits at its match and a
    # producer still writing is SIGPIPE under pipefail.
    grep -qx -- "$v" <<< "$(sections)" || refuse "${INSTANCES#"$TREE"/}: instance [$s] declares shares_with = \"$v\", which is not an instance in this file"
    param "$v" namespace
}

# --- the source instance ----------------------------------------------------
# The values the files are WRITTEN with. Each is checked against the
# instance manifests, so the source entry cannot drift from the files.
load_source() {
    SRC=$(param "" source)
    [ -n "$SRC" ] || refuse "${INSTANCES#"$TREE"/}: no \`source = \"<instance>\"\` line — which instance are the files written for?"
    SRC_NS=$(param "$SRC" namespace); SRC_TENANT=$(tenant_of "$SRC") || exit $?
    SRC_SIM=$(param "$SRC" sim);      SRC_HOST=$(param "$SRC" hostname)
    SRC_GUEST=$(guest_of "$SRC") || exit $?
    [ -n "$SRC_NS" ] && [ -n "$SRC_TENANT" ] && [ -n "$SRC_SIM" ] && [ -n "$SRC_HOST" ] \
        || refuse "${INSTANCES#"$TREE"/}: source instance [$SRC] must declare namespace, a tenant source, sim, hostname and guest"
    check_namespace "$SRC_NS"; check_tenant "$SRC_TENANT"; check_sim "$SRC_SIM"; check_hostname "$SRC_HOST"
    local files=() f
    while read -r f; do files+=("$DIR/$f"); done < <(in_set instance)
    [ "${#files[@]}" -gt 0 ] || refuse "the roster names no instance manifest — nothing to render"
    grep -qE "^[[:space:]]*namespace: ${SRC_NS}\$" "${files[@]}" \
        || refuse "source namespace \`$SRC_NS\` appears on no instance manifest — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files"
    # The directory, whole: examples/brewery must not be read off
    # examples/brewery-two.
    grep -qE -- "/opt/boss/$(re_escape "$SRC_TENANT")([^A-Za-z0-9_./-]|$)" "${files[@]}" \
        || refuse "source tenant directory \`/opt/boss/$SRC_TENANT\` appears in no instance manifest (BOSS_TENANT_DIR) — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files"
    grep -qF -- "BOSS_SIM_ENABLED, value: \"$SRC_SIM\"" "${files[@]}" \
        || refuse "source sim \`$SRC_SIM\` is not the BOSS_SIM_ENABLED value in the instance manifests — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files"
    grep -qF -- "BOSS_GUEST_ACCESS, value: \"$(guest_value "$SRC_GUEST")\"" "${files[@]}" \
        || refuse "source guest \`$SRC_GUEST\` is not the BOSS_GUEST_ACCESS value (\"$(guest_value "$SRC_GUEST")\") in the instance manifests — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files"
    grep -qF -- "$SRC_HOST" "${files[@]}" \
        || refuse "source hostname \`$SRC_HOST\` appears in no instance manifest — ${INSTANCES#"$TREE"/} [$SRC] has drifted from the files (the TLS front's vhost is what it must be)"
}

re_escape() { printf '%s' "$1" | sed -e 's,[][\.*^$/|],\\&,g'; }

# --- one file, rendered --------------------------------------------------
render_file() { # <src file> <ns> <tenant> <sim> <hostname> <guest>
    local f="$1" ns="$2" tenant="$3" sim="$4" host="$5" guest="$6"
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
        -e "s#/opt/boss/${tenant_re}([^A-Za-z0-9_./-]|\$)#/opt/boss/${tenant}\1#g" \
        -e "s|(BOSS_SIM_ENABLED, value: )\"${SRC_SIM}\"|\1\"${sim}\"|" \
        -e "s|(BOSS_GUEST_ACCESS, value: )\"$(guest_value "$SRC_GUEST")\"|\1\"$(guest_value "$guest")\"|" \
        -e "s|${host_re}|${host}|g" \
        ${pins[@]+"${pins[@]}"} \
        "$f"
}

# --- one instance, into a directory or a stream ---------------------------
render_instance() { # <ns> <tenant> <sim> <hostname> <guest> <out-dir or "-">
    local ns="$1" tenant="$2" sim="$3" host="$4" guest="$5" out="$6" f first=1
    check_namespace "$ns"; check_tenant "$tenant"; check_sim "$sim"; check_hostname "$host"; check_guest "$guest"
    if [ "$out" != - ]; then
        mkdir -p "$out"
    fi
    while read -r f; do
        if [ "$out" = - ]; then
            [ "$first" = 1 ] || printf -- '---\n'
            first=0
            render_file "$DIR/$f" "$ns" "$tenant" "$sim" "$host" "$guest"
        else
            render_file "$DIR/$f" "$ns" "$tenant" "$sim" "$host" "$guest" > "$out/$f"
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
        # Every row resolves before any is printed: a refusal after the
        # first row would read as a shorter list.
        rows=""
        for s in $(sections); do
            share=$(shares_with_ns "$s") || exit $?
            tenant=$(tenant_of "$s") || exit $?
            rows="$rows$(printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s' "$s" "$(param "$s" namespace)" "$tenant" "$(param "$s" sim)" "$(param "$s" hostname)" "$share" "$(param "$s" tenant_repo)" "$(param "$s" tenant_ref)")"$'\n'
        done
        printf '%s' "$rows"
        ;;
    --all)
        [ $# -eq 2 ] && [ -n "$2" ] || usage
        roster > /dev/null
        load_source
        # Every `shares_with` and every tenant source resolves, or
        # nothing is rendered: the render stage is where the converge
        # first reads this file, and a refusal after the first instance's
        # directory exists would read as a partial render.
        for s in $(sections); do shares_with_ns "$s" > /dev/null; tenant_of "$s" > /dev/null; guest_of "$s" > /dev/null; done
        seen_ns=""
        for s in $(sections); do
            ns=$(param "$s" namespace); tenant=$(tenant_of "$s"); sim=$(param "$s" sim); host=$(param "$s" hostname); guest=$(guest_of "$s")
            [ -n "$ns" ] && [ -n "$tenant" ] && [ -n "$sim" ] && [ -n "$host" ] \
                || refuse "${INSTANCES#"$TREE"/}: instance [$s] must declare namespace, a tenant source, sim, hostname and guest"
            case "$seen_ns" in *"|$ns|"*) refuse "${INSTANCES#"$TREE"/}: two instances share namespace \`$ns\`" ;; esac
            seen_ns="$seen_ns|$ns|"
            render_instance "$ns" "$tenant" "$sim" "$host" "$guest" "$2/$ns"
        done
        ;;
    --*|'')
        usage
        ;;
    *)
        out=-
        if [ $# -eq 7 ] && [ "$6" = --out-dir ] && [ -n "$7" ]; then
            out="$7"
        elif [ $# -ne 5 ]; then
            usage
        fi
        roster > /dev/null
        load_source
        render_instance "$1" "$2" "$3" "$4" "$5" "$out"
        ;;
esac
