//! The chart-of-accounts declare path against the real adapter
//! (backlog 41af5195): insert-if-absent by code, a starter code kept
//! with the difference named, the fact staged on the outbox INSIDE the
//! insert's transaction, and a refused batch leaving nothing behind.

use boss_core::publisher::EventStamp;
use boss_ledger::chart::{ACCOUNT_DECLARED, AccountInput, KeptAccount, declare_accounts};
use boss_ledger::error::LedgerError;
use boss_testing::TestDb;

fn stamp() -> EventStamp {
    EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("tenant-seed".into()),
    )
}

fn row(code: &str, name: &str, kind: &str, nb: &str, parent: Option<&str>) -> AccountInput {
    AccountInput {
        code: code.into(),
        name: name.into(),
        kind: kind.into(),
        normal_balance: nb.into(),
        parent: parent.map(str::to_string),
    }
}

async fn staged(db: &TestDb) -> Vec<(String, serde_json::Value)> {
    sqlx::query_as(
        "SELECT source, payload FROM event_outbox WHERE kind = $1 ORDER BY payload->>'code'",
    )
    .bind(ACCOUNT_DECLARED)
    .fetch_all(&db.pool)
    .await
    .expect("outbox reads")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_code_lands_with_its_fact_a_starter_code_is_kept_and_a_rerun_stages_nothing() {
    let db = TestDb::new().await;
    // The real tenant's chart meets the starter's: 1000, 3000 and 6200
    // are starter codes (Cash, Retained Earnings, Operating Expense —
    // Rent) and are KEPT under those names; 3100 and 6210 are new, and
    // 6210 hangs off a KEPT parent — a parent need not be inserted by
    // this batch, only declared before its child.
    let chart = [
        row("1000", "Bank", "asset", "debit", None),
        row("3000", "Owner's equity", "equity", "credit", None),
        row("3100", "Retained earnings", "equity", "credit", None),
        row("6200", "Infrastructure", "expense", "debit", None),
        row(
            "6210",
            "Infrastructure — cluster",
            "expense",
            "debit",
            Some("6200"),
        ),
    ];
    let out = declare_accounts(&db.pool, &chart, &stamp())
        .await
        .expect("the chart lands");
    assert_eq!(out.received, 5);
    assert_eq!(out.inserted, 2);
    assert_eq!(
        out.kept,
        vec![
            KeptAccount {
                code: "1000".into(),
                differs: vec!["name".into()],
            },
            KeptAccount {
                code: "3000".into(),
                differs: vec!["name".into()],
            },
            KeptAccount {
                code: "6200".into(),
                differs: vec!["name".into()],
            },
        ],
        "the starter's rows are kept, each naming the field the declaration differs on"
    );
    let (parent_code,): (String,) = sqlx::query_as(
        "SELECT p.code FROM gl_accounts a JOIN gl_accounts p ON p.id = a.parent_id \
         WHERE a.code = '6210'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        parent_code, "6200",
        "the child resolved to the kept starter row"
    );
    let s = staged(&db).await;
    assert_eq!(s.len(), 2, "{s:?}");
    assert_eq!(s[0].0, "ledger");
    assert_eq!(s[0].1["code"], "3100");
    assert_eq!(s[0].1["kind"], "equity");
    assert_eq!(s[0].1["declared_by"], "automation:tenant-seed");
    assert!(
        s[0].1["id"].as_str().is_some(),
        "the minted id rides the fact"
    );
    assert_eq!(s[1].1["code"], "6210");
    assert_eq!(s[1].1["parent"], "6200");

    let again = declare_accounts(&db.pool, &chart, &stamp()).await.unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.kept.len(), 5);
    assert!(
        again
            .kept
            .iter()
            .filter(|k| k.code == "3100" || k.code == "6210")
            .all(|k| k.differs.is_empty()),
        "{:?}",
        again.kept
    );
    assert_eq!(staged(&db).await.len(), 2, "a kept row records nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_row_refuses_the_whole_batch_before_any_write() {
    let db = TestDb::new().await;
    let err = declare_accounts(
        &db.pool,
        &[
            row("4300", "Hosting revenue", "revenue", "credit", None),
            row("4310", "Managed", "revenue", "credit", Some("4400")),
        ],
        &stamp(),
    )
    .await
    .expect_err("an undeclared parent is refused");
    match err {
        LedgerError::InvalidChart(why) => {
            assert!(
                why.contains("account #2 (4310)") && why.contains("parent `4400`"),
                "{why}"
            )
        }
        other => panic!("expected InvalidChart, got {other:?}"),
    }
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM gl_accounts WHERE code IN ('4300','4310')")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 0);
    assert!(staged(&db).await.is_empty());
}
