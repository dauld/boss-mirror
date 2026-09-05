//! Serve the frontend SPA from a static directory.
//!
//! Replaces the reverse-proxy-to-observability approach. The gateway
//! reads files from `BOSS_STATIC_DIR` (default `/var/lib/boss-web/dist`)
//! and serves them directly. Unknown paths return `index.html` so the
//! client-side router handles navigation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;
use boss_gateway::session::{self, Session, find_cookie};

/// Resolve the static directory (cached via env on first call).
pub fn static_dir() -> &'static str {
    static DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var("BOSS_STATIC_DIR").unwrap_or_else(|_| "/var/lib/boss-web/dist".to_string())
    })
}

/// Pages served without a session.
///
/// `/login` is the load-bearing one, and its absence here was a
/// circular lock: the page that hands out a session required one to
/// load, so a signed-out visitor got a bare "authentication required"
/// and no way forward.
///
/// It went unnoticed because demo mode used to mint a session for
/// anyone arriving without a valid cookie — the minter ran ahead of
/// this gate, so in practice nobody ever reached it unauthenticated.
/// Removing the minter did not create the bug; it revealed one that
/// had been masked since the gate was written.
///
/// Anything added here is readable by the entire internet. The bar is
/// "a signed-out visitor cannot proceed without it", not "it's
/// convenient".
fn is_public_path(path: &str) -> bool {
    matches!(path, "/" | "/login" | "/login/" | "/health")
        // /auth is the SSH-CA endpoint (CLI operator flow).
        || path.starts_with("/auth")
}

/// Handle all `/dashboard/*` and root `/*` requests for the SPA.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    // Session gate for /dashboard/* HTML pages.
    // Static assets (JS, CSS, fonts, images) are always served —
    // they're content-hashed and not sensitive. Only HTML pages
    // require authentication, because the SPA code inside handles
    // the auth redirect flow. If we redirect asset requests to
    // auth, the browser gets a CORS error and can't load at all.
    let path = req.uri().path();
    let is_static_asset = has_file_extension(path);
    if !is_static_asset
        && !is_public_path(path)
        && !has_valid_session(req.headers(), &state.session_key)
    {
        return unauthorized();
    }

    // Strip /dashboard prefix if present. Both / and /dashboard
    // serve the same SPA — the client-side router handles navigation.
    let stripped = path.strip_prefix("/dashboard").unwrap_or(path);
    let file_path = match stripped {
        "" | "/" => "/index.html",
        other => other,
    };

    // Resolve to a file on disk.
    let base = static_dir();
    let clean = file_path.trim_start_matches('/');
    let full_path = PathBuf::from(base).join(clean);

    // Security: don't serve files outside the static dir.
    if !full_path.starts_with(base) {
        return StatusCode::FORBIDDEN.into_response();
    }

    // Try to read the file. If it doesn't exist, serve index.html (SPA fallback).
    let (content, serving_path) = match tokio::fs::read(&full_path).await {
        Ok(bytes) => (bytes, full_path),
        // A request that NAMES A FILE and misses is a 404, not the SPA.
        //
        // The fallback exists so `/system/yard` reaches the client-side
        // router. It must not swallow `/dashboard/chunk-abc123.js`. When
        // it did, a browser holding a chunk hash from before a deploy
        // asked for that chunk, got `200 text/html`, and tried to execute
        // `<!doctype html>` as JavaScript — the app never mounted, so the
        // page rendered with no styling, no nav, and no pages, while every
        // layer reported success. A missing stylesheet was worse: browsers
        // drop a text/html stylesheet without a word.
        //
        // 2026-08-13: reported as "did we just have a huge regression /
        // where is the Train Yard / the new styling is all gone". Nothing
        // had regressed — the deploy was current and correct. The only
        // defect was this fallback answering 200 to a question whose
        // honest answer was 404, which is the same silent-loss shape as a
        // JSON endpoint falling through to index.html (see main.rs, where
        // the missing bare matcher was fixed for exactly this reason).
        Err(_) if is_static_asset => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => {
            // SPA fallback: serve index.html for any non-file path.
            let index = PathBuf::from(base).join("index.html");
            match tokio::fs::read(&index).await {
                Ok(bytes) => (bytes, index),
                Err(_) => {
                    return (
                        StatusCode::NOT_FOUND,
                        "frontend not built — run: cd apps/web && bun run build",
                    )
                        .into_response();
                }
            }
        }
    };

    // THE FIRST PAINT KNOWS THE TENANT (5578e42d). The SPA fetched the
    // manifest after first paint and rendered placeholder truth until
    // it arrived — every module, generic labels — so an operator
    // watched the application change its mind and read the first
    // frame as a stale view. The manifest rides the document instead:
    // no extra round trip, no flash. index.html is never cached, so
    // this is recomputed on every load, like the API answer it mirrors.
    let content = if serving_path.ends_with("index.html") {
        match (
            std::str::from_utf8(&content),
            serde_json::to_string(&crate::api::tenant_manifest_now()),
        ) {
            (Ok(html), Ok(json)) => inline_tenant_manifest(html, &json).into_bytes(),
            _ => content,
        }
    } else {
        content
    };
    let content_type = guess_content_type(&serving_path);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type);

    // Cache static assets (JS, CSS) aggressively — they have content hashes in filenames.
    // Don't cache index.html *anywhere* — Cloudflare's edge will hold
    // it for minutes under plain `no-cache` (which means "store but
    // revalidate"), and a stale HTML pointing at a no-longer-current
    // chunk hash causes "I redeployed but the user sees the old app"
    // exactly because the browser then trusts its own immutable
    // cache for the stale hash. The fix is to tell every layer to
    // not store it at all.
    if !serving_path.ends_with("index.html") {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    } else {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-cache, must-revalidate, max-age=0"),
        );
        // Belt-and-braces for HTTP/1.0 + CDNs that ignore Cache-Control.
        headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        // Standard CDN-only directive. Cloudflare honors this even
        // when the regular Cache-Control would be edge-cached.
        headers.insert(
            axum::http::HeaderName::from_static("cdn-cache-control"),
            HeaderValue::from_static("no-store"),
        );
        // Cloudflare-specific override. Set so a future CF page rule
        // can't accidentally re-introduce HTML caching.
        headers.insert(
            axum::http::HeaderName::from_static("cloudflare-cdn-cache-control"),
            HeaderValue::from_static("no-store"),
        );
    }

    (StatusCode::OK, headers, content).into_response()
}

