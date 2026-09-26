//! A path cannot climb out of the route it matched (backlog 1d9b7db7,
//! 2026-09-25).
//!
//! The route table is the gateway's allowlist, and it held only while
//! the path a route matched was the path its upstream received. It was
//! not: `/ics/%2e%2e/api/jobs` matched the sessionless calendar feed and
//! reached the jobs API at `/api/jobs`, because reqwest's URL parser
//! resolves dot segments after routing has already said yes. The
//! refusal is one layer around the whole router (dot_segments.rs); these
//! tests hold it to every spelling the parser collapses, under every
//! kind of door, with no session, a guest's, and a platform admin's.
//!
//! THE UPSTREAM IS THE RECORDING STUB for every route (the plumbing of
//! a_read_only_session_cannot_write.rs), so "never reached an upstream"
//! is a count, and no request here can reach a real service — not even
//! the traversals the red run lets through.

use axum::http::{Method, StatusCode};

use super::a_read_only_session_cannot_write::{
    UPSTREAM_ANSWER, gateway_declaring, guest_cookie, hits_of, recording_upstream, send,
    signed_cookie,
};
use super::*;

/// Every spelling of a single or double dot segment that url 2.5.8
/// (parser.rs, the two `match segment_before_slash` arms) collapses.
pub(crate) const DOT_SEGMENTS: [&str; 12] = [
    ".", "%2e", "%2E", "..", "%2e%2e", "%2E%2e", "%2e%2E", "%2E%2E", ".%2e", ".%2E", "%2e.", "%2E.",
];

/// The guest these tests mint: the system-audit `audit-readonly`, the
/// one the guest door handed out when they were written (design
/// 2830b6b7 split it into Basic and Audit; the refusal comes before
/// routing, so either role meets the same 400).
const GUEST: GuestAccess = GuestAccess::Audit;

/// A door of every kind: the sessionless by-design feed, a declared
/// public family, gated proxies, the simulator app proxy, the plugin
/// files, the SPA and its assets, and the root.
const DOORS: [&str; 9] = [
    "/ics",
    "/api/workflows",
    "/api/jobs",
    "/api/people",
    "/simulator",
    "/plugins",
    "/dashboard",
    "/assets",
    "",
];

/// The gateway as `main` finishes it: the route table with the demo
/// tenant's `/api/workflows` declared sessionless (the second named
/// case needs it open), entered through the refusal.
async fn gateway() -> (axum::Router, super::a_read_only_session_cannot_write::Hits) {
    let (upstream, hits) = recording_upstream().await;
    let reads = public_reads::PublicReads::resolve(&[
        "/api/workflows".to_string(),
        "/api/jobs/live".to_string(),
    ])
    .expect("declarable reads");
    (
        dot_segments::mount(gateway_declaring(&upstream, GUEST, &reads)),
        hits,
    )
}

fn admin() -> String {
    signed_cookie(
        Some(boss_core::roles::PLATFORM_ADMIN_ROLE),
        Some("emp-admin"),
    )
}

/// Send `path` as `method` under each cookie; name every answer that is
/// not the refusal.
async fn unrefused(
    app: &axum::Router,
    method: Method,
    paths: &[String],
    cookies: &[(&str, Option<String>)],
) -> Vec<String> {
    let mut out = Vec::new();
    for path in paths {
        for (who, cookie) in cookies {
            let (status, body) = send(app.clone(), method.clone(), path, cookie.as_deref()).await;
            if !(status == StatusCode::BAD_REQUEST && body == dot_segments::DOT_SEGMENT_REFUSAL) {
                out.push(format!("{who} {method} {path} -> {status} {body}"));
            }
        }
    }
    out
}

/// THE DEFECT, by name. The two sessionless doors that carried a
/// traversal to the jobs API: the calendar feed on every instance, and
/// the workflows family wherever a tenant declares it. No session, a
/// guest's, an admin's — each answered 400 before routing, and the
/// upstream records nothing.
#[tokio::test]
async fn the_sessionless_doors_cannot_reach_the_jobs_api_by_traversal() {
    let (app, hits) = gateway().await;
    let guest = guest_cookie(app.clone(), GUEST).await;
    let named = [
        "/ics/%2e%2e/api/jobs".to_string(),
        "/api/workflows/%2e%2e/jobs".to_string(),
    ];
    let cookies = [
        ("sessionless", None),
        ("guest", Some(guest)),
        ("admin", Some(admin())),
    ];
    let leaked = unrefused(&app, Method::GET, &named, &cookies).await;
    let seen = hits_of(&hits);
    assert!(
        leaked.is_empty() && seen.is_empty(),
        "a traversal crossed the gateway:\n  {}\nthe upstream received:\n  {}",
        leaked.join("\n  "),
        seen.join("\n  ")
    );
}

