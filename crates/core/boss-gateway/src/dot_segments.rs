//! No path reaches a route with a `.` or `..` segment in it (backlog
//! 1d9b7db7, 2026-09-25).
//!
//! WHY. The route table is an allowlist only if the path a route
//! matched is the path its upstream receives, and it was not. axum
//! matches on the raw, still-percent-encoded path, so `/ics/%2e%2e/x`
//! matches `/ics/{*rest}`; the proxy then hands `{upstream}{path}` to
//! reqwest, whose URL parser (url 2.5.8, parser.rs) resolves `..`,
//! `%2e%2e` in every case mix, `.%2e` and `%2e.` as a parent segment and
//! `.` / `%2e` as a current one — BEFORE the request leaves the gateway.
//! The host and port stay the route's; the path moves anywhere on that
//! upstream. Measured by the triage of this item on origin/main
//! 9df57797:
//!
//! - `/ics/{*rest}` is sessionless on EVERY instance and proxies to the
//!   jobs upstream, so `GET /ics/%2e%2e/api/<anything>` reached
//!   boss-jobs-api with no session at all;
//! - `/api/workflows/{*rest}`, where a tenant declares it public (the
//!   public demo tenant does), is the same door;
//! - for a session holder, `/api/<svc>/%2e%2e/<path>` reached paths on
//!   that upstream the gateway never routes — the table stopped being
//!   an allowlist.
//!
//! The URL standard also treats `\` as a path separator for http, and
//! the `http` crate accepts a raw `\` in a request target (uri/path.rs
//! admits 0x40..=0x5F), so `/ics/..\api/jobs` is the same traversal
//! spelled with a backslash; this module splits on both.
//!
//! And the static SPA handler (static_files.rs) joins the raw path onto
//! its directory and checks `Path::starts_with`, which is lexical: a
//! raw `..` passes that check and the OS resolves it. Refusing here
//! closes that too, without asking each handler to get it right.
//!
//! WHAT IS REFUSED. A segment — between `/` or `\` separators — that
//! reads `.` or `..` once `%2e` (either case) is read as a dot: exactly
//! the set the URL parser collapses, so nothing it would move is let
//! through and nothing it leaves alone is refused. `calendar.ics`,
//! `app.js`, `v1.2`, `..hidden` and `...` are names, not dot segments,
//! and pass. A browser never sends a dot segment (it resolves them
//! before the request), so the only senders of one are hand-built
//! requests, which get a 400 naming why.
//!
//! WHERE. Outermost, and around the router rather than inside it:
//! [`mount`] wraps the whole finished router — site, inquiry door,
//! role headers, every route and the fallback — as the ONE service a
//! request enters, so no matcher, no handler and no other layer sees a
//! refused path.

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// The named refusal a dot-segment path gets, before any routing.
pub(crate) const DOT_SEGMENT_REFUSAL: &str =
    "a request path may not contain a `.` or `..` segment, raw or percent-encoded";

/// Does `path` carry a segment the URL parser would resolve away?
pub(crate) fn has_dot_segment(path: &str) -> bool {
    path.split(['/', '\\']).any(|segment| {
        let read = segment.to_ascii_lowercase().replace("%2e", ".");
        read == "." || read == ".."
    })
}

/// The gateway, entered through the refusal: the finished router
/// becomes the ONLY service of an otherwise empty router (its
/// fallback), and the refusal is layered on that, so it runs before the
/// inner router matches anything. Called once in `main`, last, on the
/// finished app.
pub fn mount(app: axum::Router) -> axum::Router {
    axum::Router::new()
        .fallback_service(app)
        .layer(axum::middleware::from_fn(refuse))
}

async fn refuse(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if has_dot_segment(path) {
        tracing::warn!(path = %path, method = %req.method(), "refused a dot-segment path before routing");
        return (StatusCode::BAD_REQUEST, DOT_SEGMENT_REFUSAL).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::has_dot_segment;

    /// Every spelling the URL parser collapses, as a whole segment, in
    /// the middle and at the end of a path, behind either separator.
    #[test]
    fn every_spelling_the_url_parser_collapses_is_a_dot_segment() {
        for seg in crate::a_path_cannot_climb_out_of_its_route::DOT_SEGMENTS {
            for path in [
                format!("/ics/{seg}/api/jobs"),
                format!("/api/jobs/{seg}"),
                format!("/ics\\{seg}\\api"),
            ] {
                assert!(has_dot_segment(&path), "{path} was let through");
            }
        }
    }

    /// Names with dots in them are names.
    #[test]
    fn a_dot_inside_a_name_is_not_a_segment() {
        for path in [
            "/",
            "/ics/0123abcd.ics",
            "/assets/app.js",
            "/dashboard/chunk-abc123.js",
            "/api/jobs/v1.2",
            "/api/files/..hidden",
            "/api/files/.well-known",
            "/api/files/...",
            "/api/files/%2e%2e%2e",
            "/api/files/%252e%252e",
            "/api/files/%2e%2e%2fjobs",
            "/it/yard",
        ] {
            assert!(!has_dot_segment(path), "{path} was refused");
        }
    }
}
