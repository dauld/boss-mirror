//! In-memory adapter for the `JobsRepository` port.
//!
//! Used by tests and by dev/demo environments that don't need persistence.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use boss_core::job::{Job, JobId, JobStatus, Step, StepId, StepStatus};
use chrono::{DateTime, Utc};

use crate::port::{JobFilter, JobScope, JobsError, JobsRepository, LaunchCalendarRow};

#[derive(Default)]
pub struct InMemoryJobs {
    inner: Mutex<State>,
    recorded: Mutex<Vec<boss_core::event::Event>>,
    refusals: Mutex<Vec<crate::refusals::RecordedRefusal>>,
}

#[derive(Default)]
struct State {
    jobs: HashMap<String, Job>,
    steps: HashMap<String, Step>,
    /// The in-memory mirror of `steps.became_ready_at`: the instant a
    /// step FIRST landed in `Ready`, written once and never moved by a
    /// later write — which is exactly the property the queue-age lens
    /// needs and `updated_at` cannot have (packet 2a0b034e).
    step_ready_at: HashMap<String, DateTime<Utc>>,
    /// The in-memory mirror of `steps.updated_at`: the last write
    /// instant per step. The lens's labelled lower-bound fallback for
    /// steps that never passed through `Ready`.
    step_touched_at: HashMap<String, DateTime<Utc>>,
}

impl InMemoryJobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Events the outbox paths recorded — test visibility (the
    /// in-memory analogue of the Pg adapter's in-tx recording).
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().map(|v| v.clone()).unwrap_or_default()
    }

    fn record_all(&self, events: &[boss_core::event::Event]) {
        if let Ok(mut v) = self.recorded.lock() {
            v.extend_from_slice(events);
        }
    }
}

fn job_key(id: &JobId) -> String {
    id.to_string()
}

fn step_key(id: &StepId) -> String {
    id.to_string()
}

fn matches_filter(job: &Job, filter: &JobFilter) -> bool {
    if let Some(ref kind) = filter.kind
        && &job.kind != kind
    {
        return false;
    }
    if let Some(ref prefix) = filter.kind_prefix
        && !job.kind.starts_with(prefix)
    {
        return false;
    }
    // The retention window replaces the status equality when set:
    // "live OR closed on/after this date". Same contract as the SQL
    // adapter, which expresses it as a CASE over the same two columns
    // — two implementations of one rule, so the behaviour is pinned by
    // a test rather than trusted.
    match filter.closed_since {
        Some(since) => {
            let terminal = matches!(job.status, JobStatus::Closed | JobStatus::Cancelled);
            if terminal && job.closed_on.is_none_or(|c| c < since) {
                return false;
            }
        }
        None => {
            if let Some(status) = filter.status
                && job.status != status
            {
                return false;
            }
        }
    }
    if let Some(priority) = filter.priority
        && job.priority != priority
    {
        return false;
    }
    if let Some(simulated) = filter.simulated
        && job.simulated != simulated
    {
        return false;
    }
    if let Some(ref owner) = filter.owner_id
        && &job.owner_id != owner
    {
        return false;
    }
    if let Some(ref ref_id) = filter.subject_id
        && boss_core::primitives::Subject::id(&job.subject) != ref_id.as_str()
    {
        return false;
    }
    if let Some(ref blocker) = filter.waiting_on {
        // Same resolution contract as the Pg predicate: the waiter
        // wrote the blocker's full id or a >= 8-char prefix of it.
        let wrote = job
            .metadata
            .get("waiting_on")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let matches = wrote == blocker.as_str() || (wrote.len() >= 8 && blocker.starts_with(wrote));
        if !matches {
            return false;
        }
    }
    if let Some(serde_json::Value::Object(wanted)) = &filter.metadata_contains {
        // The in-memory stand-in for Postgres `metadata @> $1` over the
        // flat string-valued documents this filter accepts.
        for (key, value) in wanted {
            if job.metadata.get(key) != Some(value) {
                return false;
            }
        }
    }
    match &filter.scope {
        JobScope::All => {}
        JobScope::None => return false,
        JobScope::OwnerIs(u) => {
            if &job.owner_id != u {
                return false;
            }
        }
        JobScope::OwnerIn(us) => {
            if !us.contains(&job.owner_id) {
                return false;
            }
        }
        JobScope::AccountIn(ps) => {
            // Same territory-scope shape as policy_glue::territory_matches
            // (Wave 3): only Account/Employee subjects carry an id
            // that maps into the account list; all others deny.
            let kind = boss_core::primitives::Subject::kind(&job.subject);
            let id = boss_core::primitives::Subject::id(&job.subject);
            let matches = matches!(kind, "account" | "employee") && ps.iter().any(|p| p == id);
            if !matches {
                return false;
            }
        }
    }
    true
}

