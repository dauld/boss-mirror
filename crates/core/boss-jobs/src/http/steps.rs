//! Step handlers — list, add, update (the readiness/sign-off/dispatch
//! engine), and sign-off stamping.

use super::*;

use axum::extract::Path;

/// The wire spelling of a step status, for messages the caller reads.
/// Local rather than borrowed from the postgres adapter: an HTTP error
/// string has no business depending on the storage layer, and the two
/// are free to diverge without either noticing.
fn status_word(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "pending",
        StepStatus::Ready => "ready",
        StepStatus::Active => "active",
        StepStatus::Completed => "completed",
        StepStatus::Skipped => "skipped",
    }
}

pub(super) async fn list_steps<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };

    match state.jobs.list_steps(&job_id).await {
        Ok(steps) => Json(steps).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn add_step<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(mut step): Json<Step>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };

    // A JOB'S STEP SET IS FIXED AT ADMISSION, because a step is
    // PROTOCOL — not because appending one breaks the engine. It used
    // to do both. Readiness was recomputed by pairing spec steps with
    // job steps positionally, so one appended step misaligned every
    // pair after it and `registry::reevaluate` refused to advance
    // anything; the job froze with its terminal pending and never left
    // its owner's queue. Design review 32a4e70d did exactly that on
    // 2026-08-13, producing feedback 55c92985: "I finished the top
    // design review and it still shows the same metadata and is in the
    // same queue." The divergence was logged at warn the whole time,
    // and a warn in a log nobody reads is not a signal.
    //
    // Steps now pair by `spec_slug` (`registry::pair_steps`), so an
    // extra row is ignored and the job keeps moving. The freezing
    // consequence is gone; the refusal below is not, and the paragraph
    // after this one is now its whole justification.
    //
    // Refusing here is the honest boundary: a new step is a change to
    // the WORKFLOW, and the registry is where that belongs — publish a
    // new version and admit new packets under it. In-flight packets
    // stay pinned to the version they were admitted under, which is the
    // whole point of the versioning.
    //
    // The route stays for the case it is safe in: a job whose kind has
    // no spec to diverge from.
    if let Some(reg) = &state.kind_registry
        && let Ok(Some(job)) = state.jobs.get_job(&job_id).await
        && let Ok(spec) = reg.get_version(&job.kind, job.workflow_version).await
        && let Ok(existing) = state.jobs.list_steps(&job_id).await
        && existing.len() >= spec.steps.len()
    {
        return (
            StatusCode::CONFLICT,
            format!(
                "refusing to add a step to job {job_id}: it already has {} step(s), \
                 matching workflow {} v{}. A step is protocol, and a job's step set \
                 is fixed at admission — a step appended here would be a protocol \
                 edit on exactly one job, described by no workflow version. Add the \
                 step to the workflow and publish a new version (/system/workflows) \
                 instead; in-flight jobs stay pinned to the version they were \
                 admitted under.",
                existing.len(),
                job.kind,
                job.workflow_version
            ),
        )
            .into_response();
    }

    // Ensure the step belongs to this job.
    step.job_id = job_id;

    // Schema validation runs only when the step is being marked done —
    // required fields represent what must be true for the work to count
    // as complete, not what must be true for it to exist. A brand-new
    // scheduling step can have no `scheduled_at`; it gets filled in by
    // the person doing the work.
    if step.status == StepStatus::Completed
        && let Err(errors) = state
            .step_registry
            .validate_metadata(&step.kind, &step.metadata)
            .and_then(|()| {
                // Inline authoring: the completion contract is
                // the union of the kind bundle's fields and the step's
                // own authored fields.
                crate::step_registry::StepRegistry::validate_authored_fields(
                    &step.fields,
                    &step.metadata,
                )
            })
    {
        let msg = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::BAD_REQUEST,
            format!("invalid step metadata: {msg}"),
        )
            .into_response();
    }

    // OUTBOX (phase 2): STEP_CREATED (full row state, what the
    // rebuild consumes) records in the SAME transaction as the row.
    // The actor is stamped from the authenticated session per the
    // Level-B actor-stamping invariant. Sim / runner back-channel
    // paths (role=system-sim|system, or an `automation:`/`rule:` id)
    // include an `assignee_id` on the Step body that names the real
    // Employee taking on the work; we honor that as the audit actor
    // so step.created rows attribute to a person, not a process.
    // Otherwise the actor is the session's own identity — a human
    // operator, a named automation (`automation:<authority>`), or an
    // agent session (`<mode>:<model>`); never anonymous.
    //
    // Agents deliberately do NOT belong in `is_automation`. This flag
    // means "the caller is a proxy standing in for a person, so honor
    // the person it names" — the sim's whole purpose. An agent is not
    // a proxy: it IS the CPU that did the work, and redirecting its
    // attribution to `assignee_id` would erase exactly the agent
    // attribution the `<mode>:<model>` actor id exists to record.
    let is_automation = user.id == "anonymous"
        || user.id.starts_with("automation:")
        || user.id.starts_with("rule:")
        || user.id.ends_with("-sim")
        || user.id.ends_with("-runner")
        || user.role == "system-sim"
        || user.role == "system";
    let actor = match (is_automation, step.assignee_id.as_deref()) {
        (true, Some(emp_id)) if !emp_id.is_empty() => {
            boss_core::actor::ActorId::Human(emp_id.to_string())
        }
        _ => user
            .ambient_actor()
            .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into())),
    };
    let mut stamp = state.publisher.stamp_with_actor(actor).await;
    // Step events inherit the parent packet's admission-fixed
    // `simulated` flag (the packet, not the request's transport
    // context, is the source of truth). A step posted against a
    // missing Job keeps the chain default — Pg rejects it on the FK
    // anyway.
    if let Ok(Some(job)) = state.jobs.get_job(&job_id).await {
        stamp = stamp.with_simulated(job.simulated);
    }
    let step_event = stamp.event(events::STEP_CREATED, events::step_state_payload(&step));
    if let Err(e) = state
        .jobs
        .add_step_at(&step, stamp.timestamp, &[step_event])
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": step.id.to_string() })),
    )
        .into_response()
}

