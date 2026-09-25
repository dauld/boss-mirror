//! Serve the frontend SPA from a static directory.
//!
//! Replaces the reverse-proxy-to-observability approach. The gateway
//! reads files from `BOSS_STATIC_DIR` (default `/var/lib/boss-web/dist`)
//! and serves them directly. Unknown paths return `index.html` so the
//! client-side router handles navigation.

use std::path::{Component, Path, PathBuf};
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
    serve(&state, req, Path::new(static_dir())).await
}

/// The handler's body, with the directory it serves from as an
/// argument so a test can point it at a scratch directory — the
/// `static_dir()` cache is process-wide and read once.
async fn serve(state: &AppState, req: Request, base: &Path) -> Response {
    // Who the page is for, as the role-header layer signed it from the
    // session cookie (role_headers.rs) — the identity the flights read
    // is resolved for. Taken before anything else reads the request.
    let viewer = req.headers().get("x-boss-user").cloned();
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
    // Only a WHOLE segment is stripped: as a bare string prefix,
    // `/dashboard../x.key` became `../x.key`, a climb carrying no
    // dot-segment for the edge to refuse (dot-segment car review,
    // 2026-09-25). `resolve` below runs on what the strip leaves.
    let stripped = match path.strip_prefix("/dashboard") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
        _ => path,
    };
    let file_path = match stripped {
        "" | "/" => "/index.html",
        other => other,
    };

    // Resolve to a file on disk, refusing any path that is not plain
    // names (backlog 814a32a2). The guard this replaces joined the raw
    // path and asked `PathBuf::starts_with(base)`, which compares
    // components LEXICALLY: `dist/../secret.key` starts with `dist`, and
    // the OS resolved the `..` on read. With a dotted last segment
    // served sessionless, that was an unauthenticated read of any file
    // with an extension — the session key included. The edge refuses
    // dot-segments too (fix/gateway-refuses-dot-segments-before-routing);
    // this is the handler holding its own line regardless.
    let Some(full_path) = resolve(base, file_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Try to read the file. If it doesn't exist, serve index.html (SPA fallback).
    let (content, serving_path) = match read_inside(base, &full_path).await {
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
            let index = base.join("index.html");
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
    // THE FIRST PAINT KNOWS THE VIEWER'S FLIGHTS (design c4c2a607,
    // backlog 73c31776), for the manifest's reason: a flag read after
    // first paint would show the old path and then swap. Resolved per
    // viewer on the jobs upstream; a page with no session, or a read
    // that fails, carries no global, and the SPA reads every code as
    // off — the old path, which is the safe one.
    let content = match (serving_path.ends_with("index.html"), viewer) {
        (true, Some(viewer)) => match (
            std::str::from_utf8(&content),
            flights_for(&state.proxy_client, viewer).await,
        ) {
            (Ok(html), Some(json)) => inline_flights(html, &json).into_bytes(),
            _ => content,
        },
        _ => content,
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

pub(crate) fn guess_content_type(path: &Path) -> HeaderValue {
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

/// The file a request path names inside `base`, or `None` when the path
/// names nothing servable — site.rs's `resolve`, made stricter. Every
/// segment is read RAW first, because `Path::components` folds an
/// interior `.` away without a word: a `.` or `..` segment, or a
/// backslash anywhere, refuses the path rather than being resolved.
/// Then every component must be a plain name, so a root or a prefix
/// refuses it too. The request path is never percent-decoded here, so
/// `%2e%2e` is a literal name that exists nowhere (backlog 814a32a2).
fn resolve(base: &Path, request_path: &str) -> Option<PathBuf> {
    let rel = request_path.trim_start_matches('/');
    if rel
        .split('/')
        .any(|seg| seg == "." || seg == ".." || seg.contains('\\'))
    {
        return None;
    }
    let mut out = base.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(seg) => out.push(seg),
            _ => return None,
        }
    }
    Some(out)
}

/// Read `file` only if, with every symlink and `..` resolved by the OS,
/// it still sits inside the resolved `base`. The lexical check in
/// `resolve` is the first wall; this is the second, and it is the one a
/// symlink inside the directory cannot walk past. A file outside reads
/// as absent, so the caller's miss handling (404 for an asset, the SPA
/// for a route) applies to it unchanged and says nothing about what is
/// out there.
async fn read_inside(base: &Path, file: &Path) -> std::io::Result<Vec<u8>> {
    let base = tokio::fs::canonicalize(base).await?;
    let file = tokio::fs::canonicalize(file).await?;
    if !file.starts_with(&base) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "outside the static directory",
        ));
    }
    tokio::fs::read(&file).await
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
    inline_global(html, "__BOSS_TENANT_MANIFEST__", manifest_json)
}

