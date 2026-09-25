//! `PgDepartments::publish` on a real database — the departments
//! registry's one write (backlog 7edf0e97, car 0 of design 8c3e9599).
//!
//! Measured before it existed (2026-09-25): the 13 rows migration
//! 20260919181324 seeds into every instance could not be changed —
//! no POST, no PUT, and the only write in the tree was a test's raw
//! SQL. These pin what the in-memory double cannot: the row, its
//! `department` Subject identity row and its fact commit together; a
//! retirement is stamped once and kept; a code the tenant does not
//! declare is left alone.

use boss_core::actor::ActorId;
use boss_core::publish::PublishMode;
use boss_core::publisher::EventStamp;
use boss_jobs::department::declare::DepartmentInput;
use boss_jobs::department::registry::{DepartmentRegistry, PgDepartments};
use boss_testing::TestDb;
use serde_json::Value;

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("tenant-seed".into()))
}

fn declared(code: &str, label: &str, function: &str, sort_order: i32) -> DepartmentInput {
    DepartmentInput {
        code: code.into(),
        display_name: label.into(),
        function: function.into(),
        sort_order,
        retired: false,
    }
}

async fn live(db: &TestDb) -> Vec<String> {
    PgDepartments::new(db.pool.clone())
        .list()
        .await
        .expect("the registry answers")
        .into_iter()
        .map(|d| d.code)
        .collect()
}

async fn facts(db: &TestDb, kind: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id")
        .bind(kind)
        .fetch_all(&db.pool)
        .await
        .expect("outbox read")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_roster_lands_with_its_identity_and_its_facts() {
    let db = TestDb::new().await;
    let registry = PgDepartments::new(db.pool.clone());
    let seeded = live(&db).await;
    assert!(
        seeded.contains(&"warehouse".to_string()) && !seeded.contains(&"product".to_string()),
        "the migration's roster is the control: {seeded:?}"
    );

    let out = registry
        .publish(
            &[
                declared("product", "Product", "operations", 5),
                declared("it", "IT", "operations", 1),
                declared("sales", "Sales (renamed)", "revenue", 40),
            ],
            PublishMode::InsertIfAbsent,
            &stamp(),
        )
        .await
        .expect("publish");
    assert_eq!(out.received, 3);
    assert_eq!(out.inserted, 1, "{out:?}");
    assert_eq!(
        out.unchanged, 1,
        "it is as the migration seeded it: {out:?}"
    );
    assert_eq!(out.kept.len(), 1, "{out:?}");
    assert_eq!(out.kept[0].id, "sales");
    assert_eq!(out.kept[0].differs, ["display_name"]);
    assert!(live(&db).await.contains(&"product".to_string()));

    let identity: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM subjects WHERE kind = 'department' AND id = 'product'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("subjects read");
    assert_eq!(identity, 1, "a Job may be about the new department");
    let declared_facts = facts(&db, "department.declared").await;
    assert_eq!(declared_facts.len(), 1, "one fact per INSERTED row");
    assert_eq!(declared_facts[0]["code"], "product");
    assert_eq!(declared_facts[0]["declared_by"], "automation:tenant-seed");
    let label: String = sqlx::query_scalar("SELECT label FROM departments WHERE id = 'sales'")
        .fetch_one(&db.pool)
        .await
        .expect("label read");
    assert_eq!(label, "Sales", "the default keeps the instance's row");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_take_retires_once_and_leaves_an_undeclared_code_alone() {
    let db = TestDb::new().await;
    let registry = PgDepartments::new(db.pool.clone());
    let mut warehouse = declared("warehouse", "Warehouse", "operations", 100);
    warehouse.retired = true;

    let out = registry
        .publish(
            std::slice::from_ref(&warehouse),
            PublishMode::Take,
            &stamp(),
        )
        .await
        .expect("take");
    assert_eq!(out.updated.len(), 1, "{out:?}");
    assert_eq!(out.updated[0].id, "warehouse");
    let codes = live(&db).await;
    assert!(!codes.contains(&"warehouse".to_string()), "{codes:?}");
    assert!(
        codes.contains(&"distribution".to_string()),
        "a row the declaration does not name is never touched: {codes:?}"
    );
    let first: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT retired_at FROM departments WHERE id = 'warehouse'")
            .fetch_one(&db.pool)
            .await
            .expect("stamped");

    let again = registry
        .publish(
            std::slice::from_ref(&warehouse),
            PublishMode::Take,
            &stamp(),
        )
        .await
        .expect("take again");
    assert_eq!((again.updated.len(), again.unchanged), (0, 1));
    let second: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT retired_at FROM departments WHERE id = 'warehouse'")
            .fetch_one(&db.pool)
            .await
            .expect("still stamped");
    assert_eq!(first, second, "when it was withdrawn is a fact");
    let updated = facts(&db, "department.updated").await;
    assert_eq!(updated.len(), 1, "a no-op take records nothing");
    assert_eq!(updated[0]["changes"][0]["field"], "retired");
    assert_eq!(updated[0]["updated_by"], "automation:tenant-seed");

    // Un-retiring clears the stamp: the row is live again.
    let revived = declared("warehouse", "Warehouse", "operations", 100);
    registry
        .publish(&[revived], PublishMode::Take, &stamp())
        .await
        .expect("revive");
    assert!(live(&db).await.contains(&"warehouse".to_string()));
}
