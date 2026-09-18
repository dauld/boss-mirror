//! Postgres-backed coverage for the cadence registry's WRITE door
//! (backlog 13d1fff3, 2026-09-18): `POST /api/cadence/rules/{name}/retire`
//! and `POST /api/cadence/rules/{name}/publish`, served by the REAL
//! router over the Pg adapter on a TestDb — the same wire, handlers
//! and adapter production mounts.
//!
//! What the in-memory tests in `cadence::http` and `cadence::in_memory`
//! cannot pin, this file does:
//! - the migrations' own rows are what the door retires and re-versions
//!   (`protocol-retro-daily`, 202608282140, is the row the packet
//!   names — the operator's act after this lands is to retire it);
//! - every write stages its event in `event_outbox` in the SAME
//!   transaction as the row, with `_actor` = the operator's session;
//! - the 409 rides the Pg adapter's `MAX(version)` bound, not only the
//!   in-memory one;
//! - both `jobs.cadence.*` kinds are declared in `event_kinds` so the
//!   audit trigger admits them — an exact-set assertion, as the
//!   stations pin is, so an emitted-but-undeclared kind reds here and
//!   not inside a passing audit-integrity run.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockNow, FixedClockClient};
use boss_jobs::cadence::{CadenceRegistry, CadenceRepository, PgCadence};
use boss_jobs::events::{CADENCE_PUBLISHED, CADENCE_RETIRED};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::TestDb;
use chrono::{DateTime, TimeZone, Utc};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn door_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 18, 17, 30, 0).unwrap()
}

fn app(db: &TestDb) -> Router {
    let cadence = Arc::new(PgCadence::new(db.pool.clone()));
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Publish,
                Resource::workflow(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Retire,
                Resource::workflow(),
                Scope::All,
            )
            .build(),
    );
    boss_jobs::cadence::http::router(boss_jobs::cadence::http::CadenceApiState {
        repo: cadence.clone() as Arc<dyn CadenceRepository>,
        registry: cadence as Arc<dyn CadenceRegistry>,
        policy,
        clock: Arc::new(FixedClockClient::new(ClockNow {
            now: door_now(),
            simulated: false,
            epoch_start: None,
            epoch_end: None,
            paused: false,
            restart_in_progress: false,
            warp_factor: None,
        })),
    })
}

