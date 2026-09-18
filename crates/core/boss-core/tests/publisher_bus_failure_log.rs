//! A bus outage surfaces in the return, keeps the audit row, and logs.
//!
//! **This file holds exactly one test, and that is the point.** It
//! asserts what the publisher logged, and a captured log is
//! deterministic only when the capturing test is alone in its process
//! with the subscriber installed globally before the first event
//! (`tests/common/mod.rs` carries the mechanism and the measurement;
//! `boss-testing/tests/a_log_capturing_test_owns_its_process.rs`
//! refuses any other shape).

mod common;

use std::sync::atomic::Ordering;

use boss_core::publisher::DomainPublisher;
use common::{Captured, StubAudit, StubBus, event};

#[tokio::test]
async fn bus_failure_returns_false_audit_still_written_and_logged() {
    // A bus outage must not lose the audit row (the system of
    // record outranks the notification bus), must surface in the
    // return value, and must log.
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let audit = StubAudit::new(true);
    let publisher = DomainPublisher::new(StubBus::new(false), "test-svc").with_audit(audit.clone());

    let ok = publisher.publish(event()).await;
    assert!(!ok, "bus failure must surface in the return");
    assert_eq!(
        audit.writes.load(Ordering::SeqCst),
        1,
        "audit row must still be written when the bus is down"
    );
    let out = log.text();
    assert!(
        out.contains("event bus publish failed"),
        "bus failure must log: {out}"
    );
}
