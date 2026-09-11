#!/usr/bin/env bash
# every-spa-api-path-is-routed.sh — every `/api/<segment>` the SPA
# fetches is answered by the gateway.
#
# THE PAIR THAT DRIFTED TWICE. The SPA fetches `/api/<segment>/…`; the
# gateway answers a segment either from its proxy table (proxy.rs
# `ProxyConfig::new("jobs")`) or from a route of its own
# (`"/api/auth/…"`, `"/api/tenant/…"`). Nothing tied
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
# shellcheck source=lib/strip-comments.sh
. "$here/lib/strip-comments.sh"
export -f strip_comments
# The product sources this lint reads, one path per line.
sources() {
    find "$1/apps/web/src" "$1/libs/web-kit/src" "$1/apps/simulator/src" \
        \( -name '*.ts' -o -name '*.svelte' \) \
        ! -name '*.test.ts' ! -name 'dev-server.ts' -type f 2>/dev/null | sort
}

# Segments the gateway answers, one per line, kebab-case, sorted.
answered() {
    local gw="$1/crates/core/boss-gateway/src"
    {
        grep -rhoE 'ProxyConfig::(new|with_fallback)\("[a-z_-]+"' "$gw" 2>/dev/null \
            | grep -oE '"[a-z_-]+"' | tr -d '"'
        grep -rhoE '"/api/[a-z_-]+' "$gw" 2>/dev/null | sed 's|^"/api/||'
    } | tr '_' '-' | sort -u
}

# Segments the SPA fetches, one per line, sorted.
# A MENTION is not a USE: comments are stripped first (lib/strip-comments.sh),
# so a docstring explaining why a path is unreachable is not a fetch site
# (a1752a75).
fetched() {
    local root="$1"
    sources "$root" | xargs -d '\n' -r bash -c 'strip_comments "$@"' _ \
        | grep -oE "['\"\`]/api/[a-z_-]+" \
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
pub static POLICY: ProxyConfig = ProxyConfig::with_fallback("policy", degrade);
RS
    cat >"$fx/crates/core/boss-gateway/src/main.rs" <<'RS'
    .route("/api/auth/login", post(login))
    .route("/api/tenant/manifest", get(manifest))
RS
    cat >"$fx/apps/web/src/it/Page.svelte" <<'SV'
    <!-- a Svelte comment naming '/api/ghost-html' is a mention, not a use -->
    fetch('/api/jobs/health'); fetch(`/api/subject-kinds/${k}`); fetch("/api/policy/x");
    fetch('/api/auth/me'); fetch('/api/yard/status');
SV
    # A docstring that names unreachable paths — the a1752a75 car — plus a
    # URL whose `//` is not a comment, plus the swallow hazard: prose that
    # mentions `/api/*` must not open a block comment that eats the fetch
    # after it. Only the real fetch of /api/yard may be reported.
    cat >"$fx/apps/web/src/it/crew.ts" <<'TS'
    /// The agent-runs surface at '/api/ghost-doc' is unrouted at the gateway,
    /// so it cannot be reached from a browser. A glob like /api/* in prose
    /// is not a comment opener either.
    const docs = 'http://forge/api/ghost-url'; // trailing note names '/api/ghost-line'
    /* and a block comment
       naming "/api/ghost-block" */
    fetch('/api/yard/status');
TS
    printf "fetch('/api/things');\n" >"$fx/apps/web/src/paginated.test.ts"
    printf "['/api/snapshot', 'observability'],\n" >"$fx/apps/web/src/dev-server.ts"
    local got; got="$(unrouted "$fx" | tr '\n' ' ' | sed 's/ $//')"
    [[ "$got" == "yard" ]] || { echo "every-spa-api-path-is-routed: self-test FAILED — expected the planted 'yard' alone, got '${got}'" >&2; return 1; }
    # The stripper must keep line numbers, or "fetched at:" names the wrong line.
    local nl; nl="$(strip_comments "$fx/apps/web/src/it/crew.ts" | wc -l | tr -d ' ')"
    [[ "$nl" == "$(wc -l <"$fx/apps/web/src/it/crew.ts" | tr -d ' ')" ]] || { echo "every-spa-api-path-is-routed: self-test FAILED — stripping comments changed the line count ($nl)" >&2; return 1; }
    echo "every-spa-api-path-is-routed: self-test ok — planted /api/yard caught; jobs, subject-kinds (via subject_kinds), policy (via with_fallback), auth (gateway route) answered; a test's /api/things and the dev server's aliases ignored; four /api/ghost-* mentions in a docstring, a line comment, a block comment and an HTML comment not counted as fetches, a URL's // not read as a comment, and the line count preserved"
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
        while IFS= read -r f; do
            strip_comments "$f" | grep -nE "['\"\`]/api/$seg\b" | sed "s|^|${f#"$repo"/}:|"
        done < <(sources "$repo") | head -3 | sed 's|^|    |' >&2
    done <<<"$missing"
    echo "  Add it to the gateway's proxy table (crates/core/boss-gateway/src/proxy.rs) or route it there; a fetch the gateway cannot answer is an empty panel in production." >&2
    exit 1
fi
echo "every-spa-api-path-is-routed: ${count} /api segments fetched by the SPA, every one answered by the gateway"
exit 0
