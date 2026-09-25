//! A read-only session cannot write through the gateway (backlog
//! 07e797b4, 2026-09-25).
//!
//! The blast-radius sweep of car e2209174 found about 110 write routes
//! across the core and module services that authorize no caller —
//! dispatcher rule publish/retire, `POST /api/jobs`, the scheduling
//! calendar token, ~27 ledger writes, messages without ownership — and
//! every one of them was one proxied request away from ANY valid
//! session, including the anonymous one `POST /api/auth/guest` mints.
//! Since design 2830b6b7 that guest carries one of TWO roles — the
//! basic `visitor` (the OSS default) or `audit-readonly` (an instance's
//! opt-in to the system-audit read) — so every guest test here runs
//! once per minting mode. The edge is the one place every one of
//! them passes, so the class closes there: a read-only session may send
//! GET, HEAD and OPTIONS, and every other method is refused with a named
//! 403 before any upstream is contacted. Per-service authorization
//! follows as its own items; this is the floor under them.
//!
//! THE STUB IS THE UPSTREAM FOR EVERY ROUTE. The gateway's HTTP client
//! is built here with a forward proxy pointing at a recording stub, so
//! every request the gateway would send to ANY service lands on the stub
//! instead — whatever upstream URL a `ProxyConfig` resolves to. That is
//! what makes "before any upstream is contacted" something a test can
//! count, and it is also the safety property: no request in this module
//! can reach a real service on a loopback port (a port-forward on the
//! pod makes one production — CLAUDE.md), not even a write the edge
//! failed to refuse, which is exactly the case the red run exercises.
//!
//! THE ROUTE LIST IS READ, NOT REMEMBERED. Every proxied route is taken
//! from `main.rs`'s own text — the line after a matcher that names
//! `proxy::handle` or `proxy::handle_app` — plus the `public_reads`
//! family, whose write methods chain through the same gated proxy. A
//! route added next week is swept by this test the day it lands.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;
use boss_gateway::session::{self, Session};

/// Distinct from the routing tests' all-zero key, so a cookie from one
/// module can never pass in the other by accident.
const KEY: [u8; 32] = [0x5a; 32];

/// What the stub answers every forwarded request with.
const UPSTREAM_ANSWER: &str = "the recording upstream answered";

/// Every request the gateway sent upstream, as `METHOD uri`.
type Hits = Arc<Mutex<Vec<String>>>;

const WRITES: [Method; 4] = [Method::POST, Method::PUT, Method::PATCH, Method::DELETE];