/// Every spelling the URL parser collapses, under every kind of door,
/// in the middle of a path, at its end, and behind a backslash (which
/// the URL standard reads as `/` for http): each refused, for every
/// session and for none, reads and writes alike. And the static
/// handler's own case — a raw `..` out of its directory toward a file
/// that has an extension, which it serves without a session.
#[tokio::test]
async fn every_dot_segment_spelling_is_refused_before_routing() {
    let (app, hits) = gateway().await;
    let guest = guest_cookie(app.clone(), GUEST).await;
    let mut paths: Vec<String> = DOORS
        .iter()
        .flat_map(|door| {
            DOT_SEGMENTS.iter().flat_map(move |seg| {
                [
                    format!("{door}/{seg}/api/jobs"),
                    format!("{door}/x/{seg}/{seg}/api/jobs?limit=1"),
                    format!("{door}/{seg}"),
                    format!("{door}/{seg}\\api\\jobs"),
                ]
            })
        })
        .collect();
    paths.push("/../../boss-gateway/session.key".to_string());
    paths.push("/assets/%2e%2e/%2e%2e/%2e%2e/etc/hostname.conf".to_string());
    let cookies = [
        ("sessionless", None),
        ("guest", Some(guest)),
        ("admin", Some(admin())),
    ];

    let mut leaked = unrefused(&app, Method::GET, &paths, &cookies).await;
    leaked.extend(unrefused(&app, Method::POST, &paths, &cookies[2..]).await);
    let seen = hits_of(&hits);
    assert!(
        leaked.is_empty() && seen.is_empty(),
        "{} of {} dot-segment request(s) were not refused:\n  {}\nthe upstream received:\n  {}",
        leaked.len(),
        paths.len() * 4,
        leaked.join("\n  "),
        seen.join("\n  ")
    );
}

/// THE CONTROL. Names with dots in them are names: the calendar feed's
/// `.ics`, a versioned or dotted file name, a dot-led name, three dots —
/// each forwarded, sessionless where the door is, and arriving at the
/// upstream AS SENT (so none of them was normalised either). The
/// declared public reads still answer without a session, and the SPA's
/// routes and assets still reach the static handler.
#[tokio::test]
async fn names_with_dots_and_every_ordinary_path_still_pass() {
    let (app, hits) = gateway().await;
    let forwarded: [(&str, Option<String>); 7] = [
        ("/ics/0123abcd.ics", None),
        ("/api/workflows/some-kind", None),
        ("/api/jobs/live", None),
        ("/api/jobs/v1.2", Some(admin())),
        ("/api/files/report.final.pdf", Some(admin())),
        ("/api/files/..hidden", Some(admin())),
        ("/api/files/...", Some(admin())),
    ];
    for (path, cookie) in &forwarded {
        let (status, body) = send(app.clone(), Method::GET, path, cookie.as_deref()).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, UPSTREAM_ANSWER),
            "GET {path}"
        );
    }
    let seen = hits_of(&hits);
    assert_eq!(seen.len(), forwarded.len(), "{seen:?}");
    for ((path, _), hit) in forwarded.iter().zip(&seen) {
        assert!(
            hit.starts_with("GET ") && hit.ends_with(path),
            "{path} arrived upstream as {hit}"
        );
    }

    let (status, body) = send(app.clone(), Method::GET, "/health", None).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
    for path in [
        "/",
        "/it/yard",
        "/assets/app.js",
        "/dashboard/chunk-abc123.js",
    ] {
        let (status, body) = send(app.clone(), Method::GET, path, Some(&admin())).await;
        assert!(
            status != StatusCode::BAD_REQUEST && !body.contains(dot_segments::DOT_SEGMENT_REFUSAL),
            "GET {path} -> {status} {body}"
        );
    }
}

/// The tests above drive `dot_segments::mount` around the route table;
/// this holds `main` to doing the same, LAST — after the site and the
/// inquiry door, which are layers on the router and would otherwise
/// see a dot-segment path first — and before the listener is served.
#[test]
fn main_enters_the_gateway_through_the_refusal_last() {
    let main = include_str!("main.rs");
    let at = |needle: &str| {
        main.find(needle)
            .unwrap_or_else(|| panic!("main.rs no longer says `{needle}`"))
    };
    let inquiries = at("let app = inquiries::mount(app, door);");
    let refusal = at("let app = dot_segments::mount(app);");
    let serve = at("axum::serve(");
    assert!(
        inquiries < refusal && refusal < serve,
        "the dot-segment refusal must wrap the finished app, after inquiries::mount and \
         before axum::serve"
    );
}
