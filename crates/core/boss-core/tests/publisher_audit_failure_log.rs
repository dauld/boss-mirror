//! An audit write that fails never fails the publish — and is LOUD.
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
async fn audit_failure_never_fails_the_publish_but_logs_error() {
    // The load-bearing halves of the contract: a domain write must
    // not fail because audit_log is unavailable (return stays
    // bus_ok), AND the hole punched in the system of record must
    // be LOUD — a swallowed rejection is how 260 facts went
    // unreproducible before anyone noticed (2026-07-13).
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let audit = StubAudit::new(false);
    let publisher = DomainPublisher::new(StubBus::new(true), "test-svc").with_audit(audit.clone());

    let ok = publisher.publish(event()).await;
    assert!(ok, "audit failure must not fail the publish");
    assert_eq!(audit.writes.load(Ordering::SeqCst), 1);
    let out = log.text();
    assert!(
        out.contains("ERROR"),
        "audit failure must log at ERROR: {out}"
    );
    assert!(
        out.contains("audit_log write failed"),
        "log must name the failure: {out}"
    );
    assert!(
        out.contains("commerce.invoice.created"),
        "log must carry the event kind: {out}"
    );
    assert!(
        out.contains("insert rejected by audit_log_check_refs"),
        "log must carry the underlying error: {out}"
    );
}
