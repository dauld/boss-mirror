//! Human job-owner resolution (subject-model Q7, approved
//! 2026-07-15): every Job names a responsible HUMAN owner. Steps may
//! be automation-owned; the Job — the unit of accountability — may
//! not. Before this, 18k+ live jobs were owned by `system-sim` /
//! `automation:*` / `rule:*` actors and zero by people.
//!
//! Resolution runs server-side in the create handler, so callers
//! (the sim, dispatcher spawn rules, /shop) don't each need roster
//! knowledge:
//!
//! 1. If the requested owner is an ACTIVE EMPLOYEE, keep it — a
//!    human choice is never overridden.
//! 2. Otherwise resolve the kind's `metadata.owner_role` (registry
//!    data — same channel as `surfaces`) to an active holder,
//!    hash-spread over the job id so ownership distributes across
//!    holders yet stays deterministic across replays.
//! 3. No `owner_role`? Fall back to the first role-bearing step's
//!    `authority_role` — the platform meta-kinds resolve this way
//!    (workflow-design → a workflow-approver holder).
//! 4. Nothing resolves → the create is rejected. A Job with no
//!    responsible human is the modeling error Q7 exists to end.
//!
//! Same opt-in shape as the subject-existence gate:
//! `JobsApiState::roster: Option<Arc<dyn RosterLookup>>`; `None`
//! skips resolution (in-memory adapters without an upstream stack).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;

/// Minimal roster port: who actively holds a role, and is this id an
/// active employee at all.
#[async_trait]
pub trait RosterLookup: Send + Sync {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String>;
    async fn is_active_employee(&self, id: &str) -> Result<bool, String>;
}

/// True when the id names a machine-shaped actor rather than a
/// person: the `ActorId` union's non-human arms plus the historical
/// pseudo-owners the audit catalogued.
///
/// The union's own parse decides the first half: every colon-bearing
/// id is a machine — a named automation (`automation:<slug>`,
/// `rule:<name>`) or an agent session (`<mode>:<model>`, e.g.
/// `claude:opus-5`) — and so is a registered agent's bare id
/// (`agent-claude`, design 6fda05ae). Reading it off `ActorId` rather
/// than off a copy of its prefixes is what keeps this predicate and
/// `ActorId::is_human` one definition (§9a): the `agent-` spelling
/// would otherwise have needed adding here by hand, and until it was,
/// a registered agent would have owned a Job as if it were staff.
pub fn is_automation_shaped(owner: &str) -> bool {
    owner.is_empty()
        || owner
            .parse::<boss_core::actor::ActorId>()
            .is_ok_and(|a| !a.is_human())
        || owner.starts_with("system")
        || owner == "direct-shop"
        || owner == "bootstrap"
}

/// Resolve the responsible human for a job. `requested` is whatever
/// the caller put on the wire; `job_id` seeds the deterministic
/// spread; `owner_role` / `step_fallback_role` come from the kind
/// spec. Returns the resolved employee id, or Err with the reason
/// the create must be rejected.
pub async fn resolve_owner(
    roster: &dyn RosterLookup,
    requested: &str,
    job_id: &str,
    owner_role: Option<&str>,
    step_fallback_role: Option<&str>,
) -> Result<String, String> {
    if !is_automation_shaped(requested) {
        match roster.is_active_employee(requested).await {
            Ok(true) => return Ok(requested.to_string()),
            Ok(false) => {
                // A human-shaped id that isn't on the active roster
                // (departed employee, typo) falls through to role
                // resolution rather than silently owning work.
            }
            // Roster unavailable: keep the caller's human-shaped
            // choice rather than wedging creates on a people-api
            // blip. Automation-shaped owners below do NOT get this
            // grace — they have no claim to keep.
            Err(_) => return Ok(requested.to_string()),
        }
    }

    for role in [owner_role, step_fallback_role].into_iter().flatten() {
        let holders = roster
            .active_holders(role)
            .await
            .map_err(|e| format!("roster lookup for role `{role}` failed: {e}"))?;
        if !holders.is_empty() {
            // Deterministic spread: stable across replays (job ids
            // are deterministic in sim runs), distributed across
            // holders. Sort first — the roster's order is not part
            // of the contract.
            let mut sorted = holders;
            sorted.sort();
            let idx = (fxhash(job_id) as usize) % sorted.len();
            return Ok(sorted[idx].clone());
        }
    }

    Err(format!(
        "no responsible human resolvable for owner `{requested}` \
         (owner_role {owner_role:?}, step fallback {step_fallback_role:?}) — \
         every Job names a human owner (Q7)"
    ))
}

