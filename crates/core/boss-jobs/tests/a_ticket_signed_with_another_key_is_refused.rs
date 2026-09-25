//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// A ticket signed with any key but the gateway's is a forgery too.
#[tokio::test]
async fn a_ticket_signed_with_another_key_is_refused() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(
        SIGNED,
        b"a-key-the-forger-made-up-0123456789",
        now_epoch() + 60,
    );

    let (status, body) = sign_off(&app, Some(&t)).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(stored(&jobs, SIGNED).await.sign_offs.is_empty());
}
