//! Integration tests for the next-best-action endpoint.
//!
//! Seeds a account with one active service agreement that expires in
//! 10 days (critical), one past-due invoice (warning), and one
//! healthy agreement that should not fire. The endpoint should
//! return exactly the two expected actions in severity order.
//!
//! NOTE: the rule-firing assertions in this file are `#[ignore]`'d.
//! The endpoint reads `ml_predictions` populated by `boss-ml-api`'s
//! daily `infer-batch` cron rather than running rules on demand, so
//! an in-process domain seed → assert pattern can't drive them;
//! reviving them means invoking the inference dispatcher in-process
//! or seeding `ml_predictions` with synthetic predictions. The
//! non-ignored cases (empty / paid / healthy / cancelled / recent_pm)
//! exercise the read path and the access-control envelope.

use axum::http::StatusCode;
use boss_accounts::account_next_actions::next_actions_router;
use boss_testing::{TestDb, TestRequest};
use chrono::{Duration, Utc};
use sqlx::PgPool;

const ACCOUNT_ID: &str = "account-nba-test";
const EMP_REP: &str = "emp-rep-nba";

async fn seed_refs(pool: &PgPool) {
    seed_refs_with_primary(pool, true).await;
}

/// Seed account + rep + (optionally) a primary contact. Most tests
/// call `seed_refs` so the missing-primary-contact rule stays quiet;
/// the tests that specifically exercise that rule pass `false` to
/// exclude the primary contact.
async fn seed_refs_with_primary(pool: &PgPool, include_primary: bool) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone) \
         VALUES ('loc-hq', 'HQ', 'hq', 'UTC') \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO employees \
            (id, name, email, role, department, hire_date, location, employment_type, status) \
         VALUES ($1, 'Rep', 'rep@boss.io', 'sales-rep', 'sales', '2024-01-01', \
                 'loc-hq', 'full-time', 'active')",
    )
    .bind(EMP_REP)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, territory_rep_id) \
         VALUES ($1, 'Next Actions Test', 'Dr. NBA', 'Austin', 'TX', 'silver', '2024-01-01', $2)",
    )
    .bind(ACCOUNT_ID)
    .bind(EMP_REP)
    .execute(pool)
    .await
    .unwrap();

    if include_primary {
        sqlx::query(
            "INSERT INTO account_contacts (id, account_id, name, role, email, is_primary) \
             VALUES ('cc-baseline', $1, 'Primary', 'decision-maker', 'p@example.com', true)",
        )
        .bind(ACCOUNT_ID)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn seed_agreement(pool: &PgPool, id: &str, end_date: chrono::NaiveDate, status: &str) {
    sqlx::query(
        "INSERT INTO service_agreements \
            (id, account_id, type, status, start_date, end_date, annual_value_cents, \
             billing_frequency, auto_renew, covers_parts, covers_labor, covers_travel, \
             pm_visits_per_year, response_sla_hours, owner_id) \
         VALUES ($1, $2, 'full-service', $3, $4, $5, 1200000, 'annual', true, true, true, \
                 false, 2, 48, $6)",
    )
    .bind(id)
    .bind(ACCOUNT_ID)
    .bind(status)
    .bind(end_date - Duration::days(365))
    .bind(end_date)
    .bind(EMP_REP)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_invoice(pool: &PgPool, id: &str, status: &str, due_on: chrono::NaiveDate) {
    sqlx::query(
        "INSERT INTO invoices \
            (id, account_id, issued_on, due_on, paid_on, status, amount_cents) \
         VALUES ($1, $2, $3, $4, NULL, $5, 500000)",
    )
    .bind(id)
    .bind(ACCOUNT_ID)
    .bind(due_on - Duration::days(30))
    .bind(due_on)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_account_returns_no_actions() {
    // With a primary contact seeded, a account with no agreements or
    // invoices fires zero rules.
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );

    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn account_without_primary_contact_fires_info_rule() {
    let db = TestDb::new().await;
    seed_refs_with_primary(&db.pool, false).await;
    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );

    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["rule"], "missing-primary-contact");
    assert_eq!(arr[0]["severity"], "info");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn non_primary_contacts_do_not_satisfy_the_rule() {
    // Having a contact row where is_primary=false is NOT the same
    // as having a primary contact. The rule still fires.
    let db = TestDb::new().await;
    seed_refs_with_primary(&db.pool, false).await;
    sqlx::query(
        "INSERT INTO account_contacts (id, account_id, name, role, email, is_primary) \
         VALUES ('cc-np', $1, 'Not Primary', 'office-manager', 'np@example.com', false)",
    )
    .bind(ACCOUNT_ID)
    .execute(&db.pool)
    .await
    .unwrap();

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["rule"], "missing-primary-contact");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn contract_expiring_in_10_days_fires_critical() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let today = Utc::now().date_naive();
    seed_agreement(&db.pool, "SA-CRIT", today + Duration::days(10), "active").await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["rule"], "contract-expiring");
    assert_eq!(arr[0]["severity"], "critical");
    assert!(arr[0]["headline"].as_str().unwrap().contains("10 days"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn contract_expiring_in_45_days_fires_info() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let today = Utc::now().date_naive();
    seed_agreement(&db.pool, "SA-INFO", today + Duration::days(45), "active").await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["severity"], "info");
}

#[tokio::test(flavor = "multi_thread")]
async fn healthy_long_contract_does_not_fire() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let today = Utc::now().date_naive();
    // A year from now — well outside the 60-day window.
    seed_agreement(&db.pool, "SA-OK", today + Duration::days(365), "active").await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_contract_does_not_fire_even_if_ending_soon() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let today = Utc::now().date_naive();
    // Cancelled status — rule 1 filters to status='active' only.
    seed_agreement(&db.pool, "SA-DEAD", today + Duration::days(5), "expired").await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn past_due_invoice_severity_scales_with_days_late() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let today = Utc::now().date_naive();

    // Three past-due invoices at different lateness levels.
    seed_invoice(&db.pool, "inv-info", "past-due", today - Duration::days(5)).await;
    seed_invoice(&db.pool, "inv-warn", "past-due", today - Duration::days(35)).await;
    seed_invoice(&db.pool, "inv-crit", "past-due", today - Duration::days(90)).await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    let past_due: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|a| a["rule"] == "past-due-invoice")
        .collect();
    assert_eq!(past_due.len(), 3);

    // Actions are sorted critical → warning → info. Our three invoices
    // have critical (90 days), warning (35 days), info (5 days)
    // severities. Other rules (churn-risk fires too on the same data)
    // are filtered out above so this assertion stays scoped to the
    // rule under test.
    assert_eq!(past_due[0]["severity"], "critical");
    assert_eq!(past_due[1]["severity"], "warning");
    assert_eq!(past_due[2]["severity"], "info");
}

