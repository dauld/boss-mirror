//! Postgres adapter for `AgentsRegistry`: one primary-key read on
//! `actor_aliases` per request that carries an identity. The table is
//! two rows and the lookup is indexed; if the door's cost ever shows
//! up, a short TTL cache in front of this adapter is the fix, not a
//! wider read.

use async_trait::async_trait;
use sqlx::PgPool;

use super::port::{AgentsError, AgentsRegistry};

pub struct PgAgents {
    pool: PgPool,
}

impl PgAgents {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AgentsRegistry for PgAgents {
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError> {
        sqlx::query_scalar("SELECT actor_id FROM actor_aliases WHERE alias = $1")
            .bind(login)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| AgentsError::Storage(e.to_string()))
    }
}
