//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// A genuine ticket still binds to the step it was minted for and to
/// the person who performed the ceremony: signed is necessary, not
/// sufficient.
#[tokio::test]
async fn a_genuine_ticket_for_another_step_or_person_is_not_presence() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    // Minted for GUARDED, presented at SIGNED.
    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");

    // Minted for someone else, presented by emp-david.
    let (step_id, shape_hash) = binding(SIGNED);
    let t = PresenceTicket {
        i: "emp-someone-else".into(),
        s: step_id,
        h: shape_hash,
        n: "n".into(),
        e: now_epoch() + 60,
    }
    .encode(GATEWAY_KEY)
    .expect("a ticket signs");
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(stored(&jobs, SIGNED).await.sign_offs.is_empty());
}
