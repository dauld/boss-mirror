//! A `TestDb` that goes out of scope takes its database with it — even
//! though the runtime that dropped it is already gone.
//!
//! THE LEAK THIS PINS (backlog 9e1ea321, 2026-09-23). `Drop` used to
//! `handle.spawn` the `DROP DATABASE`, and a `#[tokio::test]` runtime is
//! torn down the moment the test function returns, so the spawned task
//! was cancelled before it ran: every scratch database leaked, and only
//! the 30-minute orphan sweep ever reclaimed any. Measured on the dev
//! pod's harness Postgres that day: 423 `test_boss_*` databases, 6.2 GB,
//! and 1,423 forced checkpoints in five hours — each one a sweeper's
//! serial `DROP DATABASE` that every later `TestDb::new` sat behind.
//!
//! The shape here is exactly a `#[tokio::test]`'s: a current-thread
//! runtime that builds the database, drops it, and is itself dropped.
//! Then, from a different runtime, the database must disappear.

use std::time::{Duration, Instant};

use sqlx::{Connection, PgConnection};

fn admin_url() -> String {
    std::env::var("BOSS_TEST_POSTGRES_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://boss:boss@127.0.0.1/postgres".to_string())
}

async fn exists(name: &str) -> bool {
    let mut admin = PgConnection::connect(&admin_url())
        .await
        .expect("connecting to the admin database");
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(name)
        .fetch_one(&mut admin)
        .await
        .expect("reading pg_database")
}

#[test]
fn a_dropped_test_db_is_dropped_after_its_runtime_is_gone() {
    // The test's own runtime: build, drop, gone — as #[tokio::test] does.
    let test_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let name = test_rt.block_on(async {
        let db = boss_testing::TestDb::new().await;
        let name = db.name().to_string();
        assert!(exists(&name).await, "{name} exists while it is held");
        name
        // `db` drops here, inside the runtime, exactly as a test's does.
    });
    drop(test_rt);

    // A drop waits on a forced checkpoint, which on a busy server is
    // tens of seconds (40 s measured on the dev pod while this was
    // written), so the bound is generous: the failure being pinned is
    // "never", not "slow".
    let observer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let deadline = Instant::now() + Duration::from_secs(240);
    observer.block_on(async {
        while exists(&name).await {
            assert!(
                Instant::now() < deadline,
                "{name} still exists four minutes after its TestDb was dropped: \
                 the drop never ran, so the database leaked until the orphan sweep"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });
}
