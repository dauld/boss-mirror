//! The accounts a tax fact posts to are the ones the instance's
//! `tax_kinds` row names — one definition, the tenant's (backlog
//! e021be29, 2026-09-19, for the liability; c0b83e13, 2026-09-22, for
//! the expense side).
//!
//! WHAT WAS MEASURED. The kind → liability-account map lived twice in
//! the ledger and once more in code: `seeds/tax_liability_accounts.toml`,
//! bundled by `rules.rs` with a `BOSS_LEDGER_TAX_LIABILITY_ACCOUNTS_TOML`
//! override that fell back to the bundle in silence on an unreadable
//! file, and read by NOTHING (`liability_account_for` had no caller);
//! `tax_kinds.liability_account`, the row the tenant declares through
//! `POST /api/ledger/tax/batch` (7f163e58) and the door stamps onto every
//! filing; and a `matches!("2150" | "2300" | "2310" | "2320")` inside
//! the two posting rules — the demo tenant's accounts, so a tenant kind
//! against any other account (`gross-receipts` → 2900, the shape the
//! batch test already registers) was refused as "not allowed" at the
//! post. The bundle and its override are gone; the posting path reads
//! the row instead; a kind with no row cannot post, by name.
//!
//! THE EXPENSE SIDE WAS THE SAME SHAPE (c0b83e13). `tax_accrued` kept
//! its own `matches!("6400" | "6500" | "6550")`, so a tenant accruing
//! against its own expense account was refused at the post while a
//! mis-stamped fact naming one of those three was waved through. It is
//! gone too: with a `kind` in the payload both accounts must be the
//! ones that kind's row names, and a kind whose row names no expense
//! account cannot accrue at all.

use boss_ledger::{FactRef, LedgerError, post_fact_in_tx};
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::{Value, json};
use uuid::Uuid;

/// Insert a fact row and post it, returning the posting's own verdict.
async fn post(db: &TestDb, kind: &str, payload: &Value) -> Result<Uuid, LedgerError> {
    let fact_id = Uuid::new_v4();
    let happened_on = NaiveDate::from_ymd_opt(2026, 4, 30).unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'tax_filings', $5, 'test')",
    )
    .bind(fact_id)
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(fact_id.to_string())
    .execute(&mut *tx)
    .await
    .unwrap();
    let fact = FactRef {
        id: fact_id,
        kind,
        happened_on,
        payload,
    };
    post_fact_in_tx(&mut tx, &fact).await?;
    tx.commit().await.unwrap();
    Ok(fact_id)
}

/// A tenant's own kind against a tenant's own account — neither in the
/// demo regime the migration seeds.
async fn declare_gross_receipts_on_2900(db: &TestDb) {
    sqlx::query(
        "INSERT INTO gl_accounts (id, code, name, kind, normal_side) \
         VALUES ($1, '2900', 'Gross receipts tax payable', 'liability', 'credit')",
    )
    .bind(Uuid::new_v4())
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tax_kinds (kind, liability_account) VALUES ('gross-receipts', '2900')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn line_debit(db: &TestDb, fact_id: Uuid, code: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(l.debit_cents), 0)::bigint FROM gl_journal_lines l \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE e.fact_id = $1 AND a.code = $2",
    )
    .bind(fact_id)
    .bind(code)
    .fetch_one(&db.pool)
    .await
    .unwrap()
}