/// Put the viewer's flights on the document, as `window.__BOSS_FLIGHTS__`
/// — the same placement and escaping as the manifest.
pub(crate) fn inline_flights(html: &str, flights_json: &str) -> String {
    inline_global(html, "__BOSS_FLIGHTS__", flights_json)
}

/// The jobs upstream's `/api/flights/mine` answer for `viewer`, as the
/// JSON the page carries — or `None` when it cannot be had: upstream
/// down, slow (a page load waits at most two seconds for it), non-2xx,
/// or a body that is not the answer's shape.
async fn flights_for(client: &reqwest::Client, viewer: HeaderValue) -> Option<String> {
    let url = format!(
        "{}/api/flights/mine",
        crate::proxy::JOBS.upstream_url().trim_end_matches('/')
    );
    let resp = client
        .get(url)
        .header("x-boss-user", viewer)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        tracing::warn!(
            status = %resp.status(),
            "flights read answered non-2xx; the page carries no flights, so every code is off"
        );
        return None;
    }
    flights_body(&resp.json::<serde_json::Value>().await.ok()?)
}

/// The page's copy of a flights answer: exactly `{"flights": [codes]}`,
/// rebuilt from the codes so nothing else the upstream says rides onto
/// the document. Anything else is `None`.
pub(crate) fn flights_body(answer: &serde_json::Value) -> Option<String> {
    let codes: Vec<&str> = answer
        .get("flights")?
        .as_array()?
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<_>>()?;
    serde_json::to_string(&serde_json::json!({ "flights": codes })).ok()
}