/// Small stable string hash (FNV-1a) — NOT DefaultHasher, whose seed
/// varies per process and would break replay determinism.
fn fxhash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// HTTP-backed roster over boss-people, with a short TTL cache so a
/// sim burst of creates doesn't turn into a people-api hammer.
pub struct ReqwestRosterLookup {
    client: reqwest::Client,
    people_base: String,
    cache: Mutex<HashMap<String, (Instant, Vec<String>)>>,
    ttl: Duration,
}

#[derive(serde::Deserialize)]
struct EmployeeRow {
    id: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

impl ReqwestRosterLookup {
    pub fn new(people_base: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            people_base: people_base.into(),
            cache: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(60),
        }
    }

    async fn fetch_all(&self) -> Result<Vec<EmployeeRow>, String> {
        let url = format!("{}/api/people?limit=1000", self.people_base);
        let mut h = reqwest::header::HeaderMap::new();
        let user = serde_json::json!({
            "id": "system-jobs",
            "role": "system",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        })
        .to_string();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&user) {
            h.insert("x-boss-user", v);
        }
        let resp = self
            .client
            .get(&url)
            .headers(h)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("{url} {}", resp.status()));
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        // Tolerate both bare-array and {employees: []} shapes.
        let rows = body.get("employees").cloned().unwrap_or(body);
        serde_json::from_value(rows).map_err(|e| e.to_string())
    }

    async fn holders_uncached(&self, role: &str) -> Result<Vec<String>, String> {
        let rows = self.fetch_all().await?;
        Ok(rows
            .into_iter()
            .filter(|r| {
                r.role.as_deref() == Some(role)
                    && r.status.as_deref().unwrap_or("active") == "active"
            })
            .map(|r| r.id)
            .collect())
    }
}

#[async_trait]
impl RosterLookup for ReqwestRosterLookup {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        if let Some((at, holders)) = self.cache.lock().unwrap().get(role)
            && at.elapsed() < self.ttl
        {
            return Ok(holders.clone());
        }
        let holders = self.holders_uncached(role).await?;
        // Absence is not a fact worth remembering. At bootstrap an
        // empty roster for a role is exactly the state about to change
        // — the baseline seed races the first spawn rules — and caching
        // it for the full TTL converted that one-second race into a
        // fatal minute: the brewery prepare's Q7 resolution failed
        // against a poisoned entry while the just-seeded admin sat
        // invisible, aborting the whole fresh install (2026-08-22).
        // Populated rosters keep the cache that shields people-api
        // from sim bursts; emptiness gets re-asked.
        if !holders.is_empty() {
            self.cache
                .lock()
                .unwrap()
                .insert(role.to_string(), (Instant::now(), holders.clone()));
        }
        Ok(holders)
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        let rows = self.fetch_all().await?;
        Ok(rows
            .iter()
            .any(|r| r.id == id && r.status.as_deref().unwrap_or("active") == "active"))
    }
}

#[cfg(test)]
pub mod test_helpers {
    //! In-memory roster for tests: role → holder ids.

    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct InMemoryRoster {
        pub by_role: HashMap<String, Vec<String>>,
    }

    impl InMemoryRoster {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn with_holder(mut self, role: &str, id: &str) -> Self {
            self.by_role
                .entry(role.to_string())
                .or_default()
                .push(id.to_string());
            self
        }
    }

