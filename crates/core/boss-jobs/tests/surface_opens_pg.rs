//! Postgres-backed coverage for the surface-opens record (backlog
//! 628f182b): the table the migration creates is the one the adapter
//! writes, the GROUP BY answers what the in-memory twin answers, and
//! the sweep deletes strictly before its cutoff.

use boss_jobs::surface_opens::{PgSurfaceOpens, RouteCount, SurfaceOpens, SurfaceOpensError};
use boss_testing::TestDb;
use chrono::{DateTime, Duration, TimeZone, Utc};

fn t(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap() + Duration::minutes(minutes)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rollup_groups_per_actor_per_route_over_a_half_open_window() {
    let db = TestDb::new().await;
    let repo = PgSurfaceOpens::new(db.pool.clone());
    for (actor, route, at) in [
        ("emp-david", "/it", t(0)),
        ("emp-david", "/it", t(5)),
        ("emp-david", "/it/codebase", t(6)),
        ("emp-032", "/ux/me", t(7)),
        // Outside the window on both edges.
        ("emp-david", "/it", t(-1)),
        ("emp-david", "/it", t(60)),
    ] {
        repo.record(actor, route, at).await.unwrap();
    }

    let r = repo.rollup(t(0), t(60)).await.unwrap();
    assert_eq!(r.since, t(0));
    assert_eq!(r.until, t(60));
    assert_eq!(r.opens(), 4);
    assert_eq!(
        r.rows,
        vec![
            RouteCount {
                actor_id: "emp-032".into(),
                route: "/ux/me".into(),
                opens: 1,
                last_at: t(7)
            },
            RouteCount {
                actor_id: "emp-david".into(),
                route: "/it".into(),
                opens: 2,
                last_at: t(5)
            },
            RouteCount {
                actor_id: "emp-david".into(),
                route: "/it/codebase".into(),
                opens: 1,
                last_at: t(6)
            },
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_window_is_refused_not_answered_empty() {
    let db = TestDb::new().await;
    let repo = PgSurfaceOpens::new(db.pool.clone());
    let err = repo.rollup(t(10), t(10)).await.unwrap_err();
    assert!(matches!(err, SurfaceOpensError::BadRequest(_)), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sweep_deletes_strictly_before_the_cutoff() {
    let db = TestDb::new().await;
    let repo = PgSurfaceOpens::new(db.pool.clone());
    repo.record("emp-david", "/it", t(-10)).await.unwrap();
    repo.record("emp-david", "/it", t(0)).await.unwrap();
    repo.record("emp-david", "/it", t(10)).await.unwrap();
    assert_eq!(repo.sweep(t(0)).await.unwrap(), 1);
    let r = repo.rollup(t(-60), t(60)).await.unwrap();
    assert_eq!(r.opens(), 2);
    assert_eq!(
        repo.sweep(t(0)).await.unwrap(),
        0,
        "a second sweep finds nothing"
    );
}
