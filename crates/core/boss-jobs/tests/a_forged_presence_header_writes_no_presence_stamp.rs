//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

/// THE BUG on the sign-off door: the forged header wrote a Presence
/// stamp naming emp-david onto the record.
#[tokio::test]
async fn a_forged_presence_header_writes_no_presence_stamp() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let (status, body) = sign_off(&app, Some(&forged_json(SIGNED))).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a forged header must be refused at the sign-off door; body: {body}"
    );
    assert!(
        stored(&jobs, SIGNED).await.sign_offs.is_empty(),
        "a refused forgery must leave no stamp on the record"
    );
}