fn operator() -> String {
    serde_json::to_string(&User {
        id: "emp-david".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("platform".into()),
    })
    .unwrap()
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("x-boss-user", operator());
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn outbox(db: &TestDb) -> Vec<(String, serde_json::Value)> {
    sqlx::query_as(
        "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'jobs.cadence.%' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read outbox")
}

async fn active_names(db: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM cadence_rules WHERE status = 'active' ORDER BY name")
        .fetch_all(&db.pool)
        .await
        .expect("read active names")
}

/// THE OPERATOR'S ACT, AS A TEST: the migrations insert
/// `protocol-retro-daily` (202608282140) on every fresh database and
/// the bundle has no file for it by decision (infra/platform/cadence/
/// README.md); this is the retire the packet says had no path.
#[tokio::test(flavor = "multi_thread")]
async fn the_migrations_row_is_retired_through_the_door_and_the_log_says_so() {
    let db = TestDb::new().await;
    let app = app(&db);
    assert!(
        active_names(&db)
            .await
            .contains(&"protocol-retro-daily".to_string()),
        "the migrations still insert the row this door exists to retire"
    );

    let (st, body) = call(
        &app,
        "POST",
        "/api/cadence/rules/protocol-retro-daily/retire",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "protocol-retro-daily");
    assert_eq!(body["status"], "retired");
    assert_eq!(body["verb"], "open:protocol-retro");
    assert_eq!(
        body["cadence"], "daily",
        "the calendar columns ride the row"
    );

    assert!(
        !active_names(&db)
            .await
            .contains(&"protocol-retro-daily".to_string()),
        "the conductor's read no longer serves it"
    );
    let (st, rules) = call(&app, "GET", "/api/cadence/rules", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        rules
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["name"] != "protocol-retro-daily"),
        "{rules}"
    );

    // The row is history, not gone.
    let (st, lineage) = call(
        &app,
        "GET",
        "/api/cadence/rules/protocol-retro-daily/versions",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(lineage.as_array().unwrap().len(), 1);
    assert_eq!(lineage[0]["status"], "retired");

    // A second retire is a 404 — nothing active, nothing recorded.
    let (st, _) = call(
        &app,
        "POST",
        "/api/cadence/rules/protocol-retro-daily/retire",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let events = outbox(&db).await;
    assert_eq!(
        events.len(),
        1,
        "exactly one fact for one retire: {events:?}"
    );
    assert_eq!(events[0].0, CADENCE_RETIRED);
    assert_eq!(events[0].1["name"], "protocol-retro-daily");
    assert_eq!(events[0].1["status"], "retired");
    assert_eq!(
        events[0].1["_actor"], "emp-david",
        "signed by the operator's session, never the platform automation"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_publish_re_versions_a_live_rule_and_409s_below_its_lineage() {
    let db = TestDb::new().await;
    let app = app(&db);
    // The live lineage of the reconcile rule, as the migrations left it.
    let (_, before) = call(
        &app,
        "GET",
        "/api/cadence/rules/train-reconcile/versions",
        None,
    )
    .await;
    let newest = before
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_i64().unwrap())
        .max()
        .expect("the migrations declare train-reconcile");

    // The bundle file's shape, version bumped — what `boss cadence
    // publish infra/platform/cadence/train-reconcile.toml` sends.
    let declared = |version: i64| {
        serde_json::json!({
            "name": "train-reconcile",
            "version": version,
            "status": "active",
            "verb": "reconcile",
            "basis": "wall",
            "every_minutes": 7,
        })
    };

    // Not above the newest: the same version and one below.
    for v in [newest, newest - 1] {
        let (st, body) = call(
            &app,
            "POST",
            "/api/cadence/rules/train-reconcile/publish",
            Some(declared(v)),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT, "v{v}: {body}");
        assert!(
            body.as_str()
                .unwrap_or_default()
                .contains(&format!("v{newest}")),
            "names the newest version: {body}"
        );
    }
    assert!(outbox(&db).await.is_empty(), "a refusal records nothing");

    let (st, body) = call(
        &app,
        "POST",
        "/api/cadence/rules/train-reconcile/publish",
        Some(declared(newest + 1)),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["version"], newest + 1);
    assert_eq!(body["every_minutes"], 7);
    assert_eq!(
        body["created_at"],
        serde_json::to_value(door_now()).unwrap()
    );

    // Exactly one active row, the new one; the old one retired.
    let (_, after) = call(
        &app,
        "GET",
        "/api/cadence/rules/train-reconcile/versions",
        None,
    )
    .await;
    let active: Vec<i64> = after
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["status"] == "active")
        .map(|r| r["version"].as_i64().unwrap())
        .collect();
    assert_eq!(active, vec![newest + 1]);
    let (_, rules) = call(&app, "GET", "/api/cadence/rules", None).await;
    let served = rules
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "train-reconcile")
        .expect("served");
    assert_eq!(
        served["every_minutes"], 7,
        "the conductor reads the new row"
    );

    let events = outbox(&db).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, CADENCE_PUBLISHED);
    assert_eq!(events[0].1["version"], newest + 1);
    assert_eq!(events[0].1["_actor"], "emp-david");
}

#[tokio::test(flavor = "multi_thread")]
async fn cadence_event_kinds_are_registered() {
    let db = TestDb::new().await;
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind_pattern FROM event_kinds WHERE kind_pattern LIKE 'jobs.cadence.%' ORDER BY kind_pattern",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read event_kinds");
    assert_eq!(
        kinds,
        // The second home of a fact that lives in the event_kinds
        // INSERT of 20260918164853 (§9a's equality test), exact-set as
        // the stations pin is.
        vec![CADENCE_PUBLISHED.to_string(), CADENCE_RETIRED.to_string()]
    );
}
