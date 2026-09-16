#!/usr/bin/env bash
#
# a-public-url-names-a-registered-oidc-redirect — BOSS_PUBLIC_URL may
# only name a hostname whose OIDC redirect the tree declares registered
# at the IdP.
#
# WHY THIS IS A CONFIG-CHANNEL CHECK AND NOT A TEST
# -------------------------------------------------
# The gateway builds its OIDC redirect_uri as
# `<BOSS_PUBLIC_URL>/api/auth/oidc/callback` (boss-gateway oidc.rs), and
# Kanidm refuses an authorization-code flow whose redirect_uri is not
# registered on the `boss` OAuth2 client. So the day BOSS_PUBLIC_URL
# flips to a hostname whose callback is not registered, every login
# breaks at the first redirect — and nothing in the gate can see it: the
# gate never talks to Kanidm, the pod cannot read Kanidm's admin API,
# and the manifest is applied by the converge, not built. The only thing
# that can hold the invariant is a shape lint reading the manifest
# against a DECLARED fact (backlog 198c5fe9: the flip to
# https://boss.algedonic.dev, whose redirect was registered on
# 2026-08-12 and recorded in infra/cluster/dns/access.toml with that
# provenance).
#
# WHAT IS ASSERTED
# ----------------
# For every `BOSS_PUBLIC_URL` value in infra/cluster/manifests/*.yaml:
# its hostname has an `[[oidc_redirect]]` entry in
# infra/cluster/dns/access.toml with `registered = true`. An entry with
# `registered = false` is a refusal naming the manifest, the hostname
# and the fact; a hostname with no entry at all is a refusal too — an
# undeclared redirect is not a registered one. Correcting the fact is a
# read of the IdP (`kanidm system oauth2 get boss`), recorded in the
# declaration's `measured`, never an edit to make the lint pass.
#
# NON-VACUITY. No BOSS_PUBLIC_URL in any manifest, or no access.toml, is
# a lint that lost its subject and says so (exit 1), the way every lint
# here refuses to report clean on nothing.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

MANIFESTS="infra/cluster/manifests"
DECLARATION="infra/cluster/dns/access.toml"

if [ ! -r "$DECLARATION" ]; then
    echo "a-public-url-names-a-registered-oidc-redirect: $DECLARATION is missing — the redirect declaration is where BOSS_PUBLIC_URL's hostname must be declared registered" >&2
    exit 1
fi

# hostname<TAB>registered, one per [[oidc_redirect]] entry, in file
# order. Comments stripped; the two keys read inside the entry only.
declared_redirects() {
    awk '
        /^[[:space:]]*#/ { next }
        /^\[\[oidc_redirect\]\]/ { if (host != "") print host "\t" reg; host = ""; reg = ""; in_entry = 1; next }
        /^\[\[/ || /^\[/ { if (host != "") print host "\t" reg; host = ""; reg = ""; in_entry = 0; next }
        in_entry && /^[[:space:]]*hostname[[:space:]]*=/ { s = $0; sub(/^[^"]*"/, "", s); sub(/".*$/, "", s); host = s; next }
        in_entry && /^[[:space:]]*registered[[:space:]]*=/ { s = $0; sub(/^[^=]*=[[:space:]]*/, "", s); sub(/[[:space:]#].*$/, "", s); reg = s; next }
        END { if (host != "") print host "\t" reg }
    ' "$DECLARATION"
}

redirects="$(declared_redirects)"
if [ -z "$redirects" ]; then
    echo "a-public-url-names-a-registered-oidc-redirect: $DECLARATION declares no [[oidc_redirect]] entry — nothing to judge BOSS_PUBLIC_URL against" >&2
    exit 1
fi

# `file<TAB>url` for every BOSS_PUBLIC_URL value in the manifests, in
# either env idiom: `{name: BOSS_PUBLIC_URL, value: "..."}` on one line,
# or `- name: BOSS_PUBLIC_URL` followed by `value: "..."`.
public_urls() {
    local f
    for f in "$MANIFESTS"/*.yaml; do
        [ -e "$f" ] || continue
        awk -v file="$f" '
            /^[[:space:]]*#/ { next }
            /BOSS_PUBLIC_URL/ && /value:/ {
                s = $0; sub(/.*value:[[:space:]]*/, "", s); gsub(/[\"'"'"'{}]/, "", s); sub(/[[:space:]].*$/, "", s)
                print file "\t" s; next
            }
            /name:[[:space:]]*BOSS_PUBLIC_URL[[:space:]]*$/ { pending = 1; next }
            pending && /value:/ {
                s = $0; sub(/.*value:[[:space:]]*/, "", s); gsub(/[\"'"'"']/, "", s); sub(/[[:space:]].*$/, "", s)
                print file "\t" s; pending = 0; next
            }
            pending && /name:/ { pending = 0 }
        ' "$f"
    done
}

urls="$(public_urls)"
if [ -z "$urls" ]; then
    echo "a-public-url-names-a-registered-oidc-redirect: no BOSS_PUBLIC_URL in $MANIFESTS/*.yaml — the lint has lost its subject" >&2
    exit 1
fi

status=0
while IFS=$'\t' read -r file url; do
    [ -n "$url" ] || continue
    host="${url#*://}"
    host="${host%%/*}"
    host="${host%%:*}"
    reg=""
    found=0
    while IFS=$'\t' read -r dhost dreg; do
        if [ "$dhost" = "$host" ]; then
            found=1
            reg="$dreg"
        fi
    done <<< "$redirects"
    if [ "$found" -eq 0 ]; then
        echo "REFUSED   $file: BOSS_PUBLIC_URL=$url names $host, and $DECLARATION has no [[oidc_redirect]] entry for it — declare the redirect (registered = true only after reading it on the IdP: kanidm system oauth2 get boss)" >&2
        status=1
    elif [ "$reg" != "true" ]; then
        echo "REFUSED   $file: BOSS_PUBLIC_URL=$url names $host, whose [[oidc_redirect]] is declared registered = false in $DECLARATION — every login would break at the first redirect; register https://$host/api/auth/oidc/callback on the boss client, re-read it, then flip the declaration" >&2
        status=1
    else
        echo "ok        $file: BOSS_PUBLIC_URL=$url — $host redirect declared registered"
    fi
done <<< "$urls"

exit "$status"
