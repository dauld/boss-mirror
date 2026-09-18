//! Postgres adapter for `AgentsRegistry`: one primary-key read on
//! `actor_aliases` per request that carries an identity (the table is
//! a handful of rows and the lookup is indexed; if the door's cost ever
//! shows up, a short TTL cache in front of this adapter is the fix, not
//! a wider read), the roster read, and the tenant batch (backlog
//! f56155f0): one transaction — an insert-if-absent on `agents` that
//! KEEPS a held row and names what it differs on (design e187198f: the
//! instance is the truth), or under `mode=take` an UPDATE of the
//! declared columns where the row differs (the rule of 09887242, now
//! by decision only), and an insert-if-absent (take: upsert) on
//! `actor_aliases` that lands each declared alias under the declared
//! id — so a refused row lands nothing and a declaration lands whole.

use std::collections::BTreeMap;

use async_trait::async_trait;
use boss_core::publish::PublishMode;
use boss_core::publisher::EventStamp;
use sqlx::PgPool;

use super::port::{AgentsError, AgentsRegistry, declared_event, updated_event};
use super::types::{AgentInput, AgentRow, AgentsBatchOutcome, KeptRow, UpdatedRow};

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
    role: Option<String>,
    department: Option<String>,
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
        role: r.role,
        department: r.department,
        hourly_budget_usd_micros: r.hourly_budget_usd_micros,
        max_concurrent_runs: r.max_concurrent_runs,
    }
}

const SELECT: &str = "SELECT id, display_name, default_model, role, department, \
                      hourly_budget_usd_micros, max_concurrent_runs FROM agents";

/// One agent's row with its aliases (`None` when the registry does
/// not hold the id), read inside the batch's transaction so the
/// writes just made are seen.
async fn row_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
) -> Result<Option<AgentRow>, AgentsError> {
    let row: Option<AgentDbRow> = sqlx::query_as(&format!("{SELECT} WHERE id = $1"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let aliases: Vec<String> =
        sqlx::query_scalar("SELECT alias FROM actor_aliases WHERE actor_id = $1 ORDER BY alias")
            .bind(id)
            .fetch_all(&mut **tx)
            .await
            .map_err(storage)?;
    let by_actor = BTreeMap::from([(id.to_string(), aliases)]);
    Ok(Some(to_row(row, &by_actor)))
}

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
        mode: PublishMode,
        stamp: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let mut inserted = 0usize;
        let mut updated = Vec::new();
        let mut kept = Vec::new();
        let mut unchanged = 0usize;
        for a in declared {
            // What the registry held before this declaration, so the
            // outcome can say what moved — read first, inside the
            // transaction.
            let before = row_in_tx(&mut tx, &a.id).await?;
            // The declaration is the whole row (AgentInput is the
            // table's columns). Under take a held row takes every
            // declared column (the WHERE keeps an identical
            // declaration from touching the row at all); under the
            // default it is left exactly as the instance holds it —
            // the instance is the truth (design e187198f).
            let on_conflict = if mode.is_take() {
                "ON CONFLICT (id) DO UPDATE SET \
                   display_name = EXCLUDED.display_name, \
                   default_model = EXCLUDED.default_model, \
                   role = EXCLUDED.role, \
                   department = EXCLUDED.department, \
                   hourly_budget_usd_micros = EXCLUDED.hourly_budget_usd_micros, \
                   max_concurrent_runs = EXCLUDED.max_concurrent_runs \
                 WHERE (agents.display_name, agents.default_model, agents.role, \
                        agents.department, agents.hourly_budget_usd_micros, \
                        agents.max_concurrent_runs) \
                       IS DISTINCT FROM \
                       (EXCLUDED.display_name, EXCLUDED.default_model, EXCLUDED.role, \
                        EXCLUDED.department, EXCLUDED.hourly_budget_usd_micros, \
                        EXCLUDED.max_concurrent_runs)"
            } else {
                "ON CONFLICT (id) DO NOTHING"
            };
            sqlx::query(&format!(
                "INSERT INTO agents \
                 (id, display_name, default_model, role, department, \
                  hourly_budget_usd_micros, max_concurrent_runs) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) {on_conflict}"
            ))
            .bind(&a.id)
            .bind(&a.display_name)
            .bind(&a.default_model)
            .bind(&a.role)
            .bind(&a.department)
            .bind(a.hourly_budget_usd_micros)
            .bind(a.max_concurrent_runs)
            .execute(&mut *tx)
            .await
            .map_err(|e| insert_error(e, &a.default_model))?;
            // A declared alias signs as the declared id: landed when
            // nobody holds it (an alias is its own row, inserted if
            // absent), moved only under take when another agent does;
            // an alias the tenant does not declare is not touched.
            let alias_on_conflict = if mode.is_take() {
                "ON CONFLICT (alias) DO UPDATE SET actor_id = EXCLUDED.actor_id \
                 WHERE actor_aliases.actor_id <> EXCLUDED.actor_id"
            } else {
                "ON CONFLICT (alias) DO NOTHING"
            };
            for alias in &a.aliases {
                sqlx::query(&format!(
                    "INSERT INTO actor_aliases (alias, actor_id) VALUES ($1, $2) \
                     {alias_on_conflict}"
                ))
                .bind(alias)
                .bind(&a.id)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            }
            let Some(before) = before else {
                // The fact rides the insert's transaction (backlog
                // d9409039): a row that lands stages its
                // `agent.declared` on the outbox here, so the row and
                // the fact commit or roll back together.
                boss_events::outbox::record_event_in_tx(&mut tx, &declared_event(stamp, a)?)
                    .await
                    .map_err(AgentsError::Storage)?;
                inserted += 1;
                continue;
            };
            let after = row_in_tx(&mut tx, &a.id)
                .await?
                .ok_or_else(|| AgentsError::Storage(format!("{} vanished mid-batch", a.id)))?;
            let changes = before.changes_to(&after);
            let changed = !changes.is_empty();
            if changed {
                boss_events::outbox::record_event_in_tx(
                    &mut tx,
                    &updated_event(stamp, &after, &changes)?,
                )
                .await
                .map_err(AgentsError::Storage)?;
                updated.push(UpdatedRow {
                    id: a.id.clone(),
                    changes,
                });
            }
            // What the row STILL differs on after the write: empty
            // under take, the kept fields under the default — named,
            // never silent.
            let differs = after.differs_from(a);
            if !differs.is_empty() {
                kept.push(KeptRow {
                    id: a.id.clone(),
                    differs,
                });
            } else if !changed {
                unchanged += 1;
            }
        }
        tx.commit().await.map_err(storage)?;
        Ok(AgentsBatchOutcome {
            received: declared.len(),
            inserted,
            updated,
            kept,
            unchanged,
        })
    }
}
