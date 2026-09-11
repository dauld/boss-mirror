//! Generic cookie-gated reverse proxy.
//!
//! Replaces 13 near-identical per-service proxy modules. Every
//! `/api/<service>/*` route behaves the same way:
//!   1. Require a valid `boss_session` cookie; return 401 on miss.
//!      Login + cookie minting runs through local-auth (the v1 OSS
//!      auth path).
//!   2. Forward the request to the owning service's HTTP port,
//!      stripping hop-by-hop headers both ways, streaming the body.
//!   3. Surface upstream errors as 502 with a short reason string.
//!
//! Each route gets a `ProxyConfig` (name, env-var-overridable default
//! upstream URL) and an optional `UpstreamFallback` that turns a
//! connection failure into a graceful 200 — used today for the
//! policy service's `my-scope` endpoint so the frontend doesn't log
//! errors every page load when the policy upstream is down.

use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use tracing::{debug, warn};

use crate::AppState;
use boss_gateway::session::{self, Session, find_cookie};

/// Static configuration for a single reverse-proxy mount point.
pub struct ProxyConfig {
    /// Service slug, e.g. `"commerce"`. Used in log messages and to
    /// derive the environment variable name (`BOSS_<NAME>_UPSTREAM`).
    pub name: &'static str,
    /// OnceLock storing the resolved upstream URL (env var or
    /// `boss_ports::url(name)`).
    pub upstream: OnceLock<String>,
    /// Optional fallback for specific (path, method) pairs when the
    /// upstream is unreachable. Returns `Some(response)` to short-
    /// circuit; `None` to proceed with the usual 502.
    pub fallback: Option<fn(path: &str, method: &Method) -> Option<Response>>,
}

impl ProxyConfig {
    /// Build a vanilla config that always proxies — no graceful fallback.
    /// The default upstream URL is pulled from `boss_ports::url(name)`
    /// at first-use; the `BOSS_<NAME>_UPSTREAM` env var still wins when
    /// set.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            upstream: OnceLock::new(),
            fallback: None,
        }
    }

    /// Build a config whose upstream failures can degrade gracefully.
    pub const fn with_fallback(
        name: &'static str,
        fallback: fn(path: &str, method: &Method) -> Option<Response>,
    ) -> Self {
        Self {
            name,
            upstream: OnceLock::new(),
            fallback: Some(fallback),
        }
    }

    fn upstream_url(&self) -> &str {
        self.upstream.get_or_init(|| {
            let env_key = format!("BOSS_{}_UPSTREAM", self.name.to_uppercase());
            if let Ok(v) = std::env::var(&env_key) {
                return v;
            }
            // boss_ports uses kebab-case slugs; this proxy table has a
            // few snake_case ones (subject_kinds). Normalize.
            //
            // A `port_alias` field used to sit beside `name` for a
            // proxy slug that looks up a DIFFERENT service's port. It
            // had one user in its life — `aliased("design", "docs")`,
            // the design corpus on boss-docs-api — and went with that
            // service on 2026-09-10 (backlog f5da586c) rather than
            // staying as a mechanism with nothing to mech. The next
            // proxy that needs one is ten lines; a constructor no
            // caller reaches is a dead_code warning and a reader's
            // wrong turn.
            boss_ports::url(&self.name.replace('_', "-"))
        })
    }
}

/// Per-route proxy entry point. Wire like:
///
/// ```ignore
/// .route(
///     "/api/commerce/{*rest}",
///     axum::routing::any(|s, r| proxy::handle(s, r, &COMMERCE)),
/// )
/// ```
pub async fn handle(
    State(state): State<Arc<AppState>>,
    req: Request,
    config: &'static ProxyConfig,
) -> Response {
    if !has_valid_session(req.headers(), &state.session_key) {
        return unauthorized();
    }

    forward_to_upstream(state, req, config).await
}

