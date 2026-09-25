//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// The gateway's two-minute expiry holds here too: a genuine ticket
/// presented late is refused.
#[tokio::test]
async fn an_expired_ticket_is_refused() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() - 1);

    let (status, body) = sign_off(&app, Some(&t)).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(stored(&jobs, SIGNED).await.sign_offs.is_empty());
}
