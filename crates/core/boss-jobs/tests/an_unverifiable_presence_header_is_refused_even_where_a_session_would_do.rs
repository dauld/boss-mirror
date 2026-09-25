//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// Refused, not quietly downgraded: a claim of presence the API cannot
/// verify is refused even on a step that asks only for a session, and
/// the refusal says why — silence is the one failure this door may not
/// have.
#[tokio::test]
async fn an_unverifiable_presence_header_is_refused_even_where_a_session_would_do() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let (status, body) = complete(&app, ORDINARY, Some(&forged_json(ORDINARY))).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(
        body.contains("did not verify"),
        "the refusal must name the unverified header: {body}"
    );
    assert_eq!(stored(&jobs, ORDINARY).await.status, StepStatus::Ready);

    // The control: with no header at all the same write goes through,
    // so the refusal above is the header's and nothing else's.
    let (status, body) = complete(&app, ORDINARY, None).await;
    assert!(status.is_success(), "{status}: {body}");
}