#[async_trait]
impl JobsRepository for InMemoryJobs {
    async fn create_job_at(
        &self,
        job: &Job,
        _now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        // Mirror the Pg replay guard: an existing id is a no-op that
        // records nothing.
        let inserted = {
            let mut state = self.inner.lock().expect("poisoned");
            match state.jobs.entry(job_key(&job.id)) {
                std::collections::hash_map::Entry::Occupied(_) => false,
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(job.clone());
                    true
                }
            }
        };
        if inserted {
            self.record_all(events);
        }
        Ok(())
    }

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        Ok(state.jobs.get(&job_key(id)).cloned())
    }

    async fn resolve_job_id_prefix(&self, prefix: &str) -> Result<Vec<JobId>, JobsError> {
        // The map is keyed by the canonical id string (`job_key`), so a
        // prefix match on the key is the same match the Pg adapter makes
        // on `id::text`. Capped at two, like the SQL's LIMIT 2.
        let state = self.inner.lock().expect("poisoned");
        Ok(state
            .jobs
            .values()
            .filter(|j| j.id.to_string().starts_with(prefix))
            .take(2)
            .map(|j| j.id)
            .collect())
    }

    async fn update_job_at(
        &self,
        job: &Job,
        _now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        {
            let mut state = self.inner.lock().expect("poisoned");
            let key = job_key(&job.id);
            let Some(existing) = state.jobs.get(&key) else {
                return Err(JobsError::NotFound(job.id));
            };
            // Mirror the Pg adapter: `simulated` is decided at
            // admission and immutable — an update carries no
            // authority over it. The storage enforces this rather
            // than trusting every caller to.
            let mut next = job.clone();
            next.simulated = existing.simulated;
            state.jobs.insert(key, next);
        }
        self.record_all(events);
        Ok(())
    }

    async fn merge_job_metadata_at(
        &self,
        id: &JobId,
        patch: &serde_json::Map<String, serde_json::Value>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError> {
        // Mirror the Pg adapter: merge under the lock against the row
        // as it stands, null removes, no envelope field moves, and the
        // JOB_UPDATED event is built from the post-merge row.
        let merged = {
            let mut state = self.inner.lock().expect("poisoned");
            let Some(job) = state.jobs.get_mut(&job_key(id)) else {
                return Err(JobsError::NotFound(*id));
            };
            let mut md = match &job.metadata {
                serde_json::Value::Object(m) => m.clone(),
                _ => serde_json::Map::new(),
            };
            for (k, v) in patch {
                if v.is_null() {
                    md.remove(k);
                } else {
                    md.insert(k.clone(), v.clone());
                }
            }
            job.metadata = serde_json::Value::Object(md);
            job.clone()
        };
        let event = stamp.event(
            crate::events::JOB_UPDATED,
            serde_json::to_value(&merged).unwrap_or_default(),
        );
        self.record_all(&[event]);
        Ok(merged)
    }

    async fn list_estate_nodes(&self) -> Result<Vec<crate::port::EstateNode>, JobsError> {
        // The estate is seeded by schema migration, so an in-memory
        // registry genuinely has none — and saying so is better than
        // inventing fixtures a test would then assert against.
        Ok(Vec::new())
    }

    async fn recent_events_by_kind(
        &self,
        kind: &str,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, JobsError> {
        // `recorded` is append-order, so newest-first is a reverse —
        // the same ordering contract the Pg impl gets from
        // `ORDER BY timestamp DESC`.
        let rows = self
            .recorded_events()
            .into_iter()
            .rev()
            .filter(|e| e.kind == kind)
            .take(limit.max(0) as usize)
            .map(|e| {
                serde_json::json!({
                    "event_id": e.id,
                    "timestamp": e.timestamp,
                    "source": e.source,
                    "kind": e.kind,
                    "payload": e.payload,
                })
            })
            .collect();
        Ok(rows)
    }

    async fn repin_workflow_version_at(
        &self,
        id: &JobId,
        to_version: i32,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError> {
        // One column, under the lock, and the event carries the row as
        // it stands afterwards — same shape as the metadata merge above.
        let repinned = {
            let mut state = self.inner.lock().expect("poisoned");
            let Some(job) = state.jobs.get_mut(&job_key(id)) else {
                return Err(JobsError::NotFound(*id));
            };
            job.workflow_version = to_version;
            job.clone()
        };
        let event = stamp.event(
            crate::events::JOB_UPDATED,
            serde_json::to_value(&repinned).unwrap_or_default(),
        );
        self.record_all(&[event]);
        Ok(repinned)
    }

    async fn list_jobs(
        &self,
        filter: &JobFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Job>, i64), JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let mut jobs: Vec<&Job> = state
            .jobs
            .values()
            .filter(|j| matches_filter(j, filter))
            .collect();
        jobs.sort_by_key(|j| std::cmp::Reverse(j.opened_on));
        let total = jobs.len() as i64;
        let page = jobs
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .cloned()
            .collect();
        Ok((page, total))
    }

    async fn add_step_at(
        &self,
        step: &Step,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        // Mirror the Pg replay guard: an existing id is a no-op that
        // records nothing.
        let inserted = {
            let mut state = self.inner.lock().expect("poisoned");
            let key = step_key(&step.id);
            let inserted = match state.steps.entry(key.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => false,
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(step.clone());
                    true
                }
            };
            if inserted {
                // Born ready IS the ready flip — same rule as the
                // INSERT's CASE in the Pg adapter.
                if step.status == StepStatus::Ready {
                    state.step_ready_at.insert(key.clone(), now);
                }
                state.step_touched_at.insert(key, now);
            }
            inserted
        };
        if inserted {
            self.record_all(events);
        }
        Ok(())
    }

    async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        Ok(state.steps.get(&step_key(id)).cloned())
    }

    async fn update_step_at(
        &self,
        step: &Step,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        let mut state = self.inner.lock().expect("poisoned");
        let key = step_key(&step.id);
        let Some(existing) = state.steps.get(&key) else {
            return Err(JobsError::StepNotFound(step.id));
        };
        // Mirror the SQL adapter: the generic update never writes the
        // stamp fields — stamps are append-only via append_sign_off,
        // requirements are set at materialization —
        // and terminal statuses are immutable at the row, so a write
        // merged against a stale pre-completion fetch cannot demote.
        let mut next = step.clone();
        next.sign_offs = existing.sign_offs.clone();
        next.sign_offs_required = existing.sign_offs_required.clone();
        next.fields = existing.fields.clone();
        if matches!(existing.status, StepStatus::Completed | StepStatus::Skipped) {
            next.status = existing.status;
            next.completed_on = existing.completed_on;
            next.metadata = existing.metadata.clone();
        }
        // The ready stamp is written once, at the write that lands the
        // step in Ready, and no later write moves it — the COALESCE in
        // the Pg adapter's UPDATE.
        if next.status == StepStatus::Ready {
            state.step_ready_at.entry(key.clone()).or_insert(now);
        }
        state.step_touched_at.insert(key.clone(), now);
        state.steps.insert(key, next);
        drop(state);
        self.record_all(events);
        Ok(())
    }

    async fn claim_step_at(
        &self,
        step_id: &StepId,
        actor: &str,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<Step, JobsError> {
        let claimed = {
            let mut state = self.inner.lock().expect("poisoned");
            let key = step_key(step_id);
            let Some(existing) = state.steps.get_mut(&key) else {
                return Err(JobsError::StepNotFound(*step_id));
            };
            let held_by_actor = existing.assignee_id.as_deref() == Some(actor);
            let claimable = existing.status == StepStatus::Ready
                && (existing.assignee_id.is_none() || held_by_actor);
            let idempotent = existing.status == StepStatus::Active && held_by_actor;
            if !claimable && !idempotent {
                return Err(JobsError::ClaimConflict {
                    holder: existing.assignee_id.clone(),
                    status: format!("{:?}", existing.status).to_lowercase(),
                });
            }
            existing.assignee_id = Some(actor.to_string());
            existing.status = StepStatus::Active;
            let claimed = existing.clone();
            // A claim bumps `updated_at` in the Pg adapter; the ready
            // stamp, already written at the flip, stays put.
            state.step_touched_at.insert(key, now);
            claimed
        };
        self.record_all(events);
        Ok(claimed)
    }

    async fn append_sign_off(
        &self,
        step_id: &StepId,
        stamp: &boss_core::job::SignOffStamp,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        {
            let mut state = self.inner.lock().expect("poisoned");
            let key = step_key(step_id);
            let Some(existing) = state.steps.get_mut(&key) else {
                return Err(JobsError::StepNotFound(*step_id));
            };
            existing.sign_offs.push(stamp.clone());
            // Mirrors the sign-off UPDATE's `updated_at = $3`.
            state.step_touched_at.insert(key, now);
        }
        self.record_all(events);
        Ok(())
    }

    async fn record_events(&self, events: &[boss_core::event::Event]) -> Result<(), JobsError> {
        self.record_all(events);
        Ok(())
    }

    async fn list_steps(&self, job_id: &JobId) -> Result<Vec<Step>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let job_key = job_id.to_string();
        let mut steps: Vec<Step> = state
            .steps
            .values()
            .filter(|s| s.job_id.to_string() == job_key)
            .cloned()
            .collect();
        steps.sort_by_key(|s| s.sort_order);
        Ok(steps)
    }

    async fn queue_age(
        &self,
        scope: &crate::port::JobScope,
    ) -> Result<Vec<crate::port::QueueAgeRow>, JobsError> {
        // Scope + open-status through `matches_filter`, so the lens
        // and `list_jobs` cannot disagree about whose packets these
        // are (CLAUDE.md §9a — one definition of the scope rule).
        let filter = JobFilter {
            status: Some(JobStatus::Open),
            scope: scope.clone(),
            ..Default::default()
        };
        let state = self.inner.lock().expect("poisoned");
        let mut rows: Vec<crate::port::QueueAgeRow> = state
            .steps
            .values()
            .filter(|s| matches!(s.status, StepStatus::Ready | StepStatus::Active))
            .filter_map(|s| {
                let job = state.jobs.get(&job_key(&s.job_id))?;
                if !matches_filter(job, &filter) {
                    return None;
                }
                let key = step_key(&s.id);
                // Ready flip when recorded; last-write lower bound
                // otherwise — COALESCE(became_ready_at, updated_at).
                let (since, exact) = match (
                    state.step_ready_at.get(&key),
                    state.step_touched_at.get(&key),
                ) {
                    (Some(at), _) => (*at, true),
                    (None, Some(at)) => (*at, false),
                    // Unreachable through the port: every step write
                    // stamps `step_touched_at`. A step with neither
                    // stamp has no honest age, so it has no row.
                    (None, None) => return None,
                };
                Some(crate::port::QueueAgeRow {
                    job_id: s.job_id,
                    job_kind: job.kind.clone(),
                    job_title: job.title.clone(),
                    step_id: s.id,
                    spec_slug: s.spec_slug.clone(),
                    step_title: s.title.clone(),
                    status: s.status,
                    assignee_id: s.assignee_id.clone(),
                    simulated: job.simulated,
                    since,
                    exact,
                })
            })
            .collect();
        // Longest-waiting first; step id as the deterministic
        // tiebreak, matching the SQL's `ORDER BY since, s.id`.
        rows.sort_by(|a, b| {
            a.since
                .cmp(&b.since)
                .then_with(|| a.step_id.to_string().cmp(&b.step_id.to_string()))
        });
        Ok(rows)
    }

    async fn count_in_flight_steps_by_kind(&self, step_kind: &str) -> Result<i64, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let n = state
            .steps
            .values()
            .filter(|s| s.kind == step_kind)
            .filter(|s| {
                // Non-terminal = occupies the resource. v2 has no
                // Blocked; the pre-execution + in-flight set is
                // Pending | Ready | Active.
                matches!(
                    s.status,
                    StepStatus::Pending | StepStatus::Ready | StepStatus::Active,
                )
            })
            .count();
        Ok(n as i64)
    }

    async fn count_open_jobs_for_workflow(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<i64, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let n = state
            .jobs
            .values()
            .filter(|j| j.kind == kind && j.workflow_version == version)
            // Same predicate as the Pg adapter's
            // `status NOT IN ('closed','cancelled')`.
            .filter(|j| !matches!(j.status, JobStatus::Closed | JobStatus::Cancelled))
            .count();
        Ok(n as i64)
    }

    async fn count_jobs_by_kind(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i64)>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
        for job in state.jobs.values() {
            if let Some(want) = status
                && job.status != want
            {
                continue;
            }
            *counts.entry(job.kind.clone()).or_insert(0) += 1;
        }
        Ok(counts.into_iter().collect())
    }

    async fn jobs_tier_distribution(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i32, i64)>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let mut counts: std::collections::BTreeMap<(String, i32), i64> =
            std::collections::BTreeMap::new();
        for job in state.jobs.values() {
            if let Some(want) = status
                && job.status != want
            {
                continue;
            }
            let min_pending = state
                .steps
                .values()
                .filter(|s| s.job_id.to_string() == job.id.to_string())
                .filter(|s| {
                    // Tier = lowest sort_order still awaiting work
                    // (non-terminal). v2 has no Blocked.
                    matches!(
                        s.status,
                        StepStatus::Pending | StepStatus::Ready | StepStatus::Active,
                    )
                })
                .map(|s| s.sort_order)
                .min();
            let tier = min_pending.unwrap_or(-1);
            *counts.entry((job.kind.clone(), tier)).or_insert(0) += 1;
        }
        Ok(counts
            .into_iter()
            .map(|((kind, tier), n)| (kind, tier, n))
            .collect())
    }

    async fn list_launch_calendar(
        &self,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    ) -> Result<Vec<LaunchCalendarRow>, JobsError> {
        use boss_core::primitives::Subject as _;
        let state = self.inner.lock().expect("poisoned");
        let mut out = Vec::new();
        for job in state.jobs.values() {
            if job.kind != "marketing-motion" {
                continue;
            }
            if matches!(job.status, JobStatus::Closed | JobStatus::Cancelled) {
                continue;
            }

            // Tier = min sort_order of any non-done step, or -1.
            let (tier, launch_step) = state.steps.values().filter(|s| s.job_id == job.id).fold(
                (None::<i32>, None::<&Step>),
                |(tier, launch), s| {
                    let new_tier = if matches!(
                        s.status,
                        StepStatus::Pending | StepStatus::Ready | StepStatus::Active,
                    ) {
                        Some(tier.map_or(s.sort_order, |t| t.min(s.sort_order)))
                    } else {
                        tier
                    };
                    // property, not kind: the launch step is whichever
                    // step carries launch_date (no-step-kind-match rule)
                    let new_launch = if s.metadata.get("launch_date").is_some() {
                        Some(s)
                    } else {
                        launch
                    };
                    (new_tier, new_launch)
                },
            );
            let current_tier = Some(tier.unwrap_or(-1));

            let (launch_date, launch_channel) = match launch_step {
                Some(s) => {
                    let d = s
                        .metadata
                        .get("launch_date")
                        .and_then(|v| v.as_str())
                        .and_then(|v| chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").ok());
                    let c = s
                        .metadata
                        .get("launch_channel")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    (d, c)
                }
                None => (None, None),
            };

            if let Some(d) = launch_date
                && (d < from || d > to)
            {
                continue;
            }

            let subject_id = Some(job.subject.id().to_string());

            out.push(LaunchCalendarRow {
                job_id: job.id,
                title: job.title.clone(),
                owner_id: Some(job.owner_id.clone()),
                subject_id,
                status: job.status,
                current_tier,
                launch_date,
                launch_channel,
            });
        }
        out.sort_by(|a, b| {
            a.launch_date
                .cmp(&b.launch_date)
                .then(a.title.cmp(&b.title))
        });
        Ok(out)
    }

    async fn resolve_blockers(
        &self,
        ids: &[StepId],
    ) -> Result<Vec<(StepId, StepStatus)>, JobsError> {
        let state = self.inner.lock().expect("poisoned");
        let mut results = Vec::new();
        for id in ids {
            if let Some(step) = state.steps.get(&step_key(id)) {
                results.push((step.id, step.status));
            }
        }
        Ok(results)
    }

    async fn record_step_write_refusal_at(
        &self,
        refusal: &crate::refusals::StepWriteRefusal,
        now: DateTime<Utc>,
    ) -> Result<(), JobsError> {
        let mut refusals = self.refusals.lock().expect("poisoned");
        // BIGSERIAL starts at 1, so the in-memory ids match what a
        // caller reading either adapter would see.
        let id = refusals.len() as i64 + 1;
        refusals.push(crate::refusals::RecordedRefusal {
            id,
            refused_at: now,
            refusal: refusal.clone(),
        });
        Ok(())
    }

    async fn step_write_refusals(
        &self,
        limit: i64,
    ) -> Result<Vec<crate::refusals::RecordedRefusal>, JobsError> {
        let refusals = self.refusals.lock().expect("poisoned");
        Ok(refusals
            .iter()
            .rev()
            .take(limit.max(0) as usize)
            .cloned()
            .collect())
    }
}

