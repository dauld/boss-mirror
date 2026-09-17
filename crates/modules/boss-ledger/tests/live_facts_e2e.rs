//! The live half of the fact projection (backlog 5621d166): a
//! `step.done.task` event matching a `when` projection rule projects
//! its fact AND posts its journal entry in the same call, the second
//! delivery writes nothing, an event no rule names writes nothing,
//! and a full facts + journal rebuild over the audit_log row the relay
//! wrote for the same event reproduces the live fact and the live
//! entry byte for byte — the same UUIDv5 id, the same lines.

use boss_core::event::Event;
use boss_ledger::live_facts::project_live_event;
use boss_ledger::rebuild_facts::ProjectionRules;
use boss_ledger::{rebuild, rebuild_facts};
use boss_testing::TestDb;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

async fn publish_rules(db: &TestDb) {
    sqlx::query(
        "INSERT INTO gl_posting_rules (fact_kind, version, lines, basis, source) \
         VALUES ('finance.sponsorship.received', 1, $1, 'cash', 'tenant:algedonic')",
    )
    .bind(json!([
        {"account_code": "1010", "side": "debit",  "amount_path": "/metadata/amount_cents", "memo": "Sponsorship {/job_id}"},
        {"account_code": "4100", "side": "credit", "amount_path": "/metadata/amount_cents"},
        {"account_code": "6100", "side": "debit",  "amount_path": "/metadata/fee_cents"},
        {"account_code": "1010", "side": "credit", "amount_path": "/metadata/fee_cents"},
    ]))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO gl_fact_projection_rules \
            (event_kind, fact_kind, source_table, source_id_path, happened_on_path, when_filter) \
         VALUES ('step.done.task', 'finance.sponsorship.received', 'jobs', '/job_id', '/completed_on', $1)",
    )
    .bind(json!({"/workflow_kind": "receive-a-sponsorship", "/spec_slug": "recognize"}))
    .execute(&db.pool)
    .await
    .unwrap();
}

async fn load_rules(db: &TestDb) -> ProjectionRules {
    let mut tx = db.pool.begin().await.unwrap();
    let rules = ProjectionRules::load_in_tx(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    rules
}

fn recognize_event(job_id: &str, workflow_kind: &str, slug: &str) -> Event {
    let ts: DateTime<Utc> = "2026-09-17T18:20:16Z".parse().unwrap();
    Event::new(
        "jobs",
        "step.done.task",
        json!({
            "job_id": job_id, "step_id": Uuid::new_v4().to_string(), "kind": "task",
            "workflow_kind": workflow_kind, "spec_slug": slug,
            "completed_on": "2026-09-17",
            "metadata": {"amount_cents": "100", "fee_cents": "33"},
            "_actor": "emp-david",
        }),
        ts,
    )
}

/// The relay's write for an event, as audit_pg / outbox store it.
async fn write_audit_row(db: &TestDb, event: &Event) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(event.id)
    .bind(event.timestamp)
    .bind(&event.source)
    .bind(&event.kind)
    .bind(&event.payload)
    .execute(&db.pool)
    .await
    .unwrap();
}

type FactRow = (
    Uuid,
    String,
    NaiveDate,
    Value,
    Option<String>,
    Option<String>,
    String,
);

