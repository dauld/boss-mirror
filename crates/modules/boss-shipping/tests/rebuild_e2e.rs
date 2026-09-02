//! End-to-end: drive shipping writes through PgShipping on the REAL
//! pipeline (outbox phase 2 — events record in the domain tx, the
//! relay drain moves them to audit_log), snapshot `shipments` +
//! `shipment_assets`, drop, rebuild from `audit_log`, assert exact
//! match.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::port::EventBus;
use boss_events::outbox::drain_outbox_once;
use boss_shipping::PgShipping;
use boss_shipping::http::{ShippingApiState, router};
use boss_shipping::rebuild_shipping;
use boss_shipping::types::*;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ShipmentRow {
    id: String,
    direction: String,
    status: String,
    carrier: Option<String>,
    tracking_number: Option<String>,
    origin: String,
    destination: String,
    po_id: Option<String>,
    order_id: Option<String>,
    account_id: Option<String>,
    created_on: chrono::NaiveDate,
    shipped_on: Option<chrono::NaiveDate>,
    estimated_delivery: Option<chrono::NaiveDate>,
    delivered_on: Option<chrono::NaiveDate>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct ShipmentSystemRow {
    shipment_id: String,
    asset_id: String,
}

async fn snapshot_shipments(pool: &PgPool) -> Vec<ShipmentRow> {
    sqlx::query_as("SELECT id, direction, status, carrier, tracking_number, origin, destination, po_id, order_id, account_id, created_on, shipped_on, estimated_delivery, delivered_on, created_at, updated_at FROM shipments ORDER BY id")
        .fetch_all(pool).await.unwrap()
}
async fn snapshot_shipment_systems(pool: &PgPool) -> Vec<ShipmentSystemRow> {
    sqlx::query_as(
        "SELECT shipment_id, asset_id FROM shipment_assets ORDER BY shipment_id, asset_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn build_app(pool: PgPool) -> Router {
    // No publisher, no direct audit writer: events reach audit_log
    // only via the outbox → relay drain (`drain_outbox`).
    let state = ShippingApiState {
        shipping: Arc::new(PgShipping::new(pool)),
        publisher: None,
        classes_client: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

/// Drain the outbox through the relay pipeline into audit_log.
async fn drain_outbox(pool: &PgPool) -> u64 {
    let bus = RecordingEventBus::new();
    drain_outbox_once(pool, &(bus as Arc<dyn EventBus>), 100)
        .await
        .expect("relay drain")
        .delivered
}

fn fixture(id: &str, status: ShipmentStatus, systems: Vec<&str>) -> Shipment {
    Shipment {
        id: id.to_string(),
        direction: ShipmentDirection::Outbound,
        status,
        carrier: Some(Carrier::new("fedex")),
        tracking_number: Some(format!("1Z{id}")),
        origin: "HQ Warehouse".into(),
        destination: "Customer Alpha".into(),
        asset_ids: systems.into_iter().map(String::from).collect(),
        po_id: None,
        order_id: Some(format!("ORD-{id}")),
        account_id: Some("acc-001".into()),
        created_on: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
        shipped_on: Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 2).unwrap()),
        estimated_delivery: Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 5).unwrap()),
        delivered_on: None,
        line_items: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_shipments_and_systems() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    // 1. Two shipments — one with multiple systems, one with one.
    let s1 = fixture(
        "ship-001",
        ShipmentStatus::PICKED_UP.into(),
        vec!["SYS-A", "SYS-B"],
    );
    let s2 = fixture("ship-002", ShipmentStatus::IN_TRANSIT.into(), vec!["SYS-C"]);
    for s in [&s1, &s2] {
        TestRequest::post("/api/shipping/shipments")
            .json(s)
            .send(&app)
            .await
            .assert_status(StatusCode::CREATED);
    }

    // 2. Update s1 — advance status, change asset_ids list.
    let mut s1_updated = s1.clone();
    s1_updated.status = ShipmentStatus::DELIVERED.into();
    s1_updated.delivered_on = Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 4).unwrap());
    s1_updated.asset_ids = vec!["SYS-A".into(), "SYS-D".into()];
    TestRequest::put(format!("/api/shipping/shipments/{}", s1.id))
        .json(&s1_updated)
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // 3. Drain the outbox into audit_log, then snapshot.
    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 3, "2 creates + 1 update arrive via the outbox");
    let shipments_before = snapshot_shipments(&db.pool).await;
    let systems_before = snapshot_shipment_systems(&db.pool).await;
    assert_eq!(shipments_before.len(), 2);
    assert_eq!(systems_before.len(), 3, "ship-001: 2 + ship-002: 1");

    // 4. Audit_log has 3 events (2 created + 1 updated).
    let event_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE kind LIKE 'shipping.shipment.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(event_count.0, 3, "got {} events", event_count.0);

    // 5. Wipe + rebuild.
    sqlx::query("DELETE FROM shipment_assets")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM shipments")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_shipping(&db.pool).await.expect("rebuild succeeds");
    assert_eq!(report.shipments_upserted, 3, "2 created + 1 updated");

    // 6. Reconstructed projections must match originals exactly.
    let shipments_after = snapshot_shipments(&db.pool).await;
    let systems_after = snapshot_shipment_systems(&db.pool).await;
    assert_eq!(shipments_before, shipments_after, "shipments mismatch");
    assert_eq!(systems_before, systems_after, "shipment_assets mismatch");
}

