//! Postgres adapter for `SurfaceOpens` — three statements over the
//! `surface_opens` table (20260916020249-the-operator-surfaces-are-
//! counted.sql). The roll-up is ONE GROUP BY, so the count per actor
//! per route is computed where the rows are and the chore script and
//! the page both read the same answer rather than each summing raw
//! rows their own way (CLAUDE.md §9a).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use super::port::{SurfaceOpens, SurfaceOpensError, validate_window};
use super::types::{Rollup, RouteCount};

pub struct PgSurfaceOpens {
    pool: PgPool,
}

impl PgSurfaceOpens {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: sqlx::Error) -> SurfaceOpensError {
    SurfaceOpensError::Storage(e.to_string())
}

#[async_trait]
impl SurfaceOpens for PgSurfaceOpens {
    async fn record(
        &self,
        actor_id: &str,
        route: &str,
        at: DateTime<Utc>,
    ) -> Result<(), SurfaceOpensError> {
        sqlx::query("INSERT INTO surface_opens (actor_id, route, opened_at) VALUES ($1, $2, $3)")
            .bind(actor_id)
            .bind(route)
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }

    async fn rollup(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Rollup, SurfaceOpensError> {
        validate_window(since, until)?;
        // The ORDER BY is the in-memory adapter's sort, stated once
        // there and once here: actor, then most-opened first, then
        // route so equal counts land in a stable order.
        let rows = sqlx::query(
            "SELECT actor_id, route, COUNT(*)::bigint AS opens, MAX(opened_at) AS last_at \
             FROM surface_opens \
             WHERE opened_at >= $1 AND opened_at < $2 \
             GROUP BY actor_id, route \
             ORDER BY actor_id, opens DESC, route",
        )
        .bind(since)
        .bind(until)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let rows = rows
            .iter()
            .map(|r| {
                Ok(RouteCount {
                    actor_id: r.try_get("actor_id").map_err(storage)?,
                    route: r.try_get("route").map_err(storage)?,
                    opens: r.try_get("opens").map_err(storage)?,
                    last_at: r.try_get("last_at").map_err(storage)?,
                })
            })
            .collect::<Result<Vec<_>, SurfaceOpensError>>()?;
        Ok(Rollup { since, until, rows })
    }

    async fn sweep(&self, before: DateTime<Utc>) -> Result<u64, SurfaceOpensError> {
        let res = sqlx::query("DELETE FROM surface_opens WHERE opened_at < $1")
            .bind(before)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(res.rows_affected())
    }
}
