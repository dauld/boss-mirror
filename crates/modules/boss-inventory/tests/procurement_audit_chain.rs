//! Audit-chain test for the vendor-CRM surfaces (procurement
//! sub-router under boss-inventory).
//!
//! The four procurement entity surfaces (vendor_contacts,
//! vendor_interactions, vendor_account_team, vendor_contracts) must
//! record an audit event for every write — on the transactional
//! outbox, inside the write's own transaction (outbox phase 2); the
//! relay drain moves it to audit_log. Without it a
//! `boss-rebuild-all` cycle would lose every row — `rebuild_inventory`
//! replays those four kinds, and the projections are otherwise wiped
//! via the ON DELETE CASCADE on vendors.
//!
//! This test exercises one write per surface on the REAL pipeline
//! (write → outbox → relay drain → audit_log), runs
//! `rebuild_inventory`, and asserts every row reappears from
//! `audit_log` alone.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_inventory::procurement::http::{ProcurementApiState, router as procurement_router};
use boss_inventory::rebuild_inventory;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use serde_json::json;
use sqlx::PgPool;

async fn seed_vendor_and_employee(pool: &PgPool) {
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
         VALUES ('emp-buyer-1', 'Buyer', 'buyer@boss.example', 'sourcing-mgr', 'procurement', '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();

    // Mirror the employee creation into audit_log so the people
    // rebuilder (which TRUNCATEs employees CASCADE) reproduces
    // the FK target if a downstream test ever calls it. Inventory
    // rebuild wipes vendors but the FK from vendor_account_team to
    // employees stays in the projection, so we only need this
    // employee available at rebuild time.
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), NOW(), 'people', 'people.employee.created', $1)",
    )
    .bind(json!({
        "id": "emp-buyer-1",
        "name": "Buyer",
        "email": "buyer@boss.example",
        "role": "sourcing-mgr",
        "department": "procurement",
        "skills": [],
        "hire_date": "2024-01-15",
        "location": "loc-test",
        "employment_type": "full-time",
        "status": "active",
        "certifications": [],
    }))
    .execute(pool)
    .await
    .unwrap();

    // Vendor projection row + matching audit_log event so
    // rebuild_inventory has the parent FK target to replay before
    // it processes the procurement events.
    let vendor_payload = json!({
        "id": "vnd-acme",
        "name": "Acme Hops",
        "contact_name": "Pat Acme",
        "contact_email": "pat@acme.example",
        "city": "Yakima",
        "state": "WA",
        "lead_time_days": 14,
        "payment_terms": "net-30",
        "category": "hops",
    });
    sqlx::query(
        "INSERT INTO vendors (id, name, contact_name, contact_email, city, state, lead_time_days, payment_terms, category, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind("vnd-acme")
    .bind("Acme Hops")
    .bind("Pat Acme")
    .bind("pat@acme.example")
    .bind("Yakima")
    .bind("WA")
    .bind(14_i16)
    .bind("net-30")
    .bind("hops")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), NOW(), 'inventory', 'inventory.vendor.created', $1)",
    )
    .bind(&vendor_payload)
    .execute(pool)
    .await
    .unwrap();
}