/// Dispatch path for the `workflow-publish` StepType — the
/// terminal step of every `workflow-design` Job. Reads
/// `workflow_spec` out of the step metadata, validates it, and
/// calls `WorkflowRegistry::publish_authored`. Returns the
/// published spec on success or a (status, message) pair the
/// caller can short-circuit with.
///
/// The stamp's `actor` + `now` ride into `publish_authored` so the
/// registry adapter records `jobs.kind.published` atomically with
/// the workflows row — the step path no longer emits its own copy.
///
/// Viability is NOT re-linted here: `publish_authored` runs
/// `workflow_lint::gate_active` itself, because it is one of the
/// paths that can set a row ACTIVE and every such path must refuse
/// on its own (a pre-check in one caller protects only that caller).
/// A refusal arrives as `WorkflowError::Unviable` and leaves as 422
/// with the problem list, matching `POST /api/workflows/{kind}/publish`.
async fn dispatch_workflow_publish(
    registry: &dyn crate::registry::WorkflowRegistry,
    step: &boss_core::job::Step,
    job_id: boss_core::job::JobId,
    actor: &boss_core::actor::ActorId,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<crate::registry::WorkflowSpec, (StatusCode, String)> {
    let spec_value = step.metadata.get("workflow_spec").ok_or((
        StatusCode::BAD_REQUEST,
        "workflow-publish step missing required metadata field `workflow_spec`".to_string(),
    ))?;

    let spec: crate::registry::WorkflowSpec =
        serde_json::from_value(spec_value.clone()).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("`workflow_spec` did not deserialize as WorkflowSpec: {e}"),
            )
        })?;

    registry
        .publish_authored(spec, job_id, actor, now)
        .await
        .map_err(|e| match e {
            crate::registry::WorkflowError::Unviable(problems) => {
                let mut msg = String::from("workflow-publish: spec is not viable:");
                for p in &problems {
                    msg.push_str(&format!("\n  {p}"));
                }
                (StatusCode::UNPROCESSABLE_ENTITY, msg)
            }
            other => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("publish_authored failed: {other}"),
            ),
        })
}

