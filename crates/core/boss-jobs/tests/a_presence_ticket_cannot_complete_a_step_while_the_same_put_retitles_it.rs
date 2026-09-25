//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

// ---------------------------------------------------------------------
// THE CONTENT JUDGED IS THE CONTENT COMPLETED (backlog c0b56fd9,
// adversarial review of car 5b30ccf9, 2026-09-25).
//
// A genuine ticket binds step, person and the step's shape hash, and
// `update_step` judged it against the step BEFORE the PUT's overlay. So
// on a presence step with no sign-off roles a valid ticket completed the
// step while the same PUT replaced the plan it had been minted over:
// 204, the stored plan replaced, and nobody's passkey ever saw the bytes
// that were completed. (On a sign-off step the same body was caught only
// incidentally, by the stale-stamp 409.) The test below is the real
// ticket the gateway would have issued — the forgery is not the point.
// ---------------------------------------------------------------------

/// The title is half the shape hash, and a PUT may write it: the same
/// hole through the other input.
#[tokio::test]
async fn a_presence_ticket_cannot_complete_a_step_while_the_same_put_retitles_it() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = put(
        &app,
        GUARDED,
        Some(&t),
        r#"{"status":"completed","title":"Approve something nobody read"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    let after = stored(&jobs, GUARDED).await;
    assert_eq!(after.status, StepStatus::Ready);
    assert_eq!(after.title, "Approve the plan");
}
