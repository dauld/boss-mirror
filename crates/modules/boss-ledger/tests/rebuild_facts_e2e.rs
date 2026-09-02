//! End-to-end tests for `boss-ledger::rebuild_facts`.
//!
//! Synthesizes audit_log events that match the seeded projection
//! rules, runs the projection, and asserts financial_facts ends up
//! with the right shape. Idempotency check: a second run produces no
//! new rows.

use boss_ledger::rebuild_facts;
use boss_testing::TestDb;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

async fn insert_audit_event(
    db: &TestDb,
    kind: &str,
    timestamp: DateTime<Utc>,
    source: &str,
    payload: &Value,
) -> Uuid {
    let event_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(event_id)
    .bind(timestamp)
    .bind(source)
    .bind(kind)
    .bind(payload)
    .execute(&db.pool)
    .await
    .unwrap();
    event_id
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_commerce_invoice_created_to_finance_invoice_issued() {
    let db = TestDb::new().await;

    let payload = serde_json::json!({
        "id": "inv-001",
        "issued_on": "2026-04-01",
        "amount_cents": 50000,
        "account_id": "acct-A",
        "currency": "USD",
        "line_items": [
            {"description": "Setup fee", "amount_cents": 50000, "category": "service-revenue"},
        ],
    });
    insert_audit_event(
        &db,
        "commerce.invoice.created",
        "2026-04-01T12:00:00Z".parse().unwrap(),
        "commerce",
        &payload,
    )
    .await;

    let report = rebuild_facts(&db.pool).await.unwrap();
    assert!(report.rules_loaded >= 12, "registry seeded with 12+ rules");
    assert_eq!(report.events_scanned, 1);
    assert_eq!(report.facts_written, 1);

    let row = sqlx::query(
        "SELECT kind, happened_on, source_table, source_id, payload \
         FROM financial_facts WHERE source_id = 'inv-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let kind: String = row.get("kind");
    let source_table: String = row.get("source_table");
    let happened_on: chrono::NaiveDate = row.get("happened_on");
    let stored_payload: Value = row.get("payload");
    assert_eq!(kind, "finance.invoice.issued");
    assert_eq!(source_table, "invoices");
    assert_eq!(happened_on.to_string(), "2026-04-01");
    assert_eq!(
        stored_payload, payload,
        "fact payload is event payload verbatim"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_is_idempotent() {
    let db = TestDb::new().await;

    let payload = serde_json::json!({
        "id": "inv-002",
        "issued_on": "2026-04-02",
        "amount_cents": 12345,
    });
    insert_audit_event(
        &db,
        "commerce.invoice.created",
        "2026-04-02T09:00:00Z".parse().unwrap(),
        "commerce",
        &payload,
    )
    .await;

    let r1 = rebuild_facts(&db.pool).await.unwrap();
    let r2 = rebuild_facts(&db.pool).await.unwrap();

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM financial_facts WHERE source_id = 'inv-002'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 1, "second rebuild produces no new rows");
    assert_eq!(r1.facts_written, 1);
    assert_eq!(
        r2.facts_written, 1,
        "facts_written counts attempted writes — natural-key dedup absorbs them"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unrelated_audit_events_are_ignored() {
    let db = TestDb::new().await;

    insert_audit_event(
        &db,
        "jobs.job.created",
        "2026-04-03T10:00:00Z".parse().unwrap(),
        "jobs",
        &serde_json::json!({"id": "job-1"}),
    )
    .await;

    let report = rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(
        report.events_scanned, 0,
        "kind not in registry → not scanned"
    );
    assert_eq!(report.facts_written, 0);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM financial_facts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_event_is_skipped_not_failed() {
    let db = TestDb::new().await;

    // Missing "/issued_on" — registry rule for commerce.invoice.created
    // requires it. Without it the projection skips, but rebuild keeps going.
    insert_audit_event(
        &db,
        "commerce.invoice.created",
        "2026-04-04T08:00:00Z".parse().unwrap(),
        "commerce",
        &serde_json::json!({"id": "inv-malformed"}),
    )
    .await;
    insert_audit_event(
        &db,
        "commerce.invoice.created",
        "2026-04-04T09:00:00Z".parse().unwrap(),
        "commerce",
        &serde_json::json!({"id": "inv-good", "issued_on": "2026-04-04"}),
    )
    .await;

    let report = rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(report.events_scanned, 2);
    assert_eq!(report.facts_written, 1);
    assert_eq!(report.events_skipped_missing_field, 1);

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM financial_facts WHERE source_id = 'inv-good'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn invoice_created_tax_lines_survive_rebuild() {
    // Regression: the live `finance.invoice.issued` fact carries a
    // `tax_lines` array (→ CR 2300). The rebuild copies the
    // `commerce.invoice.created` audit payload verbatim, so that event
    // MUST carry the same `tax_lines` or 2300 stops accruing on rebuild.
    // Commerce's http.rs now injects tax_lines into the audit payload via
    // the shared `events::tax_lines_for` helper; this asserts the
    // projection preserves it onto the reconstructed fact.
    let db = TestDb::new().await;

    let payload = serde_json::json!({
        "id": "inv-tax-001",
        "issued_on": "2026-05-01",
        "amount_cents": 110000,
        "account_id": "acct-tax",
        "currency": "USD",
        "tax_cents": 10000,
        "tax_jurisdiction": "US-CA",
        "line_items": [
            {"description": "Wholesale beer", "amount_cents": 100000, "category": "wholesale"},
        ],
        "tax_lines": [
            {"account": "2300", "jurisdiction": "US-CA", "amount_cents": 10000},
        ],
    });
    insert_audit_event(
        &db,
        "commerce.invoice.created",
        "2026-05-01T12:00:00Z".parse().unwrap(),
        "commerce",
        &payload,
    )
    .await;

    let report = rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(report.facts_written, 1);

    let stored_payload: Value = sqlx::query_scalar(
        "SELECT payload FROM financial_facts \
         WHERE kind = 'finance.invoice.issued' AND source_id = 'inv-tax-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let tax_lines = stored_payload
        .get("tax_lines")
        .and_then(|v| v.as_array())
        .expect("rebuilt invoice-issued fact carries tax_lines");
    assert_eq!(tax_lines.len(), 1);
    assert_eq!(tax_lines[0]["account"], "2300");
    assert_eq!(tax_lines[0]["amount_cents"], 10000);
    assert_eq!(tax_lines[0]["jurisdiction"], "US-CA");
}

#[tokio::test(flavor = "multi_thread")]
async fn standalone_tax_accrual_rebuilds_to_finance_tax_accrued() {
    // Regression: the standalone excise accrual (DR 6550 / CR 2320, no
    // tax_filings row) emits `ledger.tax.accrual.recorded` keyed on
    // `/accrual_id`. The income-tax `ledger.tax.accrued` rule keys on
    // `/filing_id`, which is absent here — so the standalone accrual
    // needs its OWN projection rule or the fact vanishes on rebuild.
    let db = TestDb::new().await;

    let payload = serde_json::json!({
        "accrual_id": "excise-step-77",
        "expense_account": "6550",
        "liability_account": "2320",
        "amount_cents": 35000,
        "posted_on": "2026-05-02",
        "jurisdiction": "US-FED",
    });
    insert_audit_event(
        &db,
        "ledger.tax.accrual.recorded",
        "2026-05-02T08:00:00Z".parse().unwrap(),
        "ledger",
        &payload,
    )
    .await;

    let report = rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(report.facts_written, 1);

    let row = sqlx::query(
        "SELECT kind, source_table, source_id, happened_on, payload \
         FROM financial_facts WHERE source_id = 'excise-step-77'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let kind: String = row.get("kind");
    let source_table: String = row.get("source_table");
    let happened_on: chrono::NaiveDate = row.get("happened_on");
    assert_eq!(kind, "finance.tax.accrued");
    assert_eq!(source_table, "tax_accruals");
    assert_eq!(happened_on.to_string(), "2026-05-02");
    let stored_payload: Value = row.get("payload");
    assert_eq!(stored_payload["liability_account"], "2320");
    assert_eq!(stored_payload["expense_account"], "6550");
}

#[tokio::test(flavor = "multi_thread")]
async fn inventory_item_received_rebuilds_gl_inert_fact() {
    // Goods-receipt log-backed dedup-fact. The receive path emits an
    // `inventory.item.received` audit event carrying the SAME source_id +
    // received_on it wrote into the in-tx `finance.inventory.received`
    // dedup-fact. On rebuild, the dedicated GL-inert pass in rebuild_facts
    // (deliberately OFF the gl_fact_projection_rules registry) reconstructs
    // a byte-identical fact from audit_log alone — AND the fact drives zero
    // journal lines, because it has no RuleSet arm and post_fact_in_tx skips
    // it via is_gl_inert. This is fix #1: financial_facts fully
    // reconstructable from the log + symmetric with consume's
    // INVENTORY_TRANSFERRED, but the receive contributes no GL entry (the
    // DR-1300 rides the idempotent bill-approval path).
    let db = TestDb::new().await;

    let payload = serde_json::json!({
        "source_id": "step-42:ING-MALT-2ROW-50",
        "part_sku": "ING-MALT-2ROW-50",
        "qty": 200,
        "unit_cost_cents": 50,
        "received_on": "2026-06-01",
    });
    insert_audit_event(
        &db,
        "inventory.item.received",
        "2026-06-01T08:00:00Z".parse().unwrap(),
        "inventory",
        &payload,
    )
    .await;

    let report = rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(report.events_scanned, 0, "kind is NOT in the registry pass");
    assert_eq!(
        report.facts_written, 1,
        "the dedicated GL-inert pass reconstructs the dedup-fact"
    );

    // The dedup-fact lands with the exact shape the in-tx
    // `insert_dedup_fact` writes on the live path (so the replay-check's
    // fact-set diff stays clean): kind/source_table/source_id/happened_on/
    // created_by all match, payload verbatim.
    let row = sqlx::query(
        "SELECT kind, source_table, source_id, happened_on, created_by, payload \
         FROM financial_facts WHERE source_id = 'step-42:ING-MALT-2ROW-50'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let kind: String = row.get("kind");
    let source_table: String = row.get("source_table");
    let happened_on: chrono::NaiveDate = row.get("happened_on");
    let created_by: String = row.get("created_by");
    let stored_payload: Value = row.get("payload");
    assert_eq!(kind, "finance.inventory.received");
    assert_eq!(source_table, "inventory_receipt");
    assert_eq!(happened_on.to_string(), "2026-06-01");
    assert_eq!(created_by, "inventory");
    assert_eq!(
        stored_payload, payload,
        "rebuilt dedup-fact payload is the event payload verbatim"
    );

    // GL-inert: the journal rebuild must SUCCEED (not choke on an unknown
    // fact kind) and post ZERO journal lines for this fact.
    let je_report = boss_ledger::rebuild(&db.pool).await.unwrap();
    assert!(
        je_report.is_balanced(),
        "journal rebuild stays balanced with the inert fact present"
    );

    let (fact_id,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM financial_facts \
         WHERE kind = 'finance.inventory.received' \
           AND source_table = 'inventory_receipt' \
           AND source_id = 'step-42:ING-MALT-2ROW-50'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let (je_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM gl_journal_entries WHERE fact_id = $1")
            .bind(fact_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        je_count, 0,
        "finance.inventory.received must drive NO journal line (GL-inert) on rebuild"
    );

    // Reconstruction is idempotent — a second rebuild_facts produces no
    // duplicate dedup-fact (natural-key dedup on (kind, source_table,
    // source_id)).
    rebuild_facts(&db.pool).await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM financial_facts \
         WHERE source_id = 'step-42:ING-MALT-2ROW-50'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(count, 1, "second rebuild reconstructs no duplicate");
}

// There is deliberately no `commerce.invoice.paid` →
// `finance.invoice.paid` projection test: that rule was removed from
// `gl_fact_projection_rules` (see the rationale in schema/40-ledger.sql) because
// auto-projecting it double-credited AR alongside the two-phase
// payment.received/settled pair. The `finance.invoice.paid` fact is
// emitted directly via `record_fact_in_tx`, never auto-projected from
// the audit_log.

#[tokio::test(flavor = "multi_thread")]
async fn projects_keg_deposit_events_to_both_fact_kinds() {
    // The keg-deposit settlement endpoint emits one audit event per
    // fact, payload verbatim (93f936b9). These two registry rows are
    // what let a TRUNCATE-then-replay rebuild reproduce the charge and
    // release facts — and through them the DR 1000 / CR 2400 and
    // DR 2400 / CR 1000 + CR 4150 entries — from audit_log alone.
    let db = TestDb::new().await;

    let charge_payload = serde_json::json!({
        "charge_id": "keg-charge-job-9",
        "job_id": "job-9",
        "account_id": "account-00042",
        "kegs_out": 10,
        "deposit_cents": 30000,
        "shipped_on": "2026-04-01",
    });
    insert_audit_event(
        &db,
        "ledger.keg_deposit.charged",
        "2026-04-15T12:00:00Z".parse().unwrap(),
        "ledger",
        &charge_payload,
    )
    .await;
    let release_payload = serde_json::json!({
        "release_id": "keg-release-job-9",
        "job_id": "job-9",
        "account_id": "account-00042",
        "kegs_out": 10,
        "kegs_returned": 7,
        "kegs_lost": 3,
        "deposit_cents": 30000,
        "returned_on": "2026-04-15",
    });
    insert_audit_event(
        &db,
        "ledger.keg_deposit.released",
        "2026-04-15T12:00:01Z".parse().unwrap(),
        "ledger",
        &release_payload,
    )
    .await;

    let report = rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(report.events_scanned, 2);
    assert_eq!(report.facts_written, 2);

    for (source_id, want_kind, want_date, want_payload) in [
        (
            "keg-charge-job-9",
            "finance.keg_deposit.charged",
            "2026-04-01",
            &charge_payload,
        ),
        (
            "keg-release-job-9",
            "finance.keg_deposit.released",
            "2026-04-15",
            &release_payload,
        ),
    ] {
        let row = sqlx::query(
            "SELECT kind, happened_on, source_table, payload \
             FROM financial_facts WHERE source_id = $1",
        )
        .bind(source_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let kind: String = row.get("kind");
        let source_table: String = row.get("source_table");
        let happened_on: chrono::NaiveDate = row.get("happened_on");
        let stored_payload: Value = row.get("payload");
        assert_eq!(kind, want_kind);
        assert_eq!(source_table, "keg_deposit_settlements");
        // happened_on comes from the payload's own date path
        // (/shipped_on resp. /returned_on), NOT the event timestamp —
        // both legs post at reconciliation but carry their own dates.
        assert_eq!(happened_on.to_string(), want_date);
        assert_eq!(
            &stored_payload, want_payload,
            "fact payload is event payload verbatim"
        );
    }
}