pub(super) async fn update_step<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path((id, step_id_str)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    let step_id = match parse_step_id(&step_id_str) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid step id").into_response(),
    };

    // Audit-write authorization. Every step PUT emits at least a
    // STEP_UPDATED row below, so the write is gated here on a coarse
    // (Update, step) decision — the caller's role must be permitted to
    // update steps at all. The sign-off transition adds the role-scoped
    // `step-signoff:<role>` authority on top (see further down).
    // Simulator traffic is allowed by the SimBypassPolicyClient (trusted
    // box; the write is still stamped `_simulated`), so this gate never
    // stalls a regen.
    match state
        .policy
        .check(&user, Action::Update, Resource::step())
        .await
    {
        Ok(Decision::Deny { reason }) => {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
        Ok(_) => {}
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    }

    // PATCH semantics: fetch the current step, then overlay the caller's
    // body on top. Any field the caller omits keeps its current value,
    // so clients can send `{"status": "done"}` without having to round-
    // trip the whole Step. Full replacements still work — a body that
    // includes every field just overwrites everything.
    let old = match state.jobs.get_step(&step_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, "step not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // The parent packet, fetched ONCE: the event stamp inherits its
    // `simulated` flag, the step.done / step.assigned markers read
    // its Subject identity, and the re-evaluator runs against it.
    // The step write below never touches the jobs row, so this read
    // stays current through all of those. (The auto-close pass at
    // the bottom re-fetches — close_job_on_terminal may have closed
    // the Job in between.)
    let parent_job = state.jobs.get_job(&job_id).await.ok().flatten();

    let mut merged = match serde_json::to_value(&old) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize old step: {e}"),
            )
                .into_response();
        }
    };
    let merged_obj = match merged.as_object_mut() {
        Some(obj) => obj,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "old step did not serialize to an object",
            )
                .into_response();
        }
    };
    let body_obj = match body.as_object() {
        Some(obj) => obj,
        None => return (StatusCode::BAD_REQUEST, "body must be a JSON object").into_response(),
    };
    for (k, v) in body_obj {
        merged_obj.insert(k.clone(), v.clone());
    }

    let mut step: Step = match serde_json::from_value(merged) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid step fields: {e}")).into_response();
        }
    };
    // Path params are authoritative — reject body-driven ID swaps.
    step.job_id = job_id;
    step.id = step_id;

    // Stamps are server-minted (POST .../sign-offs) and requirements
    // are materialization data — a PUT body controls neither.
    step.sign_offs = old.sign_offs.clone();
    step.sign_offs_required = old.sign_offs_required.clone();

    // `authority_role` is immutable across PUTs. Carry the persisted
    // value forward so a body can neither raise nor lower the required
    // sign-off authority — the sign-off gate above reads `old.metadata`
    // for its decision, and this keeps the stored row consistent with
    // that decision (a caller can't change it in a prior PUT either).
    if let Some(old_obj) = old.metadata.as_object()
        && let Some(auth) = old_obj.get("authority_role").cloned()
        && let Some(obj) = step.metadata.as_object_mut()
    {
        obj.insert("authority_role".into(), auth);
    }

    // A TERMINAL ROW IS FROZEN, AND SAYING SO IS THE POINT.
    //
    // Both adapters refuse to write `status`, `completed_on` and
    // `metadata` on a completed or skipped step — deliberately, so a
    // write merged against a stale pre-completion fetch cannot demote a
    // finished step. What they did NOT do was tell anyone: the row was
    // left untouched and the handler still answered 204, so a caller
    // could not distinguish a write that landed from one that vanished.
    //
    // That cost real work. Three cars' stale gate receipts were
    // "repaired", the API said 204 three times, nothing was written,
    // and the cars were reported fixed while staying unboardable
    // (09576fab). It bit again on 2026-08-27: an accepted correction
    // could not be applied to the sentence it corrected, because the
    // sentence lives in a completed step's metadata.
    //
    // SCOPED TO A REAL CHANGE, not to every write. The freeze exists to
    // make racing writers harmless — dispatcher assign retries and
    // JetStream redeliveries re-PUT content that is already stored, and
    // those are no-ops that must keep succeeding. Refusing them would
    // trade a silent bug for a noisy one. So the refusal fires only
    // when the write would actually alter a frozen field.
    //
    // Nothing on a terminal step is legitimately mutable through here:
    // the conductor's `boarded_head` stamp, the one path that visibly
    // works against finished cars, writes JOB metadata via
    // `merge_job_metadata`, never step metadata.
    //
    // `status` IS DELIBERATELY NOT CHECKED HERE. The demotion case
    // already has its own refusal further down, added for this same
    // defect (job 903e6b90), and it says something better than this
    // could: which status the step is, and which one the caller tried
    // to set. Repeating the check here would preempt that message with
    // a vaguer one. This block covers only the two fields that were
    // still being dropped in silence.
    if matches!(old.status, StepStatus::Completed | StepStatus::Skipped) {
        let mut frozen: Vec<&str> = Vec::new();
        if step.completed_on != old.completed_on {
            frozen.push("completed_on");
        }
        if step.metadata != old.metadata {
            frozen.push("metadata");
        }
        if !frozen.is_empty() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "step is terminal — these fields are immutable",
                    "step_id": step_id.to_string(),
                    "step_status": status_word(old.status),
                    "refused_fields": frozen,
                    "hint": "a completed step is a record of what happened. To correct or \
                             annotate it, write to the parent job's metadata \
                             (PATCH /api/jobs/{id}/metadata) instead.",
                })),
            )
                .into_response();
        }
    }

    // Auto-stamp completed_on on the done-transition if the caller
    // didn't send one. The simulator's LiveApiOutput sends the
    // sim-day explicitly; SPA-driven step completion ("Mark done"
    // button) doesn't, and falling through with NULL leaves the
    // step undated → dispatcher rule handlers stamp wall-clock
    // NOW() on every downstream row. Wall-clock is the right
    // default *here* because the operator pressing the button
    // really is acting in real time, but we let an explicit body
    // value win.
    let is_flipping_to_done =
        old.status != StepStatus::Completed && step.status == StepStatus::Completed;
    if is_flipping_to_done && step.completed_on.is_none() {
        step.completed_on = Some(boss_clock_client::now_from(&state.clock).await.date_naive());
    }

    // Sign-off contract: a step completes only when every required role has
    // stamped its *current* shape. Stale stamps (edits after stamping)
    // don't count.
    if is_flipping_to_done && !step.sign_offs_satisfied() {
        let current = boss_core::job::step_shape_hash(&step.title, &step.metadata);
        let missing: Vec<&str> = step
            .sign_offs_required
            .iter()
            .filter(|role| {
                !step
                    .sign_offs
                    .iter()
                    .any(|st| &&st.role == role && st.shape_hash == current)
            })
            .map(|r| r.as_str())
            .collect();
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "sign-offs incomplete",
                "missing_or_stale_roles": missing,
            })),
        )
            .into_response();
    }

    // Validate metadata only when the step is done (see add_step for
    // rationale). In-progress updates can still carry thin metadata.
    if step.status == StepStatus::Completed
        && let Err(errors) = state
            .step_registry
            .validate_metadata(&step.kind, &step.metadata)
            .and_then(|()| {
                // Inline authoring: the completion contract is
                // the union of the kind bundle's fields and the step's
                // own authored fields.
                crate::step_registry::StepRegistry::validate_authored_fields(
                    &step.fields,
                    &step.metadata,
                )
            })
    {
        let msg = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::BAD_REQUEST,
            format!("invalid step metadata: {msg}"),
        )
            .into_response();
    }

    // Blocker gate (invariant I-4 — preconditions enforced). When the
    // caller is flipping this step to `done`, every step in
    // `blocked_by` must already be in a terminal state. Otherwise the
    // machine is firing a transition whose upstream data dependencies
    // aren't satisfied. Moving a step to `active` or any other
    // non-terminal state is still fine even with open blockers (a tech
    // may start prep work before a sign-off lands); the gate only fires
    // at `done`.
    //
    // The terminal set is `Completed | Skipped`. A Skipped blocker
    // means that branch was provably not-taken (its ready_when is
    // false-forever) — for an OR-predicate dependent (`steps.a.done OR
    // steps.b.done`) where `a` completed and `b` skipped, the dependent
    // is legitimately Ready and must be completable, so a Skipped
    // blocker clears the gate. Only Pending/Ready/Active (work
    // genuinely still outstanding) or a missing blocker hold it. The
    // re-evaluator is the readiness authority and won't promote a
    // dependent whose predicate is unsatisfiable; a Skipped upstream is
    // a resolved branch, not a broken hand-off.
    let is_flipping_to_done =
        old.status != StepStatus::Completed && step.status == StepStatus::Completed;
    if is_flipping_to_done && !step.blocked_by.is_empty() {
        match state.jobs.resolve_blockers(&step.blocked_by).await {
            Ok(statuses) => {
                // Missing blockers (returned-length < asked-length) are
                // treated as unresolved — a step we can't find is
                // definitely not terminal.
                let resolved_by_id: std::collections::HashMap<_, _> =
                    statuses.into_iter().collect();
                let unresolved: Vec<String> = step
                    .blocked_by
                    .iter()
                    .filter_map(|id| match resolved_by_id.get(id) {
                        Some(StepStatus::Completed | StepStatus::Skipped) => None,
                        Some(s) => Some(format!("{id}={s:?}").to_lowercase()),
                        None => Some(format!("{id}=missing")),
                    })
                    .collect();
                if !unresolved.is_empty() {
                    return (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "error": "step has unresolved blockers",
                            "step_id": step.id.to_string(),
                            "unresolved_blockers": unresolved,
                        })),
                    )
                        .into_response();
                }
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("blocker check failed: {e}"),
                )
                    .into_response();
            }
        }
    }

    // Loud invalidation: an edit that changes the step's
    // completion-relevant shape makes existing stamps stale — they
    // attested different content. Stamps stay recorded (provenance);
    // the event tells the surface who must re-sign. Stamping itself
    // moved to POST .../sign-offs.
    let stamps_invalidated = !old.sign_offs.is_empty()
        && boss_core::job::step_shape_hash(&old.title, &old.metadata)
            != boss_core::job::step_shape_hash(&step.title, &step.metadata);

    // Calendar reservation hook — runs BEFORE the persistence
    // write so a hard-conflict 409 doesn't leave the step in the
    // new in-progress state without a reservation. The hook is a
    // no-op when calendar isn't configured or the step lacks the
    // scheduling metadata.
    match crate::calendar_hook::apply_step_transition(
        state.calendar.as_ref(),
        &old,
        &step,
        &user.id,
    )
    .await
    {
        Ok(crate::calendar_hook::HookOutcome::Conflict { existing_rows }) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "calendar conflict",
                    "step_id": step.id.to_string(),
                    "existing": existing_rows,
                })),
            )
                .into_response();
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "calendar hook errored; proceeding with step update");
        }
    }

    let now = boss_clock_client::now_from(&state.clock).await;

    // OUTBOX (phase 2): the state event + every marker this
    // transition produces record in the SAME transaction as the
    // step row. Actor stamping per the Level-B invariant: the actor
    // is the session user who PUT the step — typically a human (the
    // assignee or a manager signing off). Sim / dispatcher
    // back-channel paths set x-boss-user to a synthetic slug
    // (`brewery-sim`, `rule:<name>`) AND include a `completed_by`
    // field in the body that names the real Employee whose work the
    // step represents; we honor that override when the calling
    // identity is an automation slug so the audit_log row attributes
    // work to a person, not a process. Agent sessions
    // (`<mode>:<model>`) are excluded from that override on purpose —
    // see the note on the same flag in `create_step`: an agent is the
    // CPU, not a stand-in for one. Computed BEFORE the
    // workflow-publish dispatch below — the registry write records
    // its event under this same actor + now.
    let body_completed_by = body
        .get("completed_by")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let is_automation = user.id == "anonymous"
        || user.id.starts_with("automation:")
        || user.id.starts_with("rule:")
        || user.id.ends_with("-sim")
        || user.id.ends_with("-runner")
        || user.role == "system-sim"
        || user.role == "system";
    let actor = match (is_automation, body_completed_by.as_deref()) {
        (true, Some(emp_id)) => boss_core::actor::ActorId::Human(emp_id.to_string()),
        _ => user
            .ambient_actor()
            .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into())),
    };

    // In-process dispatch for the `workflow-publish` StepType. When a
    // step of this kind flips to Done, read `workflow_spec` from
    // metadata and call `WorkflowRegistry::publish_authored` so the
    // meta-Job's authoring closes by writing a real registry row.
    // `publish_authored` runs the viability gate itself — an unviable
    // spec comes back as 422 and the step does not flip.
    //
    // Registry-write-first: if publish_authored fails, `update_step_at`
    // is never called and no STEP_UPDATED accumulates in audit_log for
    // a step whose side effect couldn't fire — keeping audit_log
    // integrity on partial failure.
    if is_flipping_to_done && step.kind == "workflow-publish" {
        let Some(reg) = &state.kind_registry else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Workflow registry unavailable for workflow-publish dispatch",
            )
                .into_response();
        };
        if let Err((status, msg)) =
            dispatch_workflow_publish(reg.as_ref(), &step, job_id, &actor, now).await
        {
            return (status, msg).into_response();
        }
    }

    let mut stamp = state.publisher.stamp_with_actor(actor.clone()).await;
    // Step events inherit the packet's admission-fixed flag — a real
    // operator completing a step on a simulated Job records a
    // simulated event, and a sim-chain write to a real Job stays
    // real.
    if let Some(j) = &parent_job {
        stamp = stamp.with_simulated(j.simulated);
    }
    let stamp = stamp;
    let mut step_events =
        vec![stamp.event(events::STEP_UPDATED, events::step_state_payload(&step))];

    // The `workflow-publish` dispatch's WORKFLOW_PUBLISHED event —
    // the full published spec `rebuild_workflows` reads to
    // reconstruct the registry — used to be pushed into
    // `step_events` here. It moved into the registry adapter
    // (`publish_authored` records it atomically with the workflows
    // ROW), so the step path no longer duplicates it. The rebuild
    // reads the same kind either way.

    // Marker events for downstream consumers — informational
    // duplicates of state already in STEP_UPDATED. Rebuild ignores.
    if old.status != StepStatus::Completed && step.status == StepStatus::Completed {
        step_events.push(stamp.event(
            events::STEP_COMPLETED,
            serde_json::json!({
                "job_id": job_id.to_string(),
                "step_id": step_id.to_string(),
            }),
        ));

        // Dispatcher routing: rules in infra/dispatcher/rules.toml
        // listen on `step.done.<kind>` so each StepType's side
        // effects can be declared as a rule without a giant `match`
        // in the subscriber. Payload mirrors the simulator's
        // in-process SimEventBus shape so handlers don't fork by
        // source — subject_kind / subject_id come from the parent
        // Job so every handler has the Subject identity without an
        // extra fetch. (Read before the write; the step update
        // doesn't touch the job row.)
        if !step.kind.is_empty() {
            let (subject_kind, subject_id) = if let Some(job) = &parent_job {
                (
                    boss_core::primitives::Subject::kind(&job.subject).to_string(),
                    boss_core::primitives::Subject::id(&job.subject).to_string(),
                )
            } else {
                (String::new(), String::new())
            };
            step_events.push(stamp.event(
                &format!("step.done.{}", step.kind),
                serde_json::json!({
                    "job_id": job_id.to_string(),
                    "step_id": step_id.to_string(),
                    "kind": step.kind,
                    "subject_kind": subject_kind,
                    "subject_id": subject_id,
                    "completed_on": step.completed_on,
                    "metadata": step.metadata,
                    // ALWAYS present, defaulting false: the dispatcher
                    // expr binder resolves flat identifiers only, and an
                    // absent identifier is a PredicateFailed → Retry →
                    // dead-letter storm, not a quiet false. The
                    // notify-on-step-done-marked rule (migration 106)
                    // matches this field.
                    "notify_on_done": step
                        .metadata
                        .get("notify_on_done")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                }),
            ));
        }
    }

    // Assignment is a routable fact. The ready-path notification fires
    // only at the READY transition, so a step assigned AFTER it was
    // already ready told nobody (backlog 534a8dc8). Emitted only when
    // the assignee actually changed — a metadata PATCH re-sending the
    // same assignee is not an assignment. Payload mirrors
    // `step.ready.<kind>` so `messages.notify` consumes both without
    // forking; the handler's deterministic message id dedupes when the
    // ready path already told the same person.
    if step.assignee_id.is_some() && old.assignee_id != step.assignee_id && !step.kind.is_empty() {
        let (subject_kind, subject_id) = if let Some(job) = &parent_job {
            (
                boss_core::primitives::Subject::kind(&job.subject).to_string(),
                boss_core::primitives::Subject::id(&job.subject).to_string(),
            )
        } else {
            (String::new(), String::new())
        };
        step_events.push(stamp.event(
            &format!("step.assigned.{}", step.kind),
            serde_json::json!({
                "job_id": job_id.to_string(),
                "step_id": step_id.to_string(),
                "kind": step.kind,
                "subject_kind": subject_kind,
                "subject_id": subject_id,
                "assignee_id": step.assignee_id,
                "metadata": step.metadata,
            }),
        ));
    }

    if stamps_invalidated {
        let stale_roles: Vec<String> = step.sign_offs.iter().map(|st| st.role.clone()).collect();
        step_events.push(stamp.event(
            events::STEP_STAMPS_INVALIDATED,
            serde_json::json!({
                "job_id": job_id.to_string(),
                "step_id": step_id.to_string(),
                "stale_roles": stale_roles,
                "required_roles": step.sign_offs_required,
            }),
        ));
    }

    // Refuse, out loud, what the row would silently drop.
    //
    // `update_step_at`'s UPDATE freezes status, completed_on and
    // metadata on a terminal step — deliberately, so a write computed
    // against a pre-completion fetch (dispatcher assign retries,
    // JetStream redeliveries, any racing read-modify-write) cannot
    // demote it. That invariant is right and stays.
    //
    // What was wrong is that the caller was never told. The handler
    // returned 204 and the columns simply did not move, so an actor
    // that believed it had recorded something had not. Job 903e6b90
    // found it by probe; the same silence ate a correction to a car's
    // build step earlier the same day, and nobody noticed until the
    // record was read back.
    //
    // Idempotent re-sends still pass: this compares VALUES, so a
    // redelivery that re-completes an already-completed step with the
    // same status is unchanged and proceeds. Only a real conflict —
    // a different status against a terminal row — is refused.
    if matches!(old.status, StepStatus::Completed | StepStatus::Skipped)
        && step.status != old.status
    {
        return (
            StatusCode::CONFLICT,
            format!(
                "step is {} and does not move backwards: refusing to set it to {}. \
                 Terminal steps are immutable; add a new step or record the change elsewhere.",
                status_word(old.status),
                status_word(step.status)
            ),
        )
            .into_response();
    }

    if let Err(e) = state
        .jobs
        .update_step_at(&step, stamp.timestamp, &step_events)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Re-evaluate readiness: the just-updated step's status change may
    // make a downstream step's `ready_when` predicate flip (Pending →
    // Ready) or rule a branch out (Pending → Skipped). The re-evaluator
    // is the single readiness engine, driven off the active
    // WorkflowSpec's predicates rather than denormalized edges.
    if let Some(reg) = &state.kind_registry
        && let Some(job) = parent_job
    {
        reevaluate_and_persist(&state, &job, &actor).await;

        // If the step we just completed is a declared terminal,
        // close the Job with that outcome and skip every
        // still-non-terminal step. Pair the live Step back to its
        // StepSpec by index (== sort_order, the materializer's
        // contract). Resolve the pinned version — same rule as the
        // re-evaluator.
        let just_completed =
            old.status != StepStatus::Completed && step.status == StepStatus::Completed;
        if just_completed
            && let Ok(spec) = reg.get_version(&job.kind, job.workflow_version).await
            && let Some(outcome) = spec
                .steps
                .get(step.sort_order as usize)
                .and_then(|spec_step| spec_step.terminal.as_ref())
                .map(|t| t.outcome.clone())
        {
            close_job_on_terminal(&state, &job_id, &outcome, &actor, now).await;
        }
    }

    // Auto-transition job status based on step states. Acts as the
    // catch-all close path: a Job whose every step reached a terminal
    // state (Completed / Skipped) closes here even when no *declared*
    // terminal step fired (the declared-terminal close above already
    // handled that case and left the Job Closed, so this no-ops then).
    if let Ok(steps) = state.jobs.list_steps(&job_id).await {
        let new_status = compute_job_status(&steps);
        if let Ok(Some(mut job)) = state.jobs.get_job(&job_id).await
            && job.status != new_status
            && job.status != JobStatus::Cancelled
            && job.status != JobStatus::Draft
            // A Closed Job is terminal — never reopen / re-transition it
            // (the declared-terminal close above may have already closed
            // it, force-skipping a still-pending sign-off step).
            && job.status != JobStatus::Closed
        {
            let old_status = job.status;
            job.status = new_status;
            if new_status == JobStatus::Closed {
                // Same business-date contract as step.completed_on:
                // the closing transition is the step we just wrote, so
                // its completed_on (a date on the authoritative — sim-
                // aware — calendar) is the right anchor for the Job's
                // closed_on too. Falls through to the authoritative
                // clock's date only if the step somehow lacks one.
                let job_now = boss_clock_client::now_from(&state.clock).await;
                job.closed_on = step.completed_on.or(Some(job_now.date_naive()));
            }
            // OUTBOX (phase 2): the state event (full row state for
            // the rebuild) + status markers record in the SAME
            // transaction as the auto-transition. The actor is
            // inherited from the step transition that triggered it:
            // the operator (or sim slug) who flipped the terminal
            // step is the responsible CPU for the resulting Job
            // state change too.
            let close_stamp = state
                .publisher
                .stamp_with_actor(actor.clone())
                .await
                .with_simulated(job.simulated);
            let mut close_events = vec![
                close_stamp.event(
                    events::JOB_UPDATED,
                    serde_json::to_value(&job).unwrap_or_default(),
                ),
                close_stamp.event(
                    events::JOB_STATUS_CHANGED,
                    serde_json::json!({
                        "id": job.id.to_string(),
                        "old_status": old_status,
                        "new_status": new_status,
                    }),
                ),
            ];
            if new_status == JobStatus::Closed {
                close_events.push(close_stamp.event(
                    events::JOB_CLOSED,
                    serde_json::json!({
                        "id": job.id.to_string(),
                        "closed_on": job.closed_on,
                        // ALWAYS present, on all three emit sites,
                        // defaulting null — the dispatcher's expr
                        // binder makes an ABSENT identifier a
                        // PredicateFailed → Retry → dead-letter storm
                        // rather than a quiet false, so a rule gating
                        // on `kind` / `outcome` needs the keys on every
                        // close, not just the ones that have an answer.
                        // (The `notify_on_done` field on step.done,
                        // migration 106, is the same contract.) A
                        // catch-all close carries no declared outcome,
                        // so `outcome` is null here unless a terminal
                        // already stamped one.
                        "kind": job.kind,
                        "outcome": job.metadata.get("outcome"),
                        // What closed, in words. A rule that SPAWNS off
                        // a close has to title the new packet, and the
                        // only titles available to it are a literal or
                        // an identifier from this payload — the arg
                        // language has no concatenation. Without this
                        // key, `title = "title"` binds nothing and the
                        // whole event dead-letters (see below); with a
                        // literal instead, every spawned packet is
                        // named identically and the board cannot tell
                        // them apart.
                        "title": job.title,
                        // WHAT the closed packet was about. A recurring
                        // sweep names its target here
                        // (`stale-build-caches`), and that is the only
                        // stable identity a spawning rule can dedupe
                        // on: the sweep's `id` differs every firing and
                        // its `title` is templated per target, so two
                        // days of the same finding are indistinguishable
                        // without this. Present on all three sites for
                        // the same reason `kind` and `title` are.
                        "subject_id": boss_core::primitives::Subject::id(&job.subject),
                        // D7: same delegate-subjob back-link as the
                        // terminal-close path, so a child Job that
                        // closes via the all-steps-terminal catch-all
                        // (no declared `outcome` step) still triggers
                        // the parent resolve. Null when absent.
                        "parent_step_id": job.metadata.get("parent_step_id"),
                    }),
                ));
            }
            let _ = state
                .jobs
                .update_job_at(&job, close_stamp.timestamp, &close_events)
                .await;
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(super) struct SignOffBody {
    /// Which required role this stamp satisfies.
    role: String,
}

/// POST /api/jobs/{id}/steps/{step_id}/sign-offs — stamp a step in
/// its current shape. Policy decides who may sign via
/// the role-scoped resource `step-signoff:<role>`.
/// Idempotent per (role, current shape): re-stamping unchanged
/// content returns the step unchanged.
/// `POST /api/jobs/{id}/steps/{step_id}/claim` — the claim hop
/// (queue-visibility Q2). Ready→Active as a compare-and-set owned by
/// the adapter: exactly one claimant wins; the loser gets 409 with
/// the holder. Idempotent for the holder. The generic PUT keeps its
/// PATCH semantics and never adjudicates claims.
#[derive(Deserialize, Default)]
pub(super) struct ClaimQuery {
    /// The station the claimant is pulling FROM. A packet has no
    /// single derivable station — membership is a predicate, and
    /// several stations can hold the same packet — so the capability
    /// gate (stations.md Q3: enforced at the claim CAS) applies to
    /// the station the claim names, not to some inferred one. Claims
    /// without a station keep today's behavior: the CAS plus the
    /// policy check above, no station capability consulted.
    station: Option<String>,
}

pub(super) async fn claim_step<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path((id, step_id_str)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(q): axum::extract::Query<ClaimQuery>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    let step_id = match parse_step_id(&step_id_str) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid step id").into_response(),
    };
    match state
        .policy
        .check(&user, Action::Update, Resource::step())
        .await
    {
        Ok(Decision::Deny { reason }) => {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
        Ok(_) => {}
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    }
    let old = match state.jobs.get_step(&step_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, "step not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if old.job_id != job_id {
        return (StatusCode::NOT_FOUND, "step not on this job").into_response();
    }

    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    // The parent packet: the claim's events inherit its
    // admission-fixed `simulated` flag, and the assignment marker
    // reads its Subject identity.
    let parent_job = state.jobs.get_job(&job_id).await.ok().flatten();

    // Station capability gate (stations.md Q3): when the claim names
    // the station it pulls from, the packet must actually be a
    // member of that station's queue, and the station's capability
    // (Class-registry role vocabulary) must admit the claimant.
    // Checked BEFORE the CAS so a gated claim never decides the
    // race it wasn't allowed to enter.
    if let Some(station_name) = q.station.as_deref() {
        let Some(reg) = &state.stations else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "station registry not configured",
            )
                .into_response();
        };
        let row = match reg.get_active(station_name).await {
            Ok(s) => s,
            Err(crate::stations::StationError::NotFound(msg)) => {
                return (StatusCode::NOT_FOUND, msg).into_response();
            }
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        };
        // Same binding as the queue read: "is this packet at MY
        // station" is the question a per-actor station asks, and an
        // unbindable placeholder means the claimant has no queue here
        // — so the packet is not at it.
        let bound = row.bind_self(self_id(&user));
        let Some(job) = &parent_job else {
            return (StatusCode::NOT_FOUND, "job not found").into_response();
        };
        let needs_steps = bound.as_ref().is_some_and(|s| s.predicate.needs_steps());
        let steps = if needs_steps {
            state.jobs.list_steps(&job_id).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        if !bound
            .as_ref()
            .is_some_and(|s| s.predicate.matches(job, &steps))
        {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "packet is not at this station",
                    "station": station_name,
                })),
            )
                .into_response();
        }
        if let Some(capability) = &row.capability
            && !capability.allows_role(&user.role)
        {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "role not admitted by station capability",
                    "station": station_name,
                    "role": user.role,
                    "allowed_roles": capability.roles,
                })),
            )
                .into_response();
        }
    }

    let mut stamp = state.publisher.stamp_with_actor(actor).await;
    if let Some(j) = &parent_job {
        stamp = stamp.with_simulated(j.simulated);
    }
    let stamp = stamp;

    // Optimistic post-state for the events; the CAS makes it the
    // real post-state on success, and on conflict nothing records.
    let mut claimed = old.clone();
    claimed.assignee_id = Some(user.id.clone());
    claimed.status = StepStatus::Active;

    let mut claim_events = vec![stamp.event(
        events::STEP_UPDATED,
        serde_json::to_value(&claimed).unwrap_or_default(),
    )];
    // Same grammar as the PUT path: an assignment marker only when
    // the assignee genuinely changed (a re-claim is not an
    // assignment), payload mirroring step.ready for messages.notify.
    if old.assignee_id.as_deref() != Some(user.id.as_str()) && !claimed.kind.is_empty() {
        let (subject_kind, subject_id) = if let Some(job) = &parent_job {
            (
                boss_core::primitives::Subject::kind(&job.subject).to_string(),
                boss_core::primitives::Subject::id(&job.subject).to_string(),
            )
        } else {
            (String::new(), String::new())
        };
        claim_events.push(stamp.event(
            &format!("step.assigned.{}", claimed.kind),
            serde_json::json!({
                "job_id": job_id.to_string(),
                "step_id": step_id.to_string(),
                "kind": claimed.kind,
                "subject_kind": subject_kind,
                "subject_id": subject_id,
                "assignee_id": claimed.assignee_id,
                "metadata": claimed.metadata,
            }),
        ));
    }

    match state
        .jobs
        .claim_step_at(&step_id, &user.id, stamp.timestamp, &claim_events)
        .await
    {
        Ok(step) => Json(step).into_response(),
        Err(crate::port::JobsError::ClaimConflict { holder, status }) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "step already claimed or not claimable",
                "holder": holder,
                "status": status,
            })),
        )
            .into_response(),
        Err(crate::port::JobsError::StepNotFound(_)) => {
            (StatusCode::NOT_FOUND, "step not found").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn post_step_sign_off<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path((id, step_id_str)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    headers: axum::http::HeaderMap,
    Json(body): Json<SignOffBody>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(v) => v,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    let step_id = match parse_step_id(&step_id_str) {
        Some(v) => v,
        None => return (StatusCode::BAD_REQUEST, "invalid step id").into_response(),
    };
    let mut step = match state.jobs.get_step(&step_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such step").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let role = body.role;
    if !step.sign_offs_required.iter().any(|r| r == &role) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("role {role} is not required on this step"),
        )
            .into_response();
    }
    let decision = match state
        .policy
        .check(
            &user,
            Action::SignOff,
            Resource::new(format!("step-signoff:{role}")),
        )
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
    if let Decision::Deny { reason } = decision {
        return (StatusCode::FORBIDDEN, reason).into_response();
    }

    // ASSURANCE: what this step demands, and what we can actually
    // produce. The step's own requirement wins when it is stronger
    // than the kind's floor; a Workflow may raise, never lower.
    let floor = state
        .step_registry
        .get(&step.kind)
        .map(|t| t.assurance_floor)
        .unwrap_or_default();
    let required = step.assurance_required.unwrap_or_default().max(floor);

    // NO BYPASS, which is the point David settled in Q3: "an assurance
    // level with a bypass is a comment, not a control." A stamp's
    // assurance is what the server VERIFIED, never what the caller
    // asked for. `Presence` is producible exactly one way: the
    // gateway's passkey ceremony verified a WebAuthn assertion over
    // sha256(shape_hash || ":" || nonce) and swapped the resulting
    // ticket for an `x-boss-presence` header — a header the edge
    // strips from every inbound request, so its presence here means
    // the gateway itself vouched. We still re-check the binding
    // against the step's CURRENT shape: a stale hash means the
    // content moved after the ceremony, and the stamp must not
    // survive an edit it never saw.
    let shape = boss_core::job::step_shape_hash(&step.title, &step.metadata);
    let presence_claim = headers
        .get("x-boss-presence")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
    let (produced, presence_nonce, presence_detail) = match &presence_claim {
        Some(p)
            if p["step_id"] == step_id_str.as_str()
                && p["shape_hash"] == shape.as_str()
                && p["employee_id"] == user.id.as_str() =>
        {
            (
                boss_core::job::Assurance::Presence,
                p["nonce"].as_str().map(String::from),
                "",
            )
        }
        Some(_) => (
            boss_core::job::Assurance::Session,
            None,
            " A presence ticket WAS presented but did not match: either the step's \
             content changed after the ceremony (stale shape hash — re-run it against \
             the current content) or it was minted for a different step or actor.",
        ),
        None => (
            boss_core::job::Assurance::Session,
            None,
            " Complete the passkey ceremony for this step \
             (POST /api/auth/passkey/assert/begin, then .../finish) and retry with \
             the issued ticket.",
        ),
    };
    if required > produced {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "step requires stronger assurance than this request carries",
                "required": required,
                "produced": produced,
                "detail": format!(
                    "this step requires proof of presence — a passkey assertion bound \
                     to the step's shape hash.{presence_detail}"
                ),
            })),
        )
            .into_response();
    }
    if step
        .sign_offs
        .iter()
        .any(|st| st.role == role && st.shape_hash == shape)
    {
        return Json(step).into_response(); // idempotent re-stamp
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = boss_core::job::SignOffStamp {
        authority_id: user.id.clone(),
        role: role.clone(),
        stamped_at: now,
        shape_hash: shape.clone(),
        assurance: produced,
        presence_nonce: presence_nonce.clone(),
    };
    // OUTBOX (phase 2): the signed-off marker records in the SAME
    // transaction as the stamp append.
    let actor = boss_core::actor::ActorId::human(&user.id);
    let mut event_stamp = state.publisher.stamp_with_actor(actor).await;
    // The signed-off marker inherits the packet's admission-fixed
    // flag, like every other event about the Job.
    if let Ok(Some(job)) = state.jobs.get_job(&job_id).await {
        event_stamp = event_stamp.with_simulated(job.simulated);
    }
    let signed_off_event = event_stamp.event(
        events::STEP_SIGNED_OFF,
        serde_json::json!({
            "job_id": job_id.to_string(),
            "step_id": step_id.to_string(),
            "role": role,
            "authority_id": user.id,
            "shape_hash": shape,
            "assurance": produced,
            "presence_nonce": presence_nonce,
        }),
    );
    if let Err(e) = state
        .jobs
        .append_sign_off(&step_id, &stamp, event_stamp.timestamp, &[signed_off_event])
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    step.sign_offs.push(stamp);
    Json(step).into_response()
}

