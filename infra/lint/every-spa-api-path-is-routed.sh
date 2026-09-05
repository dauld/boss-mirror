#!/usr/bin/env bash
# every-spa-api-path-is-routed.sh — every `/api/<segment>` the SPA
# fetches is answered by the gateway.
#
# THE PAIR THAT DRIFTED TWICE. The SPA fetches `/api/<segment>/…`; the
# gateway answers a segment either from its proxy table (proxy.rs
# `ProxyConfig::new("jobs")`, `aliased("design", "docs")`) or from a
# route of its own (`"/api/auth/…"`, `"/api/tenant/…"`). Nothing tied
# the two lists together, so a page could ship fetching a segment the
# gateway had never heard of, and the failure is a 404 that renders as
# an empty panel: `/api/stations` on train #10, then `/api/yard/status`
# on train #192 — the gates-and-garage UI invisible from the day it
# shipped (packet 3b465a95, instance 1). CLAUDE.md §9a: a fact that
# lives twice gets an equality test. This is that test, as a lint,
# because the two facts live in different languages.
#
# WHAT IT READS. The product's sources only: apps/web, libs/web-kit,
# apps/simulator — not `*.test.ts` (a test may fetch `/api/things` on
# purpose) and not the dev server, which is a second routing table for
# local development and has its own comment on every alias. The gateway
# side is read from crates/core/boss-gateway/src, both tables. Segments
# compare kebab-case (`subject_kinds` answers `subject-kinds`).
#
# Usage: infra/lint/every-spa-api-path-is-routed.sh [--self-test]
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Segments the gateway answers, one per line, kebab-case, sorted.
answered() {
    local gw="$1/crates/core/boss-gateway/src"
    {
        grep -rhoE 'ProxyConfig::(new|aliased)\("[a-z_-]+"' "$gw" 2>/dev/null \
            | grep -oE '"[a-z_-]+"' | tr -d '"'
        grep -rhoE '"/api/[a-z_-]+' "$gw" 2>/dev/null | sed 's|^"/api/||'
    } | tr '_' '-' | sort -u
}

# Segments the SPA fetches, one per line, sorted.
fetched() {
    local root="$1"
    grep -rhoE "['\"\`]/api/[a-z_-]+" \
        "$root/apps/web/src" "$root/libs/web-kit/src" "$root/apps/simulator/src" \
        --include='*.ts' --include='*.svelte' \
        --exclude='*.test.ts' --exclude='dev-server.ts' 2>/dev/null \
        | sed -E "s|^['\"\`]/api/||" | tr '_' '-' | sort -u
}

# Lines in `fetched` and not in `answered`.
unrouted() { comm -23 <(fetched "$1") <(answered "$1"); }

self_test() {
    local fx; fx="$(mktemp -d)"
    trap 'rm -rf "$fx"' RETURN
    mkdir -p "$fx/crates/core/boss-gateway/src" "$fx/apps/web/src/it" "$fx/libs/web-kit/src" "$fx/apps/simulator/src"
    cat >"$fx/crates/core/boss-gateway/src/proxy.rs" <<'RS'
pub static JOBS: ProxyConfig = ProxyConfig::new("jobs");
pub static SUBJECT_KINDS: ProxyConfig = ProxyConfig::new("subject_kinds");
pub static DESIGN: ProxyConfig = ProxyConfig::aliased("design", "docs");
RS
    cat >"$fx/crates/core/boss-gateway/src/main.rs" <<'RS'
    .route("/api/auth/login", post(login))
    .route("/api/tenant/manifest", get(manifest))
RS
    cat >"$fx/apps/web/src/it/Page.svelte" <<'SV'
    fetch('/api/jobs/health'); fetch(`/api/subject-kinds/${k}`); fetch("/api/design/x");
    fetch('/api/auth/me'); fetch('/api/yard/status');
SV
    printf "fetch('/api/things');\n" >"$fx/apps/web/src/paginated.test.ts"
    printf "['/api/snapshot', 'observability'],\n" >"$fx/apps/web/src/dev-server.ts"
    local got; got="$(unrouted "$fx" | tr '\n' ' ' | sed 's/ $//')"
    [[ "$got" == "yard" ]] || { echo "every-spa-api-path-is-routed: self-test FAILED — expected the planted 'yard' alone, got '${got}'" >&2; return 1; }
    echo "every-spa-api-path-is-routed: self-test ok — planted /api/yard caught; jobs, subject-kinds (via subject_kinds), design (aliased), auth (gateway route) answered; a test's /api/things and the dev server's aliases ignored"
}

if [[ "${1:-}" == "--self-test" ]]; then self_test; exit $?; fi
self_test || exit 1

repo="$(cd "$here/../.." && pwd)"
missing="$(unrouted "$repo")"
count="$(fetched "$repo" | wc -l | tr -d ' ')"
if [[ -n "$missing" ]]; then
    echo "every-spa-api-path-is-routed: FAIL — the SPA fetches segment(s) the gateway does not answer:" >&2
    while read -r seg; do
        [[ -z "$seg" ]] && continue
        echo "  /api/$seg — fetched at:" >&2
        grep -rnE "['\"\`]/api/$seg\b" "$repo/apps/web/src" "$repo/libs/web-kit/src" "$repo/apps/simulator/src" \
            --include='*.ts' --include='*.svelte' --exclude='*.test.ts' --exclude='dev-server.ts' 2>/dev/null \
            | head -3 | sed "s|^$repo/|    |" >&2
    done <<<"$missing"
    echo "  Add it to the gateway's proxy table (crates/core/boss-gateway/src/proxy.rs) or route it there; a fetch the gateway cannot answer is an empty panel in production." >&2
    exit 1
fi
echo "every-spa-api-path-is-routed: ${count} /api segments fetched by the SPA, every one answered by the gateway"
exit 0