/// App-shaped proxy variant — for sub-apps the browser NAVIGATES to
/// (the /simulator SPA). A document navigation without a session gets
/// the login page with a return path instead of a bare
/// "authentication required" (572b100b: an expired session made the
/// simulator read as broken). Everything that is not a text/html GET
/// — the sub-app's own fetch/XHR traffic — keeps the plain 401 so
/// API callers see a status, not a login page.
pub async fn handle_app(
    State(state): State<Arc<AppState>>,
    req: Request,
    config: &'static ProxyConfig,
) -> Response {
    if !has_valid_session(req.headers(), &state.session_key) {
        if is_document_navigation(req.method(), req.headers()) {
            let next: String = req
                .uri()
                .path()
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                        (b as char).to_string()
                    }
                    _ => format!("%{b:02X}"),
                })
                .collect();
            let mut headers = HeaderMap::new();
            if let Ok(v) = header::HeaderValue::from_str(&format!("/login?next={next}")) {
                headers.insert(header::LOCATION, v);
            }
            return (StatusCode::SEE_OTHER, headers).into_response();
        }
        return unauthorized();
    }
    forward_to_upstream(state, req, config).await
}

/// A browser typing/clicking a URL: GET with an Accept that asks for
/// HTML. Fetch/XHR sends application/json or */* without text/html
/// unless explicitly configured — good enough to route people to
/// login and machines to a status code.
fn is_document_navigation(method: &axum::http::Method, headers: &HeaderMap) -> bool {
    method == axum::http::Method::GET
        && headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(|a| a.contains("text/html"))
            .unwrap_or(false)
}

/// Public proxy variant — skips the session check. Used for endpoints
/// where the URL itself carries sufficient authentication (e.g. the
/// `/ics/{token}.ics` calendar feed, where `token` is a 256-bit random
/// string that only its owning tech has).
pub async fn handle_public(
    State(state): State<Arc<AppState>>,
    req: Request,
    config: &'static ProxyConfig,
) -> Response {
    forward_to_upstream(state, req, config).await
}

async fn forward_to_upstream(
    state: Arc<AppState>,
    req: Request,
    config: &'static ProxyConfig,
) -> Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(str::to_owned);
    let method = req.method().clone();
    let upstream = config.upstream_url();
    let upstream_url = match query.as_deref() {
        Some(q) => format!("{upstream}{path}?{q}"),
        None => format!("{upstream}{path}"),
    };

    match forward(req, &upstream_url, &state.proxy_client).await {
        Ok(resp) => resp,
        Err(()) => {
            if let Some(fallback) = config.fallback
                && let Some(resp) = fallback(&path, &method)
            {
                debug!(service = config.name, "upstream down — returning fallback");
                return resp;
            }
            warn!(service = config.name, url = %upstream_url, "upstream request failed");
            (
                StatusCode::BAD_GATEWAY,
                format!("{} upstream unavailable", config.name),
            )
                .into_response()
        }
    }
}

// --- shared plumbing ------------------------------------------------------

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// Request headers the gateway must NOT forward upstream: hop-by-hop
/// headers (HTTP spec) plus the sim-origin trust marker. That marker
/// flips the policy bypass (SimBypassPolicyClient) for trusted internal
/// traffic; the simulator reaches backends directly, never through this
/// public gateway, so any inbound `x-sim-origin` is a forgery attempt.
fn is_blocked_request_header(name_lower: &str) -> bool {
    HOP_BY_HOP.contains(&name_lower) || name_lower == boss_core::sim_origin::SIM_ORIGIN_HEADER
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

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "authentication required").into_response()
}

async fn forward(
    req: Request,
    upstream_url: &str,
    client: &reqwest::Client,
) -> Result<Response, ()> {
    let (parts, body) = req.into_parts();

    let stream = body_to_stream(body);
    let reqwest_body = reqwest::Body::wrap_stream(stream);

    let mut builder = client
        .request(parts.method.clone(), upstream_url)
        .body(reqwest_body);
    for (name, value) in parts.headers.iter() {
        if is_blocked_request_header(&name.as_str().to_ascii_lowercase()) {
            continue;
        }
        builder = builder.header(name, value);
    }

    let resp = builder.send().await.map_err(|_| ())?;

    let status = resp.status();
    let mut out_headers = HeaderMap::new();
    for (name, value) in resp.headers().iter() {
        if HOP_BY_HOP.contains(&name.as_str().to_ascii_lowercase().as_str()) {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_ref()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out_headers.append(n, v);
        }
    }

    let body_stream = resp
        .bytes_stream()
        .map(|item| item.map_err(|e| std::io::Error::other(e.to_string())));
    let body = Body::from_stream(body_stream);
    Ok((status, out_headers, body).into_response())
}