/// Close a Job because a *declared terminal* step reached
/// `Completed`. Mirrors the `compute_job_status`-driven close (same
/// JOB_UPDATED / JOB_STATUS_CHANGED / JOB_CLOSED events, same
/// closed_on anchoring) but is outcome-aware: it stamps
/// `metadata.outcome` and marks every still-non-terminal step
/// (`Pending` / `Ready` / `Active`) `Skipped` so the closed Job has no
/// dangling open work. No-ops if the Job is already terminal
/// (Cancelled / Draft) or already Closed.
async fn close_job_on_terminal<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job_id: &boss_core::job::JobId,
    outcome: &str,
    actor: &boss_core::actor::ActorId,
    now: chrono::DateTime<chrono::Utc>,
) {
    let Ok(Some(mut job)) = state.jobs.get_job(job_id).await else {
        return;
    };
    if matches!(
        job.status,
        JobStatus::Closed | JobStatus::Cancelled | JobStatus::Draft
    ) {
        // Already terminal / not-yet-open — nothing to close.
        return;
    }

    let terminal_stamp = state
        .publisher
        .stamp_with_actor(actor.clone())
        .await
        .with_simulated(job.simulated);

    // Skip every still-non-terminal step. The Job is closing on its
    // terminal outcome; any Pending/Ready/Active step is now moot.
    if let Ok(steps) = state.jobs.list_steps(job_id).await {
        for mut s in steps {
            if matches!(
                s.status,
                StepStatus::Pending | StepStatus::Ready | StepStatus::Active
            ) {
                s.status = StepStatus::Skipped;
                // OUTBOX (phase 2): the skip's state event records in
                // the SAME transaction as the row.
                let skip_event =
                    terminal_stamp.event(events::STEP_UPDATED, events::step_state_payload(&s));
                if let Err(e) = state
                    .jobs
                    .update_step_at(&s, terminal_stamp.timestamp, &[skip_event])
                    .await
                {
                    tracing::warn!(
                        job_id = %job_id,
                        step_id = %s.id,
                        error = %e,
                        "terminal close: failed to skip non-terminal step",
                    );
                    continue;
                }
            }
        }
    }

    let old_status = job.status;
    job.status = JobStatus::Closed;
    job.closed_on = Some(now.date_naive());
    // Stamp the terminal outcome onto the Job metadata so projections
    // / the SPA can render *why* the Job closed.
    if let serde_json::Value::Object(map) = &mut job.metadata {
        map.insert(
            "outcome".to_string(),
            serde_json::Value::String(outcome.to_string()),
        );
    } else {
        job.metadata = serde_json::json!({ "outcome": outcome });
    }

    // OUTBOX (phase 2): the close's state event + markers record in
    // the SAME transaction as the row.
    let close_events = [
        terminal_stamp.event(
            events::JOB_UPDATED,
            serde_json::to_value(&job).unwrap_or_default(),
        ),
        terminal_stamp.event(
            events::JOB_STATUS_CHANGED,
            serde_json::json!({
                "id": job.id.to_string(),
                "old_status": old_status,
                "new_status": JobStatus::Closed,
            }),
        ),
        terminal_stamp.event(
            events::JOB_CLOSED,
            serde_json::json!({
                "id": job.id.to_string(),
                "closed_on": job.closed_on,
                "outcome": outcome,
                // Which protocol closed. Present on all three emit
                // sites so a rule can select the Workflow it cares
                // about as data: the close marker otherwise names no
                // kind, and every consumer had to fetch the Job to
                // find out whether the event was even about them.
                "kind": job.kind,
                // Present on all three sites for the same reason `kind`
                // is: a spawning rule can only name the packet it
                // creates from a literal or from this payload.
                "title": job.title,
                // See the status-transition site above: the subject is
                // the recurring packet's stable identity, and the only
                // key a spawn rule can dedupe a repeating finding on.
                "subject_id": boss_core::primitives::Subject::id(&job.subject),
                // D7: surface the delegate-subjob back-link (if any) on
                // the close marker so the jobs.subjob_resolve rule can
                // gate `when` on it without fetching the Job. Null for
                // an ordinary (non-delegated) Job.
                "parent_step_id": job.metadata.get("parent_step_id"),
            }),
        ),
    ];
    if let Err(e) = state
        .jobs
        .update_job_at(&job, terminal_stamp.timestamp, &close_events)
        .await
    {
        tracing::warn!(job_id = %job_id, error = %e, "terminal close: failed to persist closed Job");
    }
}

