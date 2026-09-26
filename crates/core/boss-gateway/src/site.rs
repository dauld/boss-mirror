//! The tenant SITE — a second hostname on the same gateway, answered
//! from a directory and nothing else (design b64c4377, decided by
//! David 2026-09-17; backlog c8f6b233).
//!
//! The operating company's public website is company data: static,
//! text-first pages in the tenant repository's `site/`, delivered to
//! the instance by the converge as the `boss-site` ConfigMap exactly
//! as the seeds are (`boss-tenant`). It is served by the SAME gateway
//! the instance already runs, under the SITE hostname the instance
//! declares (`site = "www.…"` in infra/cluster/instances.toml), which
//! the tunnel routes to this gateway beside the instance hostname.
//!
//! ONE decision per request, made before anything else: does the
//! request's `Host` name the site? If so the answer comes from the
//! site directory — `/` is `index.html`, `/x/y.css` is that file, a
//! path that names nothing is a plain-text 404 — and NOTHING of the
//! application is reachable: not the SPA, not `/api`, not the login.
//! No cookie is read, no session is minted, no identity header is
//! injected: the site is anonymous by construction, which is why this
//! layer sits OUTSIDE the session middleware rather than being a route
//! among the others. If the host is anything else, the request goes
//! on to the application untouched.
//!
//! `Host` is the header the tunnel connector forwards as the visitor
//! sent it (cloudflared sets the origin's Host to the public hostname
//! unless `originRequest.httpHostHeader` overrides it, and the ingress
//! render sets no override). The port, if a client sends one, is not
//! part of the name.
//!
//! ABSENT CONFIGURATION IS AN INERT LAYER. `BOSS_SITE_HOST` and
//! `BOSS_SITE_DIR` both set → the site is mounted; either empty or
//! unset → `Site::from_env` is `None` and `mount` returns the router
//! it was given. An instance without a site renders the host as `""`
//! (render-instance.sh), which is this spelling of "no site".
//!
//! ONE PATH IS COMPUTED, NOT READ: `/site/sponsors.json` (sponsors.rs)
//! is the sponsor roll, a projection of closed sponsorship packets
//! read through the jobs API with the gateway's own identity. It is
//! the site's one dynamic document, and still session-free: it holds
//! only what sponsors consented to publish (backlog f200f3d2).
//!
//! ONE THING IS COUNTED: a GET answered 200 with HTML is a page view,
//! recorded as a `www-visits` sensor reading (visits.rs, backlog
//! 0b5c5081) — path, referrer host, country, UA class, instant; no IP,
//! no cookie — through a bounded channel the answer never waits on.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::sponsors::{self, SponsorRoll};
use crate::visits::{self, Recorder};

/// The site an instance serves: the hostname it answers to and the
/// directory it answers from — plus the sponsor roll and the page-view
/// recorder, when wired.
#[derive(Clone, Debug)]
pub struct Site {
    host: String,
    dir: PathBuf,
    sponsors: Option<Arc<SponsorRoll>>,
    visits: Option<Recorder>,
}

impl Site {
    /// `BOSS_SITE_HOST` + `BOSS_SITE_DIR`, or `None` when either is
    /// unset or empty — the inert spelling.
    pub fn from_env() -> Option<Self> {
        Self::from_values(
            &std::env::var("BOSS_SITE_HOST").unwrap_or_default(),
            &std::env::var("BOSS_SITE_DIR").unwrap_or_default(),
        )
    }

    /// Pure: the same two strings always give the same answer. The
    /// host is compared case-insensitively, so it is stored folded.
    pub fn from_values(host: &str, dir: &str) -> Option<Self> {
        let host = host.trim().to_ascii_lowercase();
        let dir = dir.trim();
        if host.is_empty() || dir.is_empty() {
            return None;
        }
        Some(Self {
            host,
            dir: PathBuf::from(dir),
            sponsors: None,
            visits: None,
        })
    }

    /// The site with the sponsor roll answering [`sponsors::PATH`].
    /// Unwired, that path is a file in the directory like any other.
    pub fn with_sponsors(self, roll: SponsorRoll) -> Self {
        Self {
            sponsors: Some(Arc::new(roll)),
            ..self
        }
    }

    /// The site recording one page view per 200 HTML page it serves
    /// (visits.rs). Unwired, pages are served and nothing is counted.
    pub fn with_visits(self, recorder: Recorder) -> Self {
        Self {
            visits: Some(recorder),
            ..self
        }
    }

