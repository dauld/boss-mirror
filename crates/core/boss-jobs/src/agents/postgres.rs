//! Postgres adapter for `AgentsRegistry`: one primary-key read on
//! `actor_aliases` per request that carries an identity (the table is
//! a handful of rows and the lookup is indexed; if the door's cost ever
//! shows up, a short TTL cache in front of this adapter is the fix, not
//! a wider read), the roster read, and the tenant batch (backlog
//! f56155f0): one transaction of `ON CONFLICT DO NOTHING` inserts on
//! `agents` and `actor_aliases`, so a registered row is kept and a
//! refused row lands nothing.

use std::collections::BTreeMap;

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use sqlx::PgPool;

use super::port::{AgentsError, AgentsRegistry, declared_event};
use super::types::{AgentInput, AgentRow, AgentsBatchOutcome, KeptAgent};

pub struct PgAgents {
    pool: PgPool,
}

impl PgAgents {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: sqlx::Error) -> AgentsError {
    AgentsError::Storage(e.to_string())
}

/// The rate-card FK (`agents.default_model REFERENCES agent_rate_card`)
/// refusing a model, told apart from every other failure so the door
/// can name the model.
fn insert_error(e: sqlx::Error, model: &str) -> AgentsError {
    let is_model_fk = e.as_database_error().is_some_and(|d| {
        d.code().as_deref() == Some("23503") && d.message().contains("default_model")
    });
    if is_model_fk {
        AgentsError::Unpriced(model.to_string())
    } else {
        storage(e)
    }
}

#[derive(sqlx::FromRow)]
struct AgentDbRow {
    id: String,
    display_name: String,
    default_model: String,
    hourly_budget_usd_micros: Option<i64>,
    max_concurrent_runs: Option<i32>,
}

/// Every alias grouped under its actor, sorted.
async fn aliases_by_actor<'e, E>(exec: E) -> Result<BTreeMap<String, Vec<String>>, AgentsError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let pairs: Vec<(String, String)> =
        sqlx::query_as("SELECT actor_id, alias FROM actor_aliases ORDER BY actor_id, alias")
            .fetch_all(exec)
            .await
            .map_err(storage)?;
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (actor, alias) in pairs {
        out.entry(actor).or_default().push(alias);
    }
    Ok(out)
}

fn to_row(r: AgentDbRow, aliases: &BTreeMap<String, Vec<String>>) -> AgentRow {
    AgentRow {
        aliases: aliases.get(&r.id).cloned().unwrap_or_default(),
        id: r.id,
        display_name: r.display_name,
        default_model: r.default_model,
        hourly_budget_usd_micros: r.hourly_budget_usd_micros,
        max_concurrent_runs: r.max_concurrent_runs,
    }
}

const SELECT: &str = "SELECT id, display_name, default_model, hourly_budget_usd_micros, \
                      max_concurrent_runs FROM agents";

#[async_trait]
impl AgentsRegistry for PgAgents {
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError> {
        sqlx::query_scalar("SELECT actor_id FROM actor_aliases WHERE alias = $1")
            .bind(login)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)
    }

    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError> {
        let rows: Vec<AgentDbRow> = sqlx::query_as(&format!("{SELECT} ORDER BY id"))
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        let aliases = aliases_by_actor(&self.pool).await?;
        Ok(rows.into_iter().map(|r| to_row(r, &aliases)).collect())
    }

    async fn publish(
        &self,
        declared: &[AgentInput],
        stamp: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let mut inserted = 0usize;
        let mut kept_ids: Vec<&AgentInput> = Vec::new();
        for a in declared {
            let n = sqlx::query(
                "INSERT INTO agents \
                 (id, display_name, default_model, hourly_budget_usd_micros, max_concurrent_runs) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(&a.id)
            .bind(&a.display_name)
            .bind(&a.default_model)
            .bind(a.hourly_budget_usd_micros)
            .bind(a.max_concurrent_runs)
            .execute(&mut *tx)
            .await
            .map_err(|e| insert_error(e, &a.default_model))?
            .rows_affected();
            if n == 1 {
                // The fact rides the insert's transaction (backlog
                // d9409039): a row that lands stages its
                // `agent.declared` on the outbox here, so the row and
                // the fact commit or roll back together; a kept row
                // is already named in the outcome and records nothing.
                boss_events::outbox::record_event_in_tx(&mut tx, &declared_event(stamp, a)?)
                    .await
                    .map_err(AgentsError::Storage)?;
                inserted += 1;
            } else {
                kept_ids.push(a);
            }
            for alias in &a.aliases {
                sqlx::query(
                    "INSERT INTO actor_aliases (alias, actor_id) VALUES ($1, $2) \
                     ON CONFLICT (alias) DO NOTHING",
                )
                .bind(alias)
                .bind(&a.id)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            }
        }
        // What the kept rows hold, read inside the same transaction so
        // the aliases just landed are seen.
        let mut kept = Vec::with_capacity(kept_ids.len());
        if !kept_ids.is_empty() {
            let aliases = aliases_by_actor(&mut *tx).await?;
            for a in kept_ids {
                let row: AgentDbRow = sqlx::query_as(&format!("{SELECT} WHERE id = $1"))
                    .bind(&a.id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(storage)?;
                kept.push(KeptAgent {
                    id: a.id.clone(),
                    differs: to_row(row, &aliases).differs_from(a),
                });
            }
        }
        tx.commit().await.map_err(storage)?;
        Ok(AgentsBatchOutcome {
            received: declared.len(),
            inserted,
            kept,
        })
    }
}
