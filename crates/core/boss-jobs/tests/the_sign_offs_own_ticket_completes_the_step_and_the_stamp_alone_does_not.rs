//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// THE WEB'S SEQUENCE, on the step that has both doors (backlog
/// b568044a, round-4 review of car 7abc0154, 2026-09-25). A surface
/// stamps with the ceremony's ticket and then completes with a PUT. The
/// stamp is not the completion's assurance: the PUT is judged on its
/// OWN request, so a PUT carrying no ticket stays refused after a
/// genuine presence stamp — which is why both web surfaces stopped at
/// the stamp. The ticket the sign-off was granted on is the one the
/// completion carries: same step, same person, same shape (the stamp
/// writes no metadata, so the shape it bound still stands), and nothing
/// wider — the binding and the expiry are checked again here.
#[tokio::test]
async fn the_sign_offs_own_ticket_completes_the_step_and_the_stamp_alone_does_not() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        stored(&jobs, SIGNED).await.sign_offs[0].assurance,
        Assurance::Presence
    );

    // The stamp alone: a presence stamp on the step does not lend its
    // assurance to a ticketless completion.
    let (status, body) = complete(&app, SIGNED, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(
        body.contains("\"required\":\"presence\""),
        "the refusal names what it wants, so a surface can answer it: {body}"
    );
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Ready);

    // The same ticket, carried on the completion: it completes.
    let (status, body) = complete(&app, SIGNED, Some(&t)).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Completed);
}