fn body_to_stream(
    body: Body,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    use http_body_util::BodyExt;
    let mut body = body;
    async_stream::stream! {
        loop {
            match body.frame().await {
                None => break,
                Some(Ok(frame)) => {
                    if let Ok(chunk) = frame.into_data() {
                        yield Ok::<_, std::io::Error>(chunk);
                    }
                }
                Some(Err(e)) => {
                    yield Err(std::io::Error::other(e.to_string()));
                    break;
                }
            }
        }
    }
}

// --- service mounts -------------------------------------------------------
//
// One static per proxy route. Each replaces ~140 lines of a
// per-service module with a single one-liner.

pub static COMMERCE: ProxyConfig = ProxyConfig::new("commerce");
/// Audit-log tail / stream / export endpoints. Hosted by the
/// dedicated boss-events-api service on port 7150 (2026-06 split
/// out of boss-people-api — see crates/core/boss-events/src/bin/
/// boss_events_api.rs). `ProxyConfig::new("events")` resolves
/// through boss_ports::prod("events") = 7150.
pub static EVENTS: ProxyConfig = ProxyConfig::new("events");
pub static CONTENT: ProxyConfig = ProxyConfig::new("content");
pub static ASSETS: ProxyConfig = ProxyConfig::new("assets");
pub static INVENTORY: ProxyConfig = ProxyConfig::new("inventory");
pub static JOBS: ProxyConfig = ProxyConfig::new("jobs");
pub static DISPATCHER: ProxyConfig = ProxyConfig::new("dispatcher");
pub static SEARCH: ProxyConfig = ProxyConfig::new("search");
pub static VIEWS: ProxyConfig = ProxyConfig::new("views");
pub static CATALOG: ProxyConfig = ProxyConfig::new("catalog");
pub static LEDGER: ProxyConfig = ProxyConfig::new("ledger");
pub static MESSAGES: ProxyConfig = ProxyConfig::new("messages");
pub static ML: ProxyConfig = ProxyConfig::new("ml");
pub static PEOPLE: ProxyConfig = ProxyConfig::new("people");
/// Accounts service (port 7550). Hosts /api/people/accounts/*,
/// /api/people/account-team*, /api/people/accounts/{id}/notes,
/// /api/people/account-account-team/batch, /api/people/{id}/next-actions,
/// /api/people/{id}/risk, /api/cases/*, /api/account-cases/*. Routes
/// all 7 path families to the dedicated accounts service.
pub static ACCOUNTS: ProxyConfig = ProxyConfig::new("accounts");
pub static SHIPPING: ProxyConfig = ProxyConfig::new("shipping");
pub static CLASSES: ProxyConfig = ProxyConfig::new("classes");
pub static LOCATIONS: ProxyConfig = ProxyConfig::new("locations");
pub static SUBJECT_KINDS: ProxyConfig = ProxyConfig::new("subject_kinds");
pub static CALENDAR: ProxyConfig = ProxyConfig::new("calendar");
/// Cross-VM Cybernetics dashboard aggregator. Hosts /api/snapshot
/// (which the SPA's Operations page reads) + the per-VM
/// cybernetics rollup. Port 7880, declared in boss_ports.
pub static OBSERVABILITY: ProxyConfig = ProxyConfig::new("observability");
pub static PRODUCTS: ProxyConfig = ProxyConfig::new("products");
pub static CAMPAIGNS: ProxyConfig = ProxyConfig::new("campaigns");
pub static CUSTOMERS: ProxyConfig = ProxyConfig::new("customers");
/// Simulator UX service — hosts the /simulator SPA bundle + the
/// /simulator/api/* control+status surface. Port 7010, declared in
/// boss_ports.
pub static SIMULATOR: ProxyConfig = ProxyConfig::new("simulator");

