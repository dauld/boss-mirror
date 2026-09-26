//! A calendar feed token never reaches the gateway log.
//!
//! The feed is `/ics/<token>/calendar.ics`, read sessionless by a
//! calendar app on a timer, and the token IS the credential: whoever
//! holds the URL reads the employee's schedule. Until backlog d9f64c4a
//! (2026-09-26) the request timer logged `req.uri().path()` raw at
//! info for every request, so every poll wrote a live token into the
//! gateway's pod log — the last place one rested once the table and
//! the event held only its hash (car
//! fix/a-calendar-token-rests-as-its-hash, review finding 1).
//!
//! **This file holds exactly one test, and that is the point.** It
//! asserts what the gateway LOGGED, and a captured log is
//! deterministic only when the capturing test is alone in its process
//! with the subscriber installed globally before the first event
//! (`boss-testing/tests/a_log_capturing_test_owns_its_process.rs`
//! refuses any other shape).

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use boss_gateway::perf::PerfCollector;
use boss_gateway::timing::request_timer;
use tower::ServiceExt;

/// The shape `rotate_calendar_token` mints: two simple UUIDs, 64 hex.
const MIXED_TOKEN: &str = "3f9a1c7e5b2d4086a1e3c5b7d9f02468ace13579bdf02468ace13579bdf0246a";
/// A 64-hex token with no digit at all. The perf path normaliser's id
/// heuristic wants two digits and would let this one through, so it
/// pins that the log redacts by POSITION, not by what the token looks
/// like.
const LETTER_TOKEN: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

async fn get_status(router: &Router, path: &str) -> StatusCode {
    router
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_calendar_feed_poll_logs_its_request_without_the_token() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let perf = Arc::new(PerfCollector::new());
    let router = Router::new()
        .route("/ics/{*rest}", get(|| async { "BEGIN:VCALENDAR" }))
        .route("/health", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            perf.clone(),
            request_timer,
        ));

    // The feed as the calendar app polls it, both token shapes, plus the
    // older `/ics/<token>.ics` spelling a stale subscription may still send.
    for path in [
        format!("/ics/{MIXED_TOKEN}/calendar.ics"),
        format!("/ics/{LETTER_TOKEN}/calendar.ics"),
        format!("/ics/{LETTER_TOKEN}.ics"),
    ] {
        assert_eq!(get_status(&router, &path).await, StatusCode::OK, "{path}");
    }
    // A control: a path with no secret in it is logged as it came.
    assert_eq!(get_status(&router, "/health").await, StatusCode::OK);

    let text = log.text();
    let requests: Vec<&str> = text.lines().filter(|l| l.contains("request")).collect();
    // Silence is not a pass: the four requests each wrote their line,
    // the feed's with the token's place held by a placeholder.
    assert_eq!(
        requests.len(),
        4,
        "one line per request; the log was:\n{text}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|l| l.contains("path=/ics/{token}/calendar.ics"))
            .count(),
        2,
        "the feed is logged with its token redacted; the log was:\n{text}"
    );
    assert!(
        requests.iter().any(|l| l.contains("path=/ics/{token} ")),
        "the old spelling is redacted too; the log was:\n{text}"
    );
    assert!(
        requests.iter().any(|l| l.contains("path=/health ")),
        "a path with no secret is logged unchanged; the log was:\n{text}"
    );

    for token in [MIXED_TOKEN, LETTER_TOKEN] {
        assert!(
            !text.contains(token),
            "a calendar feed token reached the gateway log:\n{text}"
        );
        // The perf table is served at /api/gateway/perf; it keys on the
        // same path the log writes, so it holds no token either.
        assert!(
            perf.snapshot()
                .endpoints
                .iter()
                .all(|e| !e.path.contains(token)),
            "a calendar feed token reached the perf table"
        );
    }
}
