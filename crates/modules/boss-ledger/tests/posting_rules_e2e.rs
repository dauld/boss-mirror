//! Posting rules are registry data (backlog a40541cb): a tenant's
//! `[[posting_rule]]` lands through `POST /api/ledger/posting-rules/batch`,
//! the posting path evaluates a fact of that kind by the registry row
//! and the code rules for every other kind, an unbalanced rule is
//! refused at the door naming the rule, the tenant source survives the
//! round trip, and a projection rule's `when` picks one workflow's step
//! out of `step.done.task`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_ledger::http::{LedgerApiState, router};
use boss_ledger::{FactRef, post_fact_in_tx, rebuild_facts};
use boss_testing::{RecordingEventBus, TestDb};
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;
use uuid::Uuid;

fn make_router(db: &TestDb) -> axum::Router {
    router(LedgerApiState {
        pool: db.pool.clone(),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        policy: std::sync::Arc::new(boss_policy_client::PermissivePolicyClient),
    })
}

const OPERATOR: &str = r#"{"id":"automation:tenant-seed","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;
const AUDITOR: &str = r#"{"id":"emp-audit","role":"auditor","access_tier":"auditor","territory_account_ids":[],"direct_report_ids":[],"department":null}"#;

async fn send(
    app: axum::Router,
    method: &str,
    path: &str,
    body: Value,
    user: &str,
) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header("x-boss-user", user)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, parsed)
}

async fn drain(db: &TestDb) {
    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
}

fn sponsorship_rule() -> Value {
    json!({
        "fact_kind": "finance.sponsorship.received",
        "basis": "cash",
        "lines": [
            {"account_code": "1010", "side": "debit",  "amount_path": "/amount_cents", "memo": "Sponsorship {/charge_id}"},
            {"account_code": "4100", "side": "credit", "amount_path": "/amount_cents"},
            {"account_code": "6100", "side": "debit",  "amount_path": "/fee_cents"},
            {"account_code": "1010", "side": "credit", "amount_path": "/fee_cents"},
        ]
    })
}

