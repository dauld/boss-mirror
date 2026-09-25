//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// THE BUG on the completion door: a machine-token holder completes a
/// presence-required step as emp-david with a hand-written header.
#[tokio::test]
async fn a_forged_presence_header_cannot_complete_a_presence_required_step() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let (status, body) = complete(&app, GUARDED, Some(&forged_json(GUARDED))).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unsigned x-boss-presence header must not buy a passkey's assurance; body: {body}"
    );
    assert_eq!(stored(&jobs, GUARDED).await.status, StepStatus::Ready);
}