fn inline_global(html: &str, global: &str, json: &str) -> String {
    let Some(idx) = html.find("</head>") else {
        return html.to_string();
    };
    let safe = json.replace("</", "<\\/");
    let tag = format!("<script>window.{global} = {safe};</script>\n");
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

    #[test]
    fn the_flights_ride_the_document_beside_the_manifest() {
        let html = "<html><head></head><body><script type=\"module\" src=\"/m.js\"></script></body></html>";
        let out = super::inline_flights(html, r#"{"flights":["it-map-motion"]}"#);
        let script = out
            .find("window.__BOSS_FLIGHTS__")
            .expect("the global is defined");
        assert!(
            script < out.find("</head>").unwrap(),
            "defined inside <head>"
        );
        assert!(script < out.find("type=\"module\"").unwrap());
        assert!(out.contains(r#"["it-map-motion"]"#), "{out}");
    }

    /// The page carries codes and nothing else; a malformed answer
    /// carries nothing, which the SPA reads as every code off.
    #[test]
    fn only_a_list_of_codes_reaches_the_page() {
        let ok = serde_json::json!({"flights": ["a", "b"], "audience": {"secret": 1}});
        assert_eq!(
            super::flights_body(&ok).as_deref(),
            Some(r#"{"flights":["a","b"]}"#)
        );
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"flights": "a"}),
            serde_json::json!({"flights": ["a", 1]}),
            serde_json::json!(["a"]),
        ] {
            assert_eq!(super::flights_body(&bad), None, "{bad}");
        }
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

/// A request path cannot read outside the static directory (backlog
/// 814a32a2). Until 2026-09-25 the handler joined the raw path onto
/// the directory and guarded with `PathBuf::starts_with`, a LEXICAL
/// component compare that `dist/../secret.key` passes; the OS then
/// resolved the `..` on read. A dotted last segment is served without
/// a session, so `GET /../../<dir>/<file.ext>` sent as-is — directly on
/// the LAN, where no edge normalises it — could read any readable file
/// with an extension, the gateway's session key among them. Every
/// answer below is the handler's own, against a scratch directory with
/// secrets planted one and two levels above it.
#[cfg(test)]
mod traversal_tests {
    use super::*;
    use crate::perf::PerfCollector;
    use axum::body::Body;
    use http_body_util::BodyExt;

    const SECRET: &str = "SECRET-BYTES-THAT-MUST-NEVER-LEAVE";
    const KEY: [u8; 32] = [7u8; 32];

    /// `<root>/web/dist` is the static directory; a `secret.key` and an
    /// extensionless `secret` sit in `<root>/web` AND in `<root>`, so a
    /// one-level and a two-level climb each has a real file to reach.
    fn fixture(case: &str) -> (PathBuf, PathBuf) {
        let root = boss_testing::scratch_dir(&format!("gateway-static-{case}"));
        let dist = root.join("web").join("dist");
        boss_testing::create_dir(&dist.join("kb-assets"));
        boss_testing::write_file(
            &dist.join("index.html"),
            "<!doctype html><html><head></head><body>the spa</body></html>",
        );
        boss_testing::write_file(&dist.join("chunk-abc123.js"), "console.log('asset')");
        boss_testing::write_file(&dist.join("kb-assets/one.svg"), "<svg/>");
        for dir in [root.join("web"), root.clone()] {
            boss_testing::write_file(&dir.join("secret.key"), SECRET);
            boss_testing::write_file(&dir.join("secret"), SECRET);
        }
        (root, dist)
    }

    fn state() -> AppState {
        AppState {
            session_key: KEY.to_vec(),
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(PerfCollector::new()),
        }
    }

    /// The handler's answer for `path`, sent as-is (no client-side
    /// normalisation), with or without a valid session.
    async fn get(dist: &Path, path: &str, signed_in: bool) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder().uri(path);
        if signed_in {
            let cookie = Session::new("tester", 3600).encode(&KEY);
            req = req.header(header::COOKIE, format!("{}={cookie}", session::COOKIE_NAME));
        }
        let req = req.body(Body::empty()).unwrap();
        assert_eq!(
            req.uri().path(),
            path,
            "the request must carry the path raw"
        );
        let resp = serve(&state(), req, dist).await;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    /// Every shape of climb named in the packet, each with a dotted
    /// last segment so it needs no session: raw `..`, deeper chains,
    /// `..` behind a real directory, encoded dots and slashes,
    /// backslash forms, an absolute path, and the `/dashboard` prefix.
    fn climbs(root: &Path) -> Vec<String> {
        let abs = root.join("secret.key");
        let abs = abs.to_str().unwrap();
        let mut paths: Vec<String> = [
            "/../secret.key",
            "/../../secret.key",
            "/./../secret.key",
            "/kb-assets/../../secret.key",
            "/kb-assets/../../../secret.key",
            "/%2e%2e/secret.key",
            "/%2e%2e/%2e%2e/secret.key",
            "/%2E%2E/%2E%2E/secret.key",
            "/..%2fsecret.key",
            "/..%2f..%2fsecret.key",
            "/..%5csecret.key",
            "/..\\secret.key",
            "/..\\..\\secret.key",
            "/dashboard/../secret.key",
            "/dashboard/../../secret.key",
            "/dashboard/./../../secret.key",
            "/dashboard/%2e%2e/%2e%2e/secret.key",
            "/dashboard../secret.key",
            "/dashboard../../secret.key",
            "/dashboard..%2fsecret.key",
            "/dashboard..%2f..%2fsecret.key",
        ]
        .map(String::from)
        .to_vec();
        paths.push(format!("/{abs}"));
        paths.push(format!("//{abs}"));
        paths.push(format!("/dashboard/{abs}"));
        paths.push(format!("/dashboard//{abs}"));
        paths
    }

    #[tokio::test]
    async fn no_climb_reads_a_file_outside_the_static_dir_without_a_session() {
        let (root, dist) = fixture("climb-anon");
        for path in climbs(&root) {
            let (status, _, body) = get(&dist, &path, false).await;
            assert!(!body.contains(SECRET), "{path} read outside the static dir");
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        }
    }

    /// A session widens what reaches the resolver (an extensionless
    /// last segment is no longer turned away at 401), so the climbs are
    /// re-run signed in, plus the extensionless `secret`.
    #[tokio::test]
    async fn no_climb_reads_a_file_outside_the_static_dir_with_a_session() {
        let (root, dist) = fixture("climb-session");
        let mut paths = climbs(&root);
        for p in [
            "/../secret",
            "/../../secret",
            "/kb-assets/../../../secret",
            "/dashboard/../../secret",
        ] {
            paths.push(p.to_string());
        }
        for path in paths {
            let (_, _, body) = get(&dist, &path, true).await;
            assert!(!body.contains(SECRET), "{path} read outside the static dir");
        }
    }

    /// `/dashboard` is stripped only as a whole segment. Stripped as a
    /// bare string prefix, `/dashboard../x.key` became `../x.key` — a
    /// climb with no dot-segment in the request for the edge to see
    /// (review of the dot-segment car, 2026-09-25). A name that merely
    /// begins with `dashboard` is a file of that name.
    #[tokio::test]
    async fn the_dashboard_prefix_is_stripped_only_as_a_whole_segment() {
        let (_, dist) = fixture("prefix");
        boss_testing::write_file(&dist.join("dashboardx.js"), "prefix-bound");
        let (status, _, body) = get(&dist, "/dashboardx.js", false).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "prefix-bound"));
        let (status, _, body) = get(&dist, "/dashboard/dashboardx.js", false).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "prefix-bound"));
    }

    /// The second wall: a path of plain names that the OS resolves out
    /// of the directory — a symlink inside it pointing up — reads as
    /// absent. The lexical resolver alone would pass it.
    #[tokio::test]
    async fn a_symlink_out_of_the_static_dir_is_not_followed() {
        let (root, dist) = fixture("symlink");
        std::os::unix::fs::symlink(root.join("secret.key"), dist.join("link.key")).unwrap();
        std::os::unix::fs::symlink(root.clone(), dist.join("up")).unwrap();
        for path in [
            "/link.key",
            "/up/secret.key",
            "/dashboard/up/web/secret.key",
        ] {
            let (status, _, body) = get(&dist, path, true).await;
            assert!(!body.contains(SECRET), "{path} followed a link out");
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        }
    }

    /// The resolver alone, on the shapes that matter: only plain names
    /// survive, and nothing it returns can sit outside the directory.
    #[test]
    fn only_plain_names_resolve() {
        let base = Path::new("/s");
        assert_eq!(
            resolve(base, "/chunk.js"),
            Some(PathBuf::from("/s/chunk.js"))
        );
        assert_eq!(
            resolve(base, "/kb-assets/one.svg"),
            Some(PathBuf::from("/s/kb-assets/one.svg"))
        );
        for bad in [
            "/..",
            "/../x.key",
            "/a/../x.key",
            "/./x.js",
            "/a/./x.js",
            "/..\\x.key",
            "/a\\b.js",
        ] {
            assert_eq!(resolve(base, bad), None, "{bad} must not resolve");
        }
    }

    #[tokio::test]
    async fn a_legitimate_asset_is_served_without_a_session() {
        let (_, dist) = fixture("asset");
        for path in ["/chunk-abc123.js", "/dashboard/chunk-abc123.js"] {
            let (status, headers, body) = get(&dist, path, false).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            assert_eq!(body, "console.log('asset')", "{path}");
            assert_eq!(
                headers[header::CONTENT_TYPE],
                "application/javascript; charset=utf-8"
            );
            assert_eq!(
                headers[header::CACHE_CONTROL],
                "public, max-age=31536000, immutable"
            );
        }
        let (status, _, body) = get(&dist, "/kb-assets/one.svg", false).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "<svg/>"));
    }

    /// A request that names a file and misses is a 404, never the SPA.
    #[tokio::test]
    async fn a_missing_named_file_is_a_404_not_the_spa() {
        let (_, dist) = fixture("missing");
        for path in ["/chunk-gone.js", "/dashboard/chunk-gone.css"] {
            let (status, _, body) = get(&dist, path, true).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
            assert!(!body.contains("the spa"), "{path}");
        }
    }

    /// A route reaches index.html, uncached, for a signed-in viewer; a
    /// signed-out one is turned away except on the public pages.
    #[tokio::test]
    async fn a_route_falls_back_to_the_spa() {
        let (_, dist) = fixture("spa");
        for path in ["/system/yard", "/dashboard", "/dashboard/", "/jobs/abc"] {
            let (status, headers, body) = get(&dist, path, true).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            assert!(body.contains("the spa"), "{path}: {body}");
            assert_eq!(
                headers[header::CACHE_CONTROL],
                "no-store, no-cache, must-revalidate, max-age=0"
            );
        }
        let (status, _, _) = get(&dist, "/system/yard", false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, body) = get(&dist, "/", false).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("the spa"));
    }
}
