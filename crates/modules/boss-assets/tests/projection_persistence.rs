//! Integration tests proving `PgAssets::append` keeps the `devices`
//! projection in lockstep with the `device_events` log.
//!
//! Background: before this work, `PgAssets::append` only inserted into
//! `device_events`. The `devices` table was populated by an old TS
//! seed script and was never written by the live HTTP path. The replay
//! verification script caught the gap (devices: 0 rows, expected
//! 100K+) after a fresh Rust-driven replay.
//!
//! These tests lock in the new contract:
//!   - Every `append` re-projects the serial and upserts the row.
//!   - The upsert happens in the same transaction as the event insert.
//!   - `rebuild_projection` recovers a stale or empty projection table
//!     from the event log alone.

use boss_assets::PgAssets;
use boss_assets::port::AssetsRepository;
use boss_assets::types::{
    AssetEvent, AssetEventId, AssetEventKind, AssetId, AssetLifecyclePhase, IntakeSource,
};
use boss_testing::TestDb;
use chrono::NaiveDate;
use sqlx::PgPool;

const TEST_SKU: &str = "Boss-PROJ-TEST-2024";

async fn seed_device_model(pool: &PgPool, sku: &str) {
    sqlx::query(
        "INSERT INTO asset_models ( \
            sku, name, manufacturer, model_year, category, \
            regulator_device_class, \
            preventive_maintenance_interval_months, preventive_maintenance_hours, calibration_interval_months, \
            required_skill_level, depot_required, \
            list_price_new_cents, tagline, description, \
            width_cm, depth_cm, height_cm, weight_kg, power_requirements \
         ) VALUES ( \
            $1, $2, $3, $4, $5, \
            $6, \
            $7, $8, $9, \
            $10, $11, \
            $12, $13, $14, \
            $15, $16, $17, $18, $19 \
         )",
    )
    .bind(sku)
    .bind(format!("{sku} Test Device"))
    .bind("TestCo")
    .bind(2024_i32)
    .bind("fractional-co2")
    .bind(2_i32)
    .bind(6_i32)
    .bind(2.0_f32)
    .bind(12_i32)
    .bind(3_i32)
    .bind(false)
    .bind(5_000_000_i64)
    .bind("Test device")
    .bind("For projection tests")
    .bind(50.0_f32)
    .bind(50.0_f32)
    .bind(100.0_f32)
    .bind(80.0_f32)
    .bind("120V")
    .execute(pool)
    .await
    .expect("insert device_model");
}

fn evt(id: &str, serial: &str, day: u32, kind: AssetEventKind) -> AssetEvent {
    AssetEvent {
        id: AssetEventId::new(id),
        asset_id: AssetId::new(serial),
        ts: NaiveDate::from_ymd_opt(2026, 1, day).unwrap(),
        actor_id: boss_core::actor::ActorId::Automation("test".into()),
        kind,
    }
}