    /// The hostname the site answers to, as it will be compared.
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// The router with the site in front of it — or the router itself
/// when there is no site. Called once in `main`, outermost, so a site
/// request never reaches the session middleware.
pub fn mount(app: axum::Router, site: Option<Site>) -> axum::Router {
    match site {
        Some(site) => app.layer(axum::middleware::from_fn_with_state(Arc::new(site), serve)),
        None => app,
    }
}

/// The layer: answer from the directory when `Host` names the site,
/// otherwise hand the request on unchanged.
async fn serve(State(site): State<Arc<Site>>, req: Request, next: Next) -> Response {
    if !names_host(req.headers(), &site.host) {
        return next.run(req).await;
    }
    let resp = answer(&site, req.method(), req.uri().path()).await;
    // A page view is a GET answered 200 with HTML — not a stylesheet,
    // not a miss, not a HEAD, not the roll. Recorded AFTER the answer
    // is built and never awaited on: one non-blocking send (visits.rs).
    if let Some(visits) = &site.visits
        && req.method() == Method::GET
        && resp.status() == StatusCode::OK
        && is_html(resp.headers())
    {
        visits.record(&visits::View::of(
            req.uri().path(),
            req.headers(),
            boss_clock_client::wall_now(),
        ));
    }
    resp
}

fn is_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"))
}

/// `Host` equals `host`, port stripped, case folded. The one test of
/// "is this the site" — the inquiry door (inquiries.rs) asks it too,
/// so the two layers cannot disagree about which requests are the
/// site's.
pub(crate) fn names_host(headers: &HeaderMap, host: &str) -> bool {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(host_name)
        .is_some_and(|h| h.eq_ignore_ascii_case(host))
}

/// The name part of a `Host` value: `www.example:443` → `www.example`.
/// A bracketed IPv6 literal keeps its brackets and loses its port.
fn host_name(value: &str) -> &str {
    let v = value.trim();
    if let Some(end) = v.strip_prefix('[').and_then(|_| v.find(']')) {
        return &v[..=end];
    }
    v.rsplit_once(':').map_or(v, |(name, _)| name)
}

/// The file a request path names inside the site directory, or `None`
/// when the path names nothing servable: any component that is not a
/// plain name (`..`, a root, a prefix) is refused rather than resolved,
/// so a request can never read outside the directory. A path ending
/// in `/` (the root included) names that directory's `index.html`.
fn resolve(dir: &Path, path: &str) -> Option<PathBuf> {
    let rel = path.trim_start_matches('/');
    let mut out = dir.to_path_buf();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if rel.is_empty() || rel.ends_with('/') {
        out.push("index.html");
    }
    Some(out)
}

/// The response for a site request. Only reads are answered: the site
/// has nothing to write to.
async fn answer(site: &Site, method: &Method, path: &str) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return (StatusCode::METHOD_NOT_ALLOWED, "site: GET or HEAD only\n").into_response();
    }
    if path == sponsors::PATH
        && let Some(roll) = &site.sponsors
    {
        return roll.respond().await;
    }
    let Some(file) = resolve(&site.dir, path) else {
        return not_found();
    };
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                crate::static_files::guess_content_type(&file),
            );
            // Short and honest: the site changes when the tenant repo
            // does, delivered by the next converge; five minutes at
            // the edge is the staleness a text-first site can afford,
            // and no `immutable`, since these names carry no hash.
            headers.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=300"),
            );
            (StatusCode::OK, headers, bytes).into_response()
        }
        // A directory, a missing file, a permission refusal: none of
        // them is a page, and none of them is the application.
        Err(_) => not_found(),
    }
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found\n").into_response()
}

#[cfg(test)]
mod tests {
    //! Through the gateway's own router, with a site directory in
    //! scratch: every answer below is one the mounted router gave.

    use super::*;
    use crate::AppState;
    use crate::perf::PerfCollector;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const HOST: &str = "www.site.test";

    /// A site directory holding index.html, a stylesheet in a
    /// subdirectory, and a file OUTSIDE the site that traversal must
    /// never reach.
    fn site_dir(case: &str) -> PathBuf {
        let root = boss_testing::scratch_dir(&format!("gateway-site-{case}"));
        let dir = root.join("site");
        boss_testing::create_dir(&dir.join("css"));
        boss_testing::create_dir(&dir.join("docs"));
        boss_testing::write_file(
            &dir.join("index.html"),
            "<!doctype html><title>the site</title>",
        );
        boss_testing::write_file(&dir.join("css/site.css"), "body{}");
        boss_testing::write_file(&dir.join("docs/index.html"), "<p>docs</p>");
        boss_testing::write_file(&root.join("outside.txt"), "not yours");
        dir
    }

    fn app(site: Option<Site>) -> axum::Router {
        let state = Arc::new(AppState {
            session_key: vec![0u8; 32],
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(PerfCollector::new()),
            machine_token: Default::default(),
        });
        mount(
            crate::build_router(None, &crate::public_reads::PublicReads::none()).with_state(state),
            site,
        )
    }

