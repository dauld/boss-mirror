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

/// DECIDED, AND PINNED: re-sending a ticket to the sign-off door writes
/// no second stamp and does not move the first one's time. The door's
/// idempotence is keyed on (role, current shape hash), so a re-send of
/// the same ticket — or a second ceremony over the same content — is
/// answered with the stamp already on the record. A new stamp needs new
/// CONTENT (the old one goes stale), never merely a new ticket.
#[tokio::test]
async fn re_sending_a_ticket_to_the_sign_off_door_does_not_re_stamp() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let first = stored(&jobs, SIGNED).await.sign_offs;
    assert_eq!(first.len(), 1);

    // The same ticket again, then a second ceremony's ticket.
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (step_id, shape_hash) = binding(SIGNED);
    let second = PresenceTicket {
        i: "emp-david".into(),
        s: step_id,
        h: shape_hash,
        n: "ceremony-nonce-8".into(),
        e: now_epoch() + 60,
    }
    .encode(GATEWAY_KEY)
    .expect("a ticket signs");
    let (status, body) = sign_off(&app, Some(&second)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let after = stored(&jobs, SIGNED).await.sign_offs;
    assert_eq!(
        after, first,
        "the first stamp stands: same time, same nonce"
    );
}
