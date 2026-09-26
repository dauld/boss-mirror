//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

// ---------------------------------------------------------------------
// A TICKET STAMPS ITS STEP ONCE (backlog 3977b3d2, triage run 44eed541,
// 2026-09-26).
//
// A genuine ticket binds step, person and shape, and lives 120 s. The
// sign-off door answers a re-send idempotently only while the stamp it
// wrote is LIVE (design 87329a13), so an A-B-A inside those 120 s —
// stamp on A, the content moves to B (the stamp is voided), the content
// comes back to A — let the ORIGINAL ticket write a fresh live stamp on
// A. No passkey touched that second stamp; single-use, claimed by design
// 17835005 and migration 151's comment, was never enforced. The step's
// own stamps, live and voided, are the record of which tickets it has
// consumed, so the door refuses a ticket whose nonce is already on one.
// ---------------------------------------------------------------------

/// The plan `presence::step` seeds — shape A.
const PLAN_A: &str = r#"{"plan":"{\"verb\":\"wipe-a-disk\"}"}"#;
/// Any other plan — shape B.
const PLAN_B: &str = r#"{"plan":"{\"verb\":\"wipe-another-disk\"}"}"#;

async fn merge(app: &Router, body: &str) -> (StatusCode, String) {
    send(
        app,
        "PATCH",
        format!("/api/jobs/{JOB}/steps/{SIGNED}/metadata"),
        None,
        body,
    )
    .await
}

#[tokio::test]
async fn a_ticket_replayed_after_its_stamp_was_voided_is_refused() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let first = stored(&jobs, SIGNED).await.sign_offs;
    assert_eq!(first.len(), 1);
    let spent = first[0].presence_nonce.clone();
    assert_eq!(spent.as_deref(), Some("ceremony-nonce-7"));

    // A → B → A: the edit voids the stamp, the second edit restores the
    // exact shape the ticket was minted over.
    let (status, body) = merge(&app, PLAN_B).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    let (status, body) = merge(&app, PLAN_A).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    let row = stored(&jobs, SIGNED).await;
    assert_eq!(row.shape_hash(), binding(SIGNED).1, "back on shape A");
    assert!(row.sign_offs[0].voided_at.is_some(), "the stamp died on B");

    // THE REPLAY: the captured ticket, still unexpired, still bound to
    // this step, this person and shape A.
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a spent ticket wrote a second stamp: {body}"
    );
    assert!(
        body.contains("already stamped this step"),
        "the refusal names the reason: {body}"
    );
    assert!(
        body.contains("\"required\":\"presence\""),
        "and what would answer it — a fresh ceremony: {body}"
    );
    let after = stored(&jobs, SIGNED).await.sign_offs;
    assert_eq!(after.len(), 1, "nothing was written: {after:?}");
    assert_eq!(
        after.iter().filter(|s| s.presence_nonce == spent).count(),
        1,
        "the voided stamp is the only one carrying that nonce"
    );

    // A FRESH ceremony signs A again — the door refuses the ticket, never
    // the signature — and the completion still carries that same fresh
    // ticket a second time (the web's `completeWithPresence`), because
    // the completion judges assurance and consumes nothing.
    let (step_id, shape_hash) = binding(SIGNED);
    let fresh = PresenceTicket {
        i: "emp-david".into(),
        s: step_id,
        h: shape_hash,
        n: "ceremony-nonce-9".into(),
        e: now_epoch() + 60,
    }
    .encode(GATEWAY_KEY)
    .expect("a ticket signs");
    let (status, body) = sign_off(&app, Some(&fresh)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let row = stored(&jobs, SIGNED).await;
    assert_eq!(row.live_stamps().count(), 1, "{:?}", row.sign_offs);
    let (status, body) = complete(&app, SIGNED, Some(&fresh)).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Completed);
}