    async fn get(app: axum::Router, host: &str, path: &str) -> (StatusCode, HeaderMap, String) {
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::HOST, host)
                    // A session cookie rides along: the site must not
                    // read it, and must not answer differently for it.
                    .header(header::COOKIE, "boss_session=forged")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router responds");
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    fn content_type(h: &HeaderMap) -> &str {
        h.get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    #[tokio::test]
    async fn the_site_host_is_answered_from_the_directory_and_never_the_application() {
        let dir = site_dir("serves");
        let site = Site::from_values(HOST, dir.to_str().unwrap());
        let (status, h, body) = get(app(site.clone()), HOST, "/").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(content_type(&h), "text/html; charset=utf-8");
        assert!(body.contains("the site"), "/ is index.html: {body}");
        assert!(
            h.get(header::SET_COOKIE).is_none(),
            "no session is minted for the site"
        );
        assert!(
            !body.contains("__BOSS_TENANT_MANIFEST__"),
            "the site's index is not the SPA's index"
        );

        let (status, h, body) = get(app(site.clone()), HOST, "/css/site.css").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(content_type(&h), "text/css; charset=utf-8");
        assert_eq!(body, "body{}");

        // A directory path names its index.
        let (status, _, body) = get(app(site.clone()), HOST, "/docs/").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "<p>docs</p>");

        // Nothing of the application answers on the site host: the API
        // catch-all, a live API route, the SPA shell, the login page.
        for path in [
            "/api/session",
            "/api/workflow",
            "/dashboard",
            "/login",
            "/health",
        ] {
            let (status, h, body) = get(app(site.clone()), HOST, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
            assert!(
                content_type(&h).starts_with("text/plain"),
                "{path}: a site miss is plain text, not JSON and not HTML: {}",
                content_type(&h)
            );
            assert!(
                !body.contains("no such API route") && !body.to_lowercase().contains("<!doctype"),
                "{path} reached the application: {body}"
            );
        }

        // The port a client may send is not part of the name, and the
        // case is not either.
        for host in ["www.site.test:443", "WWW.Site.Test"] {
            let (status, _, body) = get(app(site.clone()), host, "/").await;
            assert_eq!(status, StatusCode::OK, "{host}: {body}");
        }
    }

    #[tokio::test]
    async fn a_path_that_leaves_the_directory_is_refused_not_resolved() {
        let dir = site_dir("traversal");
        let site = Site::from_values(HOST, dir.to_str().unwrap());
        for path in [
            "/../outside.txt",
            "/css/../../outside.txt",
            "/./../outside.txt",
        ] {
            let (status, _, body) = get(app(site.clone()), HOST, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
            assert!(!body.contains("not yours"), "{path} read outside the site");
        }
        // The pure resolver, pinned on the shapes that matter.
        assert_eq!(
            resolve(Path::new("/s"), "/"),
            Some(PathBuf::from("/s/index.html"))
        );
        assert_eq!(
            resolve(Path::new("/s"), "/a/b.css"),
            Some(PathBuf::from("/s/a/b.css"))
        );
        assert_eq!(
            resolve(Path::new("/s"), "/a/"),
            Some(PathBuf::from("/s/a/index.html"))
        );
        assert_eq!(resolve(Path::new("/s"), "/../x"), None);
        assert_eq!(resolve(Path::new("/s"), "/a/../../x"), None);
    }

    #[tokio::test]
    async fn any_other_host_reaches_the_application_untouched() {
        let dir = site_dir("other-host");
        let site = Site::from_values(HOST, dir.to_str().unwrap());
        // The API catch-all answers its JSON miss, as it does without
        // a site mounted.
        let (status, _, body) = get(app(site.clone()), "boss.site.test", "/api/workflow").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body.contains("no such API route"), "{body}");
        // An application page reaches the SPA handler, which gates on
        // the session (the forged cookie does not decode): 401, never
        // the site. `/` itself is a public path there, so a page is
        // the discriminating request.
        let (status, _, body) = get(app(site), "boss.site.test", "/ux/jobs").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert!(
            !body.contains("the site"),
            "the site leaked onto another host"
        );
    }

    #[tokio::test]
    async fn without_a_site_the_layer_is_inert() {
        assert!(Site::from_values("", "/x").is_none());
        assert!(Site::from_values("www.site.test", "").is_none());
        assert!(Site::from_values("  ", "  ").is_none());
        assert_eq!(
            Site::from_values("WWW.Site.Test", "/x").unwrap().host(),
            "www.site.test"
        );
        // No site: the site host is just another host to the
        // application — the SPA handler answers, not a directory.
        let (status, _, body) = get(app(None), HOST, "/ux/jobs").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        let (_, _, body) = get(app(None), HOST, "/api/workflow").await;
        assert!(body.contains("no such API route"), "{body}");
    }

    #[tokio::test]
    async fn the_site_answers_reads_only() {
        let dir = site_dir("methods");
        let site = Site::from_values(HOST, dir.to_str().unwrap());
        let resp = app(site)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/auth/login")
                    .header(header::HOST, HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    /// THE EQUALITY THE PUBLISH LOOP RESTS ON (backlog e114238a). The
    /// tenant's publish step records sha256(site/index.html) at the
    /// merge; the converge records sha256 of the body the gateway
    /// serves for `/` (cluster-deploy-lib.sh observe_sites); the
    /// dispatcher completes the `live` step when the two are equal. So
    /// `/` must be index.html BYTE FOR BYTE — no manifest injection
    /// (static_files.rs does that for the SPA's index, never here), no
    /// newline normalisation, no re-encoding — pinned here against a
    /// file with CRLF, a trailing newline, non-ASCII and the SPA's
    /// placeholder text, and judged by the hash both sides use.
    #[tokio::test]
    async fn the_root_is_index_html_byte_for_byte() {
        use sha2::{Digest, Sha256};
        let dir = site_dir("bytes");
        let index = dir.join("index.html");
        let file = b"<!doctype html>\r\n<title>Algedonic \xe2\x80\x94 BOSS</title>\n<script>window.__BOSS_TENANT_MANIFEST__ = null;</script>\n\n";
        std::fs::write(&index, file).unwrap();
        let site = Site::from_values(HOST, dir.to_str().unwrap());
        let resp = app(site)
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::HOST, HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            body.as_ref(),
            &file[..],
            "the body is the file, unrewritten"
        );
        assert_eq!(
            Sha256::digest(&body)[..],
            Sha256::digest(std::fs::read(&index).unwrap())[..],
            "the hash the converge records equals the hash the publish step records"
        );
    }

    /// One www-visits reading per 200 HTML page (backlog 0b5c5081),
    /// through the mounted router: the page's path, the referrer's
    /// host, the tunnel's country and the UA class — and no reading
    /// for a stylesheet, a miss, or a HEAD. The cookie the request
    /// carries reaches no reading.
    #[tokio::test]
    async fn a_served_html_page_records_one_view_and_nothing_else_does() {
        use crate::visits::Recorder;
        let dir = site_dir("visits");
        let (recorder, mut rx) = Recorder::channel(16);
        let site =
            Site::from_values(HOST, dir.to_str().unwrap()).map(|s| s.with_visits(recorder.clone()));
        let app = app(site);
        let send = |method: Method, path: &'static str| {
            let app = app.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header(header::HOST, HOST)
                        .header(header::COOKIE, "boss_session=forged")
                        .header(header::REFERER, "https://lobste.rs/s/abc")
                        .header("cf-ipcountry", "de")
                        .header("cf-connecting-ip", "203.0.113.9")
                        .header(header::USER_AGENT, "Mozilla/5.0 (iPhone) Mobile Safari")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
            }
        };
        assert_eq!(send(Method::GET, "/").await, StatusCode::OK);
        let reading = rx.try_recv().expect("the page view was recorded");
        assert_eq!(reading["payload"]["path"], "/");
        assert_eq!(reading["payload"]["referrer_host"], "lobste.rs");
        assert_eq!(reading["payload"]["country"], "DE");
        assert_eq!(reading["payload"]["ua_class"], "phone");
        assert!(!reading.to_string().contains("forged"));
        assert!(!reading.to_string().contains("203.0.113.9"));

        assert_eq!(send(Method::GET, "/docs/").await, StatusCode::OK);
        assert_eq!(rx.try_recv().unwrap()["payload"]["path"], "/docs/");

        assert_eq!(send(Method::GET, "/css/site.css").await, StatusCode::OK);
        assert_eq!(send(Method::GET, "/nope.html").await, StatusCode::NOT_FOUND);
        assert_eq!(send(Method::HEAD, "/").await, StatusCode::OK);
        assert!(
            rx.try_recv().is_err(),
            "a stylesheet, a miss and a HEAD are not page views"
        );
        assert_eq!(recorder.dropped(), 0);
    }

    #[test]
    fn a_host_value_loses_its_port_and_nothing_else() {
        assert_eq!(host_name("www.site.test"), "www.site.test");
        assert_eq!(host_name("www.site.test:443"), "www.site.test");
        assert_eq!(host_name(" www.site.test "), "www.site.test");
        assert_eq!(host_name("[::1]:8080"), "[::1]");
        assert_eq!(host_name("[::1]"), "[::1]");
    }
}