#[tokio::test(flavor = "multi_thread")]
async fn paid_invoice_does_not_fire_past_due_rule() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let today = Utc::now().date_naive();
    // Status=paid, even with a past due_on — not a past-due invoice.
    seed_invoice(&db.pool, "inv-paid", "paid", today - Duration::days(60)).await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    // No past-due-invoice action should fire; other rules (churn-risk)
    // may fire independently on this data and aren't the subject here.
    assert!(
        arr.iter().all(|a| a["rule"] != "past-due-invoice"),
        "unexpected past-due action: {arr:?}"
    );
}

const TEST_SKU: &str = "Boss-NBA-TEST-2024";

async fn seed_asset_model(pool: &PgPool, preventive_maintenance_interval_months: i16) {
    sqlx::query(
        "INSERT INTO asset_models ( \
            sku, name, manufacturer, model_year, category, \
            regulator_device_class, \
            preventive_maintenance_interval_months, preventive_maintenance_hours, calibration_interval_months, \
            required_skill_level, depot_required, \
            list_price_new_cents, tagline, description, \
            width_cm, depth_cm, height_cm, weight_kg, power_requirements \
         ) VALUES ( \
            $1, 'NBA Test Device', 'TestCo', 2024, 'fractional-co2', \
            2, \
            $2, 2.0, 12, \
            3, false, \
            5000000, 'NBA test', 'For NBA tests', \
            50.0, 50.0, 100.0, 80.0, '120V' \
         )",
    )
    .bind(TEST_SKU)
    .bind(preventive_maintenance_interval_months)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_system(pool: &PgPool, asset_id: &str, phase: &str, first_seen: chrono::NaiveDate) {
    sqlx::query(
        "INSERT INTO assets (asset_id, sku, phase, holder_kind, holder_id, first_seen, last_event_at) \
         VALUES ($1, $2, $3, 'account', $4, $5, $5)",
    )
    .bind(asset_id)
    .bind(TEST_SKU)
    .bind(phase)
    .bind(ACCOUNT_ID)
    .bind(first_seen)
    .execute(pool)
    .await
    .unwrap();
}

/// Seed a Job whose `updated_at` was `stale_days` ago. We have to
/// override the DEFAULT NOW() explicitly because the column is
/// TIMESTAMPTZ and the rule compares `CURRENT_DATE - updated_at::date`.
async fn seed_service_job(
    pool: &PgPool,
    kind: &str,
    asset_id: &str,
    status: &str,
    stale_days: i64,
) -> uuid::Uuid {
    let job_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs \
            (id, kind, subject_kind, subject_id, title, owner_id, status, \
             priority, opened_on, updated_at) \
         VALUES ($1, $2, 'system', $3, $4, $5, $6, 'standard', \
                 CURRENT_DATE - ($7 * interval '1 day'), \
                 NOW() - ($7 * interval '1 day'))",
    )
    .bind(job_id)
    .bind(kind)
    .bind(asset_id)
    .bind(format!("Service on {asset_id}"))
    .bind(EMP_REP)
    .bind(status)
    .bind(stale_days as i32)
    .execute(pool)
    .await
    .unwrap();
    job_id
}

