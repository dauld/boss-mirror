//! Postgres coverage for the moves record (design e765b3fc §3, car M1):
//! the migration's table and its once-per-cause key, and the two reads
//! of `audit_log` the mover makes through boss-events.
//!
//! WHAT MUST HOLD: a replayed batch writes nothing twice (idempotence,
//! the unique key on `(cause_event_id, packet)`); the record reads back
//! in its own order; and the mover's log read returns exactly the
//! packet-naming rows of its slice — a step's under `job_id`, a job's
//! under `id` — and nothing else.

use boss_jobs::moves::{Move, MovesStore, PgMoves};
use boss_testing::TestDb;
use chrono::{DateTime, Utc};
use uuid::Uuid;

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().into()
}

fn moved(
    n: u128,
    packet: &str,
    from: Option<&str>,
    to: Option<&str>,
    declared: Option<bool>,
) -> Move {
    Move {
        at: t("2026-09-26T03:00:00Z") + chrono::Duration::seconds(n as i64),
        packet: packet.into(),
        kind: "pr-train".into(),
        label: "PR train 2026-09-26 03:04".into(),
        from: from.map(str::to_string),
        to: to.map(str::to_string),
        declared,
        cause_event_id: Uuid::from_u128(n),
        cause_seq: n as i64,
        cause_kind: "jobs.job.updated".into(),
        handoff_from: None,
        lineage: Some("boarded_jobs".into()),
        aboard: vec!["car-1".into(), "car-2".into()],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_batch_writes_nothing_twice_and_reads_back_in_order() {
    let db = TestDb::new().await;
    let store = PgMoves::new(db.pool.clone());
    assert_eq!(store.latest_seq().await.unwrap(), 0, "an empty record");

    let batch = [
        moved(1, "train-1", Some("dock"), Some("track"), Some(true)),
        moved(2, "train-1", Some("track"), Some("shed"), Some(false)),
        moved(3, "train-2", None, Some("track"), None),
    ];
    assert_eq!(store.record(&batch).await.unwrap(), 3);
    assert_eq!(
        store.record(&batch).await.unwrap(),
        0,
        "the same causes, the same packets"
    );

    let back = store.since(0, 10).await.unwrap();
    assert_eq!(back.len(), 3);
    assert!(
        back.windows(2).all(|w| w[0].seq < w[1].seq),
        "the record's own order"
    );
    let moves: Vec<Move> = back.iter().map(|r| r.r#move.clone()).collect();
    assert_eq!(moves, batch, "every column round-trips, aboard included");
    assert_eq!(store.latest_seq().await.unwrap(), back[2].seq);
    assert_eq!(store.since(back[0].seq, 10).await.unwrap().len(), 2);

    // Every route taken in the window, counted, declared or not — onto
    // the map included (a NULL end groups like any other); whether each
    // is declared is judged at read time against the derived routes.
    let taken = store.crossings(t("2026-09-26T00:00:00Z")).await.unwrap();
    assert_eq!(
        taken
            .iter()
            .map(|r| (r.from.as_deref(), r.to.as_deref(), r.moves))
            .collect::<Vec<_>>(),
        [
            (Some("dock"), Some("track"), 1),
            (Some("track"), Some("shed"), 1),
            (None, Some("track"), 1),
        ]
    );
    assert!(
        store
            .crossings(t("2026-09-26T04:00:00Z"))
            .await
            .unwrap()
            .is_empty(),
        "outside the window"
    );

    // A move to the place it came from is refused by the table itself.
    let nowhere = moved(9, "train-3", Some("dock"), Some("dock"), Some(false));
    assert!(store.record(&[nowhere]).await.is_err());
}

async fn log(pool: &sqlx::PgPool, kind: &str, payload: serde_json::Value) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, NOW(), 'jobs', $2, $3)",
    )
    .bind(Uuid::new_v4())
    .bind(kind)
    .bind(payload)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mover_reads_the_packet_naming_rows_of_its_slice_and_nothing_else() {
    let db = TestDb::new().await;
    let store = PgMoves::new(db.pool.clone());
    let before = store.log_head().await.unwrap();

    log(
        &db.pool,
        "jobs.job.created",
        serde_json::json!({ "id": "car-1" }),
    )
    .await;
    log(
        &db.pool,
        "jobs.step.updated",
        serde_json::json!({ "job_id": "car-1", "id": "step-9" }),
    )
    .await;
    log(
        &db.pool,
        "step.done.task",
        serde_json::json!({ "job_id": "gate-1" }),
    )
    .await;
    // Neither of these names a packet the mover places.
    log(
        &db.pool,
        "jobs.kind.published",
        serde_json::json!({ "id": "wf-1" }),
    )
    .await;
    log(
        &db.pool,
        "people.employee.updated",
        serde_json::json!({ "id": "emp-1" }),
    )
    .await;
    let head = store.log_head().await.unwrap();
    assert_eq!(head - before, 5, "the head is the log's newest id");

    let causes = store.causes(before, head, 100).await.unwrap();
    let named: Vec<(&str, &str)> = causes
        .iter()
        .map(|c| (c.kind.as_str(), c.packet.as_str()))
        .collect();
    assert_eq!(
        named,
        [
            ("jobs.job.created", "car-1"),
            ("jobs.step.updated", "car-1"),
            ("step.done.task", "gate-1"),
        ],
        "a step's event names its job, not the step"
    );
    assert!(causes.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(
        store.causes(before, head, 1).await.unwrap().len(),
        1,
        "the limit bounds the page"
    );
    assert!(
        store.causes(head, head, 100).await.unwrap().is_empty(),
        "an empty slice"
    );
}
