//! A batch publish logs every failure, bus and audit alike.
//!
//! **This file holds exactly one test, and that is the point.** It
//! asserts what the publisher logged, and a captured log is
//! deterministic only when the capturing test is alone in its process
//! with the subscriber installed globally before the first event
//! (`tests/common/mod.rs` carries the mechanism and the measurement;
//! `boss-testing/tests/a_log_capturing_test_owns_its_process.rs`
//! refuses any other shape).

mod common;

use boss_core::publisher::DomainPublisher;
use common::{Captured, StubAudit, StubBus, event};

#[tokio::test]
async fn publish_batch_logs_every_failure() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let publisher =
        DomainPublisher::new(StubBus::new(false), "test-svc").with_audit(StubAudit::new(false));

    publisher.publish_batch(vec![event(), event()]).await;
    let out = log.text();
    assert_eq!(
        out.matches("event bus publish failed").count(),
        2,
        "each bus failure logs: {out}"
    );
    assert!(
        out.contains("audit_log batch write failed"),
        "batch audit failure logs: {out}"
    );
}