/// Tracking-scan rows and the status rollup they trigger must replay
/// byte-identical. Before packet d7b8158e both sides leaked wall
/// time: the live scan row's `created_at` fell to the column DEFAULT
/// NOW(), the live rollup stamped `updated_at = NOW()`, and the
/// rebuild re-stamped both at replay time — three different instants
/// for one recorded fact.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct TrackingEventRow {
    // `id` (BIGSERIAL) deliberately excluded: TRUNCATE does not
    // restart the sequence, so replayed rows draw fresh serials.
    shipment_id: String,
    status: String,
    occurred_on: chrono::NaiveDate,
    stage_index: Option<i16>,
    detail: Option<String>,
    created_at: DateTime<Utc>,
}

async fn snapshot_tracking_events(pool: &PgPool) -> Vec<TrackingEventRow> {
    sqlx::query_as(
        "SELECT shipment_id, status, occurred_on, stage_index, detail, created_at \
         FROM shipment_tracking_events ORDER BY shipment_id, occurred_on, status",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_tracking_scans_byte_identical() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    let s = fixture(
        "ship-scan",
        ShipmentStatus::LABEL_CREATED.into(),
        vec!["SYS-T"],
    );
    TestRequest::post("/api/shipping/shipments")
        .json(&s)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Two carrier scans through the port — an in-transit ping and the
    // final delivery. The delivered scan also rolls up the shipment's
    // status + delivered_on + updated_at.
    let shipping = PgShipping::new(db.pool.clone());
    let stamp = boss_core::publisher::EventStamp::new(
        "shipping",
        boss_core::actor::ActorId::Automation("test".into()),
    );
    boss_shipping::port::ShippingRepository::record_tracking_scan(
        &shipping,
        "ship-scan",
        "in-transit",
        chrono::NaiveDate::from_ymd_opt(2026, 4, 3).unwrap(),
        Some(1),
        &stamp,
    )
    .await
    .expect("in-transit scan");
    boss_shipping::port::ShippingRepository::record_tracking_scan(
        &shipping,
        "ship-scan",
        "delivered",
        chrono::NaiveDate::from_ymd_opt(2026, 4, 5).unwrap(),
        Some(2),
        &stamp,
    )
    .await
    .expect("delivered scan");

    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 3, "create + 2 scans arrive via the outbox");

    let shipments_before = snapshot_shipments(&db.pool).await;
    let scans_before = snapshot_tracking_events(&db.pool).await;
    assert_eq!(scans_before.len(), 2);
    assert_eq!(shipments_before[0].status, "delivered");

    // Rebuild wipes all four shipping projections and replays.
    rebuild_shipping(&db.pool).await.expect("rebuild");

    let shipments_after = snapshot_shipments(&db.pool).await;
    let scans_after = snapshot_tracking_events(&db.pool).await;
    assert_eq!(
        shipments_before, shipments_after,
        "shipments must replay byte-identical (updated_at included)"
    );
    assert_eq!(
        scans_before, scans_after,
        "tracking scans must replay byte-identical (created_at included)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_handles_shipment_delete() {
    let db = TestDb::new().await;
    let app = build_app(db.pool.clone());

    let s = fixture(
        "ship-doomed",
        ShipmentStatus::LABEL_CREATED.into(),
        vec!["SYS-X"],
    );
    TestRequest::post("/api/shipping/shipments")
        .json(&s)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::delete(format!("/api/shipping/shipments/{}", s.id))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let delivered = drain_outbox(&db.pool).await;
    assert_eq!(delivered, 2, "create + delete arrive via the outbox");

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM shipments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);

    let report = rebuild_shipping(&db.pool).await.unwrap();
    assert!(report.shipments_upserted >= 1);
    assert!(report.shipments_deleted >= 1);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM shipments")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "rebuild should reproduce post-delete state");
}