/// Policy's `my-scope` POST is called on every page load. When the
/// upstream is down we'd otherwise log a 502 into every browser
/// console — return an empty-scope payload instead so the frontend's
/// MyScopeContext silently falls into its defaults-table path.
pub static POLICY: ProxyConfig = ProxyConfig::with_fallback("policy", |path, method| {
    if path == "/api/policy/my-scope" && method == Method::POST {
        return Some(
            axum::Json(serde_json::json!({
                "allow_read": [],
                "scope_filters": {},
                "version": 0,
            }))
            .into_response(),
        );
    }
    None
});

#[cfg(test)]
mod tests {
    use super::*;

    /// Proxy `name` → boss_ports lookup uses underscore→hyphen
    /// normalization. The `subject_kinds` proxy slug routes to the
    /// `subject-kinds` port-table entry.
    #[test]
    fn underscore_slug_normalizes_to_kebab() {
        let cfg = ProxyConfig::new("subject_kinds");
        assert_eq!(cfg.upstream_url(), "http://127.0.0.1:7830");
    }

    /// Vanilla case: name maps directly to the port table.
    #[test]
    fn name_resolves_via_boss_ports() {
        let cfg = ProxyConfig::new("policy");
        assert_eq!(cfg.upstream_url(), "http://127.0.0.1:7250");
    }

    /// Exhaustive: every static below resolves without panicking
    /// (boss_ports::url panics on unknown names — this regression-pins
    /// the proxy's name list against the boss_ports table).
    #[test]
    fn all_proxy_statics_resolve() {
        let _ = COMMERCE.upstream_url();
        let _ = EVENTS.upstream_url();
        let _ = CONTENT.upstream_url();
        let _ = ASSETS.upstream_url();
        let _ = INVENTORY.upstream_url();
        let _ = JOBS.upstream_url();
        let _ = CATALOG.upstream_url();
        let _ = LEDGER.upstream_url();
        let _ = MESSAGES.upstream_url();
        let _ = ML.upstream_url();
        let _ = PEOPLE.upstream_url();
        let _ = SHIPPING.upstream_url();
        let _ = CLASSES.upstream_url();
        let _ = LOCATIONS.upstream_url();
        let _ = SUBJECT_KINDS.upstream_url();
        let _ = CALENDAR.upstream_url();
        let _ = POLICY.upstream_url();
        let _ = SIMULATOR.upstream_url();
    }

    /// Security: the gateway never forwards a client-supplied
    /// `x-sim-origin` (forwarding it would let external ingress forge
    /// the sim policy bypass) nor hop-by-hop headers — but it does
    /// forward the trusted `x-boss-*` identity headers + ordinary ones.
    #[test]
    fn blocks_sim_origin_and_hop_by_hop_but_forwards_identity() {
        assert!(is_blocked_request_header("x-sim-origin"));
        assert!(is_blocked_request_header("connection"));
        assert!(is_blocked_request_header("transfer-encoding"));
        assert!(!is_blocked_request_header("x-boss-user"));
        assert!(!is_blocked_request_header("x-boss-role"));
        assert!(!is_blocked_request_header("cookie"));
        assert!(!is_blocked_request_header("content-type"));
    }
}

#[cfg(test)]
mod app_gate_tests {
    use super::*;

    fn headers_accepting(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, header::HeaderValue::from_str(v).unwrap());
        h
    }

    /// The discrimination the whole fix rides on: people go to login,
    /// machines get a status code.
    #[test]
    fn browser_navigations_and_only_those_redirect() {
        let get = axum::http::Method::GET;
        let post = axum::http::Method::POST;
        // A browser address-bar / link navigation.
        assert!(is_document_navigation(
            &get,
            &headers_accepting("text/html,application/xhtml+xml,*/*;q=0.8")
        ));
        // fetch() default and JSON callers: status code, not a login page.
        assert!(!is_document_navigation(&get, &headers_accepting("*/*")));
        assert!(!is_document_navigation(
            &get,
            &headers_accepting("application/json")
        ));
        // Non-GET never redirects, whatever it accepts.
        assert!(!is_document_navigation(
            &post,
            &headers_accepting("text/html")
        ));
        // No Accept header at all: a machine.
        assert!(!is_document_navigation(&get, &HeaderMap::new()));
    }
}
