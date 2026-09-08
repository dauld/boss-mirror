//! End-to-end: drive accounts writes through the API, snapshot
//! `accounts` + `account_contacts`, drop them, rebuild from
//! `audit_log`, assert exact match.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_accounts::accounts::accounts_router;
use boss_accounts::rebuild_accounts;
use boss_assets_client::FakeAssetsClient;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::NaiveDate;
use sqlx::PgPool;

/// Drain the outbox through the relay pipeline into audit_log.
async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn boss_core::port::EventBus>), 100)
        .await
        .expect("relay drain");
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct AccountRow {
    id: String,
    name: String,
    director: String,
    city: String,
    state: String,
    tier: String,
    customer_since: NaiveDate,
    territory_rep_id: String,
    account_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ContactRow {
    id: String,
    account_id: String,
    name: String,
    role: String,
    email: String,
    phone: Option<String>,
    is_primary: bool,
    // Included in the byte-identity check: before packet d7b8158e
    // this column fell to the DEFAULT NOW() on both the live insert
    // and the replay, so a rebuild silently re-dated every contact.
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn snapshot_accounts(pool: &PgPool) -> Vec<AccountRow> {
    sqlx::query_as("SELECT id, name, director, city, state, tier, customer_since, territory_rep_id, account_type FROM accounts ORDER BY id")
        .fetch_all(pool).await.unwrap()
}

async fn snapshot_contacts(pool: &PgPool) -> Vec<ContactRow> {
    sqlx::query_as("SELECT id, account_id, name, role, email, phone, is_primary, created_at FROM account_contacts ORDER BY id")
        .fetch_all(pool).await.unwrap()
}

async fn build_app(pool: PgPool) -> Router {
    // No publisher, no direct audit writer: events reach audit_log
    // only via the outbox -> relay drain.
    accounts_router(
        pool,
        None,
        Arc::new(FakeAssetsClient::with_count(0)),
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    )
}

/// Create the territory-rep employee that the FK on
/// `account_team_members.employee_id` requires.
async fn seed_employee(pool: &PgPool, id: &str) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-test', 'Test Location', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO employees (id, name, email, role, department, hire_date, location, employment_type, status, manager_id) \
         VALUES ($1, 'Pat Tester', $2, 'territory-rep', 'sales', '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(format!("{id}@boss.example"))
    .execute(pool)
    .await
    .unwrap();
}

fn body(
    id: &str,
    name: &str,
    account_type: &str,
    contacts: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "director": "Dr. Test",
        "city": "Austin",
        "state": "TX",
        "tier": "gold",
        "customer_since": "2025-06-01",
        "territory_rep_id": "emp-rep-001",
        "account_type": account_type,
        "contacts": contacts,
    })
}