/// Check if all blockers for a step are satisfied (done or skipped).
pub fn blockers_satisfied(statuses: &[(StepId, StepStatus)]) -> bool {
    statuses
        .iter()
        .all(|(_, s)| matches!(s, StepStatus::Completed | StepStatus::Skipped))
}

/// Compute the job status from its steps.
///
/// v2 has no per-step Blocked state, so a Job is `Open` until every
/// step reaches a terminal state (`Completed` / `Skipped`), at which
/// point it's `Closed` (or `PendingSignOff` if a completed step still
/// awaits sign-off). External pauses are a dispatcher concern, not a
/// derived status here.
pub fn compute_job_status(steps: &[Step]) -> JobStatus {
    if steps.is_empty() {
        return JobStatus::Open;
    }
    let all_terminal = steps
        .iter()
        .all(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped));
    if all_terminal {
        // Defensive: completion validation refuses to complete a step
        // with unsatisfied stamps, so this state should be unreachable
        // under the sign-off contract; kept while the PendingSignOff
        // status exists.
        let unsigned = steps.iter().any(|s| !s.sign_offs_satisfied());
        if unsigned {
            return JobStatus::PendingSignOff;
        }
        return JobStatus::Closed;
    }
    JobStatus::Open
}

#[cfg(test)]
mod tests {
    use boss_core::job::{Priority, Subject};
    use chrono::NaiveDate;

