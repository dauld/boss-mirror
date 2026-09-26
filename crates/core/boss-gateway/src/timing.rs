//! Request timing middleware — logs method, path, status, and duration
//! for every request passing through the gateway, and records into the
//! shared `PerfCollector` for the `/api/gateway/perf` endpoint.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::perf::PerfCollector;

/// The calendar feed's prefix. The segment after it is the feed token,
/// and the token IS the credential: whoever holds the URL reads the
/// employee's schedule (boss-jobs `scheduling/http.rs`,
/// `/ics/{token}/calendar.ics`).
const FEED_PREFIX: &str = "/ics/";

/// The path as the log and the perf table may hold it: the calendar
/// feed's token segment becomes `{token}`, every other path is
/// returned as it came.
///
/// By POSITION, not by shape (backlog d9f64c4a, 2026-09-26): the perf
/// normaliser's id heuristic wants two digits in a segment, and a
/// 64-hex token with fewer is rare but mintable. Everything up to the
/// next `/` goes, so the older `/ics/<token>.ics` spelling a stale
/// subscription may still poll is covered too.
pub fn loggable_path(path: &str) -> Cow<'_, str> {
    match path.strip_prefix(FEED_PREFIX) {
        Some(rest) => {
            let tail = rest.find('/').map_or("", |i| &rest[i..]);
            Cow::Owned(format!("{FEED_PREFIX}{{token}}{tail}"))
        }
        None => Cow::Borrowed(path),
    }
}

pub async fn request_timer(
    State(perf): State<Arc<PerfCollector>>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    // Redacted before it is kept: until d9f64c4a every calendar-app
    // poll wrote a live feed token into the pod log at info.
    let path = loggable_path(req.uri().path()).into_owned();
    let start = Instant::now();

    let response = next.run(req).await;

    let duration = start.elapsed();
    let status = response.status().as_u16();
    let ms = duration.as_secs_f64() * 1000.0;

    perf.record(method.as_str(), &path, ms, status);

    tracing::info!(
        method = %method,
        path = %path,
        status = status,
        duration_ms = format!("{ms:.1}"),
        "request"
    );

    response
}

#[cfg(test)]
mod tests {
    use super::loggable_path;

    #[test]
    fn the_feed_token_segment_is_redacted_wherever_it_ends() {
        assert_eq!(
            loggable_path("/ics/abcdefabcdef/calendar.ics"),
            "/ics/{token}/calendar.ics"
        );
        assert_eq!(loggable_path("/ics/0123abcd.ics"), "/ics/{token}");
        assert_eq!(loggable_path("/ics/"), "/ics/{token}");
    }

    #[test]
    fn a_path_with_no_feed_token_is_unchanged() {
        for path in [
            "/health",
            "/ics",
            "/api/jobs/5e2a7c9f-1b3d-4e6a-8c0f-2d4b6a8c0e1f",
            "/api/scheduling/ics/abc",
        ] {
            assert_eq!(loggable_path(path), path);
        }
    }
}