fn contact(id: &str, account: &str, role: &str, primary: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "account_id": account,
        "name": format!("Contact {id}"),
        "role": role,
        "email": format!("{id}@example.com"),
        "phone": "555-0100",
        "is_primary": primary,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_accounts_and_contacts() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-001").await;
    let app = build_app(db.pool.clone()).await;

    // 1. Create two accounts, each with two contacts.
    TestRequest::post("/api/people/accounts")
        .json(&body(
            "acc-001",
            "Hopswell Brewing",
            "wholesale-distributor",
            serde_json::json!([
                contact("contact-001", "acc-001", "buyer", true),
                contact("contact-002", "acc-001", "accounts-payable", false),
            ]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/accounts")
        .json(&body(
            "acc-002",
            "Maltworks Co.",
            "bar-restaurant",
            serde_json::json!([contact("contact-003", "acc-002", "buyer", true)]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 2. Update acc-001 — change name and contact list.
    TestRequest::put("/api/people/accounts/acc-001")
        .json(&body(
            "acc-001",
            "Hopswell Brewing (Renamed)",
            "wholesale-distributor",
            serde_json::json!([contact("contact-099", "acc-001", "buyer", true)]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 3. Snapshot.
    let accounts_before = snapshot_accounts(&db.pool).await;
    let contacts_before = snapshot_contacts(&db.pool).await;
    assert_eq!(accounts_before.len(), 2);
    assert_eq!(contacts_before.len(), 2, "1 from acc-001 + 1 from acc-002");
    assert_eq!(accounts_before[0].name, "Hopswell Brewing (Renamed)");
    assert_eq!(accounts_before[0].account_type, "wholesale-distributor");

    // 4. Drain the outbox into audit_log, then verify the events.
    drain_outbox(&db.pool).await;
    let event_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind LIKE 'accounts.account.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    // 2 created + 1 updated = 3 account events, plus 3 territory-rep
    // mirror events (one per create/update via mirror_territory_rep)
    // = 6 total. The team-assigned events ship with each account
    // mutation as of 2026-05-04 to keep `account_team_members`
    // rebuildable from audit_log alone.
    assert_eq!(event_count.0, 6, "got {} events", event_count.0);

    // 5. Wipe + rebuild.
    sqlx::query("DELETE FROM account_contacts")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM accounts")
        .execute(&db.pool)
        .await
        .unwrap();

    drain_outbox(&db.pool).await;
    let report = rebuild_accounts(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.accounts_upserted, 3, "2 created + 1 updated");

    // 6. Reconstructed projections must match originals exactly.
    let accounts_after = snapshot_accounts(&db.pool).await;
    let contacts_after = snapshot_contacts(&db.pool).await;
    assert_eq!(accounts_before, accounts_after, "accounts mismatch");
    assert_eq!(contacts_before, contacts_after, "account_contacts mismatch");
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_handles_account_delete() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-001").await;
    let app = build_app(db.pool.clone()).await;

    TestRequest::post("/api/people/accounts")
        .json(&body(
            "acc-doomed",
            "Doomed Inc",
            "unspecified",
            serde_json::json!([]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::delete("/api/people/accounts/acc-doomed")
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);

    drain_outbox(&db.pool).await;
    let report = rebuild_accounts(&db.pool).await.unwrap();
    assert!(report.accounts_upserted >= 1);
    assert!(report.accounts_deleted >= 1);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "rebuild should reproduce post-delete state");
}

// ---------------------------------------------------------------------------
// Children byte-identity (packet d7b8158e): account_notes,
// account_team_members and support_cases each carry a `created_at`
// that used to fall to the DEFAULT NOW() on both the live insert and
// the replay — so a rebuild silently re-dated every child row.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct NoteRow {
    id: String,
    account_id: String,
    actor_id: String,
    kind: String,
    body: String,
    occurred_at: chrono::DateTime<chrono::Utc>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct TeamRow {
    id: String,
    account_id: String,
    employee_id: String,
    role: String,
    assigned_on: NaiveDate,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct CaseRow {
    id: String,
    account_id: String,
    channel: String,
    category: String,
    subject: String,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn build_full_app(pool: PgPool) -> Router {
    let clock: Arc<dyn boss_clock_client::ClockClient> =
        Arc::new(boss_clock_client::WallClockClient);
    accounts_router(
        pool.clone(),
        None,
        Arc::new(FakeAssetsClient::with_count(0)),
        clock.clone(),
        None,
    )
    .merge(boss_accounts::account_notes::account_notes_router(
        pool.clone(),
        None,
        clock.clone(),
        None,
    ))
    .merge(boss_accounts::account_team_members::account_team_router(
        pool.clone(),
        None,
        clock.clone(),
        None,
    ))
    .merge(boss_accounts::support_cases::support_cases_router(
        pool, None, clock,
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_account_children_byte_identical() {
    let db = TestDb::new().await;
    seed_employee(&db.pool, "emp-rep-001").await;
    let app = build_full_app(db.pool.clone()).await;

    TestRequest::post("/api/people/accounts")
        .json(&body(
            "acc-kids",
            "Kids & Co",
            "wholesale-distributor",
            serde_json::json!([contact("ct-kid-1", "acc-kids", "buyer", true)]),
        ))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/accounts/acc-kids/notes")
        .json(&serde_json::json!({
            "kind": "call",
            "body": "Quarterly pricing call",
            "actor_id": "emp-rep-001",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/accounts/acc-kids/account-team")
        .json(&serde_json::json!({
            "employee_id": "emp-rep-001",
            "role": "customer-success",
            "actor_id": "emp-rep-001",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/support-cases")
        .json(&serde_json::json!({
            "id": "sc-kids-1",
            "account_id": "acc-kids",
            "channel": "phone",
            "category": "billing",
            "subject": "Disputed invoice",
            "body": "Customer disputes the latest invoice line item.",
            "opened_on": "2026-05-04",
            "assignee_id": null,
            "status": "open",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    drain_outbox(&db.pool).await;

    let notes_before: Vec<NoteRow> = sqlx::query_as(
        "SELECT id, account_id, actor_id, kind, body, occurred_at, created_at \
         FROM account_notes ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let team_before: Vec<TeamRow> = sqlx::query_as(
        "SELECT id, account_id, employee_id, role, assigned_on, created_at \
         FROM account_team_members ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let cases_before: Vec<CaseRow> = sqlx::query_as(
        "SELECT id, account_id, channel, category, subject, status, created_at \
         FROM support_cases ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let contacts_before = snapshot_contacts(&db.pool).await;
    // note posted directly + interaction note auto-posted by the
    // team assign; team has territory-rep mirror + customer-success.
    assert_eq!(notes_before.len(), 2);
    assert_eq!(team_before.len(), 2);
    assert_eq!(cases_before.len(), 1);
    assert_eq!(contacts_before.len(), 1);

    rebuild_accounts(&db.pool).await.expect("rebuild");

    let notes_after: Vec<NoteRow> = sqlx::query_as(
        "SELECT id, account_id, actor_id, kind, body, occurred_at, created_at \
         FROM account_notes ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let team_after: Vec<TeamRow> = sqlx::query_as(
        "SELECT id, account_id, employee_id, role, assigned_on, created_at \
         FROM account_team_members ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let cases_after: Vec<CaseRow> = sqlx::query_as(
        "SELECT id, account_id, channel, category, subject, status, created_at \
         FROM support_cases ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let contacts_after = snapshot_contacts(&db.pool).await;

    assert_eq!(
        notes_before, notes_after,
        "account_notes must replay byte-identical (created_at included)"
    );
    assert_eq!(
        team_before, team_after,
        "account_team_members must replay byte-identical (created_at included)"
    );
    assert_eq!(
        cases_before, cases_after,
        "support_cases must replay byte-identical (created_at included)"
    );
    assert_eq!(
        contacts_before, contacts_after,
        "account_contacts must replay byte-identical (created_at included)"
    );
}