    use super::*;

    fn test_date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 4, 16).unwrap()
    }

    /// THE PARTITION HAS TO HAPPEN IN THE QUERY, not on the page.
    ///
    /// Measured on the live system 2026-08-17: 5,201 of 5,964 packets
    /// (87%) are simulated. A surface that fetches a page and then
    /// drops the simulated rows shows a nearly empty list AND a
    /// `total` that disagrees with it — the same failure the retention
    /// window exists to prevent, an order of magnitude worse. So the
    /// filter is asserted through `list_jobs`, including its total.
    #[tokio::test]
    async fn simulated_partitions_both_the_rows_and_the_total() {
        let repo = InMemoryJobs::default();
        for i in 0..3 {
            let mut j = make_job("wholesale-keg-order");
            j.simulated = true;
            j.title = format!("sim {i}");
            repo.create_job(&j).await.expect("create sim");
        }
        let mut real = make_job("ship-a-change");
        real.title = "real one".into();
        repo.create_job(&real).await.expect("create real");

        let only_real = JobFilter {
            simulated: Some(false),
            ..Default::default()
        };
        let (rows, total) = repo.list_jobs(&only_real, 50, 0).await.expect("list");
        assert_eq!(rows.len(), 1, "one real packet");
        assert_eq!(
            total, 1,
            "the total must agree with the rows, not count the sim ones"
        );
        assert_eq!(rows[0].title, "real one");

        let only_sim = JobFilter {
            simulated: Some(true),
            ..Default::default()
        };
        let (rows, total) = repo.list_jobs(&only_sim, 50, 0).await.expect("list");
        assert_eq!(rows.len(), 3);
        assert_eq!(total, 3);

        // Absent means everything — every existing caller keeps its
        // answer, which is what makes this safe to land before any
        // surface opts in.
        let (rows, total) = repo
            .list_jobs(&JobFilter::default(), 50, 0)
            .await
            .expect("list");
        assert_eq!(rows.len(), 4);
        assert_eq!(total, 4);
    }

    fn make_job(kind: &str) -> Job {
        Job::new(
            kind,
            Subject::new("asset", "SN-001"),
            "Test job",
            "emp-1",
            Priority::Standard,
            test_date(),
        )
    }

    #[tokio::test]
    async fn create_and_get_job() {
        let repo = InMemoryJobs::new();
        let job = make_job("refurb");
        repo.create_job(&job).await.unwrap();
        let got = repo.get_job(&job.id).await.unwrap().unwrap();
        assert_eq!(got.id, job.id);
        assert_eq!(got.kind, "refurb");
    }

    #[tokio::test]
    async fn update_job() {
        let repo = InMemoryJobs::new();
        let mut job = make_job("refurb");
        repo.create_job(&job).await.unwrap();
        job.status = JobStatus::Open;
        repo.update_job(&job).await.unwrap();
        let got = repo.get_job(&job.id).await.unwrap().unwrap();
        assert_eq!(got.status, JobStatus::Open);
    }

    #[tokio::test]
    async fn update_unknown_job_errors() {
        let repo = InMemoryJobs::new();
        let job = make_job("refurb");
        let result = repo.update_job(&job).await;
        assert!(matches!(result, Err(JobsError::NotFound(_))));
    }

    #[tokio::test]
    async fn list_jobs_with_filter() {
        let repo = InMemoryJobs::new();
        let j1 = make_job("refurb");
        let j2 = make_job("sale");
        repo.create_job(&j1).await.unwrap();
        repo.create_job(&j2).await.unwrap();

        let filter = JobFilter {
            kind: Some("refurb".into()),
            ..Default::default()
        };
        let (jobs, total) = repo.list_jobs(&filter, 100, 0).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(jobs[0].kind, "refurb");
    }

    #[tokio::test]
    async fn add_and_list_steps() {
        let repo = InMemoryJobs::new();
        let job = make_job("refurb");
        repo.create_job(&job).await.unwrap();

        let s1 = Step::new(job.id, "generic", "Triage", 0).with_assignee("emp-2");
        let s2 =
            Step::new(job.id, "generic", "QA", 1).with_sign_offs_required(vec!["qa-lead".into()]);
        repo.add_step(&s1).await.unwrap();
        repo.add_step(&s2).await.unwrap();

        let steps = repo.list_steps(&job.id).await.unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].title, "Triage");
        assert_eq!(steps[1].title, "QA");
        assert_eq!(steps[1].sign_offs_required, vec!["qa-lead".to_string()]);
    }

    #[tokio::test]
    async fn list_assigned_workable_returns_only_assigned_open_workable() {
        let repo = InMemoryJobs::new();
        let mut job = make_job("ingredient-restock").with_workflow_version(3);
        job.status = JobStatus::Open;
        repo.create_job(&job).await.unwrap();

        // Assigned + Ready -> included.
        let mut a = Step::new(job.id, "procurement", "Place PO", 0).with_assignee("emp-1");
        a.status = StepStatus::Ready;
        repo.add_step(&a).await.unwrap();
        // Assigned + Active -> included.
        let mut b = Step::new(job.id, "billing", "Send bill", 1).with_assignee("emp-2");
        b.status = StepStatus::Active;
        repo.add_step(&b).await.unwrap();
        // Unassigned + Ready -> excluded (no assignee to attribute to).
        let mut c = Step::new(job.id, "bill-approval", "Approve", 2);
        c.status = StepStatus::Ready;
        repo.add_step(&c).await.unwrap();
        // Assigned + Completed -> excluded (terminal).
        let mut d = Step::new(job.id, "trigger", "Trig", 3).with_assignee("emp-1");
        d.status = StepStatus::Completed;
        repo.add_step(&d).await.unwrap();
        // Assigned + Pending -> excluded (not workable yet).
        let e = Step::new(job.id, "receiving", "Receive", 4).with_assignee("emp-1");
        repo.add_step(&e).await.unwrap();

        // Assigned + Ready but on a CLOSED job -> excluded.
        let mut closed = make_job("ingredient-restock");
        closed.status = JobStatus::Closed;
        repo.create_job(&closed).await.unwrap();
        let mut f = Step::new(closed.id, "billing", "Stale", 0).with_assignee("emp-1");
        f.status = StepStatus::Ready;
        repo.add_step(&f).await.unwrap();

        let rows = repo.list_assigned_workable(100).await.unwrap();
        let titles: Vec<&str> = rows.iter().map(|r| r.step.title.as_str()).collect();
        assert_eq!(titles.len(), 2, "only assigned+open+workable: {titles:?}");
        assert!(titles.contains(&"Place PO"));
        assert!(titles.contains(&"Send bill"));
        // The row names the protocol version the packet is pinned to,
        // so an executor can resolve the step's spec (e.g. its
        // spec-authored duration) against the exact Workflow row the
        // Job was admitted under.
        assert!(rows.iter().all(|r| r.workflow_version == 3));
    }

    #[tokio::test]
    async fn list_assignments_by_assignee_or_role_only_workable_steps() {
        let repo = InMemoryJobs::new();
        let mut job = make_job("ingredient-restock");
        job.status = JobStatus::Open;
        repo.create_job(&job).await.unwrap();

        // Assigned to emp-1, Ready -> included (assignee match).
        let mut s_assigned = Step::new(job.id, "procurement", "Place PO", 0).with_assignee("emp-1");
        s_assigned.status = StepStatus::Ready;
        repo.add_step(&s_assigned).await.unwrap();

        // Unassigned, authority_role=bookkeeper, Ready -> included (role match).
        let mut s_role = Step::new(job.id, "bill-approval", "Approve bill", 1);
        s_role.status = StepStatus::Ready;
        s_role.metadata = serde_json::json!({ "authority_role": "bookkeeper" });
        repo.add_step(&s_role).await.unwrap();

        // Assigned to emp-1, Active -> included (active is workable).
        let mut s_active = Step::new(job.id, "billing", "Send bill", 2).with_assignee("emp-1");
        s_active.status = StepStatus::Active;
        repo.add_step(&s_active).await.unwrap();

        // Active, assigned to ANOTHER bookkeeper -> included: role-match on
        // an Active step ignores assignee, so the role's workforce can
        // finish in-progress work (a worker re-finds its multi-day step).
        let mut s_active_other =
            Step::new(job.id, "bill-approval", "Approve other bill", 6).with_assignee("emp-2");
        s_active_other.status = StepStatus::Active;
        s_active_other.metadata = serde_json::json!({ "authority_role": "bookkeeper" });
        repo.add_step(&s_active_other).await.unwrap();

        // Ready, already assigned to someone else -> excluded: not poachable
        // (role-match on a Ready step requires it to be unassigned).
        let mut s_ready_other =
            Step::new(job.id, "bill-approval", "Their bill", 7).with_assignee("emp-2");
        s_ready_other.status = StepStatus::Ready;
        s_ready_other.metadata = serde_json::json!({ "authority_role": "bookkeeper" });
        repo.add_step(&s_ready_other).await.unwrap();

        // Unassigned, authority_role=brewer (not mine) -> excluded.
        let mut s_other = Step::new(job.id, "production-consume", "Mash in", 3);
        s_other.status = StepStatus::Ready;
        s_other.metadata = serde_json::json!({ "authority_role": "brewer" });
        repo.add_step(&s_other).await.unwrap();

        // Completed -> excluded.
        let mut s_done = Step::new(job.id, "trigger", "Trigger", 4).with_assignee("emp-1");
        s_done.status = StepStatus::Completed;
        repo.add_step(&s_done).await.unwrap();

        // Pending -> excluded (not yet eligible).
        let s_pending = Step::new(job.id, "receiving", "Receive", 5).with_assignee("emp-1");
        repo.add_step(&s_pending).await.unwrap();

        // A Ready step on a CLOSED job -> excluded (job not Open).
        let mut closed = make_job("ingredient-restock");
        closed.status = JobStatus::Closed;
        repo.create_job(&closed).await.unwrap();
        let mut s_closed = Step::new(closed.id, "billing", "Stale bill", 0).with_assignee("emp-1");
        s_closed.status = StepStatus::Ready;
        repo.add_step(&s_closed).await.unwrap();

        let rows = repo
            .list_assignments(Some("emp-1"), &["bookkeeper".to_string()], 100)
            .await
            .unwrap();
        let titles: Vec<&str> = rows.iter().map(|r| r.step.title.as_str()).collect();
        assert!(titles.contains(&"Place PO"), "assignee+ready: {titles:?}");
        assert!(titles.contains(&"Approve bill"), "role+ready: {titles:?}");
        assert!(titles.contains(&"Send bill"), "assignee+active: {titles:?}");
        assert!(
            titles.contains(&"Approve other bill"),
            "active role-match ignores assignee: {titles:?}"
        );
        assert!(!titles.contains(&"Mash in"), "other role excluded");
        assert!(
            !titles.contains(&"Their bill"),
            "ready+assigned-other excluded"
        );
        assert!(!titles.contains(&"Trigger"), "completed excluded");
        assert!(!titles.contains(&"Receive"), "pending excluded");
        assert!(!titles.contains(&"Stale bill"), "closed-job step excluded");
        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| r.workflow == "ingredient-restock"));
    }

    #[tokio::test]
    async fn assignment_rows_carry_the_jobs_sim_facts() {
        // A simulated packet must read as simulated in every queue lens,
        // My Day included — the row is the only thing that lens sees.
        let repo = InMemoryJobs::new();
        let mut sim = make_job("ingredient-restock");
        sim.status = JobStatus::Open;
        sim.simulated = true;
        sim.tags = vec!["nightly".to_string()];
        repo.create_job(&sim).await.unwrap();
        let mut s = Step::new(sim.id, "procurement", "Place PO", 0).with_assignee("emp-1");
        s.status = StepStatus::Ready;
        repo.add_step(&s).await.unwrap();

        let mut real = make_job("ingredient-restock");
        real.status = JobStatus::Open;
        repo.create_job(&real).await.unwrap();
        let mut r = Step::new(real.id, "procurement", "Place real PO", 0).with_assignee("emp-1");
        r.status = StepStatus::Ready;
        repo.add_step(&r).await.unwrap();

        let rows = repo
            .list_assignments(Some("emp-1"), &[], 100)
            .await
            .unwrap();
        let sim_row = rows.iter().find(|row| row.job_id == sim.id).unwrap();
        assert!(sim_row.simulated, "simulated job's row reports it");
        assert_eq!(sim_row.tags, vec!["nightly".to_string()]);
        let real_row = rows.iter().find(|row| row.job_id == real.id).unwrap();
        assert!(!real_row.simulated, "a real job's row stays real");
        assert!(real_row.tags.is_empty());

        // The sim workforce's bulk pull reads the same row shape.
        let bulk = repo.list_assigned_workable(100).await.unwrap();
        assert!(
            bulk.iter()
                .find(|row| row.job_id == sim.id)
                .unwrap()
                .simulated,
            "bulk backlog rows carry the flag too"
        );
    }

    #[tokio::test]
    async fn count_in_flight_steps_by_kind_only_counts_non_terminal() {
        let repo = InMemoryJobs::new();
        let job = make_job("refurb");
        repo.create_job(&job).await.unwrap();

        // Three pending `demo-plugin` steps, one active, one done.
        for i in 0..3 {
            let s = Step::new(job.id, "demo-plugin", "pending step", i);
            repo.add_step(&s).await.unwrap();
        }
        let mut active = Step::new(job.id, "demo-plugin", "in flight", 98);
        active.status = StepStatus::Active;
        repo.add_step(&active).await.unwrap();
        let mut done = Step::new(job.id, "demo-plugin", "finished", 99);
        done.status = StepStatus::Completed;
        repo.add_step(&done).await.unwrap();

        // Pending × 3 + Active × 1 = 4 non-terminal. Done is excluded.
        let n = repo
            .count_in_flight_steps_by_kind("demo-plugin")
            .await
            .unwrap();
        assert_eq!(n, 4);

        // A different kind returns zero.
        let n = repo.count_in_flight_steps_by_kind("other").await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn cross_job_resolve_blockers() {
        let repo = InMemoryJobs::new();
        let job_a = make_job("refurb");
        let job_b = make_job("sale");
        repo.create_job(&job_a).await.unwrap();
        repo.create_job(&job_b).await.unwrap();

        let mut step_a = Step::new(job_a.id, "generic", "Finish refurb", 0);
        step_a.status = StepStatus::Completed;
        repo.add_step(&step_a).await.unwrap();

        let step_b =
            Step::new(job_b.id, "generic", "Ship device", 0).with_blocked_by(vec![step_a.id]);
        repo.add_step(&step_b).await.unwrap();

        let statuses = repo.resolve_blockers(&step_b.blocked_by).await.unwrap();
        assert!(blockers_satisfied(&statuses));
    }

    #[tokio::test]
    async fn cross_job_blockers_not_satisfied() {
        let repo = InMemoryJobs::new();
        let job_a = make_job("refurb");
        let job_b = make_job("sale");
        repo.create_job(&job_a).await.unwrap();
        repo.create_job(&job_b).await.unwrap();

        let step_a = Step::new(job_a.id, "generic", "Finish refurb", 0);
        // status is Pending — not done
        repo.add_step(&step_a).await.unwrap();

        let step_b =
            Step::new(job_b.id, "generic", "Ship device", 0).with_blocked_by(vec![step_a.id]);
        repo.add_step(&step_b).await.unwrap();

        let statuses = repo.resolve_blockers(&step_b.blocked_by).await.unwrap();
        assert!(!blockers_satisfied(&statuses));
    }

    #[test]
    fn compute_status_all_done_no_signoff() {
        let job_id = JobId::new();
        let mut s = Step::new(job_id, "generic", "Do it", 0);
        s.status = StepStatus::Completed;
        assert_eq!(compute_job_status(&[s]), JobStatus::Closed);
    }

    #[test]
    fn compute_status_all_done_needs_signoff() {
        let job_id = JobId::new();
        let mut s =
            Step::new(job_id, "generic", "QA", 0).with_sign_offs_required(vec!["qa-lead".into()]);
        s.status = StepStatus::Completed;
        // no stamp collected — sign-off outstanding (defensive state;
        // completion validation normally prevents reaching this)
        assert_eq!(compute_job_status(&[s]), JobStatus::PendingSignOff);
    }

    #[test]
    fn compute_status_completed_plus_skipped_is_closed() {
        // v2: a Skipped branch is terminal. A Job whose steps are all
        // Completed or Skipped (no sign-off pending) is Closed.
        let job_id = JobId::new();
        let mut s1 = Step::new(job_id, "generic", "A", 0);
        s1.status = StepStatus::Completed;
        let mut s2 = Step::new(job_id, "generic", "B", 1);
        s2.status = StepStatus::Skipped;
        assert_eq!(compute_job_status(&[s1, s2]), JobStatus::Closed);
    }

    #[test]
    fn compute_status_mixed_is_open() {
        let job_id = JobId::new();
        let mut s1 = Step::new(job_id, "generic", "A", 0);
        s1.status = StepStatus::Completed;
        let s2 = Step::new(job_id, "generic", "B", 1); // Pending
        assert_eq!(compute_job_status(&[s1, s2]), JobStatus::Open);
    }

    /// The terminal retention window, in-memory half.
    ///
    /// The Postgres adapter expresses this rule as a CASE over
    /// `status` and `closed_on`; this adapter expresses it as Rust.
    /// Two implementations of one contract, so the contract is pinned
    /// here as well as in `tests/postgres_filter.rs` — the sibling
    /// there carries the full reasoning for why the window exists.
    #[tokio::test]
    async fn closed_since_keeps_live_and_recent_and_drops_the_rest() {
        let repo = InMemoryJobs::new();
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();

        let mut live = make_job("user-feedback");
        // Blocked, not Open: live is live regardless of age, and this
        // is the half of the rule a bare `closed_on >= x` deletes.
        live.status = JobStatus::Blocked;
        let mut recent = make_job("user-feedback");
        recent.status = JobStatus::Closed;
        recent.closed_on = Some(d(8, 14));
        let mut old = make_job("user-feedback");
        old.status = JobStatus::Closed;
        old.closed_on = Some(d(1, 5));
        let mut cancelled = make_job("user-feedback");
        cancelled.status = JobStatus::Cancelled;
        cancelled.closed_on = Some(d(1, 6));
        // Terminal with no close date recorded. Postgres drops it
        // (`NULL >= date` is NULL, not true); this must agree.
        let mut undated = make_job("user-feedback");
        undated.status = JobStatus::Closed;

        for j in [&live, &recent, &old, &cancelled, &undated] {
            repo.create_job(j).await.unwrap();
        }

        let filter = JobFilter {
            kind: Some("user-feedback".into()),
            closed_since: Some(d(8, 1)),
            ..Default::default()
        };
        let (rows, total) = repo.list_jobs(&filter, 100, 0).await.unwrap();
        assert_eq!(total, 2, "count must apply the same window as the page");
        assert!(
            rows.iter().any(|j| j.id == live.id),
            "live packets always survive"
        );
        assert!(rows.iter().any(|j| j.id == recent.id));
        assert!(!rows.iter().any(|j| j.id == old.id));
        assert!(
            !rows.iter().any(|j| j.id == cancelled.id),
            "cancelled is terminal too — an old cancellation is not recent work"
        );
        assert!(
            !rows.iter().any(|j| j.id == undated.id),
            "a terminal packet with no closed_on cannot prove it is recent"
        );
    }

    /// `closed_since` wins over `status` rather than intersecting it.
    ///
    /// If they combined as AND, `status=open&closed_within=14` would
    /// return open packets only and the terminal columns would be
    /// empty again — the exact bug the window exists to fix.
    #[tokio::test]
    async fn closed_since_overrides_status_rather_than_intersecting_it() {
        let repo = InMemoryJobs::new();
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();

        // Job::new starts a packet at Draft, so say Open out loud —
        // otherwise the status filter below is testing nothing.
        let mut open = make_job("user-feedback");
        open.status = JobStatus::Open;
        let mut recent = make_job("user-feedback");
        recent.status = JobStatus::Closed;
        recent.closed_on = Some(d(8, 14));
        repo.create_job(&open).await.unwrap();
        repo.create_job(&recent).await.unwrap();

        let filter = JobFilter {
            kind: Some("user-feedback".into()),
            status: Some(JobStatus::Open),
            closed_since: Some(d(8, 1)),
            ..Default::default()
        };
        let (rows, total) = repo.list_jobs(&filter, 100, 0).await.unwrap();
        assert_eq!(
            rows.len(),
            2,
            "the recently-closed packet survives status=open"
        );
        assert_eq!(total, 2);
    }

    /// With no window, `status` behaves exactly as it always has.
    #[tokio::test]
    async fn no_window_means_the_old_status_behaviour() {
        let repo = InMemoryJobs::new();
        let mut open = make_job("user-feedback");
        open.status = JobStatus::Open;
        let mut closed = make_job("user-feedback");
        closed.status = JobStatus::Closed;
        closed.closed_on = NaiveDate::from_ymd_opt(2026, 1, 5);
        repo.create_job(&open).await.unwrap();
        repo.create_job(&closed).await.unwrap();

        let all = JobFilter {
            kind: Some("user-feedback".into()),
            ..Default::default()
        };
        assert_eq!(repo.list_jobs(&all, 100, 0).await.unwrap().1, 2);

        let only_open = JobFilter {
            kind: Some("user-feedback".into()),
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        let (rows, _) = repo.list_jobs(&only_open, 100, 0).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, open.id);
    }
}