/// Cash on hand, so the remittance's CR 1000 clears the cash floor.
async fn fund_cash(db: &TestDb) {
    post(
        db,
        "finance.invoice.paid",
        &json!({"invoice_id": "inv-funding", "amount_cents": 1_000_000}),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remittance_drains_the_account_the_kinds_row_names() {
    let db = TestDb::new().await;
    declare_gross_receipts_on_2900(&db).await;
    fund_cash(&db).await;
    let fact_id = post(
        &db,
        "finance.tax.remitted",
        &json!({
            "filing_id": "tf-gross-receipts-US-DE-2026-Q1",
            "kind": "gross-receipts",
            "jurisdiction": "US-DE",
            "liability_account": "2900",
            "amount_cents": 40_000,
            "period_start": "2026-01-01",
            "period_end": "2026-03-31",
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("the tenant's kind posts to the tenant's account: {e}"));
    assert_eq!(line_debit(&db, fact_id, "2900").await, 40_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_accrual_credits_the_account_the_kinds_row_names() {
    // Both accounts are the tenant's own, and neither is one the demo
    // regime knows — the expense side joined the liability side in
    // c0b83e13, when the `matches!` allowlist carrying three demo codes
    // came out of rules.rs.
    let db = TestDb::new().await;
    declare_gross_receipts_on_2900(&db).await;
    declare_gross_receipts_expense_on_6910(&db).await;
    let fact_id = post(
        &db,
        "finance.tax.accrued",
        &json!({
            "filing_id": "tf-gross-receipts-US-DE-2026-Q1",
            "kind": "gross-receipts",
            "jurisdiction": "US-DE",
            "expense_account": "6910",
            "liability_account": "2900",
            "amount_cents": 40_000,
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("the tenant's kind accrues on the tenant's accounts: {e}"));
    assert_eq!(line_debit(&db, fact_id, "6910").await, 40_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_with_no_row_cannot_post_and_the_refusal_names_it() {
    let db = TestDb::new().await;
    let err = post(
        &db,
        "finance.tax.remitted",
        &json!({
            "filing_id": "tf-franchise-US-DE-2026-Q1",
            "kind": "franchise",
            "jurisdiction": "US-DE",
            "liability_account": "2300",
            "amount_cents": 100,
            "period_start": "2026-01-01",
            "period_end": "2026-03-31",
        }),
    )
    .await
    .expect_err("a kind the instance does not hold cannot post");
    assert!(
        matches!(&err, LedgerError::TaxKindNotRegistered { .. }),
        "a named refusal, not a payload error: {err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("franchise"), "{text}");
    assert!(text.contains("tax_kinds"), "{text}");
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_journal_entries")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(entries, 0, "a refused post leaves no entry");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fact_naming_another_account_than_its_row_is_refused_by_both_names() {
    // The migration's `sales` row names 2300; a fact claiming 2310 for
    // it is a mis-stamped emitter, not a tenant choice.
    let db = TestDb::new().await;
    let err = post(
        &db,
        "finance.tax.remitted",
        &json!({
            "filing_id": "tf-sales-US-CA-2026-03",
            "kind": "sales",
            "jurisdiction": "US-CA",
            "liability_account": "2310",
            "amount_cents": 100,
            "period_start": "2026-03-01",
            "period_end": "2026-03-31",
        }),
    )
    .await
    .expect_err("an account other than the row's cannot post");
    assert!(
        matches!(&err, LedgerError::TaxKindNotRegistered { .. }),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("2310") && text.contains("2300"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kindless_accrual_posts_only_to_an_account_some_row_names() {
    // The standalone accrual door (POST /api/ledger/tax-accruals, the
    // excise dispatcher rule) stamps no `kind`: its account must still
    // be one a tax_kinds row on this instance names as a liability.
    let db = TestDb::new().await;
    let accrual = |liability: &str| {
        json!({
            "accrual_id": format!("excise-step-{liability}"),
            "jurisdiction": "US-FEDERAL",
            "expense_account": "6550",
            "liability_account": liability,
            "amount_cents": 3_500,
            "posted_on": "2026-04-30",
        })
    };
    post(&db, "finance.tax.accrued", &accrual("2320"))
        .await
        .unwrap_or_else(|e| panic!("the migration's excise row names 2320: {e}"));
    let err = post(&db, "finance.tax.accrued", &accrual("1000"))
        .await
        .expect_err("1000 Cash is no tax kind's liability account");
    assert!(
        matches!(&err, LedgerError::TaxKindNotRegistered { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("1000"), "{err}");
}

// --- the expense account is the row's too (backlog c0b83e13) ---------------

/// A tenant kind that ACCRUES: its row names both accounts, and neither
/// is one the demo regime knows.
async fn declare_gross_receipts_expense_on_6910(db: &TestDb) {
    sqlx::query(
        "INSERT INTO gl_accounts (id, code, name, kind, normal_side) \
         VALUES ($1, '6910', 'Gross receipts tax expense', 'expense', 'debit')",
    )
    .bind(Uuid::new_v4())
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tax_kinds SET expense_account = '6910' WHERE kind = 'gross-receipts'")
        .execute(&db.pool)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_accrual_naming_another_expense_than_its_row_is_refused_by_both_names() {
    // The demo `income` row names 6500; a fact claiming 6550 for it is a
    // mis-stamped emitter. 6550 passed the old allowlist unread.
    let db = TestDb::new().await;
    let err = post(
        &db,
        "finance.tax.accrued",
        &json!({
            "filing_id": "tf-income-US-FEDERAL-2026-Q1",
            "kind": "income",
            "jurisdiction": "US-FEDERAL",
            "expense_account": "6550",
            "liability_account": "2310",
            "amount_cents": 40_000,
        }),
    )
    .await
    .expect_err("an expense account other than the row's cannot post");
    assert!(
        matches!(&err, LedgerError::TaxKindNotRegistered { .. }),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("6550") && text.contains("6500"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_that_names_no_expense_account_cannot_accrue() {
    // `sales` drains a liability built up per invoice: its row names no
    // expense account, so an accrual against it is a mis-stamped fact,
    // refused naming the kind rather than debiting whatever it asked for.
    let db = TestDb::new().await;
    let err = post(
        &db,
        "finance.tax.accrued",
        &json!({
            "filing_id": "tf-sales-US-CA-2026-03",
            "kind": "sales",
            "jurisdiction": "US-CA",
            "expense_account": "6500",
            "liability_account": "2300",
            "amount_cents": 40_000,
        }),
    )
    .await
    .expect_err("a kind with no expense account cannot accrue");
    assert!(
        matches!(&err, LedgerError::TaxKindNotRegistered { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("sales"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_excise_row_names_the_expense_account_the_brewery_accrues_against() {
    // The excise rule's args carried 6550 / 2320 until c0b83e13; the row
    // is the one definition now, so it must name both.
    let db = TestDb::new().await;
    let (liability, expense): (String, Option<String>) = sqlx::query_as(
        "SELECT liability_account, expense_account FROM tax_kinds WHERE kind = 'excise'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(liability, "2320");
    assert_eq!(expense.as_deref(), Some("6550"));
}
