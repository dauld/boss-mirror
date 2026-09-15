//! In-memory adapter for `AgentsRegistry` — the port-level test double.
//!
//! Mirrors the one Pg invariant that matters here: an alias exists
//! only under a registered agent (`actor_aliases.actor_id` is a foreign
//! key into `agents`), so the builder takes the agent and its logins
//! together and there is no way to register a login to nothing.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::port::{AgentsError, AgentsRegistry};

#[derive(Default)]
pub struct InMemoryAgents {
    /// login -> registered agent id.
    aliases: Mutex<HashMap<String, String>>,
}

impl InMemoryAgents {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `agent_id` with the logins it may sign as.
    pub fn with_agent<'a>(self, agent_id: &str, logins: impl IntoIterator<Item = &'a str>) -> Self {
        {
            let mut aliases = self.aliases.lock().expect("aliases lock");
            for login in logins {
                aliases.insert(login.to_string(), agent_id.to_string());
            }
        }
        self
    }
}

#[async_trait]
impl AgentsRegistry for InMemoryAgents {
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError> {
        Ok(self
            .aliases
            .lock()
            .expect("aliases lock")
            .get(login)
            .cloned())
    }
}
