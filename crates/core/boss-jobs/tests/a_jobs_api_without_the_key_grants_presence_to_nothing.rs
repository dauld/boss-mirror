//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// Fail closed: a jobs API that never found the key cannot verify
/// anything, so it grants presence to nothing — not even a genuine
/// ticket — rather than falling back to trusting the header.
#[tokio::test]
async fn a_jobs_api_without_the_key_grants_presence_to_nothing() {
    let (app, jobs) = seed(None).await;
    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = sign_off(&app, Some(&t)).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(stored(&jobs, SIGNED).await.sign_offs.is_empty());
}