    #[async_trait]
    impl RosterLookup for InMemoryRoster {
        async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
            Ok(self.by_role.get(role).cloned().unwrap_or_default())
        }
        async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
            Ok(self.by_role.values().any(|v| v.iter().any(|h| h == id)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_helpers::InMemoryRoster;

    #[tokio::test]
    async fn automation_owner_resolves_to_role_holder_deterministically() {
        let roster = InMemoryRoster::new()
            .with_holder("bookkeeper", "emp-bk-2")
            .with_holder("bookkeeper", "emp-bk-1");
        let a = resolve_owner(&roster, "system-sim", "job-1", Some("bookkeeper"), None)
            .await
            .unwrap();
        let b = resolve_owner(&roster, "system-sim", "job-1", Some("bookkeeper"), None)
            .await
            .unwrap();
        assert_eq!(a, b, "same job id must resolve identically");
        assert!(a.starts_with("emp-bk-"));
    }

    #[tokio::test]
    async fn human_owner_is_kept_and_unresolvable_is_rejected() {
        let roster = InMemoryRoster::new().with_holder("brewer", "emp-brew-1");
        let kept = resolve_owner(&roster, "emp-brew-1", "j", Some("brewer"), None)
            .await
            .unwrap();
        assert_eq!(kept, "emp-brew-1");

        let err = resolve_owner(&roster, "automation:seed", "j", Some("bookkeeper"), None).await;
        assert!(err.is_err(), "no holder for the role and no fallback");
    }

    /// Q7 says every Job names a responsible HUMAN owner. An agent is
    /// a CPU, not a person, so an agent-shaped owner must resolve away
    /// to a role holder exactly like an automation does — including
    /// when the roster is unreachable, where a human-shaped id is kept
    /// on faith and a machine-shaped one has no claim to keep.
    #[tokio::test]
    async fn an_agent_owner_is_machine_shaped_and_resolves_to_a_human() {
        assert!(is_automation_shaped("claude:opus-5"));
        assert!(is_automation_shaped("claude:fable"));
        // The registry spelling (design 6fda05ae) is the same class in
        // a colon-free id; the shape test reads it off the ActorId
        // parse rather than off a second copy of the prefix.
        assert!(is_automation_shaped("agent-claude"));
        assert!(!is_automation_shaped("emp-brew-1"));

        let roster = InMemoryRoster::new().with_holder("brewer", "emp-brew-1");
        for agent in ["claude:fable", "agent-claude"] {
            let owner = resolve_owner(&roster, agent, "j", Some("brewer"), None)
                .await
                .unwrap();
            assert_eq!(owner, "emp-brew-1", "{agent}");
        }
    }

    #[tokio::test]
    async fn step_authority_role_is_the_fallback() {
        let roster = InMemoryRoster::new().with_holder("workflow-approver", "emp-lead-1");
        let owner = resolve_owner(&roster, "system-sim", "j", None, Some("workflow-approver"))
            .await
            .unwrap();
        assert_eq!(owner, "emp-lead-1");
    }

    /// The 2026-08-22 fresh-install abort: docs indexing fired a spawn
    /// rule before the baseline seed ran, the resulting empty
    /// platform-admin lookup was cached for the full TTL, and the
    /// brewery prepare's morning-brew bootstrap then failed Q7 against
    /// the poisoned cache while the just-seeded admin sat invisible —
    /// aborting the install. Absence at bootstrap is exactly the state
    /// about to change; it must be re-asked, while populated rosters
    /// keep the cache that protects people-api from sim bursts.
    #[tokio::test]
    async fn an_empty_holder_list_is_not_cached() {
        use std::sync::Arc;
        use std::sync::Mutex as StdMutex;

        let people: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(vec![]));
        let served = people.clone();
        let app = axum::Router::new().route(
            "/api/people",
            axum::routing::get(move || {
                let rows = served.lock().unwrap().clone();
                async move { axum::Json(serde_json::Value::Array(rows)) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let roster = ReqwestRosterLookup::new(base);
        assert_eq!(
            roster.active_holders("platform-admin").await.unwrap(),
            Vec::<String>::new(),
            "roster starts empty"
        );

        // The baseline seed lands one second later.
        people.lock().unwrap().push(serde_json::json!({
            "id": "emp-admin", "role": "platform-admin", "status": "active"
        }));
        assert_eq!(
            roster.active_holders("platform-admin").await.unwrap(),
            vec!["emp-admin".to_string()],
            "the just-seeded admin must be visible immediately — empty is not a fact"
        );

        // A populated roster IS cached: a second holder appearing does
        // not show up until the TTL expires.
        people.lock().unwrap().push(serde_json::json!({
            "id": "emp-admin-2", "role": "platform-admin", "status": "active"
        }));
        assert_eq!(
            roster.active_holders("platform-admin").await.unwrap(),
            vec!["emp-admin".to_string()],
            "populated results keep the TTL cache"
        );
    }
}
