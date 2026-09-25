//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// THE GATEWAY PATH KEEPS WORKING: a ticket the gateway signed, for
/// this step's current shape, stamps Presence with its nonce on the
/// sign-off door and completes the step on the completion door.
#[tokio::test]
async fn a_genuine_ticket_still_stamps_presence_and_completes() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let stamp = stored(&jobs, SIGNED)
        .await
        .sign_offs
        .first()
        .cloned()
        .expect("a stamp was appended");
    assert_eq!(stamp.assurance, Assurance::Presence);
    assert_eq!(stamp.presence_nonce.as_deref(), Some("ceremony-nonce-7"));

    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = complete(&app, GUARDED, Some(&t)).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, GUARDED).await.status, StepStatus::Completed);
}
