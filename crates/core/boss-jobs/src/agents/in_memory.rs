//! In-memory adapter for `AgentsRegistry` — the port-level test double.
//!
//! Mirrors the Pg invariants that matter here: an alias exists only
//! under a registered agent (`actor_aliases.actor_id` is a foreign key
//! into `agents`), so the builder takes the agent and its logins
//! together and there is no way to register a login to nothing; and a
//! publish inserts a row the registry lacks, UPDATES a held row on the
//! declared fields that differ (a declared alias lands under the
//! declared id, an undeclared alias is kept), and names each change in
//! the outcome. The rate-card FK is NOT mirrored (the Pg test proves
//! that refusal).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::publisher::EventStamp;

use super::port::{AgentsError, AgentsRegistry, declared_event, updated_event};
use super::types::{AgentInput, AgentRow, AgentsBatchOutcome, UpdatedRow};

#[derive(Default)]
struct Rows {
    /// id -> the row (without its aliases).
    agents: BTreeMap<String, AgentInput>,
    /// login -> registered agent id.
    aliases: BTreeMap<String, String>,
}

impl Rows {
    fn row(&self, a: &AgentInput) -> AgentRow {
        AgentRow {
            id: a.id.clone(),
            display_name: a.display_name.clone(),
            default_model: a.default_model.clone(),
            role: a.role.clone(),
            department: a.department.clone(),
            hourly_budget_usd_micros: a.hourly_budget_usd_micros,
            max_concurrent_runs: a.max_concurrent_runs,
            aliases: self
                .aliases
                .iter()
                .filter(|(_, id)| **id == a.id)
                .map(|(alias, _)| alias.clone())
                .collect(),
        }
    }
}

#[derive(Default)]
pub struct InMemoryAgents {
    rows: Mutex<Rows>,
    events: Mutex<Vec<Event>>,
}

impl InMemoryAgents {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event recorded through this adapter, in order — what a
    /// Pg deployment would find on the outbox.
    pub fn recorded_events(&self) -> Vec<Event> {
        self.events.lock().expect("events lock").clone()
    }

    /// Register `agent_id` with the logins it may sign as.
    pub fn with_agent<'a>(self, agent_id: &str, logins: impl IntoIterator<Item = &'a str>) -> Self {
        {
            let mut rows = self.rows.lock().expect("agents lock");
            rows.agents
                .entry(agent_id.to_string())
                .or_insert_with(|| AgentInput {
                    id: agent_id.to_string(),
                    display_name: agent_id.to_string(),
                    default_model: "opus-5".to_string(),
                    aliases: Vec::new(),
                    role: None,
                    department: None,
                    hourly_budget_usd_micros: None,
                    max_concurrent_runs: None,
                });
            for login in logins {
                rows.aliases.insert(login.to_string(), agent_id.to_string());
            }
        }
        self
    }
}

#[async_trait]
impl AgentsRegistry for InMemoryAgents {
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError> {
        Ok(self
            .rows
            .lock()
            .expect("agents lock")
            .aliases
            .get(login)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError> {
        let rows = self.rows.lock().expect("agents lock");
        Ok(rows.agents.values().map(|a| rows.row(a)).collect())
    }

    async fn publish(
        &self,
        declared: &[AgentInput],
        stamp: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError> {
        let mut rows = self.rows.lock().expect("agents lock");
        let mut events = self.events.lock().expect("events lock");
        let mut inserted = 0usize;
        let mut updated = Vec::new();
        let mut unchanged = 0usize;
        for a in declared {
            let before = rows.agents.get(&a.id).map(|held| rows.row(held));
            let mut stored = a.clone();
            stored.aliases.clear();
            rows.agents.insert(a.id.clone(), stored);
            // A declared alias signs as the declared id — moved when
            // another agent held it; one the tenant did not declare
            // stays where it is.
            for alias in &a.aliases {
                rows.aliases.insert(alias.clone(), a.id.clone());
            }
            let Some(before) = before else {
                events.push(declared_event(stamp, a)?);
                inserted += 1;
                continue;
            };
            let after = rows.row(&rows.agents[&a.id]);
            let changes = before.changes_to(&after);
            if changes.is_empty() {
                unchanged += 1;
            } else {
                events.push(updated_event(stamp, &after, &changes)?);
                updated.push(UpdatedRow {
                    id: a.id.clone(),
                    changes,
                });
            }
        }
        Ok(AgentsBatchOutcome {
            received: declared.len(),
            inserted,
            updated,
            unchanged,
        })
    }
}
