//! Job CRUD, the dashboard/landing read surfaces, and step-type
//! discovery.

use super::*;

use axum::extract::{Path, Query};

// ---------------------------------------------------------------------------
// Step type discovery
// ---------------------------------------------------------------------------

pub(super) async fn list_step_types<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
) -> Response {
    let types: Vec<_> = state.step_registry.all().into_iter().cloned().collect();
    Json(types).into_response()
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ListJobsQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    kind: Option<String>,
    /// Prefix match on kind (e.g. `kind_prefix=refurb` returns both
    /// `refurb-used` and `refurb-oem-new` jobs).
    kind_prefix: Option<String>,
    status: Option<JobStatus>,
    owner_id: Option<String>,
    /// Filter by the Job's subject id, regardless of subject kind. One
    /// generic spelling for every kind — there are no per-kind aliases
    /// to pick the wrong one of.
    subject_id: Option<String>,
    /// Only jobs whose `metadata.waiting_on` names this Job (full id
    /// here; the stored value may be a >= 8-char prefix). The
    /// clear-on-close handler's query.
    waiting_on: Option<String>,
    /// Terminal retention window, in days. `closed_within=14` returns
    /// live packets plus anything closed in the last fortnight, and
    /// drops the rest — what a BOARD wants, since it must fetch
    /// terminal packets to place them in terminal columns but has no
    /// use for a year of them. The feedback board was pulling all 173
    /// user-feedback packets to show 14 live ones and was 27 short of
    /// silently truncating at its own limit.
    closed_within: Option<i64>,
    /// `simulated=false` drops the demo tenant's packets; `true` keeps
    /// only those; absent is everything, so no existing caller moves.
    /// 87% of packets are simulated, so a surface that wants real work
    /// has to say so in the query rather than filter the page it got.
    simulated: Option<bool>,
}

