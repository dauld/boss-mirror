//! Which period owns a journal entry — and the two paths agreeing on it.
//!
//! The ledger is a projection of the fact log, so replaying the log
//! must reproduce the live ledger exactly. It did not: a year-end
//! close posted live landed in the **yearly** period (deliberately —
//! `insert_closing_entry` exists for that), while replaying the same
//! fact ran through `ensure_period_for`, which knew only about months
//! and filed it under December.
//!
//! Nothing was wrong with the money. The nightly deep replay-check
//! reported `fact_divergences=0, entry_divergences=1`, and the one
//! entry was "only-in-replay" — the live copy existed, in a different
//! period. Live excluded it from the comparison because the year was
//! locked; replay included it because December was open. The first
//! year-end close ever posted was what surfaced it.
//!
//! The rule now lives in one place, `owning_period_kind`, which both
//! paths consult. These tests pin that, and pin the SQL copy of it
//! against the Rust one.

use boss_ledger::periods::lock_period;
use boss_ledger::postgres::{OPEN_PERIOD_FACTS_SQL, PERIOD_CLOSED_FACT, owning_period_kind};
use boss_ledger::{FactRef, post_fact_in_tx};
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

fn dec31(year: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, 12, 31).expect("Dec 31")
}

fn close_payload(period_id: Uuid, period_end: NaiveDate) -> Value {
    json!({
        "period_id": period_id,
        "period_end": period_end,
        "retained_earnings_account": "3000",
        "revenue_lines": [ { "account_code": "4100", "balance_cents": 70_000 } ],
        "expense_lines": [ { "account_code": "6100", "balance_cents": 30_000 } ],
    })
}

/// Insert a fact and project it, returning the fact id.
async fn seed_and_post(db: &TestDb, kind: &str, happened_on: NaiveDate, payload: &Value) -> Uuid {
    let id = Uuid::new_v4();
    let mut tx = db.pool.begin().await.expect("begin");
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'test', $5, 'test')",
    )
    .bind(id)
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(id.to_string())
    .execute(&mut *tx)
    .await
    .expect("seed fact");

    let fact = FactRef {
        id,
        kind,
        happened_on,
        payload,
    };
    post_fact_in_tx(&mut tx, &fact).await.expect("post fact");
    tx.commit().await.expect("commit");
    id
}

async fn period_of_entry(db: &TestDb, fact_id: Uuid) -> (String, String) {
    let row = sqlx::query(
        "SELECT p.kind, p.status FROM gl_journal_entries e \
         JOIN gl_periods p ON p.id = e.period_id \
         WHERE e.fact_id = $1",
    )
    .bind(fact_id)
    .fetch_one(&db.pool)
    .await
    .expect("entry exists with a period");
    (row.get("kind"), row.get("status"))
}

/// The heart of it. Replaying a close through the ordinary projection
/// path must file it where the live close filed it: the year.
#[tokio::test]
async fn a_year_end_close_is_owned_by_the_year_not_december() {
    let db = TestDb::new().await;
    let year_period = Uuid::new_v4();
    let fact = seed_and_post(
        &db,
        PERIOD_CLOSED_FACT,
        dec31(2025),
        &close_payload(year_period, dec31(2025)),
    )
    .await;

    let (kind, _) = period_of_entry(&db, fact).await;
    assert_eq!(
        kind, "year",
        "a close dated Dec 31 belongs to the year it closes — filing it in \
         December puts the entry that zeroes December's revenue inside \
         December's own balances"
    );
}

/// The other half: nothing else moved. An ordinary posting on the same
/// date still belongs to its month.
#[tokio::test]
async fn an_ordinary_posting_on_dec_31_still_belongs_to_december() {
    let db = TestDb::new().await;
    let fact = seed_and_post(
        &db,
        "finance.invoice.issued",
        dec31(2025),
        &json!({
            "invoice_id": "inv-dec-31",
            "line_items": [{ "category": "distribution", "amount_cents": 50_000 }],
        }),
    )
    .await;

    let (kind, _) = period_of_entry(&db, fact).await;
    assert_eq!(kind, "month");
}

/// Locked periods are immutable, and that has to hold through the rule
/// change: once the year is locked, its close must not re-project.
/// Before the fix the re-post filter asked whether DECEMBER was open,
/// so a locked year's close re-posted anyway — into the wrong period,
/// which is exactly the divergence the nightly check reported.
#[tokio::test]
async fn a_locked_years_close_does_not_re_project() {
    let db = TestDb::new().await;
    let year_period = Uuid::new_v4();
    let fact = seed_and_post(
        &db,
        PERIOD_CLOSED_FACT,
        dec31(2025),
        &close_payload(year_period, dec31(2025)),
    )
    .await;

    let (_, status) = period_of_entry(&db, fact).await;
    assert_eq!(status, "open", "precondition: the year starts open");

    // Lock the year the entry actually landed in.
    let year_id: Uuid = sqlx::query_scalar(
        "SELECT p.id FROM gl_journal_entries e JOIN gl_periods p ON p.id = e.period_id \
         WHERE e.fact_id = $1",
    )
    .bind(fact)
    .fetch_one(&db.pool)
    .await
    .expect("year period");
    let stamp = boss_core::publisher::EventStamp::new(
        "ledger",
        boss_core::actor::ActorId::Automation("test".into()),
    )
    .with_timestamp(chrono::Utc::now());
    lock_period(&db.pool, year_id, "test", &stamp, "test")
        .await
        .expect("lock the year");

    let replayed: Vec<Uuid> = sqlx::query(OPEN_PERIOD_FACTS_SQL)
        .bind(PERIOD_CLOSED_FACT)
        .fetch_all(&db.pool)
        .await
        .expect("select re-projectable facts")
        .iter()
        .map(|r| r.get::<Uuid, _>("id"))
        .collect();

    assert!(
        !replayed.contains(&fact),
        "a close whose year is locked must not re-project — the month it is \
         dated in being open is not the question"
    );
}

/// §9a: the rule lives in Rust and in the SQL CASE, and they cannot be
/// collapsed into one expression. So they get an equality test — one
/// that names the offending kind when it drifts.
#[tokio::test]
async fn rust_and_sql_agree() {
    let db = TestDb::new().await;
    for kind in [
        PERIOD_CLOSED_FACT,
        "finance.invoice.issued",
        "finance.payment.received",
        "finance.payroll.run",
    ] {
        let from_sql: String =
            sqlx::query_scalar("SELECT CASE WHEN $1 = $2 THEN 'year' ELSE 'month' END")
                .bind(kind)
                .bind(PERIOD_CLOSED_FACT)
                .fetch_one(&db.pool)
                .await
                .expect("evaluate the SQL rule");
        assert_eq!(
            from_sql,
            owning_period_kind(kind),
            "Rust and SQL disagree on which period owns `{kind}` — \
             OPEN_PERIOD_FACTS_SQL and owning_period_kind have drifted"
        );
    }
}