async fn fact_rows(db: &TestDb) -> Vec<FactRow> {
    sqlx::query_as(
        "SELECT id, kind, happened_on, payload, source_table, source_id, created_by \
         FROM financial_facts WHERE kind = 'finance.sponsorship.received' ORDER BY source_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

/// The journal as an operator would compare it across a rebuild:
/// (fact_id, posted_on, memo, [(account, debit, credit)]) — entry ids
/// are minted per insert and are not part of the identity.
async fn entries(db: &TestDb) -> Vec<(Uuid, NaiveDate, Option<String>, Vec<(String, i64, i64)>)> {
    let heads = sqlx::query(
        "SELECT e.id, e.fact_id, e.posted_on, e.memo FROM gl_journal_entries e \
         JOIN financial_facts f ON f.id = e.fact_id \
         WHERE f.kind = 'finance.sponsorship.received' ORDER BY f.source_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let mut out = Vec::new();
    for h in heads {
        let id: Uuid = h.get("id");
        let lines = sqlx::query(
            "SELECT a.code, l.debit_cents, l.credit_cents FROM gl_journal_lines l \
             JOIN gl_accounts a ON a.id = l.account_id \
             WHERE l.journal_entry_id = $1 ORDER BY l.sort_order",
        )
        .bind(id)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        out.push((
            h.get("fact_id"),
            h.get("posted_on"),
            h.get("memo"),
            lines
                .iter()
                .map(|l| (l.get("code"), l.get("debit_cents"), l.get("credit_cents")))
                .collect(),
        ));
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_matching_step_done_projects_and_posts_in_one_call_and_a_rebuild_changes_nothing() {
    let db = TestDb::new().await;
    publish_rules(&db).await;
    let rules = load_rules(&db).await;

    // The live delivery: the recognize step of the sponsorship workflow.
    let event = recognize_event("job-sponsor", "receive-a-sponsorship", "recognize");
    let out = project_live_event(&db.pool, &rules, &event).await;
    assert!(out.permanent.is_empty(), "{out:?}");
    assert!(out.retry.is_none(), "{out:?}");
    assert_eq!(out.facts.len(), 1, "{out:?}");

    // Fact AND entry, from the one call.
    let live_facts = fact_rows(&db).await;
    assert_eq!(live_facts.len(), 1);
    let (id, kind, happened_on, payload, source_table, source_id, created_by) = &live_facts[0];
    assert_eq!(*id, out.facts[0]);
    assert_eq!(kind, "finance.sponsorship.received");
    assert_eq!(happened_on.to_string(), "2026-09-17");
    assert_eq!(source_table.as_deref(), Some("jobs"));
    assert_eq!(source_id.as_deref(), Some("job-sponsor"));
    assert_eq!(created_by, "jobs");
    assert!(
        payload.get("_actor").is_none(),
        "envelope stripped: {payload}"
    );
    let live_entries = entries(&db).await;
    assert_eq!(live_entries.len(), 1);
    assert_eq!(live_entries[0].0, *id);
    assert_eq!(
        live_entries[0].2.as_deref(),
        Some("finance.sponsorship.received — posting rule v1")
    );
    assert_eq!(
        live_entries[0].3,
        vec![
            ("1010".to_string(), 100, 0),
            ("4100".to_string(), 0, 100),
            ("6100".to_string(), 33, 0),
            ("1010".to_string(), 0, 33),
        ]
    );

    // A redelivery of the same event writes nothing twice: the fact
    // resolves to its id, the entry is left alone.
    let again = project_live_event(&db.pool, &rules, &event).await;
    assert_eq!(again.facts, out.facts, "{again:?}");
    assert_eq!(fact_rows(&db).await, live_facts);
    assert_eq!(entries(&db).await, live_entries);

    // Events no rule names — another workflow's step of the same slug,
    // another slug of the same workflow — write nothing.
    for (wf, slug) in [
        ("close-the-month", "recognize"),
        ("receive-a-sponsorship", "reconcile"),
    ] {
        let none =
            project_live_event(&db.pool, &rules, &recognize_event("job-other", wf, slug)).await;
        assert!(
            none.facts.is_empty() && none.permanent.is_empty(),
            "{none:?}"
        );
    }
    assert_eq!(fact_rows(&db).await.len(), 1);

    // The relay wrote the same event to audit_log; a full facts rebuild
    // (TRUNCATE + reproject, cascading the journal) and a journal
    // rebuild reproduce the live rows exactly — same id, same bytes,
    // same lines.
    write_audit_row(&db, &event).await;
    let report = rebuild_facts(&db.pool).await.unwrap();
    assert_eq!(report.facts_written, 1, "{report:?}");
    let posted = rebuild(&db.pool).await.unwrap();
    assert!(posted.is_balanced(), "{posted:?}");
    assert_eq!(
        fact_rows(&db).await,
        live_facts,
        "the rebuilt fact is the live fact"
    );
    assert_eq!(
        entries(&db).await,
        live_entries,
        "the rebuilt entry is the live entry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fact_kind_with_no_posting_rule_is_a_permanent_failure_that_records_nothing() {
    let db = TestDb::new().await;
    // The projection rule exists, the posting rule does not: the fact
    // has no entry to post, so the call rolls back — nothing half
    // written, the failure named — and the caller ACKs (never retries).
    sqlx::query(
        "INSERT INTO gl_fact_projection_rules \
            (event_kind, fact_kind, source_table, source_id_path, happened_on_path, when_filter) \
         VALUES ('step.done.task', 'finance.sponsorship.received', 'jobs', '/job_id', '/completed_on', $1)",
    )
    .bind(json!({"/workflow_kind": "receive-a-sponsorship", "/spec_slug": "recognize"}))
    .execute(&db.pool)
    .await
    .unwrap();
    let rules = load_rules(&db).await;
    let event = recognize_event("job-sponsor", "receive-a-sponsorship", "recognize");
    let out = project_live_event(&db.pool, &rules, &event).await;
    assert!(out.facts.is_empty(), "{out:?}");
    assert!(out.retry.is_none(), "{out:?}");
    assert_eq!(out.permanent.len(), 1, "{out:?}");
    assert!(
        out.permanent[0].contains("finance.sponsorship.received"),
        "names the kind: {out:?}"
    );
    assert!(
        fact_rows(&db).await.is_empty(),
        "the fact did not outlive its failed post"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redelivery_does_not_resurrect_a_superseded_facts_entry() {
    // The platform stream buffers three days of events and a fresh
    // durable consumer starts at the buffer's head, so the live path
    // WILL see events whose facts already exist — including one an
    // operator retired since (a supersede marks the fact and drops its
    // entry). A fact that already existed has been handled by whoever
    // wrote it; the live path must not post it again.
    let db = TestDb::new().await;
    publish_rules(&db).await;
    let rules = load_rules(&db).await;
    let event = recognize_event("job-sponsor", "receive-a-sponsorship", "recognize");
    let out = project_live_event(&db.pool, &rules, &event).await;
    assert_eq!(out.facts.len(), 1, "{out:?}");
    assert_eq!(entries(&db).await.len(), 1);

    let mut tx = db.pool.begin().await.unwrap();
    let outcome = boss_ledger::apply_supersede_in_tx(
        &mut tx,
        &boss_ledger::SupersedeRequest {
            kind: "finance.sponsorship.received".into(),
            source_table: Some("jobs".into()),
            source_id: Some("job-sponsor".into()),
            reason: "recorded against the wrong job".into(),
            superseded_by: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, boss_ledger::SupersedeOutcome::Applied { .. }),
        "{outcome:?}"
    );
    tx.commit().await.unwrap();
    assert!(
        entries(&db).await.is_empty(),
        "the supersede dropped the entry"
    );

    let again = project_live_event(&db.pool, &rules, &event).await;
    assert!(
        again.permanent.is_empty() && again.retry.is_none(),
        "{again:?}"
    );
    assert!(
        entries(&db).await.is_empty(),
        "a redelivery must not re-post a retracted fact"
    );
}
