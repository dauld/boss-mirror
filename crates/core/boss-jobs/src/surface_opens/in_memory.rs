//! In-memory adapter for `SurfaceOpens` — the port-level test double.
//! Mirrors the Pg semantics that matter: the roll-up groups by (actor,
//! route) over a half-open window and orders actor, then opens
//! descending, then route; the sweep deletes strictly before the cutoff.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use super::port::{SurfaceOpens, SurfaceOpensError, validate_window};
use super::types::{Rollup, RouteCount};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    pub actor_id: String,
    pub route: String,
    pub at: DateTime<Utc>,
}

#[derive(Default)]
pub struct InMemorySurfaceOpens {
    rows: RwLock<Vec<Open>>,
}

impl InMemorySurfaceOpens {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test visibility into every row held, in record order.
    pub async fn rows(&self) -> Vec<Open> {
        self.rows.read().await.clone()
    }
}

/// The roll-up as a pure function of the rows — the stated twin of the
/// Pg adapter's GROUP BY / ORDER BY, so the two answer alike.
pub fn roll_up(rows: &[Open], since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<RouteCount> {
    let by_pair = rows.iter().filter(|r| r.at >= since && r.at < until).fold(
        BTreeMap::<(String, String), (i64, DateTime<Utc>)>::new(),
        |mut m, r| {
            let e = m
                .entry((r.actor_id.clone(), r.route.clone()))
                .or_insert((0, r.at));
            *e = (e.0 + 1, e.1.max(r.at));
            m
        },
    );
    let mut out: Vec<RouteCount> = by_pair
        .into_iter()
        .map(|((actor_id, route), (opens, last_at))| RouteCount {
            actor_id,
            route,
            opens,
            last_at,
        })
        .collect();
    out.sort_by(|a, b| {
        a.actor_id
            .cmp(&b.actor_id)
            .then(b.opens.cmp(&a.opens))
            .then(a.route.cmp(&b.route))
    });
    out
}

#[async_trait]
impl SurfaceOpens for InMemorySurfaceOpens {
    async fn record(
        &self,
        actor_id: &str,
        route: &str,
        at: DateTime<Utc>,
    ) -> Result<(), SurfaceOpensError> {
        self.rows.write().await.push(Open {
            actor_id: actor_id.to_string(),
            route: route.to_string(),
            at,
        });
        Ok(())
    }

    async fn rollup(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Rollup, SurfaceOpensError> {
        validate_window(since, until)?;
        let rows = roll_up(&self.rows.read().await, since, until);
        Ok(Rollup { since, until, rows })
    }

    async fn sweep(&self, before: DateTime<Utc>) -> Result<u64, SurfaceOpensError> {
        let mut guard = self.rows.write().await;
        let n = guard.len();
        guard.retain(|r| r.at >= before);
        Ok((n - guard.len()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn t(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap() + Duration::minutes(minutes)
    }

    #[tokio::test]
    async fn the_rollup_counts_per_actor_per_route_inside_the_window_only() {
        let repo = InMemorySurfaceOpens::new();
        repo.record("emp-david", "/it", t(0)).await.unwrap();
        repo.record("emp-david", "/it", t(5)).await.unwrap();
        repo.record("emp-david", "/it/codebase", t(6))
            .await
            .unwrap();
        repo.record("emp-032", "/ux/me", t(7)).await.unwrap();
        // Outside the window on both edges: before since, at until.
        repo.record("emp-david", "/it", t(-1)).await.unwrap();
        repo.record("emp-david", "/it", t(60)).await.unwrap();

        let r = repo.rollup(t(0), t(60)).await.unwrap();
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

    #[tokio::test]
    async fn an_empty_window_is_refused_not_answered_empty() {
        let repo = InMemorySurfaceOpens::new();
        let err = repo.rollup(t(10), t(10)).await.unwrap_err();
        assert!(matches!(err, SurfaceOpensError::BadRequest(_)), "{err}");
    }

    #[tokio::test]
    async fn the_sweep_deletes_strictly_before_the_cutoff_and_says_how_many() {
        let repo = InMemorySurfaceOpens::new();
        repo.record("emp-david", "/it", t(-10)).await.unwrap();
        repo.record("emp-david", "/it", t(0)).await.unwrap();
        repo.record("emp-david", "/it", t(10)).await.unwrap();
        assert_eq!(repo.sweep(t(0)).await.unwrap(), 1);
        assert_eq!(repo.rows().await.len(), 2);
    }
}
