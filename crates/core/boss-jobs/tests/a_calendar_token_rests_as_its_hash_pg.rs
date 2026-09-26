//! A calendar-feed token rests as its SHA-256 — in the table, in the
//! event, in the log — and the tokens the log already holds in the clear
//! are revoked by events that survive a rebuild (backlog 4aaff4dc,
//! design 3101c506, 2026-09-26).
//!
//! What only a real database can answer:
//! - the migration's SQL digest is byte-for-byte the Rust digest, run
//!   from THE MIGRATION FILE against the table shape it was written for;
//! - a rotate's payload in `audit_log` carries the digest and no token;
//! - a rebuild over a mixed log (legacy raw events + digest events)
//!   reproduces the migrated live table, `logged_raw` included;
//! - the revoke of the logged tokens is events, so a replay of the log
//!   does not bring a logged token back to life;
//! - an operator's revoke of an id with no feed writes no row and no
//!   event.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::actor::ActorId;
use boss_core::publisher::{DomainPublisher, EventStamp};
use boss_jobs::scheduling::feed_token::SQL_DIGEST_OF_TOKEN_COLUMN;
use boss_jobs::scheduling::http::{SchedulingApiState, router as scheduling_router};
use boss_jobs::scheduling::{
    CalendarTokenSha256, PgScheduling, events::CALENDAR_TOKEN_ROTATED, mint_calendar_token,
    rebuild_scheduling,
};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use serde_json::{Value, json};
use sqlx::PgPool;

const MIGRATION: &str =
    "infra/postgres/schema/20260926003019-a-calendar-token-rests-as-its-hash.sql";

fn migration_sql() -> String {
    std::fs::read_to_string(boss_testing::repo_root().join(MIGRATION))
        .unwrap_or_else(|e| panic!("read {MIGRATION}: {e}"))
}

fn build_app(pool: PgPool) -> Router {
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "scheduling");
    scheduling_router(SchedulingApiState {
        repo: Arc::new(PgScheduling::new(pool)),
        publisher: Some(publisher),
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
}

async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn boss_core::port::EventBus>), 100)
        .await
        .expect("relay drain");
}

/// Put the table back in the shape 28-scheduling.sql created and every
/// deployment held until this car — so the migration file runs against
/// what it was written for.
async fn legacy_table(pool: &PgPool) {
    sqlx::raw_sql(
        "DROP TABLE tech_calendar_tokens; \
         CREATE TABLE tech_calendar_tokens ( \
             employee_id TEXT PRIMARY KEY, \
             token       TEXT NOT NULL UNIQUE, \
             created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()); \
         CREATE INDEX tech_calendar_tokens_lookup ON tech_calendar_tokens (token);",
    )
    .execute(pool)
    .await
    .expect("legacy table");
}