/// A server that records every request it is sent and answers 200. It
/// is reached as a FORWARD PROXY, so the uri it records is the absolute
/// upstream URL the gateway meant to reach.
async fn recording_upstream() -> (String, Hits) {
    let hits: Hits = Arc::default();
    let seen = hits.clone();
    let stub = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let seen = seen.clone();
        async move {
            seen.lock()
                .expect("hits lock")
                .push(format!("{} {}", req.method(), req.uri()));
            UPSTREAM_ANSWER
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the recording upstream");
    let addr = listener.local_addr().expect("stub address");
    tokio::spawn(async move {
        axum::serve(listener, stub)
            .await
            .expect("the recording upstream serves");
    });
    (format!("http://{addr}"), hits)
}

/// The two ways this gateway can mint a guest, each a read-only role
/// (design 2830b6b7): `Basic` hands out `visitor`, `Audit` hands out
/// `audit-readonly`. `Off` mints none, so it has no place here.
const GUEST_MODES: [GuestAccess; 2] = [GuestAccess::Basic, GuestAccess::Audit];

fn local_auth(guest_access: GuestAccess) -> Arc<LocalAuthState> {
    // `load` on a path that does not exist yields an empty store.
    let store = CredentialStore::load("/nonexistent/boss-test-credentials.toml")
        .expect("empty credential store");
    Arc::new(LocalAuthState {
        store,
        session_key: KEY.to_vec(),
        http: reqwest::Client::new(),
        audit: boss_gateway::audit::AuthAudit::disabled(),
        guest_access,
        oidc: None,
        mail: boss_gateway::mail::from_env(),
        public_url: "https://boss.test".into(),
        forgot_seen: Default::default(),
    })
}

/// The gateway's own route table, local auth mounted with the guest
/// door minting `guests`, every forward diverted to `upstream`.
fn gateway(upstream: &str, guests: GuestAccess) -> axum::Router {
    let proxy_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .proxy(reqwest::Proxy::all(upstream).expect("stub proxy url"))
        .build()
        .expect("proxy client");
    let state = Arc::new(AppState {
        session_key: KEY.to_vec(),
        proxy_client,
        perf: Arc::new(PerfCollector::new()),
    });
    build_router(Some(local_auth(guests)), &public_reads::PublicReads::none()).with_state(state)
}

/// A proxied route as the table registers it: the probe path to send,
/// and whether the matcher takes every method (`any`) or GET alone.
#[derive(Debug)]
struct Proxied {
    path: String,
    every_method: bool,
}

/// Every proxied route in the table, read out of main.rs and the
/// `public_reads` family table. `{*rest}` is probed with one segment.
fn proxied_routes() -> Vec<Proxied> {
    let lines: Vec<&str> = include_str!("main.rs").lines().collect();
    let from_main = lines.windows(2).filter_map(|pair| {
        let matcher = pair[0].trim();
        let handler = pair[1].trim();
        let path = matcher.strip_prefix('"')?.strip_suffix("\",")?;
        let proxied = handler.contains("proxy::handle(") || handler.contains("proxy::handle_app(");
        let every_method = handler.starts_with("axum::routing::any(");
        let get_only = handler.starts_with("axum::routing::get(");
        (path.starts_with('/') && proxied && (every_method || get_only)).then(|| Proxied {
            path: path.to_string(),
            every_method,
        })
    });
    // Undeclared (the default this gateway is built with), each family
    // matcher is `any(proxy::handle)`; declared, its writes still chain
    // through `proxy::handle` — public_reads.rs `mount`.
    let from_public_reads = public_reads::PUBLISHABLE
        .iter()
        .flat_map(|read| read.matchers.iter())
        .map(|m| Proxied {
            path: m.to_string(),
            every_method: true,
        });
    from_main
        .chain(from_public_reads)
        .map(|p| Proxied {
            path: p.path.replace("{*rest}", "probe"),
            ..p
        })
        .collect()
}

async fn send(
    app: axum::Router,
    method: Method,
    path: &str,
    cookie: Option<&str>,
) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let resp = app
        .oneshot(
            req.header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
        .expect("router responds");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The cookie `POST /api/auth/guest` hands an anonymous visitor on a
/// gateway minting `mode` — minted by the gateway's own `guest()`
/// through its own route, so the role it carries is exactly the one a
/// stranger presents. The role is read back out of the signed cookie
/// and held to the mode's, so a Basic run is provably a `visitor` and an
/// Audit run an `audit-readonly`, not two runs of one role.
async fn guest_cookie(app: axum::Router, mode: GuestAccess) -> String {
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/guest")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("guest door responds");
    assert_eq!(resp.status(), StatusCode::OK, "the guest door is open");
    let set = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("guest() sets a cookie");
    let pair = set.split(';').next().expect("name=value").to_string();
    let raw = pair
        .strip_prefix(&format!("{}=", session::COOKIE_NAME))
        .expect("the session cookie");
    let minted = Session::decode(raw, &KEY).expect("the gateway signed it");
    assert_eq!(minted.role.as_deref(), mode.role(), "{mode:?} guest's role");
    pair
}

/// A session signed with the gateway's key, as login would mint it.
fn signed_cookie(role: Option<&str>, employee_id: Option<&str>) -> String {
    let mut sess = Session::new("someone@boss.test", 600);
    sess.role = role.map(str::to_string);
    sess.employee_id = employee_id.map(str::to_string);
    format!("{}={}", session::COOKIE_NAME, sess.encode(&KEY))
}

fn hits_of(hits: &Hits) -> Vec<String> {
    hits.lock().expect("hits lock").clone()
}

/// THE DEFECT. A guest-minted session — the basic `visitor` AND the
/// system-audit `audit-readonly`, each minted by the guest door in its
/// own mode — sends POST, PUT, PATCH and DELETE to every proxied route:
/// each `any` route answers the named 403, each GET-only route its 405,
/// and the upstream records NOTHING.
#[tokio::test]
async fn a_guest_session_cannot_write_to_any_upstream() {
    let routes = proxied_routes();
    assert!(
        routes.iter().filter(|r| r.every_method).count() >= 55,
        "the scan found only {} proxied routes — main.rs's route shape moved or the \
         scan broke: {routes:?}",
        routes.len()
    );

    let mut leaked = Vec::new();
    for mode in GUEST_MODES {
        let (upstream, hits) = recording_upstream().await;
        let app = gateway(&upstream, mode);
        let guest = guest_cookie(app.clone(), mode).await;
        for route in &routes {
            for method in WRITES {
                let (status, body) =
                    send(app.clone(), method.clone(), &route.path, Some(&guest)).await;
                let refused = if route.every_method {
                    status == StatusCode::FORBIDDEN && body.contains(proxy::READ_ONLY_REFUSAL)
                } else {
                    status == StatusCode::METHOD_NOT_ALLOWED
                };
                if !refused {
                    leaked.push(format!(
                        "{mode:?}: {method} {} -> {status} {body}",
                        route.path
                    ));
                }
            }
        }
        assert_eq!(
            hits_of(&hits),
            Vec::<String>::new(),
            "a refused {mode:?} guest write must never reach an upstream"
        );
    }
    assert!(
        leaked.is_empty(),
        "{} guest write(s) were not refused at the edge:\n  {}",
        leaked.len(),
        leaked.join("\n  ")
    );
}

/// The read half is untouched: each guest's GET on every route is
/// forwarded, and the upstream sees one request per route. What a
/// `visitor` may then READ is the upstream's policy answer, not the
/// edge's.
#[tokio::test]
async fn a_guest_session_still_reads_every_upstream() {
    let routes = proxied_routes();
    for mode in GUEST_MODES {
        let (upstream, hits) = recording_upstream().await;
        let app = gateway(&upstream, mode);
        let guest = guest_cookie(app.clone(), mode).await;
        for route in &routes {
            for method in [Method::GET, Method::HEAD] {
                let (status, _) =
                    send(app.clone(), method.clone(), &route.path, Some(&guest)).await;
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "{mode:?} guest {method} {}",
                    route.path
                );
            }
        }
        let seen = hits_of(&hits);
        assert_eq!(
            seen.len(),
            routes.len() * 2,
            "one forward per {mode:?} read: {seen:?}"
        );
        assert!(
            seen.iter()
                .all(|h| h.starts_with("GET ") || h.starts_with("HEAD ")),
            "{seen:?}"
        );
    }
}

/// A read-only role signed by login rather than the guest door is
/// refused the same: the seeded external auditor (`audit-readonly`), a
/// `visitor`, and a session whose role is absent — which acts as a
/// `visitor` (`boss_core::roles::effective_role`), downstream through
/// role_headers and here at the edge alike.
#[tokio::test]
async fn an_auditor_a_visitor_and_a_roleless_session_are_read_only_too() {
    let (upstream, hits) = recording_upstream().await;
    let app = gateway(&upstream, GuestAccess::Off);
    for cookie in [
        signed_cookie(
            Some(boss_core::roles::AUDIT_READONLY_ROLE),
            Some("emp-audit"),
        ),
        signed_cookie(Some(boss_core::roles::VISITOR_ROLE), None),
        signed_cookie(None, None),
    ] {
        let (status, body) = send(app.clone(), Method::POST, "/api/jobs", Some(&cookie)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(body.contains(proxy::READ_ONLY_REFUSAL), "{body}");
    }
    assert!(hits_of(&hits).is_empty());
}

/// THE CONTROL that makes the refusals above mean something: the same
/// plumbing carrying a platform-admin session forwards every write on
/// every route, so a 403 there is the edge's answer and not a cookie
/// the proxy failed to read or a route that never forwards.
#[tokio::test]
async fn a_platform_admin_session_still_writes_through() {
    let (upstream, hits) = recording_upstream().await;
    let app = gateway(&upstream, GuestAccess::Off);
    let admin = signed_cookie(
        Some(boss_core::roles::PLATFORM_ADMIN_ROLE),
        Some("emp-admin"),
    );
    let routes: Vec<Proxied> = proxied_routes()
        .into_iter()
        .filter(|r| r.every_method)
        .collect();
    for route in &routes {
        for method in WRITES {
            let (status, body) = send(app.clone(), method.clone(), &route.path, Some(&admin)).await;
            assert_eq!(
                (status, body.as_str()),
                (StatusCode::OK, UPSTREAM_ANSWER),
                "admin {method} {}",
                route.path
            );
        }
    }
    let seen = hits_of(&hits);
    assert_eq!(seen.len(), routes.len() * WRITES.len(), "{seen:?}");
    for method in WRITES {
        assert!(
            seen.iter().any(|h| h.starts_with(&format!("{method} "))),
            "no {method} reached the upstream: {seen:?}"
        );
    }
}

/// The gateway's OWN write that is not an auth ceremony: resetting the
/// latency histograms answered anyone, with or without a session. A
/// stranger is now asked for a session and a read-only one — either
/// guest — is refused by name; a writer still resets.
#[tokio::test]
async fn resetting_gateway_perf_needs_a_session_that_may_write() {
    let path = "/api/gateway/perf/reset";
    for mode in GUEST_MODES {
        let (upstream, hits) = recording_upstream().await;
        let app = gateway(&upstream, mode);

        let (status, _) = send(app.clone(), Method::POST, path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a stranger");

        let guest = guest_cookie(app.clone(), mode).await;
        let (status, body) = send(app.clone(), Method::POST, path, Some(&guest)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a {mode:?} guest: {body}");
        assert!(body.contains(proxy::READ_ONLY_REFUSAL), "{body}");

        let admin = signed_cookie(
            Some(boss_core::roles::PLATFORM_ADMIN_ROLE),
            Some("emp-admin"),
        );
        let (status, body) = send(app, Method::POST, path, Some(&admin)).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
        assert!(hits_of(&hits).is_empty(), "perf reset is the gateway's own");
    }
}

/// The doors a read-only session legitimately needs are the gateway's
/// own, not proxied, and stay open to it: signing out, and asking
/// whether a guest door exists. (The passkey and break-glass ceremonies
/// carry their own gates — an employee-bearing session, a break-glass
/// session or the bootstrap token — and are not this edge's to close.)
#[tokio::test]
async fn a_guest_can_still_sign_out() {
    for mode in GUEST_MODES {
        let (upstream, hits) = recording_upstream().await;
        let app = gateway(&upstream, mode);
        let guest = guest_cookie(app.clone(), mode).await;
        let (status, _) = send(app.clone(), Method::POST, "/api/auth/logout", Some(&guest)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{mode:?}");
        let (status, _) = send(app, Method::GET, "/api/auth/me", Some(&guest)).await;
        assert_eq!(status, StatusCode::OK, "{mode:?}");
        assert!(hits_of(&hits).is_empty());
    }
}
