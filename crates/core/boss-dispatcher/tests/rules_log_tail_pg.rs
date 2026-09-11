//! The log tail — dispatcher-rules consuming `audit_log` by id cursor
//! (transactional-audit-log Q6, stage 1).
//!
//! Contracts pinned here:
//!
//! 1. **First run is new-only**: the cursor initializes to
//!    MAX(audit_log.id), mirroring the JetStream durable consumer's
//!    position — never a full-log replay.
//! 2. **Per-item durable advance**: each settled row moves the cursor;
//!    a crash re-runs at most the unsettled tail (handlers are
//!    idempotent — the established contract).
//! 3. **Retry blocks the cursor** (ordered delivery has no NAK): the
//!    row is re-presented with an incremented attempt count, the
//!    cursor stays put, and rows behind it wait.
//! 4. **The budget dead-letters**: after MAX_ATTEMPTS the row is
//!    dead-lettered LOUDLY and the cursor advances past it — a poison
//!    event delays the tail, never wedges it.
//! 5. **Permanent advances immediately**: a deterministic data error
//!    is not retried (the Settle::Permanent contract).

use std::sync::{Arc, Mutex};

use boss_dispatcher::rules::log_tail::{DrainReport, LogTail, MAX_ATTEMPTS};
use boss_nats::durable::Settle;
use boss_testing::TestDb;

async fn seed_audit(pool: &sqlx::PgPool, kind: &str, payload: serde_json::Value) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, NOW(), 'test', $2, $3) RETURNING id",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(kind)
    .bind(payload)
    .fetch_one(pool)
    .await
    .expect("audit insert")
}

async fn cursor_of(pool: &sqlx::PgPool, consumer: &str) -> i64 {
    sqlx::query_scalar("SELECT last_audit_id FROM dispatcher_log_cursor WHERE consumer = $1")
        .bind(consumer)
        .fetch_one(pool)
        .await
        .expect("cursor row")
}

#[tokio::test]
async fn new_only_init_then_per_item_advance() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    // Rows that exist BEFORE the tail's first run are history.
    seed_audit(pool, "step.done.task", serde_json::json!({"n": "old"})).await;
    let last_old = seed_audit(pool, "step.done.task", serde_json::json!({"n": "old2"})).await;

    let mut tail = LogTail::new(pool.clone(), "test-rules");
    tail.ensure_cursor().await.expect("init");
    assert_eq!(
        cursor_of(pool, "test-rules").await,
        last_old,
        "first run initializes at MAX(id) — new-only, no full-log replay"
    );

    let a = seed_audit(pool, "step.done.task", serde_json::json!({"n": "a"})).await;
    let b = seed_audit(pool, "jobs.job.created", serde_json::json!({"n": "b"})).await;

    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen2 = seen.clone();
    let report = tail
        .drain_once(200, move |topic, _event_id, payload, _attempt| {
            let seen = seen2.clone();
            let label = format!("{topic}:{}", payload["n"].as_str().unwrap_or("?"));
            async move {
                seen.lock().unwrap().push(label);
                Settle::Ack
            }
        })
        .await
        .expect("drain");

    assert_eq!(report.processed, 2);
    assert_eq!(report.dead_lettered, 0);
    assert!(report.blocked.is_none());
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["step.done.task:a", "jobs.job.created:b"],
        "rows arrive in id order with their kind as the topic"
    );
    assert_eq!(cursor_of(pool, "test-rules").await, b.max(a));
}

#[tokio::test]
async fn retry_blocks_then_budget_dead_letters_and_advances() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    let mut tail = LogTail::new(pool.clone(), "test-rules");
    tail.ensure_cursor().await.expect("init");

    let poison = seed_audit(pool, "step.done.task", serde_json::json!({"n": "poison"})).await;
    let after = seed_audit(pool, "step.done.task", serde_json::json!({"n": "after"})).await;

    // Every presentation of the poison row fails; the row behind it
    // must NOT be reached while the budget lasts.
    for attempt in 1..MAX_ATTEMPTS {
        let report: DrainReport = tail
            .drain_once(200, |_t, _e, payload, handed| async move {
                if payload["n"] == "poison" {
                    // The handler is handed THIS presentation's count,
                    // which is what lets it know a failure is the last
                    // one the budget allows (`a9c498eb`).
                    assert_eq!(handed, attempt, "the handler sees its own attempt");
                    Settle::Retry("still failing".into())
                } else {
                    Settle::Ack
                }
            })
            .await
            .expect("drain");
        assert_eq!(report.processed, 0, "attempt {attempt}: nothing settles");
        let blocked = report.blocked.expect("blocked on the poison row");
        assert_eq!(blocked.audit_id, poison);
        assert_eq!(blocked.attempts, attempt);
        assert_eq!(
            cursor_of(pool, "test-rules").await + 1,
            poison,
            "cursor holds before the failing row"
        );
    }

    // The budget's final presentation: dead-letter, advance, and the
    // row behind flows.
    let report = tail
        .drain_once(200, |_t, _e, payload, handed| async move {
            if payload["n"] == "poison" {
                assert_eq!(
                    handed, MAX_ATTEMPTS,
                    "the final presentation is handed the spent budget"
                );
                Settle::Retry("still failing".into())
            } else {
                Settle::Ack
            }
        })
        .await
        .expect("drain");
    assert_eq!(report.dead_lettered, 1, "budget exhausted → dead-letter");
    assert_eq!(report.processed, 1, "the row behind the poison flows");
    assert_eq!(cursor_of(pool, "test-rules").await, after);
}

#[tokio::test]
async fn permanent_is_not_retried() {
    let db = TestDb::new().await;
    let pool = &db.pool;

    let mut tail = LogTail::new(pool.clone(), "test-rules");
    tail.ensure_cursor().await.expect("init");
    let bad = seed_audit(pool, "step.done.task", serde_json::json!({"n": "bad-data"})).await;

    let report = tail
        .drain_once(200, |_t, _e, _p, _attempt| async move {
            Settle::Permanent("deterministic data error".into())
        })
        .await
        .expect("drain");
    assert_eq!(report.processed, 1, "permanent settles the row");
    assert!(report.blocked.is_none());
    assert_eq!(cursor_of(pool, "test-rules").await, bad);
}