/// The lifecycle vocabulary lives twice — the phases the projector can
/// emit (`AssetLifecyclePhase::ORDER`) and the Class rows the migrations
/// leave ACTIVE under `(asset, phase)` — so it gets an equality test
/// (CLAUDE.md §9a). Until backlog a8991c86 retired the used-device
/// shop's refurb pipeline, four of the ten rows (triaging, refurbing,
/// qa, ready) named phases only that tenant's events could reach.
#[tokio::test(flavor = "multi_thread")]
async fn the_active_phase_classes_are_the_phases_the_projector_emits() {
    let db = TestDb::new().await;
    let codes: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM classes \
         WHERE subject_kind = 'asset' AND member_attribute = 'phase' \
           AND retired_at IS NULL \
         ORDER BY sort_order",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        codes,
        AssetLifecyclePhase::ORDER,
        "the active asset.phase Class rows, in pipeline order"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn append_received_event_creates_devices_row() {
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    assets
        .append(evt(
            "evt-1",
            "SN-PROJ-1",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ))
        .await
        .expect("append");

    let row: (String, String, Option<String>) =
        sqlx::query_as("SELECT asset_id, sku, holder_id FROM assets WHERE asset_id = $1")
            .bind("SN-PROJ-1")
            .fetch_one(&db.pool)
            .await
            .expect("devices row should exist after append");

    assert_eq!(row.0, "SN-PROJ-1");
    assert_eq!(row.1, TEST_SKU);
    assert_eq!(row.2, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn subsequent_events_update_projection_in_place() {
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    assets
        .append(evt(
            "evt-1",
            "SN-PROJ-2",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ))
        .await
        .unwrap();

    assets
        .append(evt(
            "evt-2",
            "SN-PROJ-2",
            10,
            AssetEventKind::Installed {
                holder_kind: "account".into(),
                holder_id: "account-007".into(),
            },
        ))
        .await
        .unwrap();

    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM assets WHERE asset_id = $1")
        .bind("SN-PROJ-2")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "must be exactly one projection row per serial");

    let (phase, account): (String, Option<String>) =
        sqlx::query_as("SELECT phase, holder_id FROM assets WHERE asset_id = $1")
            .bind("SN-PROJ-2")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(phase, "installed");
    assert_eq!(account, Some("account-007".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_open_and_close_round_trip_through_projection() {
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    for e in [
        evt(
            "e1",
            "SN-PROJ-3",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ),
        evt(
            "e2",
            "SN-PROJ-3",
            5,
            AssetEventKind::Installed {
                holder_kind: "account".into(),
                holder_id: "account-99".into(),
            },
        ),
        evt(
            "e3",
            "SN-PROJ-3",
            10,
            AssetEventKind::ServiceJobOpened {
                job_id: "tkt-1".into(),
                summary: "x".into(),
            },
        ),
    ] {
        assets.append(e).await.unwrap();
    }

    let (phase, count): (String, i32) =
        sqlx::query_as("SELECT phase, open_ticket_count FROM assets WHERE asset_id = $1")
            .bind("SN-PROJ-3")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(phase, "out-for-service");
    assert_eq!(count, 1);

    assets
        .append(evt(
            "e4",
            "SN-PROJ-3",
            15,
            AssetEventKind::ServiceJobClosed {
                job_id: "tkt-1".into(),
                turnaround_days: 5,
            },
        ))
        .await
        .unwrap();

    let (phase, count): (String, i32) =
        sqlx::query_as("SELECT phase, open_ticket_count FROM assets WHERE asset_id = $1")
            .bind("SN-PROJ-3")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        phase, "installed",
        "closing the only ticket must revert OutForService to Installed"
    );
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_event_does_not_corrupt_projection() {
    // The append must roll back the projection upsert if the event
    // insert fails on a duplicate id. Otherwise a redelivered event
    // would see "this event already exists" but still get its
    // projection effect — leading to off-by-one ticket counts.
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    assets
        .append(evt(
            "e1",
            "SN-PROJ-4",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ))
        .await
        .unwrap();
    assets
        .append(evt(
            "e2",
            "SN-PROJ-4",
            5,
            AssetEventKind::Installed {
                holder_kind: "account".into(),
                holder_id: "account-1".into(),
            },
        ))
        .await
        .unwrap();
    assets
        .append(evt(
            "e3",
            "SN-PROJ-4",
            10,
            AssetEventKind::ServiceJobOpened {
                job_id: "tkt-dup".into(),
                summary: "x".into(),
            },
        ))
        .await
        .unwrap();

    // Replay the same Open event (same id). Must error and leave the
    // projection unchanged.
    let result = assets
        .append(evt(
            "e3",
            "SN-PROJ-4",
            10,
            AssetEventKind::ServiceJobOpened {
                job_id: "tkt-dup".into(),
                summary: "x".into(),
            },
        ))
        .await;
    assert!(result.is_err(), "duplicate event id should error");

    let (count,): (i32,) =
        sqlx::query_as("SELECT open_ticket_count FROM assets WHERE asset_id = $1")
            .bind("SN-PROJ-4")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "duplicate event must not double-count tickets in projection"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_projection_recreates_rows_from_event_log() {
    // Simulates the recovery path: an existing DB has device_events
    // populated but devices is empty (the gap this work fixes).
    // Calling rebuild_projection() must restore devices entirely from
    // the event log.
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    for e in [
        evt(
            "e1",
            "SN-RB-1",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ),
        evt(
            "e2",
            "SN-RB-1",
            5,
            AssetEventKind::Installed {
                holder_kind: "account".into(),
                holder_id: "account-1".into(),
            },
        ),
        evt(
            "e3",
            "SN-RB-2",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("buyback"),
                oem_serial: None,
            },
        ),
    ] {
        assets.append(e).await.unwrap();
    }

    // Wipe the projection to simulate the pre-fix state.
    sqlx::query("DELETE FROM assets")
        .execute(&db.pool)
        .await
        .unwrap();
    let (before,): (i64,) = sqlx::query_as("SELECT count(*) FROM assets")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(before, 0, "wipe should leave devices empty");

    let written = assets.rebuild_projection().await.expect("rebuild");
    assert_eq!(
        written, 2,
        "rebuild should write one row per distinct serial"
    );

    let (after,): (i64,) = sqlx::query_as("SELECT count(*) FROM assets")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(after, 2);

    // Cross-check: the SN-RB-1 row reflects the Installed projection.
    let (sku, phase, account): (String, String, Option<String>) =
        sqlx::query_as("SELECT sku, phase, holder_id FROM assets WHERE asset_id = $1")
            .bind("SN-RB-1")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(sku, TEST_SKU);
    assert_eq!(phase, "installed");
    assert_eq!(account, Some("account-1".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn list_assets_reads_back_what_append_wrote() {
    // End-to-end: append goes through the AssetsRepository trait, the
    // projection populates, and list_assets (which queries the
    // projection table) returns it. This is the path the gateway uses.
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    assets
        .append(evt(
            "e1",
            "SN-LIST-1",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ))
        .await
        .unwrap();

    let (devices, total) = assets.list_assets(50, 0, None).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].asset_id.0, "SN-LIST-1");
    assert_eq!(devices[0].sku.as_deref(), Some(TEST_SKU));
    assert_eq!(devices[0].phase.as_str(), AssetLifecyclePhase::RECEIVED);
}

#[tokio::test(flavor = "multi_thread")]
async fn open_ticket_lands_in_asset_open_tickets_table() {
    // The incremental fast path writes a row to asset_open_tickets
    // when ServiceJobOpened lands and removes it on close.
    // list_open_tickets should reflect that without scanning the
    // event log.
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    for e in [
        evt(
            "e1",
            "SN-DOT-1",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ),
        evt(
            "e2",
            "SN-DOT-1",
            5,
            AssetEventKind::Installed {
                holder_kind: "account".into(),
                holder_id: "account-X".into(),
            },
        ),
        evt(
            "e3",
            "SN-DOT-1",
            10,
            AssetEventKind::ServiceJobOpened {
                job_id: "tkt-DOT-1".into(),
                summary: "the cooler".into(),
            },
        ),
    ] {
        assets.append(e).await.unwrap();
    }

    // The dedicated table has the row.
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM asset_open_tickets")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    // Closing the ticket removes the row.
    assets
        .append(evt(
            "e4",
            "SN-DOT-1",
            15,
            AssetEventKind::ServiceJobClosed {
                job_id: "tkt-DOT-1".into(),
                turnaround_days: 5,
            },
        ))
        .await
        .unwrap();

    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM asset_open_tickets")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_append_inserts_all_and_projects_per_serial() {
    // The bulk path: one transaction inserting many events at once,
    // then walking each touched serial to update its projection.
    // Same observable result as N append() calls.
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    let events = vec![
        evt(
            "b1",
            "SN-BATCH-A",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ),
        evt(
            "b2",
            "SN-BATCH-A",
            5,
            AssetEventKind::Installed {
                holder_kind: "account".into(),
                holder_id: "account-A".into(),
            },
        ),
        evt(
            "b3",
            "SN-BATCH-B",
            2,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("buyback"),
                oem_serial: None,
            },
        ),
        evt(
            "b4",
            "SN-BATCH-B",
            10,
            AssetEventKind::Installed {
                holder_kind: "account".into(),
                holder_id: "account-B".into(),
            },
        ),
        evt(
            "b5",
            "SN-BATCH-B",
            12,
            AssetEventKind::ServiceJobOpened {
                job_id: "tkt-batch".into(),
                summary: "test".into(),
            },
        ),
    ];

    let stats = assets.batch_append(events).await.unwrap();
    assert_eq!(stats.inserted, 5);
    assert_eq!(stats.duplicates, 0);

    // Both ids have an assets row with the latest projected state.
    let (phase_a,): (String,) = sqlx::query_as("SELECT phase FROM assets WHERE asset_id = $1")
        .bind("SN-BATCH-A")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(phase_a, "installed");

    let (phase_b, count_b): (String, i32) =
        sqlx::query_as("SELECT phase, open_ticket_count FROM assets WHERE asset_id = $1")
            .bind("SN-BATCH-B")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(phase_b, "out-for-service");
    assert_eq!(count_b, 1);

    // The open ticket landed in the projection table.
    let (open_count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM asset_open_tickets WHERE asset_id = $1")
            .bind("SN-BATCH-B")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(open_count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_append_skips_duplicates_silently() {
    // Replaying a batch with overlapping ids must count duplicates,
    // not error. This is the contract the simulator's replay path
    // depends on.
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    assets
        .append(evt(
            "dup-1",
            "SN-DUP",
            1,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ))
        .await
        .unwrap();

    let stats = assets
        .batch_append(vec![
            // Already inserted — should be a duplicate.
            evt(
                "dup-1",
                "SN-DUP",
                1,
                AssetEventKind::Received {
                    sku: Some(TEST_SKU.into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            // New — should be inserted.
            evt(
                "dup-2",
                "SN-DUP",
                5,
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "account-X".into(),
                },
            ),
        ])
        .await
        .unwrap();
    assert_eq!(stats.inserted, 1);
    assert_eq!(stats.duplicates, 1);

    // Final state reflects the new event being applied.
    let (phase,): (String,) = sqlx::query_as("SELECT phase FROM assets WHERE asset_id = $1")
        .bind("SN-DUP")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(phase, "installed");
}

#[tokio::test(flavor = "multi_thread")]
async fn out_of_order_event_falls_back_to_full_reproject() {
    // The fast path is "in chronological order"; an event arriving
    // with a ts earlier than the existing last_event_at must hit the
    // slow path. Verify the slow path produces the correct state.
    let db = TestDb::new().await;
    seed_device_model(&db.pool, TEST_SKU).await;
    let assets = PgAssets::new(db.pool.clone());

    // Apply two events in chronological order.
    assets
        .append(evt(
            "e1",
            "SN-OOO",
            10,
            AssetEventKind::Received {
                sku: Some(TEST_SKU.into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        ))
        .await
        .unwrap();
    assets
        .append(evt(
            "e2",
            "SN-OOO",
            20,
            AssetEventKind::Installed {
                holder_kind: "account".into(),
                holder_id: "account-late".into(),
            },
        ))
        .await
        .unwrap();

    // Now arrive an event with an EARLIER ts. This should trigger
    // the slow-path full reprojection. The event is itself a
    // semantically valid one (a Shipped that should have happened
    // before Installed). Final state should still be Installed
    // because Installed has the latest ts.
    assets
        .append(evt(
            "e3-late",
            "SN-OOO",
            15,
            AssetEventKind::Shipped {
                holder_kind: "account".into(),
                holder_id: "account-late".into(),
            },
        ))
        .await
        .unwrap();

    let (phase,): (String,) = sqlx::query_as("SELECT phase FROM assets WHERE asset_id = $1")
        .bind("SN-OOO")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        phase, "installed",
        "out-of-order Shipped at ts=15 must not unwind the Installed at ts=20"
    );
}