/// D6 ready marker — build the `step.ready.<kind>` event for a step
/// that just transitioned into `Ready`. Mirrors the `step.done.<kind>`
/// marker (informational duplicate of state already in STEP_UPDATED /
/// STEP_CREATED; the rebuilder ignores it). The dispatcher rule
/// registry subscribes to it the same way it subscribes to
/// `step.done.<kind>`; the D7 delegate-subjob spawn fork is its first
/// consumer. OUTBOX (phase 2): callers record the built event either
/// in the promoting write's transaction (re-eval) or via
/// `record_events` (the post-materialization pass).
///
/// The payload is shape-compatible with the `step.done` marker — same
/// `job_id` / `step_id` / `kind` / `subject_kind` / `subject_id` /
/// `metadata` keys — so handlers reuse the same `StepEvent` view. The
/// Subject identity comes from the parent `job` the caller already
/// holds, so no extra fetch.
/// Re-evaluate a Job's step readiness against its PINNED Workflow and
/// persist every promotion (Pending → Ready, with the `step.ready`
/// marker in the same transaction; Pending → Skipped). The single
/// persistence glue over `registry::reevaluate` — shared by step
/// updates (a status change flips downstream predicates) and Job
/// updates (a metadata write can flip a metadata-gated predicate; the
/// v3 ship-a-change `merged` marker was invisible without this,
/// aa9980c8).
///
/// Resolves the version the Job was OPENED under, not whatever is
/// active now. A Job materializes its steps once and keeps them;
/// evaluating those steps against a newer spec asks predicates about
/// steps the Job never had, and the length-guard then bails and the
/// Job stops advancing — silently, as a Job that simply never closes.
/// Two feedback Jobs were stranded exactly this way.
pub(super) async fn reevaluate_and_persist<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job: &Job,
    actor: &boss_core::actor::ActorId,
) {
    let Some(reg) = &state.kind_registry else {
        return;
    };
    match reg.get_version(&job.kind, job.workflow_version).await {
        Ok(spec) => {
            let Ok(mut steps) = state.jobs.list_steps(&job.id).await else {
                return;
            };
            // `reevaluate` requires steps in spec order (sort_order ==
            // index); list_steps returns them sorted by sort_order, so
            // the invariant holds. Invariant (expose, don't swallow):
            // a Job's live step set must match its active Workflow
            // spec, or `reevaluate`'s length-guard bails and the Job
            // can no longer advance. With atomic materialization this
            // only fires on a genuine mid-flight republish that
            // changed the step count. Surface it loudly instead of
            // silently stalling the Job.
            if spec.steps.len() != steps.len() {
                tracing::warn!(
                    job_id = %job.id,
                    kind = %job.kind,
                    spec_len = spec.steps.len(),
                    steps_len = steps.len(),
                    "re-eval: live step count != active Workflow spec — \
                     readiness cannot advance this Job (its step graph \
                     is inconsistent with its Workflow)"
                );
            }
            let changed =
                crate::registry::reevaluate(&spec, &mut steps, &job.subject, &job.metadata);
            let stamp = state
                .publisher
                .stamp_with_actor(actor.clone())
                .await
                .with_simulated(job.simulated);
            for idx in changed {
                let changed_step = &steps[idx];
                // OUTBOX (phase 2): the promoted step's state event +
                // D6 ready marker (when it lands in `Ready` — lets
                // dispatcher rules react to a step *becoming
                // eligible*, the delegate-subjob spawn fork D7) record
                // in the SAME transaction as the promotion.
                let mut reeval_events = vec![stamp.event(
                    events::STEP_UPDATED,
                    events::step_state_payload(changed_step),
                )];
                if changed_step.status == StepStatus::Ready && !changed_step.kind.is_empty() {
                    reeval_events
                        .push(build_step_ready_event(state, job, changed_step, actor).await);
                }
                if let Err(e) = state
                    .jobs
                    .update_step_at(changed_step, stamp.timestamp, &reeval_events)
                    .await
                {
                    tracing::warn!(
                        job_id = %job.id,
                        step_id = %changed_step.id,
                        error = %e,
                        "re-eval: failed to persist promoted step",
                    );
                    continue;
                }
            }
        }
        Err(crate::registry::WorkflowError::NotFound(_)) => {
            // No active spec (ad-hoc / registry-less kind): nothing to
            // re-evaluate. The compute_job_status auto-close still
            // handles the all-steps-terminal case.
        }
        Err(e) => {
            tracing::warn!(error = %e, job_id = %job.id, version = job.workflow_version, "re-eval: pinned Workflow version not resolvable");
        }
    }
}

pub(super) async fn build_step_ready_event<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job: &Job,
    step: &Step,
    actor: &boss_core::actor::ActorId,
) -> boss_core::event::Event {
    let subject_kind = boss_core::primitives::Subject::kind(&job.subject).to_string();
    let subject_id = boss_core::primitives::Subject::id(&job.subject).to_string();
    let stamp = state
        .publisher
        .stamp_with_actor(actor.clone())
        .await
        .with_simulated(job.simulated);
    stamp.event(
        &format!("step.ready.{}", step.kind),
        serde_json::json!({
            "job_id": step.job_id.to_string(),
            "step_id": step.id.to_string(),
            "kind": step.kind,
            "subject_kind": subject_kind,
            "subject_id": subject_id,
            // A step assigned BEFORE it became ready notifies its
            // assignee, not the role's on-call member — the handler
            // prefers a named assignee when the payload carries one.
            "assignee_id": step.assignee_id,
            "metadata": step.metadata,
        }),
    )
}