/// A mint exactly as the adapter did it before digests: the raw token
/// into the row AND into the event payload, in one transaction.
async fn legacy_mint(pool: &PgPool, employee_id: &str, token: &str) {
    let stamp = EventStamp::new("jobs", ActorId::Human(employee_id.into()));
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO tech_calendar_tokens (employee_id, token, created_at) VALUES ($1, $2, $3)",
    )
    .bind(employee_id)
    .bind(token)
    .bind(stamp.timestamp)
    .execute(&mut *tx)
    .await
    .unwrap();
    let event = stamp.event(
        CALENDAR_TOKEN_ROTATED,
        json!({"employee_id": employee_id, "token": token, "rotated_at": stamp.timestamp}),
    );
    boss_events::outbox::record_event_in_tx(&mut tx, &event)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct FeedRow {
    employee_id: String,
    token_sha256: String,
    logged_raw: bool,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn snapshot(pool: &PgPool) -> Vec<FeedRow> {
    sqlx::query_as(
        "SELECT employee_id, token_sha256, logged_raw, created_at \
         FROM tech_calendar_tokens ORDER BY employee_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn opens(app: &Router, token: &str) -> bool {
    let r = TestRequest::get(format!("/ics/{token}/calendar.ics"))
        .send(app)
        .await;
    match r.status {
        StatusCode::OK => true,
        StatusCode::NOT_FOUND => false,
        other => panic!("the feed answered {other}: {}", r.body_text()),
    }
}

async fn own_rotate(app: &Router, employee_id: &str) -> String {
    let r = TestRequest::post(format!(
        "/api/scheduling/techs/{employee_id}/calendar-token"
    ))
    .as_user(employee_id, "service-tech")
    .send(app)
    .await;
    r.assert_status(StatusCode::OK);
    r.assert_json::<Value>()["token"]
        .as_str()
        .expect("the owner is handed the token")
        .to_string()
}

async fn revoke_logged(app: &Router) -> Value {
    let r = TestRequest::post("/api/scheduling/calendar-tokens/logged-raw/revoke")
        .as_user("emp-david", "platform-admin")
        .send(app)
        .await;
    r.assert_status(StatusCode::OK);
    r.assert_json()
}

async fn logged_raw(app: &Router) -> Value {
    let r = TestRequest::get("/api/scheduling/calendar-tokens/logged-raw")
        .send(app)
        .await;
    r.assert_status(StatusCode::OK);
    r.assert_json::<Value>()["logged_raw"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_migration_rehashes_every_row_with_the_rust_digest() {
    let sql = migration_sql();
    assert!(
        sql.contains(SQL_DIGEST_OF_TOKEN_COLUMN),
        "{MIGRATION} no longer applies feed_token::SQL_DIGEST_OF_TOKEN_COLUMN \
         ({SQL_DIGEST_OF_TOKEN_COLUMN}) — the SQL and Rust digests are one fact"
    );

    let db = TestDb::new().await;
    legacy_table(&db.pool).await;
    let minted = mint_calendar_token();
    legacy_mint(&db.pool, "emp-tech-001", &minted).await;
    legacy_mint(&db.pool, "emp-tech-002", "a-token-of-another-shape").await;
    let before: Vec<(String, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT employee_id, created_at FROM tech_calendar_tokens ORDER BY 1")
            .fetch_all(&db.pool)
            .await
            .unwrap();

    sqlx::raw_sql(&sql)
        .execute(&db.pool)
        .await
        .expect("migrate");

    let rows = snapshot(&db.pool).await;
    assert_eq!(
        rows.iter()
            .map(|r| (r.employee_id.clone(), r.created_at))
            .collect::<Vec<_>>(),
        before,
        "the re-hash keeps every row and its created_at"
    );
    assert_eq!(
        rows[0].token_sha256,
        CalendarTokenSha256::of(&minted).as_str()
    );
    assert_eq!(
        rows[1].token_sha256,
        CalendarTokenSha256::of("a-token-of-another-shape").as_str()
    );
    assert!(
        rows.iter().all(|r| r.logged_raw),
        "every re-hashed row's token is in the log"
    );
    let raw_column: Option<(String,)> = sqlx::query_as(
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_name = 'tech_calendar_tokens' AND column_name = 'token'",
    )
    .fetch_optional(&db.pool)
    .await
    .unwrap();
    assert_eq!(raw_column, None, "the raw token column survived");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rotate_logs_the_digest_and_never_the_token() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());
    let token = own_rotate(&app, "emp-tech-001").await;
    drain_outbox(&db.pool).await;

    let payloads: Vec<(Value,)> = sqlx::query_as("SELECT payload FROM audit_log WHERE kind = $1")
        .bind(CALENDAR_TOKEN_ROTATED)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(payloads.len(), 1);
    let payload = &payloads[0].0;
    assert!(
        payload.get("token").is_none(),
        "the log holds a token: {payload}"
    );
    assert!(
        !payload.to_string().contains(&token),
        "the token rode the payload under another key: {payload}"
    );
    assert_eq!(
        payload["token_sha256"],
        CalendarTokenSha256::of(&token).as_str()
    );

    let rows = snapshot(&db.pool).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].token_sha256,
        CalendarTokenSha256::of(&token).as_str()
    );
    assert!(!rows[0].logged_raw);
    assert!(opens(&app, &token).await);
    assert!(
        !opens(&app, CalendarTokenSha256::of(&token).as_str()).await,
        "the digest the log holds opened the feed"
    );
}

/// The whole life of a pre-digest feed: minted raw, re-hashed by the
/// migration, replayed, revoked by the bounded verb, replayed again.
#[tokio::test(flavor = "multi_thread")]
async fn a_rebuild_over_a_mixed_log_reproduces_the_migrated_table_and_the_revoke() {
    let db = TestDb::new().await;
    legacy_table(&db.pool).await;
    let logged = mint_calendar_token();
    legacy_mint(&db.pool, "emp-tech-001", &logged).await;
    sqlx::raw_sql(&migration_sql())
        .execute(&db.pool)
        .await
        .expect("migrate");

    let app = build_app(db.pool.clone());
    let fresh = own_rotate(&app, "emp-tech-002").await;
    drain_outbox(&db.pool).await;

    // Migrated live table == the table the mixed log replays to.
    let live = snapshot(&db.pool).await;
    assert_eq!(live.len(), 2);
    rebuild_scheduling(&db.pool).await.expect("rebuild");
    assert_eq!(
        snapshot(&db.pool).await,
        live,
        "a replay of legacy raw events and digest events must give the migrated table"
    );
    // A logged token still opens until the revoke: the log cannot be
    // edited, and its token hashes to the stored digest.
    assert!(opens(&app, &logged).await);
    assert_eq!(logged_raw(&app).await, 1);

    assert_eq!(revoke_logged(&app).await["revoked"], 1);
    assert!(!opens(&app, &logged).await, "a logged token still opens");
    assert!(opens(&app, &fresh).await, "a digest-era feed was revoked");
    assert_eq!(logged_raw(&app).await, 0);
    assert_eq!(
        revoke_logged(&app).await["revoked"],
        0,
        "a second run must revoke nothing"
    );
    drain_outbox(&db.pool).await;

    // The revoke is events, so a replay keeps it.
    let revoked = snapshot(&db.pool).await;
    rebuild_scheduling(&db.pool).await.expect("rebuild");
    assert_eq!(snapshot(&db.pool).await, revoked);
    assert!(
        !opens(&app, &logged).await,
        "a replay brought a logged token back to life"
    );
    assert!(opens(&app, &fresh).await);
    assert_eq!(logged_raw(&app).await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_operators_revoke_of_an_id_with_no_feed_writes_nothing() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());
    TestRequest::post("/api/scheduling/techs/emp-made-up/calendar-token")
        .as_user("emp-david", "platform-admin")
        .send(&app)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    let outboxed: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM event_outbox WHERE kind = $1")
        .bind(CALENDAR_TOKEN_ROTATED)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(outboxed.0, 0, "a refused revoke recorded an event");
    assert!(
        snapshot(&db.pool).await.is_empty(),
        "an orphan row was minted"
    );
}