fn build_app(pool: PgPool) -> Router {
    // No publisher: the handler stamps fall back to
    // source="inventory" and the adapters record on the outbox —
    // there is deliberately NO direct audit writer here, so the test
    // can only pass through the real outbox → relay → audit_log path.
    procurement_router(ProcurementApiState {
        pool,
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn vendor_crm_writes_survive_rebuild() {
    let db = TestDb::new().await;
    seed_vendor_and_employee(&db.pool).await;
    let app = build_app(db.pool.clone());

    // Exercise one write per surface.
    TestRequest::post("/api/inventory/vendors/vnd-acme/contacts")
        .json(&json!({
            "id": "vc-1", "vendor_id": "vnd-acme",
            "name": "Pat Acme", "role": "sales-rep",
            "email": "pat@acme.example", "phone": "555-0101",
            "territory": "PNW", "specialties": ["citra", "mosaic"],
            "is_primary": true, "relationship_start": "2024-01-15",
            "notes": "Primary buyer",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    TestRequest::post("/api/inventory/vendors/vnd-acme/interactions")
        .json(&json!({
            "id": "vi-1", "vendor_id": "vnd-acme",
            "vendor_contact_id": "vc-1",
            "actor_id": "emp-buyer-1",
            "kind": "call",
            "body": "Q3 hop allocations confirmed",
            "commitments": [],
            "linked_po_id": null, "linked_part_sku": null,
            "linked_job_id": null,
            "occurred_at": "2026-05-04T10:00:00Z",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    TestRequest::post("/api/inventory/vendors/vnd-acme/account-team")
        .json(&json!({
            "id": "vat-1", "vendor_id": "vnd-acme",
            "employee_id": "emp-buyer-1",
            "role": "primary",
            "assigned_on": "2024-01-15",
            "notes": "Owns the hops relationship",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    TestRequest::post("/api/inventory/vendors/vnd-acme/contracts")
        .json(&json!({
            "id": "vk-1", "vendor_id": "vnd-acme",
            "kind": "master-supply", "title": "FY26 Master Supply",
            "effective_on": "2026-01-01", "expires_on": "2026-12-31",
            "auto_renew": true,
            "terms": {"payment": "net-30", "rebate_pct": 2},
            "document_uri": null, "status": "active",
            "signed_by_employee_id": "emp-buyer-1",
            "signed_at": "2025-12-15T15:30:00Z",
            "notes": null,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    // Drain the outbox through the relay pipeline: outbox →
    // audit_log (chained) → bus → delivered. Rebuild reads audit_log.
    let bus = RecordingEventBus::new();
    let stats = drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");
    assert_eq!(stats.delivered, 4, "one event per CRM surface");

    // Pre-rebuild snapshot.
    let pre_contacts: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM vendor_contacts WHERE vendor_id = 'vnd-acme'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let pre_inter: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM vendor_interactions WHERE vendor_id = 'vnd-acme'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let pre_team: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM vendor_account_team WHERE vendor_id = 'vnd-acme'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let pre_contracts: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM vendor_contracts WHERE vendor_id = 'vnd-acme'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(pre_contacts.0, 1);
    assert_eq!(pre_inter.0, 1);
    assert_eq!(pre_team.0, 1);
    assert_eq!(pre_contracts.0, 1);

    // Rebuild and verify every row round-trips.
    let report = rebuild_inventory(&db.pool).await.expect("rebuild");
    assert!(report.vendor_contacts_upserted >= 1, "{report:?}");
    assert!(report.vendor_interactions_recorded >= 1, "{report:?}");
    assert!(report.vendor_team_assigned >= 1, "{report:?}");
    assert!(report.vendor_contracts_upserted >= 1, "{report:?}");

    let post_contacts: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM vendor_contacts WHERE vendor_id = 'vnd-acme'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let post_inter: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM vendor_interactions WHERE vendor_id = 'vnd-acme'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let post_team: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM vendor_account_team WHERE vendor_id = 'vnd-acme'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let post_contracts: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM vendor_contracts WHERE vendor_id = 'vnd-acme'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(post_contacts, pre_contacts);
    assert_eq!(post_inter, pre_inter);
    assert_eq!(post_team, pre_team);
    assert_eq!(post_contracts, pre_contracts);
}

/// Soft-deleted interactions must replay byte-identical, `deleted_at`
/// included. The live path used to stamp `deleted_at = NOW()` at the
/// database while the event payload carried the stamp's timestamp —
/// two different instants for one recorded fact, so the rebuilt row
/// never matched the live one (packet d7b8158e).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
struct InteractionRow {
    id: String,
    vendor_id: String,
    actor_id: String,
    kind: String,
    body: String,
    occurred_at: chrono::DateTime<chrono::Utc>,
    created_at: chrono::DateTime<chrono::Utc>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    deleted_by: Option<String>,
}

async fn snapshot_interactions(pool: &PgPool) -> Vec<InteractionRow> {
    sqlx::query_as(
        "SELECT id, vendor_id, actor_id, kind, body, occurred_at, created_at, \
                deleted_at, deleted_by \
         FROM vendor_interactions ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn soft_deleted_interaction_replays_byte_identical() {
    let db = TestDb::new().await;
    seed_vendor_and_employee(&db.pool).await;
    let app = build_app(db.pool.clone());

    for id in ["vi-keep", "vi-gone"] {
        TestRequest::post("/api/inventory/vendors/vnd-acme/interactions")
            .json(&json!({
                "id": id, "vendor_id": "vnd-acme",
                "vendor_contact_id": null,
                "actor_id": "emp-buyer-1",
                "kind": "email",
                "body": "Delivery window confirmed",
                "commitments": [],
                "linked_po_id": null, "linked_part_sku": null,
                "linked_job_id": null,
                "occurred_at": "2026-05-06T09:00:00Z",
            }))
            .send(&app)
            .await
            .assert_status(StatusCode::OK);
    }

    TestRequest::delete("/api/inventory/vendors/vnd-acme/interactions/vi-gone")
        .json(&json!({"deleted_by": "emp-buyer-1"}))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let bus = RecordingEventBus::new();
    drain_outbox_once(&db.pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain");

    let before = snapshot_interactions(&db.pool).await;
    assert_eq!(before.len(), 2);
    assert!(
        before
            .iter()
            .any(|r| r.id == "vi-gone" && r.deleted_at.is_some()),
        "soft delete stamped"
    );

    rebuild_inventory(&db.pool).await.expect("rebuild");

    let after = snapshot_interactions(&db.pool).await;
    assert_eq!(
        before, after,
        "vendor_interactions must replay byte-identical (deleted_at included)"
    );
}