pub(super) async fn list_jobs<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListJobsQuery>,
) -> Response {
    // Policy: compute the caller's read-scope Predicate. Denied →
    // return an empty collection so the UI shows a clean empty state
    // instead of a 403 noise. (If you need to know *why*, call /check
    // explicitly.)
    let predicate = match state.policy.scope_predicate(&user, Resource::job()).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };

    // Translate the Predicate into a JobScope the adapter can push
    // into SQL — shared with the station queue lens
    // (`job_scope_from_predicate`), so every packet read surface
    // passes through one policy path.
    let scope = job_scope_from_predicate(&user, &predicate);

    let filter = JobFilter {
        kind: q.kind,
        kind_prefix: q.kind_prefix,
        status: q.status,
        owner_id: q.owner_id,
        subject_id: q.subject_id,
        waiting_on: q.waiting_on,
        closed_since: closed_since_from(q.closed_within, &state).await,
        scope,
        simulated: q.simulated,
        ..Default::default()
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = q.offset.unwrap_or(0).max(0);

    // Scope is applied in the adapter — the returned `total` is the
    // DB-wide count of jobs matching the filter AND the caller's
    // policy scope, and every row in `jobs` is already visible to
    // them. No post-filter pass needed.
    let (jobs, total) = match state.jobs.list_jobs(&filter, limit, offset).await {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    // Enrich each job with its steps so the list view can show progress.
    let mut enriched: Vec<serde_json::Value> = Vec::with_capacity(jobs.len());
    for job in &jobs {
        let steps = state.jobs.list_steps(&job.id).await.unwrap_or_default();
        let mut j = serde_json::to_value(job).unwrap_or_default();
        j["steps"] = serde_json::to_value(&steps).unwrap_or_default();
        enriched.push(j);
    }
    Json(serde_json::json!({
        "data": enriched,
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
    .into_response()
}

/// Resolve `closed_within=<days>` to a date on the authoritative
/// clock, not the process's wall time — the sim runs on boss-clock and
/// a board asking for "the last 14 days" must mean the same fortnight
/// the packets were closed in.
async fn closed_since_from<R: JobsRepository, B: EventBus>(
    days: Option<i64>,
    state: &JobsApiState<R, B>,
) -> Option<chrono::NaiveDate> {
    let days = days?;
    // Negative or absurd values are a caller mistake, not a filter:
    // clamp rather than return an empty board with no explanation.
    let days = days.clamp(0, 3650);
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    Some(today - chrono::Duration::days(days))
}

#[derive(Deserialize)]
pub(super) struct AssignmentsQuery {
    /// Steps assigned to this employee are returned.
    assignee_id: Option<String>,
    /// Comma-separated role slugs; unassigned steps whose
    /// `authority_role` is one of these are returned (claimable by
    /// role). e.g. `roles=bookkeeper,head-brewer`.
    roles: Option<String>,
    /// Bulk mode: when true, ignore `assignee_id`/`roles` and return the
    /// entire assigned-and-workable backlog (every open-Job Ready/Active
    /// step that has an assignee) in one query. The sim workforce's pull.
    #[serde(default)]
    all_assigned: bool,
    limit: Option<i64>,
}

/// Pull surface for the "human-powered state machine" dispatcher:
/// the open, workable steps (Ready | Active) an executor can act on
/// right now — assigned to them, or unassigned and matching a role
/// they hold. Consumed by the SPA My Day surface and the sim's
/// workforce loop (which queries as each simulated employee). Returns
/// `{ data: [AssignmentRow], total }`.
pub(super) async fn list_assignments<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Query(q): Query<AssignmentsQuery>,
) -> Response {
    // Bulk path: the whole assigned backlog in one query (sim workforce).
    if q.all_assigned {
        let limit = q
            .limit
            .unwrap_or(BULK_ASSIGNED_LIMIT)
            .clamp(1, BULK_ASSIGNED_LIMIT);
        return match state.jobs.list_assigned_workable(limit).await {
            Ok(rows) => {
                let total = rows.len();
                Json(serde_json::json!({ "data": rows, "total": total })).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
    }
    let roles: Vec<String> = q
        .roles
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // No selector at all → empty (avoid returning the whole table).
    if q.assignee_id.is_none() && roles.is_empty() {
        return Json(serde_json::json!({ "data": [], "total": 0 })).into_response();
    }
    match state
        .jobs
        .list_assignments(q.assignee_id.as_deref(), &roles, limit)
        .await
    {
        Ok(rows) => {
            let total = rows.len();
            let data: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| assignment_row_json(r, &state.step_registry))
                .collect();
            Json(serde_json::json!({ "data": data, "total": total })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// One assignment row, plus the completion contract of its step kind.
///
/// WHY THE SERVER ANSWERS THIS. David, 2026-08-16: *"it probably makes
/// sense to have a special separation between jobs that are in a queue
/// with a human-only policy with jobs that agents are also eligible
/// for as a practical consideration"* — and, on why the distinction
/// matters at all: *"We intentionally do not want many protocols where
/// policy requires a human because that is slow."*
///
/// That separation is already data. `StepType::completion` is the
/// closed axis of the step alphabet: `human` means an operator holding
/// the `authority_role` completes it, `agent` means the dispatcher
/// executes it on `step.ready` and the human workforce never pulls it.
/// So the queue split needs no new field, no new policy concept, and
/// no list of kinds in the frontend — just the registry fact the
/// server already holds, carried on the row.
///
/// `null` when the StepType registry does not know the kind. That is
/// a real condition (a tenant protocol naming a kind this deployment
/// has not registered) and it is reported rather than smoothed over;
/// the reader's job is to treat an unknown contract as needing a
/// person, which is the safe direction — it puts the packet in front
/// of somebody instead of filing it under "an agent will get it".
fn assignment_row_json(
    row: &crate::port::AssignmentRow,
    steps: &crate::step_registry::StepRegistry,
) -> serde_json::Value {
    let mut v = serde_json::to_value(row).unwrap_or_default();
    v["step"]["completion"] = serde_json::to_value(steps.get(&row.step.kind).map(|t| t.completion))
        .unwrap_or(serde_json::Value::Null);
    // The second registry fact the queues split on (291a73a7, option
    // c): is completing this step a DECISION? Same null-when-unknown
    // contract as `completion`, and the same safe direction for the
    // reader — an unknown kind is treated as a decision for a person.
    v["step"]["decision_shaped"] =
        serde_json::to_value(steps.get(&row.step.kind).map(|t| t.decision_shaped))
            .unwrap_or(serde_json::Value::Null);
    v
}

#[derive(Deserialize)]
pub(super) struct JobsSummaryQuery {
    status: Option<JobStatus>,
}

/// Lightweight counts-by-kind for the operating-model view.
///
/// Returns `{ "counts": {kind: n}, "total": N, "status": "open"? }`.
/// Unlike /api/jobs, this doesn't paginate — the result is O(kinds)
/// not O(jobs), and the caller uses it to light up per-phase
/// counts on a company-map view without pulling 5k+ rows of
/// Job JSON over the wire.
pub(super) async fn jobs_summary<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Query(q): Query<JobsSummaryQuery>,
) -> Response {
    match state.jobs.count_jobs_by_kind(q.status).await {
        Ok(pairs) => {
            let total: i64 = pairs.iter().map(|(_, n)| *n).sum();
            let counts: serde_json::Map<String, serde_json::Value> = pairs
                .into_iter()
                .map(|(k, n)| (k, serde_json::Value::from(n)))
                .collect();
            Json(serde_json::json!({
                "counts": counts,
                "total": total,
                "status": q.status.map(job_status_str_public),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Public landing-page surface. Returns a small bundle of
/// "what's the brewery doing right now": per-Workflow counts of
/// open Jobs + a list of the most recently-opened in-flight
/// Jobs with their current step. No auth — the gateway proxies
/// this unauth so the public landing can render a live window
/// into the operating company.
pub(super) async fn jobs_live<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
) -> Response {
    use crate::port::{JobFilter, JobScope};
    // Counts by kind (open only — closed Jobs are history; the
    // landing wants in-flight).
    let by_kind = match state
        .jobs
        .count_jobs_by_kind(Some(boss_core::job::JobStatus::Open))
        .await
    {
        Ok(pairs) => pairs,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let total: i64 = by_kind.iter().map(|(_, n)| *n).sum();
    let counts: serde_json::Map<String, serde_json::Value> = by_kind
        .into_iter()
        .map(|(k, n)| (k, serde_json::Value::from(n)))
        .collect();

    // Latest 12 open Jobs as a "what's running right now" feed.
    let filter = JobFilter {
        kind: None,
        kind_prefix: None,
        status: Some(boss_core::job::JobStatus::Open),
        closed_since: None,
        priority: None,
        owner_id: None,
        subject_id: None,
        waiting_on: None,
        metadata_contains: None,
        scope: JobScope::All,
        // Unchanged on purpose. This feed currently shows every
        // packet, 87% of which are the demo tenant's; narrowing it is
        // a decision about what this surface is FOR, not part of
        // adding the capability, so it is left to the caller that
        // owns the surface.
        simulated: None,
    };
    let jobs = match state.jobs.list_jobs(&filter, 12, 0).await {
        Ok((jobs, _total)) => jobs,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Strip the heavy bits — the public surface only needs the
    // shape that lights up the landing's "in-flight" panel.
    let recent: Vec<serde_json::Value> = jobs
        .into_iter()
        .map(|j| {
            serde_json::json!({
                "id": j.id.to_string(),
                "kind": j.kind,
                "title": j.title,
                "status": j.status,
                "priority": j.priority,
                "subject_kind": boss_core::primitives::Subject::kind(&j.subject),
                "subject_id": boss_core::primitives::Subject::id(&j.subject),
                "opened_on": j.opened_on,
            })
        })
        .collect();

    Json(serde_json::json!({
        "counts": counts,
        "open_total": total,
        "recent": recent,
        // Sim_clock snapshot drives the public landing's
        // "simulated time: 2026-07-09" indicator + the /workflows
        // header. Derived from clock-api directly, so the date the
        // SPA shows always matches the timestamp events stamp.
        "sim_clock": sim_clock_state_from_clock(state.clock.as_ref()).await,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct LaunchCalendarQuery {
    /// ISO date (YYYY-MM-DD); defaults to today (UTC).
    from: Option<chrono::NaiveDate>,
    /// ISO date (YYYY-MM-DD); defaults to `from + 90 days`.
    to: Option<chrono::NaiveDate>,
}

/// Launch-calendar projection per examples/used-device-shop/design/marketing-needs.md E2. Returns every
/// open/in-flight `marketing-motion` Job with its tier-4
/// `marketing-launch` step's date + channel, plus the Job's current
/// tier. Frontend renders at `/calendar` (standalone) and in the exec
/// dashboard next-30-days panel.
pub(super) async fn launch_calendar<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Query(q): Query<LaunchCalendarQuery>,
) -> Response {
    let from = q
        .from
        .unwrap_or(boss_clock_client::now_from(&state.clock).await.date_naive());
    let to = q.to.unwrap_or_else(|| from + chrono::Duration::days(90));
    match state.jobs.list_launch_calendar(from, to).await {
        Ok(rows) => {
            #[derive(Serialize)]
            struct Out {
                data: Vec<LaunchCalendarRow>,
                from: chrono::NaiveDate,
                to: chrono::NaiveDate,
            }
            Json(Out {
                data: rows,
                from,
                to,
            })
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Return the public kebab-case string for a JobStatus.
/// Mirrors the DB storage format used by the adapters.
pub(super) fn job_status_str_public(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Draft => "draft",
        JobStatus::Open => "open",
        JobStatus::Blocked => "blocked",
        JobStatus::PendingSignOff => "pending-sign-off",
        JobStatus::Closed => "closed",
        JobStatus::Cancelled => "cancelled",
    }
}

/// Per-kind distribution of Jobs across step sort_order tiers.
///
/// Response shape:
/// ```json
/// {
///   "by_kind": {
///     "refurb-used": { "tiers": { "0": 12, "1": 55153, "2": 4, "-1": 100 } },
///     "sale":        { "tiers": { "0": 55779, "1": 3, "-1": 177 } }
///   },
///   "status": "open"
/// }
/// ```
/// `tiers[-1]` = Jobs with every step terminal (completed/skipped)
/// but the Job not yet closed (e.g. awaiting sign-off). Frontend maps
/// tier → lifecycle phase via the Workflow's step list (sort_order
/// buckets).
pub(super) async fn jobs_phase_distribution<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Query(q): Query<JobsSummaryQuery>,
) -> Response {
    match state.jobs.jobs_tier_distribution(q.status).await {
        Ok(rows) => {
            let mut by_kind: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            for (kind, tier, count) in rows {
                let entry = by_kind
                    .entry(kind)
                    .or_insert_with(|| serde_json::json!({ "tiers": {} }));
                if let Some(tiers) = entry.get_mut("tiers").and_then(|v| v.as_object_mut()) {
                    tiers.insert(tier.to_string(), serde_json::Value::from(count));
                }
            }
            Json(serde_json::json!({
                "by_kind": by_kind,
                "status": q.status.map(job_status_str_public),
            }))
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct CreateJobQuery {
    /// When false, the handler creates the Job row but skips
    /// materializing the Workflow's steps. Used by the brewery
    /// engine, which emits its own deterministic-UUID step
    /// creates via POST /api/jobs/{id}/steps and would otherwise
    /// land 2× the steps per Job. SPA / admin creates omit the
    /// param so steps auto-materialize (default true).
    #[serde(default = "default_materialize_steps")]
    materialize_steps: bool,
}

fn default_materialize_steps() -> bool {
    true
}

/// Map a persistence error to the response the caller deserves. The
/// `job_edges` check trigger rejects an unresolvable declared link
/// with a message that IS the guard's whole value ("job edge
/// ship-a-change.backlog_item references unresolvable Job X") — as a
/// bare 500 it took its own builder two attempts to understand
/// (`8424fb8d`). A caller error gets a 400 carrying the guard's text;
/// everything else stays a 500.
fn persist_error_response(e: impl std::fmt::Display) -> Response {
    let msg = e.to_string();
    if msg.contains("job edge") && msg.contains("unresolvable") {
        (StatusCode::BAD_REQUEST, edge_guidance(msg)).into_response()
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
    }
}

/// A rejected edge tells the author WHAT was refused; this adds what
/// to do instead.
///
/// `backlog_item` is the declared, ref-checked link from a change to
/// the feedback packet it answers, and the dispatcher's
/// `complete-feedback-branch-on-car-merged` rule follows it to close
/// that packet when the change merges. So it is worth preferring, and
/// worth refusing when it does not resolve — a link to nothing closes
/// nothing.
///
/// But not every motivating item IS a Job on this instance. Some
/// predate the packet model, some arrived as a sentence in chat. The
/// free-text field `backlog_text` carries those: it is not a declared
/// edge, so nothing ref-checks it and nothing follows it. Without
/// this sentence an author who hit the guard had two moves and no way
/// to tell which was intended — the observed response was to drop the
/// reference entirely, which is how sixteen packets ended up
/// unlinked.
fn edge_guidance(msg: String) -> String {
    if msg.contains("backlog_item") {
        format!(
            "{msg} — `backlog_item` is a declared job edge and must name a Job on this \
             instance (it is what closes that packet when this change merges). For a \
             legacy or free-text referent, put it in `backlog_text` instead, which is \
             prose and is not ref-checked."
        )
    } else {
        msg
    }
}

pub(super) async fn create_job<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<CreateJobQuery>,
    Json(mut raw): Json<serde_json::Value>,
) -> Response {
    // `opened_on` is optional on the wire: dispatcher- and
    // operator-initiated creates omit it and inherit the authoritative
    // (sim-aware) clock; the simulator supplies it explicitly to stamp
    // historical sim-dates. Inject the default before deser since the
    // shared `Job` type requires the field.
    let now = boss_clock_client::now_from(&state.clock).await;
    if raw.get("opened_on").is_none_or(|v| v.is_null())
        && let Some(obj) = raw.as_object_mut()
    {
        obj.insert("opened_on".to_string(), serde_json::json!(now.date_naive()));
    }
    let mut job: Job = match serde_json::from_value(raw) {
        Ok(j) => j,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid job body: {e}"),
            )
                .into_response();
        }
    };

    // Admission decides sim-vs-real ONCE, here, and the flag never
    // moves again (03-jobs.sql: the epoch trim leans on a Job's rows
    // all sharing one fate). Two admissible sources, OR-ed: an
    // explicit `simulated: true` on the body (demo seeding, tests),
    // or the request arriving on a sim chain (`x-sim-origin` — how
    // every sim-engine create presents). The OR means a sim chain can
    // never mint real work, even with a body that claims otherwise.
    job.simulated = job.simulated || boss_core::sim_origin::is_in_sim_chain();

    // Validate the kind against the Workflow registry. When no registry
    // is plumbed (older tests) we accept any kind string. We capture
    // the active spec here so the step-materialization pass below
    // doesn't need a second registry lookup.
    let kind_spec = if let Some(ref reg) = state.kind_registry {
        match reg.get_active(&job.kind).await {
            Ok(spec) => Some(spec),
            Err(crate::registry::WorkflowError::NotFound(_)) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("unknown or inactive job kind: {}", job.kind),
                )
                    .into_response();
            }
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    } else {
        None
    };

    // Pin the Job to the kind's active version — the version it opens
    // under. Per docs/architecture-decisions.md §Jobs, Workflows, Steps:
    // in-flight Jobs pin to the version they opened under, and creation
    // is blocked against draft/retired kinds (enforced by get_active
    // above, which 400s on an inactive kind). Server-assigned —
    // overrides any value a client put on the wire.
    if let Some(ref spec) = kind_spec {
        job.workflow_version = spec.version;
    }

    // Q7: every Job names a responsible HUMAN owner. Automation-shaped
    // owners (system-sim, automation:*, rule:*) resolve server-side to
    // the kind's owner_role holder (registry metadata), falling back
    // to the first role-bearing step's authority_role; a human-shaped
    // owner is kept. Unresolvable → reject: a Job nobody owns is the
    // modeling error this gate ends. Steps stay automation-ownable.
    if let Some(ref roster) = state.roster {
        let owner_role = kind_spec
            .as_ref()
            .and_then(|s| s.metadata.get("owner_role"))
            .and_then(|v| v.as_str());
        let step_fallback = kind_spec
            .as_ref()
            .and_then(|s| s.steps.iter().find_map(|st| st.authority_role.as_deref()));
        match crate::owner_resolution::resolve_owner(
            roster.as_ref(),
            &job.owner_id,
            &job.id.to_string(),
            owner_role,
            step_fallback,
        )
        .await
        {
            Ok(owner) => job.owner_id = owner,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, e).into_response();
            }
        }
    }

    // When `subject = Subject::Custom`, validate `custom_kind` against
    // the SubjectKind registry. Closed-variant subjects (System,
    // Account, Vendor, …) are intrinsically valid and bypass the check.
    if let Err(resp) = validate_custom_subject(&state, &job.subject).await {
        return resp;
    }

    // When the existence checker is plumbed, ask the upstream service
    // whether the Subject id exists. NotFound → 400; Unavailable →
    // fail CLOSED (503). The abort-by-default posture (subject-model
    // design Q2, resolved 2026-07-15): a Job about an unverifiable
    // subject is exactly the phantom class the gate exists to stop,
    // and with the PgSubjectExistence adapter "unavailable" means the
    // same Postgres this handler is about to write to — nothing
    // downstream would succeed anyway.
    if let Some(check) = &state.subject_existence {
        match check.check(&job.subject).await {
            Ok(()) => {}
            Err(crate::subject_existence::SubjectExistenceError::NotFound(id)) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("subject does not exist: {id}"),
                )
                    .into_response();
            }
            Err(crate::subject_existence::SubjectExistenceError::Unavailable(msg)) => {
                tracing::warn!(
                    %msg,
                    workflow = %job.kind,
                    "subject existence check unavailable; failing closed"
                );
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "subject existence check unavailable — retry",
                )
                    .into_response();
            }
        }
    }

    let job_id = job.id;

    // OUTBOX (phase 2): JOB_CREATED records on the transactional
    // outbox INSIDE the job-insert transaction — the log and the
    // projection commit or fail together, which subsumes the old
    // log-first two-phase dance (emit, then write, 500 in between).
    // The adapter's ON CONFLICT replay guard gates the event, so a
    // re-emitted Job (deterministic sim runs) records nothing —
    // before, every replay published a duplicate created event.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    // Every event about the Job inherits its admission-fixed flag as
    // the `_simulated` marker — the packet, not the transport context
    // of the write, is the source of truth for sim-vs-real.
    let job_stamp = state
        .publisher
        .stamp_with_actor(actor)
        .await
        .with_simulated(job.simulated);
    let job_event = job_stamp.event(
        events::JOB_CREATED,
        serde_json::to_value(&job).unwrap_or_default(),
    );
    // Row-touch columns bind the stamp's wall time — the rebuilder
    // reproduces them from audit_log.timestamp, so live and replay
    // must read the same instant. Business dates (opened_on, `{day}`
    // tokens below) keep the authoritative clock's `now`.
    if let Err(e) = state
        .jobs
        .create_job_at(&job, job_stamp.timestamp, &[job_event])
        .await
    {
        return persist_error_response(e);
    }

    // Materialize the Workflow's steps into actual `steps` rows. Job
    // kinds with no steps (`ad-hoc`, where the user defines work as
    // they go) materialize into zero steps.
    //
    // The brewery engine sets ?materialize_steps=false because it
    // emits its own deterministic-UUID step creates via
    // POST /api/jobs/{id}/steps; without the opt-out, every Job
    // would carry 2× the spec's step count.
    if q.materialize_steps
        && let Some(spec) = kind_spec
    {
        // Live-API path stamps `{day}` tokens against the
        // clock-api's current day so payroll / period-end
        // metadata derives from the system clock (sim or wall
        // depending on the deploy's clock mode), matching what
        // the sim engine does with its own day cursor.
        let steps = crate::registry::materialize_steps_at(
            &spec,
            &job.subject,
            job_id,
            &job.metadata,
            boss_core::job::StepId::new,
            Some(now.date_naive()),
            // Resolve trigger provenance at materialization: the firing
            // trigger (named by `metadata.trigger_name`) is born
            // `Completed`, its alternatives `Skipped`. Every production
            // Job — dispatcher-spawned, sim, operator — flows through
            // here, so this is the single point that makes triggers
            // honest.
            Some(state.step_registry.as_ref()),
        );
        // Materialization is ATOMIC from an observer's view. A consumer
        // that reacts to a `step.ready` event — the dispatcher's marker
        // auto-complete, a delegate-subjob fork — must see the COMPLETE
        // step graph. So two passes: (1) persist EVERY step with its
        // STEP_CREATED event recorded in the SAME transaction (outbox
        // phase 2 — the old emit-then-write window is gone); (2) only
        // once the whole graph is durable, record `step.ready`.
        // Materialized steps need their STEP_CREATED events or the
        // rebuilder at boss-jobs/src/rebuild.rs can't reconstruct the
        // rows from audit_log and the projection diverges from the log.
        for step in &steps {
            let step_actor = user
                .ambient_actor()
                .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
            let step_stamp = state
                .publisher
                .stamp_with_actor(step_actor)
                .await
                .with_simulated(job.simulated);
            let step_event =
                step_stamp.event(events::STEP_CREATED, events::step_state_payload(step));
            if let Err(e) = state
                .jobs
                .add_step_at(step, step_stamp.timestamp, &[step_event])
                .await
            {
                tracing::warn!(
                    job_id = %job_id,
                    step_id = %step.id,
                    error = %e,
                    "failed to write materialized step projection",
                );
            }
        }
        // Second pass: the full step graph is now persisted, so any observer
        // of a `step.ready` event sees a complete, consistent Job.
        // `materialize_steps_at` ran the open-time readiness pass, so the
        // trigger (and any step whose `ready_when` already holds) is `Ready`
        // here — record `step.ready.<kind>` on the outbox so the
        // dispatcher's marker auto-complete + delegate-subjob forks (D7)
        // react against the whole graph, never a partial one.
        let mut ready_events = Vec::new();
        for step in &steps {
            if step.status == StepStatus::Ready && !step.kind.is_empty() {
                let ready_actor = user
                    .ambient_actor()
                    .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
                ready_events.push(build_step_ready_event(&state, &job, step, &ready_actor).await);
            }
        }
        if !ready_events.is_empty()
            && let Err(e) = state.jobs.record_events(&ready_events).await
        {
            tracing::warn!(job_id = %job_id, error = %e, "failed to record step.ready markers");
        }
        if !steps.is_empty() {
            tracing::debug!(
                job_id = %job_id,
                kind = %job.kind,
                step_count = steps.len(),
                "materialized job kind steps",
            );
        }
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": job_id.to_string() })),
    )
        .into_response()
}

#[derive(Serialize)]
pub(super) struct JobDetail {
    #[serde(flatten)]
    job: Job,
    steps: Vec<Step>,
}

pub(super) async fn get_job<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };

    match state.jobs.get_job(&job_id).await {
        Ok(Some(job)) => {
            let steps = state.jobs.list_steps(&job_id).await.unwrap_or_default();
            Json(JobDetail { job, steps }).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "job not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Per-Job SSE stream. Server-side polls the Job + its steps
/// every 2s and pushes a `JobDetail` JSON frame when something
/// observable has changed (job status / priority / closed_on,
/// or any step's status / completed_on / metadata-version
/// signature), so an operator viewing the page sees a step go
/// from in-progress to done the moment the daemon (or another
/// operator) makes the transition.
///
/// Doesn't subscribe to NATS directly — boss-jobs-api carries
/// its own DB pool but isn't a bus subscriber. Server-side
/// polling is the same shape as `/api/jobs/sim-clock/stream`;
/// the dedupe keeps push volume low (most ticks hold steady).
pub(super) async fn job_stream<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
) -> impl axum::response::IntoResponse {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use std::convert::Infallible;
    use std::time::Duration;

    // Parse upfront; on invalid id the stream emits one `error`
    // frame and exits. Keeping a single stream type lets `Sse::new`
    // accept it (each `stream!` macro produces a unique type, so
    // branched-return variants don't unify).
    let parsed_id = parse_job_id(&id);

    let stream = async_stream::stream! {
        let Some(job_id) = parsed_id else {
            yield Ok::<_, Infallible>(
                SseEvent::default().event("error").data("invalid job id"),
            );
            return;
        };
        // Compact change-detection signature: (job_status,
        // priority, closed_on, [(step_id, status, completed_on)]).
        // Cheap to hash, doesn't depend on the metadata blob's
        // exact JSON serialization (per-step `updated_at` would
        // be ideal but isn't always tracked uniformly).
        type JobSig = (
            Option<String>,    // job status as text
            Option<String>,    // priority as text
            Option<String>,    // closed_on as ISO string
            Vec<(boss_core::job::StepId, String, Option<String>)>,
        );
        fn signature(
            job: &boss_core::job::Job,
            steps: &[boss_core::job::Step],
        ) -> JobSig {
            let mut step_sig: Vec<_> = steps
                .iter()
                .map(|s| {
                    (
                        s.id,
                        format!("{:?}", s.status),
                        s.completed_on.map(|d| d.to_string()),
                    )
                })
                .collect();
            // StepId wraps a Uuid; sort by its display form so the
            // signature is stable across runs without StepId itself
            // needing an Ord impl.
            step_sig.sort_by_key(|s| s.0.to_string());
            (
                Some(format!("{:?}", job.status)),
                Some(format!("{:?}", job.priority)),
                job.closed_on.map(|d| d.to_string()),
                step_sig,
            )
        }

        // Push initial snapshot. last_sig is bound from the
        // success branch so the compiler doesn't warn about a
        // dead-write on a `None` initializer that's never read.
        let initial = state.jobs.get_job(&job_id).await;
        let mut last_sig: JobSig = if let Ok(Some(job)) = initial {
            let steps = state.jobs.list_steps(&job_id).await.unwrap_or_default();
            let sig = signature(&job, &steps);
            let detail = JobDetail { job, steps };
            if let Ok(json) = serde_json::to_string(&detail) {
                yield Ok::<_, Infallible>(SseEvent::default().data(json));
            }
            sig
        } else {
            yield Ok::<_, Infallible>(
                SseEvent::default().event("error").data("job not found"),
            );
            return;
        };

        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(
            tokio::time::MissedTickBehavior::Delay,
        );
        loop {
            tick.tick().await;
            let Ok(Some(job)) = state.jobs.get_job(&job_id).await else {
                // Job vanished mid-stream (cancelled, deleted).
                yield Ok::<_, Infallible>(
                    SseEvent::default().event("gone").data(""),
                );
                break;
            };
            let steps = state.jobs.list_steps(&job_id).await.unwrap_or_default();
            let sig = signature(&job, &steps);
            if last_sig != sig {
                let detail = JobDetail { job, steps };
                if let Ok(json) = serde_json::to_string(&detail) {
                    yield Ok::<_, Infallible>(SseEvent::default().data(json));
                }
                last_sig = sig;
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(super) async fn update_job<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(mut job): Json<Job>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };

    // Ensure path ID matches body ID.
    job.id = job_id;

    let existing = match state.jobs.get_job(&job_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return (StatusCode::NOT_FOUND, "job not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let old_status = existing.status;

    // `simulated` is IMMUTABLE after admission. Ignore-not-reject,
    // matching how the other server-owned field on this route is
    // treated (the path-authoritative `id` above): the stored value
    // wins over anything on the wire, so a client round-tripping a
    // full Job body never has to strip the field, and a client trying
    // to flip it simply doesn't. The Pg adapter's UPDATE never
    // touches the column; carrying the stored value forward here
    // keeps the JOB_UPDATED event payload (and the in-memory
    // adapter) agreeing with the row.
    job.simulated = existing.simulated;

    // Pick the right policy action: transitioning to Closed is a Close
    // action (more restricted than Update); everything else is Update.
    let action = if job.status == JobStatus::Closed && old_status != JobStatus::Closed {
        Action::Close
    } else {
        Action::Update
    };

    // Policy check: role allowed to perform this action on Jobs at all?
    let decision = match state.policy.check(&user, action, Resource::job()).await {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    let scope = match decision {
        Decision::Deny { reason } => {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
        Decision::Allow { scope } => scope,
    };

    // Scope check: is THIS specific Job inside the caller's scope?
    if !scope_matches(&user, &scope, &existing) {
        return (StatusCode::FORBIDDEN, "job is outside your scope").into_response();
    }

    // Same opt-in subject validation as create_job. Catches a body that
    // swaps Subject::System(…) for Subject::Custom { custom_kind:
    // "made-up" } on update.
    if let Err(resp) = validate_custom_subject(&state, &job.subject).await {
        return resp;
    }

    // OUTBOX (phase 2): the state event (full row state, what the
    // rebuild consumes) + the status-transition markers (topic-only
    // duplicates for downstream consumers; rebuild ignores them)
    // record in the SAME transaction as the row.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = state
        .publisher
        .stamp_with_actor(actor)
        .await
        .with_simulated(job.simulated);
    let mut job_events = vec![stamp.event(
        events::JOB_UPDATED,
        serde_json::to_value(&job).unwrap_or_default(),
    )];
    if old_status != job.status {
        job_events.push(stamp.event(
            events::JOB_STATUS_CHANGED,
            serde_json::json!({
                "id": job.id.to_string(),
                "old_status": old_status,
                "new_status": job.status,
            }),
        ));
        if job.status == JobStatus::Closed {
            // Same four keys as the two step-driven close paths
            // (`close_job_on_terminal` and the all-steps-terminal
            // catch-all, both in http/steps.rs). The close marker is
            // ONE contract with three emit sites, and a rule's `when`
            // binds identifiers off whichever one fired: an absent key
            // is a PredicateFailed → Retry → dead-letter, not a quiet
            // false. This site used to carry only `id` / `closed_on`,
            // so a Job closed by a direct status PUT dead-lettered the
            // `parent_step_id != null` subjob rule instead of skipping
            // it.
            job_events.push(stamp.event(
                events::JOB_CLOSED,
                serde_json::json!({
                    "id": job.id.to_string(),
                    "closed_on": job.closed_on,
                    "kind": job.kind,
                    "outcome": job.metadata.get("outcome"),
                    // Same contract as `kind` above: always present, so
                    // a rule that spawns off a close can name the new
                    // packet after the one that caused it.
                    "title": job.title,
                    // Third of the three close sites. Same contract:
                    // the subject is what a spawning rule dedupes on.
                    "subject_id": boss_core::primitives::Subject::id(&job.subject),
                    "parent_step_id": job.metadata.get("parent_step_id"),
                }),
            ));
        }
    }
    if let Err(e) = state
        .jobs
        .update_job_at(&job, stamp.timestamp, &job_events)
        .await
    {
        return persist_error_response(e);
    }

    // A Job update can flip a metadata-gated `ready_when` (the v3
    // ship-a-change Merged outcome waits on `job.metadata.merged` —
    // the conductor writes that marker through THIS endpoint at merge
    // time, aa9980c8). Wake any Pending step whose predicate now
    // holds; closed/cancelled Jobs stay untouched.
    if job.status == JobStatus::Open {
        let actor = user
            .ambient_actor()
            .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
        super::steps::reevaluate_and_persist(&state, &job, &actor).await;
    }

    StatusCode::NO_CONTENT.into_response()
}

/// `PATCH /api/jobs/{id}/metadata` — merge top-level metadata keys
/// into the Job, atomically, server-side.
///
/// Body: a JSON object of top-level metadata keys. A `null` value
/// REMOVES the key — the conductor's `overlay_metadata` convention —
/// and every other value replaces that key wholesale. Status, steps,
/// and every other envelope field are untouchable through this route.
///
/// WHY IT EXISTS: with only the full-replacement PUT, every caller
/// that wanted to set one metadata key ran GET → spread → PUT client-
/// side. The 2026-08-21 UX audit caught what that costs: the board
/// closes the packet (status closed + `metadata.outcome` stamped)
/// between a dismisser's GET and PUT, and the dismiss resurrects it
/// open with the outcome erased — on the system of record. The merge
/// now happens inside one adapter transaction against the row as it
/// stands, so the envelope a concurrent writer produced survives.
///
/// Policy: the same `Action::Update` gate + scope check as the job
/// PUT. A metadata patch cannot transition status, so the PUT's
/// stricter `Close` action never applies here.
pub(super) async fn patch_job_metadata<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(patch): Json<serde_json::Value>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    let serde_json::Value::Object(patch) = patch else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "metadata patch must be a JSON object of top-level keys",
        )
            .into_response();
    };

    let existing = match state.jobs.get_job(&job_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return (StatusCode::NOT_FOUND, "job not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Policy gate mirrors update_job's Update arm exactly.
    let decision = match state
        .policy
        .check(&user, Action::Update, Resource::job())
        .await
    {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    let scope = match decision {
        Decision::Deny { reason } => {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
        Decision::Allow { scope } => scope,
    };
    if !scope_matches(&user, &scope, &existing) {
        return (StatusCode::FORBIDDEN, "job is outside your scope").into_response();
    }

    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    // Same enrichment as the PUT path: the packet's admission-fixed
    // flag — not the transport context — marks the event. The adapter
    // builds JOB_UPDATED from the post-merge row with this stamp and
    // records it in the same transaction as the write.
    let stamp = state
        .publisher
        .stamp_with_actor(actor.clone())
        .await
        .with_simulated(existing.simulated);
    let merged = match state
        .jobs
        .merge_job_metadata_at(&job_id, &patch, &stamp)
        .await
    {
        Ok(job) => job,
        Err(crate::port::JobsError::NotFound(_)) => {
            return (StatusCode::NOT_FOUND, "job not found").into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Same wake as the PUT: a metadata write can flip a metadata-gated
    // `ready_when` (ship-a-change's Merged outcome waits on
    // `job.metadata.merged`). Closed/cancelled Jobs stay untouched.
    if merged.status == JobStatus::Open {
        super::steps::reevaluate_and_persist(&state, &merged, &actor).await;
    }

    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/jobs/{id}/convert` — pull a packet forward to a newer
/// protocol version, if where it stands allows it.
///
/// THE DOOR IS NARROW ON PURPOSE. `workflow_version` is excluded from
/// `update_job`'s SET list, so no ordinary PUT can re-pin a packet by
/// accident; conversion is an explicit act that must first pass
/// [`crate::protocol_conversion::convertibility_for_packet`]. A refusal
/// returns the obstacles rather than a bare no, because each one names
/// the step it concerns and an operator's next question is always
/// "which step, and what changed".
pub(super) async fn convert_job<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(job_id) = parse_job_id(&id) else {
        return (StatusCode::BAD_REQUEST, "invalid job id").into_response();
    };
    let existing = match state.jobs.get_job(&job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => return (StatusCode::NOT_FOUND, "job not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Same gate as any other job write.
    let decision = match state
        .policy
        .check(&user, Action::Update, Resource::job())
        .await
    {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    let scope = match decision {
        Decision::Deny { reason } => return (StatusCode::FORBIDDEN, reason).into_response(),
        Decision::Allow { scope } => scope,
    };
    if !scope_matches(&user, &scope, &existing) {
        return (StatusCode::FORBIDDEN, "job is outside your scope").into_response();
    }

    let Some(ref reg) = state.kind_registry else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no workflow registry: conversion cannot be judged without both specs",
        )
            .into_response();
    };
    let from = match reg
        .get_version(&existing.kind, existing.workflow_version)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::CONFLICT,
                format!(
                    "cannot read the version this packet is pinned to ({} v{}): {e}",
                    existing.kind, existing.workflow_version
                ),
            )
                .into_response();
        }
    };
    let want = body.get("to_version").and_then(serde_json::Value::as_i64);
    let to = match want {
        Some(v) => reg.get_version(&existing.kind, v as i32).await,
        None => reg.get_active(&existing.kind).await,
    };
    let to = match to {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::CONFLICT, format!("no such target version: {e}")).into_response();
        }
    };
    if to.version == existing.workflow_version {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "converted": false,
                "reason": "already pinned to that version",
                "workflow_version": existing.workflow_version,
            })),
        )
            .into_response();
    }

    // Where the packet actually stands: the slugs it has completed.
    let steps = match state.jobs.list_steps(&job_id).await {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let done: std::collections::BTreeSet<String> = steps
        .iter()
        .filter(|s| s.status == boss_core::job::StepStatus::Completed)
        .filter_map(|s| s.spec_slug.clone())
        .collect();

    let verdict = crate::protocol_conversion::convertibility_for_packet(&from, &to, &done);
    if !verdict.is_automatic() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "converted": false,
                "from": from.version,
                "to": to.version,
                "obstacles": verdict.obstacles().iter().map(|o| serde_json::json!({
                    "step": o.step, "reason": o.reason,
                })).collect::<Vec<_>>(),
            })),
        )
            .into_response();
    }

    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = state
        .publisher
        .stamp_with_actor(actor)
        .await
        .with_simulated(existing.simulated);
    match state
        .jobs
        .repin_workflow_version_at(&job_id, to.version, &stamp)
        .await
    {
        Ok(job) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "converted": true,
                "from": from.version,
                "to": job.workflow_version,
            })),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /api/estate/nodes` — the machines BOSS declares it runs on.
///
/// The estate tables have existed since `144-estate-subjects.sql` and
/// nothing has ever served them, so "what hardware is running" was
/// unanswerable from inside BOSS. Every answer had to be re-derived by
/// shelling into machines, and on 2026-08-30 three separate accounts of
/// the estate — the registry, a prose inventory, and an operator's
/// recollection — were wrong in the same direction because none was
/// connected to the machines (59ef456a).
///
/// Read-only on purpose: declaring a machine is a schema migration that
/// converges, not an API write.
pub(super) async fn list_estate_nodes<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(_user): CurrentUser,
) -> Response {
    match state.jobs.list_estate_nodes().await {
        Ok(nodes) => Json(serde_json::json!({ "data": nodes })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, serde::Deserialize, Default)]
pub(super) struct EstateEventsQuery {
    limit: Option<i64>,
}

/// `GET /api/estate/observations` and `/api/estate/comparisons` — the
/// read half of the estate loop's event series (d471a8ce).
///
/// The observe and compare doors above record these as events, and
/// until this pair existed the series was readable only through an
/// in-pod port-forward to the events service: the loop went live on
/// 2026-08-30 with its first observation and comparison recorded, and
/// the two cars that built it had proven arbiters that were SATISFIED
/// yet unprobeable from any exposed surface. These are also the IT
/// page's data source — David has asked repeatedly for the running
/// hardware rendered from the registry, declared beside observed.
///
/// Guest-readable like `/api/estate/nodes`, and rows verbatim as
/// recorded: a reader that reshapes its instrument is a second
/// instrument. Small default, hard cap — this is a status surface,
/// not an export (the events service's export door owns bulk).
pub(super) async fn list_estate_observations<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(_user): CurrentUser,
    Query(q): Query<EstateEventsQuery>,
) -> Response {
    estate_events(&state, crate::events::ESTATE_OBSERVED, q.limit).await
}

pub(super) async fn list_estate_comparisons<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(_user): CurrentUser,
    Query(q): Query<EstateEventsQuery>,
) -> Response {
    estate_events(&state, crate::events::ESTATE_COMPARED, q.limit).await
}

async fn estate_events<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    kind: &str,
    limit: Option<i64>,
) -> Response {
    let limit = limit.unwrap_or(5).clamp(1, 50);
    match state.jobs.recent_events_by_kind(kind, limit).await {
        Ok(rows) => Json(serde_json::json!({ "data": rows })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::edge_guidance;

    /// The guard's own text is the valuable part and must survive
    /// verbatim — an author reads WHICH edge and WHICH id was refused
    /// off this string.
    #[test]
    fn edge_guidance_keeps_the_guards_message_first() {
        let raw = "job edge ship-a-change.backlog_item references unresolvable Job 2c4ae549";
        let out = edge_guidance(raw.to_string());
        assert!(
            out.starts_with(raw),
            "the guard's own text must lead: {out}"
        );
    }

    /// …and a refused `backlog_item` names the free-text escape
    /// hatch. Without it the observed move was to drop the reference
    /// entirely, which is how packets ended up unlinked.
    #[test]
    fn a_refused_backlog_item_names_backlog_text_as_the_alternative() {
        let out = edge_guidance(
            "job edge ship-a-change.backlog_item references unresolvable Job 2c4ae549".to_string(),
        );
        assert!(
            out.contains("backlog_text"),
            "an author who hit the guard needs the alternative named: {out}"
        );
    }

    /// Other edges get no feedback-specific advice bolted on.
    #[test]
    fn an_unrelated_edge_failure_is_passed_through_untouched() {
        let raw = "job edge pr-train.boarded_jobs references unresolvable Job abc";
        assert_eq!(edge_guidance(raw.to_string()), raw);
    }
}