fn guess_content_type(path: &Path) -> HeaderValue {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let ct = match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    HeaderValue::from_static(ct)
}

fn has_valid_session(headers: &HeaderMap, key: &[u8]) -> bool {
    let Some(cookie_header) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(raw) = find_cookie(cookie_header, session::COOKIE_NAME) else {
        return false;
    };
    Session::decode(raw, key).is_ok()
}

/// True if the path has a file extension (e.g., .js, .css, .woff2).
/// Used to distinguish asset requests from SPA page navigations.
fn has_file_extension(path: &str) -> bool {
    let last_segment = path.rsplit('/').next().unwrap_or("");
    last_segment.contains('.')
}

/// Put the tenant manifest on the document, as `window.__BOSS_TENANT_MANIFEST__`,
/// just before `</head>` so it is defined before any module script
/// runs. Pure: the same html and json always give the same page. A
/// document with no `</head>` is returned untouched — nowhere safe to
/// put it. `</` inside the JSON becomes `<\/` so a label can never
/// close the script tag early; JSON reads it back as the same string.
pub(crate) fn inline_tenant_manifest(html: &str, manifest_json: &str) -> String {
    let Some(idx) = html.find("</head>") else {
        return html.to_string();
    };
    let safe = manifest_json.replace("</", "<\\/");
    let tag = format!("<script>window.__BOSS_TENANT_MANIFEST__ = {safe};</script>\n");
    let mut out = String::with_capacity(html.len() + tag.len());
    out.push_str(&html[..idx]);
    out.push_str(&tag);
    out.push_str(&html[idx..]);
    out
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "authentication required").into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_manifest_rides_the_document_before_the_head_closes() {
        let html = "<html><head><title>x</title></head><body><script type=\"module\" src=\"/m.js\"></script></body></html>";
        let out = super::inline_tenant_manifest(
            html,
            r#"{"modules":{"hr":false},"labels":{"assets.entity_singular":"vessel"}}"#,
        );
        let script = out
            .find("window.__BOSS_TENANT_MANIFEST__")
            .expect("the global is defined");
        let head_end = out.find("</head>").unwrap();
        let module = out.find("type=\"module\"").unwrap();
        assert!(script < head_end, "defined inside <head>");
        assert!(script < module, "defined before any module script runs");
        assert!(out.contains(r#""hr":false"#) && out.contains("vessel"));
    }

    #[test]
    fn a_document_with_no_head_is_left_alone() {
        let html = "<div>not a document</div>";
        assert_eq!(super::inline_tenant_manifest(html, "{}"), html);
    }

    #[test]
    fn a_label_cannot_close_the_script_early() {
        let json = r#"{"labels":{"x":"</script><script>alert(1)</script>"}}"#;
        let out = super::inline_tenant_manifest("<head></head>", json);
        // Exactly one closing tag: ours. The label's `</` is escaped so
        // the browser never sees a second one.
        assert_eq!(out.matches("</script>").count(), 1, "{out}");
        assert!(out.contains(r"<\/script>"), "{out}");
    }

    use super::*;

    /// Regression, and the expensive kind: a signed-out visitor asked
    /// for the sign-in page and got "authentication required". Demo
    /// mode had masked it by minting a session ahead of this gate, so
    /// the circularity only surfaced once the minter was removed.
    #[test]
    fn the_sign_in_page_is_reachable_without_a_session() {
        assert!(is_public_path("/login"));
        assert!(is_public_path("/login/"));
    }

    #[test]
    fn the_landing_surface_and_probes_stay_public() {
        assert!(is_public_path("/"));
        assert!(is_public_path("/health"));
        assert!(is_public_path("/auth/ssh-ca"));
    }

    /// The gate still has to gate. If this ever passes, the session
    /// check has been widened into a no-op.
    #[test]
    fn application_pages_are_not_public() {
        for path in ["/ux/jobs", "/system", "/me", "/finance"] {
            assert!(!is_public_path(path), "{path} must require a session");
        }
    }

    #[test]
    fn guess_content_type_for_known_extensions() {
        assert_eq!(
            guess_content_type(Path::new("app.js")),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            guess_content_type(Path::new("style.css")),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            guess_content_type(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn unknown_extension_is_octet_stream() {
        assert_eq!(
            guess_content_type(Path::new("data.bin")),
            "application/octet-stream"
        );
    }
}

#[cfg(test)]
mod asset_fallback_tests {
    use super::has_file_extension;

    /// The predicate the fallback branches on. A path naming a file is
    /// an ASSET request and must 404 when it misses; a path naming a
    /// route is an SPA request and must reach index.html.
    #[test]
    fn asset_paths_and_route_paths_are_told_apart() {
        // Assets — a miss here must be a 404, or the browser executes
        // HTML as JavaScript and the app dies silently.
        for asset in [
            "/dashboard/chunk-f2m8gpyh.js",
            "/dashboard/chunk-50737a54.css",
            "/dashboard/chunk-abc.js.map",
            "/favicon.ico",
            "/kb-assets/01-primitives.svg",
        ] {
            assert!(
                has_file_extension(asset),
                "{asset} must be read as an asset"
            );
        }

        // Routes — these MUST still fall through to the SPA, which is
        // the whole reason the fallback exists. `/system/yard` reaching
        // index.html is what makes a deep link work at all.
        for route in [
            "/",
            "/system/yard",
            "/it/yard",
            "/system/design",
            "/jobs/6fde677f-b3f1-468b-ae54-47c8b44d0823",
            "/dashboard",
        ] {
            assert!(
                !has_file_extension(route),
                "{route} must still reach the SPA fallback"
            );
        }
    }

    /// A job id carries no dot, but a Subject id or a doc path could.
    /// Pinning the boundary: the check looks at the LAST segment only,
    /// so a dot earlier in the path does not turn a route into an asset.
    #[test]
    fn a_dot_earlier_in_the_path_does_not_make_a_route_an_asset() {
        assert!(!has_file_extension("/docs/design/the-three-layers.md/view"));
        assert!(has_file_extension("/docs/design/the-three-layers.md"));
    }
}
