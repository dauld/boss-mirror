//! The locations batch against the real table (backlog 1ec8312a,
//! 2026-09-17): insert-if-absent by id, so a tenant's publish is
//! idempotent and never clobbers an operator's edit; a parent named
//! later in the same batch still lands, because the self-FK is
//! DEFERRABLE and the batch is one transaction.

use boss_core::primitives::Location;
use boss_locations::port::LocationRepository;
use boss_locations::postgres::PgLocations;
use boss_testing::TestDb;

fn row(id: &str, name: &str, parent_id: Option<&str>) -> Location {
    Location {
        id: id.to_string(),
        name: name.to_string(),
        kind: "office".to_string(),
        parent_id: parent_id.map(str::to_string),
        timezone: "America/Los_Angeles".to_string(),
        latitude: None,
        longitude: None,
        address: None,
        account_id: None,
        metadata: serde_json::json!({}),
        retired_at: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_inserts_if_absent_and_a_second_run_keeps_the_first_rows() {
    let db = TestDb::new().await;
    let repo = PgLocations::new(db.pool.clone());

    // The child is listed BEFORE its parent: one transaction, the
    // deferred FK, both land.
    let inserted = repo
        .batch_upsert(&[
            row("loc-t-desk", "Desk", Some("loc-t-hq")),
            row("loc-t-hq", "HQ", None),
        ])
        .await
        .expect("the batch lands");
    assert_eq!(inserted, 2);
    assert!(repo.exists_active("loc-t-hq").await.unwrap());
    assert!(repo.exists_active("loc-t-desk").await.unwrap());

    // A re-run with one edited name and one new row: the edit is NOT
    // applied (insert-if-absent), the new row is.
    let inserted = repo
        .batch_upsert(&[
            row("loc-t-hq", "HQ renamed", None),
            row("loc-t-lab", "Lab", Some("loc-t-hq")),
        ])
        .await
        .expect("the second batch lands");
    assert_eq!(inserted, 1, "only the new row counts");
    let hq = repo.get("loc-t-hq").await.unwrap().expect("hq stays");
    assert_eq!(hq.name, "HQ", "the existing row is left untouched");
    let kids = repo.children_of(Some("loc-t-hq")).await.unwrap();
    let ids: Vec<&str> = kids.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["loc-t-desk", "loc-t-lab"]);
}