async fn post_fact(db: &TestDb, kind: &str, payload: &Value, source_id: &str) -> Uuid {
    let id = Uuid::new_v4();
    let happened_on = NaiveDate::from_ymd_opt(2026, 9, 17).unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, $2, $3, $4, 'jobs', $5, 'test')",
    )
    .bind(id)
    .bind(kind)
    .bind(happened_on)
    .bind(payload)
    .bind(source_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    post_fact_in_tx(
        &mut tx,
        &FactRef {
            id,
            kind,
            happened_on,
            payload,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn entry_lines(db: &TestDb, fact_id: Uuid) -> (Option<String>, Vec<(String, i64, i64)>) {
    let memo: Option<String> =
        sqlx::query_scalar("SELECT memo FROM gl_journal_entries WHERE fact_id = $1")
            .bind(fact_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let rows = sqlx::query(
        "SELECT a.code, l.debit_cents, l.credit_cents \
         FROM gl_journal_lines l \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         JOIN gl_accounts a ON a.id = l.account_id \
         WHERE e.fact_id = $1 ORDER BY l.sort_order",
    )
    .bind(fact_id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    (
        memo,
        rows.iter()
            .map(|r| (r.get("code"), r.get("debit_cents"), r.get("credit_cents")))
            .collect(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_published_rule_posts_the_fact_and_the_code_rules_post_the_rest() {
    let db = TestDb::new().await;
    let (status, out) = send(
        make_router(&db),
        "POST",
        "/api/ledger/posting-rules/batch",
        json!({"tenant_id": "algedonic", "rules": [sponsorship_rule()]}),
        OPERATOR,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["received"], 1);
    assert_eq!(out["inserted"], 1);

    // The registry reads back with the tenant source.
    let (status, listed) = send(
        make_router(&db),
        "GET",
        "/api/ledger/posting-rules",
        Value::Null,
        OPERATOR,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["data"][0]["source"], "tenant:algedonic");
    assert_eq!(listed["data"][0]["version"], 1);
    assert_eq!(listed["data"][0]["basis"], "cash");

    // The fact posts by the data rule: DR 1010 100 / CR 4100 100 /
    // DR 6100 33 / CR 1010 33, provenance in the memo.
    let fact_id = post_fact(
        &db,
        "finance.sponsorship.received",
        &json!({"amount_cents": 100, "fee_cents": 33, "charge_id": "ch_1"}),
        "job-1",
    )
    .await;
    let (memo, lines) = entry_lines(&db, fact_id).await;
    assert_eq!(
        memo.as_deref(),
        Some("finance.sponsorship.received — posting rule v1")
    );
    assert_eq!(
        lines,
        vec![
            ("1010".to_string(), 100, 0),
            ("4100".to_string(), 0, 100),
            ("6100".to_string(), 33, 0),
            ("1010".to_string(), 0, 33),
        ]
    );

    // A kind with no data rule still posts by the code rules.
    let paid = post_fact(
        &db,
        "finance.invoice.paid",
        &json!({"invoice_id": "inv-1", "amount_cents": 500}),
        "inv-1",
    )
    .await;
    let (memo, lines) = entry_lines(&db, paid).await;
    assert_eq!(memo.as_deref(), Some("Invoice paid: inv-1"));
    assert_eq!(
        lines,
        vec![("1000".to_string(), 500, 0), ("1100".to_string(), 0, 500)]
    );

    // The declaration left a fact: one ledger.posting_rule.declared,
    // signed by the batch's actor.
    drain(&db).await;
    let declared: Vec<(Value,)> =
        sqlx::query_as("SELECT payload FROM audit_log WHERE kind = 'ledger.posting_rule.declared'")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].0["fact_kind"], "finance.sponsorship.received");
    assert_eq!(declared[0].0["source"], "tenant:algedonic");
    assert_eq!(declared[0].0["declared_by"], "automation:tenant-seed");

    // A second publish inserts nothing and records nothing; an edited
    // row under the same version is named, not silently kept.
    let mut edited = sponsorship_rule();
    edited["lines"][1]["account_code"] = json!("4110");
    let (status, out) = send(
        make_router(&db),
        "POST",
        "/api/ledger/posting-rules/batch",
        json!({"tenant_id": "algedonic", "rules": [edited]}),
        OPERATOR,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["inserted"], 0);
    let differs = out["differs"].as_array().unwrap();
    assert_eq!(differs.len(), 1, "{out}");
    assert!(
        differs[0].as_str().unwrap().contains("publish version 2"),
        "{out}"
    );
    drain(&db).await;
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_log WHERE kind = 'ledger.posting_rule.declared'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "a kept row records nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unbalanced_rule_is_refused_at_the_door_naming_the_rule() {
    let db = TestDb::new().await;
    let mut rule = sponsorship_rule();
    rule["lines"].as_array_mut().unwrap().pop();
    let (status, out) = send(
        make_router(&db),
        "POST",
        "/api/ledger/posting-rules/batch",
        json!({"tenant_id": "algedonic", "rules": [sponsorship_rule(), rule]}),
        OPERATOR,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
    let why = out.as_str().unwrap();
    assert!(
        why.contains("finance.sponsorship.received v1"),
        "names the rule: {why}"
    );
    assert!(why.contains("not balanced for every fact"), "{why}");
    // The whole batch is refused: the good twin did not land either.
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM gl_posting_rules")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(n, 0);

    // The write gate: an auditor session cannot publish.
    let (status, _) = send(
        make_router(&db),
        "POST",
        "/api/ledger/posting-rules/batch",
        json!({"rules": [sponsorship_rule()]}),
        AUDITOR,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_projection_with_when_fires_on_one_workflows_step_only() {
    let db = TestDb::new().await;
    let projection = json!({
        "event_kind": "step.done.task",
        "when": {"/workflow_kind": "receive-a-sponsorship", "/spec_slug": "recognize"},
        "fact_kind": "finance.sponsorship.received",
        "source_table": "jobs",
        "source_id_path": "/job_id",
        "happened_on_path": "/completed_on",
    });
    let (status, out) = send(
        make_router(&db),
        "POST",
        "/api/ledger/fact-projection-rules/batch",
        json!({"rules": [projection]}),
        OPERATOR,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["inserted"], 1);
    let (status, listed) = send(
        make_router(&db),
        "GET",
        "/api/ledger/fact-projection-rules",
        Value::Null,
        OPERATOR,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let mine: Vec<&Value> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["event_kind"] == "step.done.task")
        .collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["when"]["/spec_slug"], "recognize");
    // The seeded rows carry no `when`, and read back without one.
    assert!(
        listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["event_kind"] == "commerce.invoice.created" && r.get("when").is_none()),
        "{listed}"
    );

    // Three step.done.task events: the recognize step of the
    // sponsorship workflow (fires), the same slug on another workflow
    // (does not), another step of the same workflow (does not).
    let ts: chrono::DateTime<chrono::Utc> = "2026-09-17T12:00:00Z".parse().unwrap();
    for (job, wf, slug) in [
        ("job-sponsor", "receive-a-sponsorship", "recognize"),
        ("job-month", "close-the-month", "recognize"),
        ("job-sponsor", "receive-a-sponsorship", "reconcile"),
    ] {
        sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             VALUES ($1, $2, 'jobs', 'step.done.task', $3)",
        )
        .bind(Uuid::new_v4())
        .bind(ts)
        .bind(json!({
            "job_id": job, "step_id": Uuid::new_v4().to_string(), "kind": "task",
            "workflow_kind": wf, "spec_slug": slug, "completed_on": "2026-09-17",
            "metadata": {"amount_cents": "100", "fee_cents": "33"},
        }))
        .execute(&db.pool)
        .await
        .unwrap();
    }
    let report = rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(report.events_scanned, 3);
    assert_eq!(report.facts_written, 1, "{report:?}");
    let facts: Vec<(String, String, Value)> = sqlx::query_as(
        "SELECT kind, source_id, payload FROM financial_facts WHERE kind = 'finance.sponsorship.received'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].1, "job-sponsor");
    assert_eq!(facts[0].2["metadata"]["amount_cents"], "100");

    // Published twice: kept, nothing recorded twice.
    drain(&db).await;
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_log WHERE kind = 'ledger.fact_projection_rule.declared'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
}

/// Backlog 94f20e76: a projection on an event family the platform
/// stream does not ingest is dead air live — the subscriber's filter
/// hears nothing, with no error. The door refuses the whole batch (422)
/// naming the family, and the registry keeps only what the schema
/// seeded on that family (the two `products.*` rows boss-products
/// writes in-tx anyway).
#[tokio::test(flavor = "multi_thread")]
async fn a_projection_on_an_unstreamed_family_is_refused_at_the_door() {
    let db = TestDb::new().await;
    let seeded: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM gl_fact_projection_rules WHERE event_kind LIKE 'products.%'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(seeded.0, 2, "40-ledger.sql seeds two products.* rows");

    let dead_air = json!({
        "event_kind": "products.returned",
        "fact_kind": "finance.return.recognized",
        "source_table": "products_return",
        "source_id_path": "/source_id",
        "happened_on_path": "/happened_on",
    });
    let (status, out) = send(
        make_router(&db),
        "POST",
        "/api/ledger/fact-projection-rules/batch",
        json!({"rules": [dead_air]}),
        OPERATOR,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
    let why = out.as_str().unwrap_or_default().to_string();
    assert!(why.contains("products.>"), "{why}");
    assert!(why.contains("products.returned"), "{why}");
    assert!(why.contains("stream_subjects"), "{why}");

    let after: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM gl_fact_projection_rules WHERE event_kind LIKE 'products.%'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(after.0, 2, "nothing landed; the seeded rows are untouched");
}