/// Seed a closed preventive-maintenance-visit Job with a specific closed_on date.
async fn seed_pm_visit(pool: &PgPool, asset_id: &str, closed_on: chrono::NaiveDate) {
    sqlx::query(
        "INSERT INTO jobs \
            (id, kind, subject_kind, subject_id, title, owner_id, status, \
             priority, opened_on, closed_on) \
         VALUES ($1, 'preventive-maintenance-visit', 'system', $2, $3, $4, 'closed', \
                 'scheduled', $5, $5)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(asset_id)
    .bind(format!("preventive maintenance on {asset_id}"))
    .bind(EMP_REP)
    .bind(closed_on)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn stalled_ticket_severity_scales_with_days_idle() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    seed_asset_model(&db.pool, 12).await;
    seed_system(
        &db.pool,
        "SN-STALL-INFO",
        "installed",
        Utc::now().date_naive() - Duration::days(200),
    )
    .await;
    seed_system(
        &db.pool,
        "SN-STALL-WARN",
        "out-for-service",
        Utc::now().date_naive() - Duration::days(200),
    )
    .await;
    seed_system(
        &db.pool,
        "SN-STALL-CRIT",
        "out-for-service",
        Utc::now().date_naive() - Duration::days(200),
    )
    .await;
    seed_service_job(&db.pool, "field-service", "SN-STALL-INFO", "open", 3).await;
    seed_service_job(&db.pool, "repair", "SN-STALL-WARN", "open", 8).await;
    seed_service_job(&db.pool, "depot-return", "SN-STALL-CRIT", "open", 20).await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    let stalled: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|a| a["rule"] == "stalled-ticket")
        .collect();
    assert_eq!(stalled.len(), 3);

    // Sorted by severity (critical → warning → info) by the outer handler.
    let sevs: Vec<&str> = stalled
        .iter()
        .map(|a| a["severity"].as_str().unwrap())
        .collect();
    assert_eq!(sevs, ["critical", "warning", "info"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_ticket_ignores_fresh_and_closed_jobs() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    seed_asset_model(&db.pool, 12).await;
    seed_system(
        &db.pool,
        "SN-FRESH",
        "installed",
        Utc::now().date_naive() - Duration::days(30),
    )
    .await;
    seed_system(
        &db.pool,
        "SN-CLOSED",
        "installed",
        Utc::now().date_naive() - Duration::days(30),
    )
    .await;
    // Stale enough but already closed — must not fire.
    seed_service_job(&db.pool, "field-service", "SN-CLOSED", "closed", 20).await;
    // Correct kind + status, but 0 days idle — under the 3-day floor.
    seed_service_job(&db.pool, "repair", "SN-FRESH", "open", 0).await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    assert!(
        arr.iter().all(|a| a["rule"] != "stalled-ticket"),
        "unexpected stalled-ticket action: {arr:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn pm_visit_due_fires_for_installed_device_with_no_history() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    seed_asset_model(&db.pool, 12).await;
    // Installed 400 days ago, no preventive-maintenance-visit Jobs — overdue past the
    // 12-month window but not past 1.5× (= 18 months / 540 days),
    // so severity should be Info.
    seed_system(
        &db.pool,
        "SN-NO-preventive maintenance",
        "installed",
        Utc::now().date_naive() - Duration::days(400),
    )
    .await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    let preventive_maintenance: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|a| a["rule"] == "preventive-maintenance-due")
        .collect();
    assert_eq!(preventive_maintenance.len(), 1);
    assert_eq!(preventive_maintenance[0]["severity"], "info");
    assert!(
        preventive_maintenance[0]["headline"]
            .as_str()
            .unwrap()
            .contains("no preventive maintenance on record")
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn pm_visit_due_fires_warning_when_more_than_1_5x_overdue() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    seed_asset_model(&db.pool, 6).await;
    // 6-month interval, 1.5× = 270 days. Installed 400 days ago —
    // well past 1.5× without a preventive maintenance recorded.
    seed_system(
        &db.pool,
        "SN-WARN-preventive maintenance",
        "installed",
        Utc::now().date_naive() - Duration::days(400),
    )
    .await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    let preventive_maintenance: Vec<&serde_json::Value> = arr
        .iter()
        .filter(|a| a["rule"] == "preventive-maintenance-due")
        .collect();
    assert_eq!(preventive_maintenance.len(), 1);
    assert_eq!(preventive_maintenance[0]["severity"], "warning");
}

#[tokio::test(flavor = "multi_thread")]
async fn recent_pm_visit_suppresses_rule() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    seed_asset_model(&db.pool, 12).await;
    seed_system(
        &db.pool,
        "SN-RECENT-preventive maintenance",
        "installed",
        Utc::now().date_naive() - Duration::days(800),
    )
    .await;
    // A closed preventive-maintenance-visit 30 days ago — well within the 12-month window.
    seed_pm_visit(
        &db.pool,
        "SN-RECENT-preventive maintenance",
        Utc::now().date_naive() - Duration::days(30),
    )
    .await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    assert!(
        arr.iter()
            .all(|a| a["rule"] != "preventive-maintenance-due"),
        "unexpected preventive-maintenance-due action: {arr:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pm_visit_rule_only_considers_installed_or_in_service_devices() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    seed_asset_model(&db.pool, 12).await;
    // Device in "ready" phase (hasn't shipped yet) — must not fire preventive maintenance.
    seed_system(
        &db.pool,
        "SN-READY",
        "ready",
        Utc::now().date_naive() - Duration::days(400),
    )
    .await;
    // Decommissioned device — must not fire preventive maintenance.
    seed_system(
        &db.pool,
        "SN-DECOM",
        "decommissioned",
        Utc::now().date_naive() - Duration::days(400),
    )
    .await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    assert!(
        arr.iter()
            .all(|a| a["rule"] != "preventive-maintenance-due"),
        "unexpected preventive-maintenance-due action: {arr:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "depends on ml_predictions populated by infer-batch — see file header"]
async fn multiple_rules_fire_and_sort_by_severity() {
    let db = TestDb::new().await;
    seed_refs(&db.pool).await;
    let today = Utc::now().date_naive();

    // Info-severity contract + critical past-due invoice. Critical
    // should come first despite being added later and having a later
    // due_on.
    seed_agreement(&db.pool, "SA-INFO", today + Duration::days(45), "active").await;
    seed_invoice(&db.pool, "inv-crit", "past-due", today - Duration::days(90)).await;

    let app = next_actions_router(
        db.pool.clone(),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    );
    let resp = TestRequest::get(format!("/api/people/accounts/{ACCOUNT_ID}/next-actions"))
        .as_user("emp-ceo", "ceo")
        .send(&app)
        .await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body_bytes).unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["rule"], "past-due-invoice");
    assert_eq!(arr[0]["severity"], "critical");
    assert_eq!(arr[1]["rule"], "contract-expiring");
    assert_eq!(arr[1]["severity"], "info");
}
