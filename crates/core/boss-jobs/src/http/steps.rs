//! Step handlers — list, add, update (the readiness/sign-off/dispatch
//! engine), and sign-off stamping.

use super::*;

use axum::extract::Path;

use crate::registry::ProtocolReading;

/// How deep the concurrency gate reads an actor's assignments when it
/// counts its runs in flight (backlog 57c108c2). A limit is not a
/// filter: truncation here could only UNDER-count, which under-refuses,
/// so this sits far above any cap an `agents` row would plausibly
/// declare — a bound of a thousand concurrent agent runs is not a
/// bound. The query is one indexed lookup by assignee on the Pg
/// adapter, run once per claim, and claims are rare.
const IN_FLIGHT_SCAN_LIMIT: i64 = 1_000;

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
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    // Every step's metadata is the packet's content: the detail's read
    // scope, asked before the id is parsed (backlog 046832d3).
    let scope = match job_read_scope(&state, &user).await {
        Ok(scope) => scope,
        Err(refusal) => return refusal,
    };
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    if let Err(refusal) = readable_job(&state, &user, &scope, &job_id).await {
        return refusal;
    }

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

    // The same coarse (Update, step) authority every other step write
    // is gated on. This door had none at all (afbf4f73); a write that
    // creates a step is at least a write to steps.
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
            return e.into_response();
        }
    }

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
    //
    // WHOLE, NOT ONLY WHEN FULL (backlog afbf4f73). This guard used to
    // refuse only a job that already held as many steps as its spec, so
    // an EMPTY packet — which `?materialize_steps=false` produced on
    // request — passed it, and a caller could post that packet's steps
    // itself: an ops-request's presence-assured `approve`, born
    // completed, carrying a passkey stamp for a person who never touched
    // a passkey. The opt-out is refused at admission now; this refuses
    // the residue it left, because a spec'd packet's steps come from its
    // spec and from nowhere else. And it fails CLOSED: an unreadable job
    // or registry is an error, not a reason to let the write through.
    if let Some(reg) = &state.kind_registry {
        let job = match state.jobs.get_job(&job_id).await {
            Ok(Some(job)) => job,
            Ok(None) => return (StatusCode::NOT_FOUND, "job not found").into_response(),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        };
        match reg.get_version(&job.kind, job.workflow_version).await {
            Ok(_) => {
                return (
                    StatusCode::CONFLICT,
                    format!(
                        "refusing to add a step to job {job_id}: its steps are those of \
                         workflow {} v{}, fixed at admission. A step is PROTOCOL, so adding \
                         one is a change to the WORKFLOW, not to a single in-flight job: add \
                         it to the workflow and publish a new version, and new packets are \
                         admitted under it. In-flight jobs stay pinned to the version they \
                         were admitted under, which is the whole point of the versioning.",
                        job.kind, job.workflow_version
                    ),
                )
                    .into_response();
            }
            Err(crate::registry::WorkflowError::NotFound(_)) => {}
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    }

    // A POSTED STEP CARRIES NO EVIDENCE (afbf4f73). A sign-off stamp is
    // the record of a ceremony — the sign-off door verifies the role
    // and, for presence, the gateway-vouched passkey assertion — and
    // this door runs none, so a body's stamps are refused, not kept.
    // Refused rather than stripped so the caller learns the rule at the
    // call; a silently emptied list is a write that half-landed.
    if !step.sign_offs.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "a posted step cannot carry sign-offs",
                "detail": "a sign-off stamp is written only by the sign-off door \
                           (POST /api/jobs/{id}/steps/{step_id}/sign-offs), which runs \
                           the ceremony the stamp records. Post the step with \
                           `sign_offs: []` and stamp it there.",
            })),
        )
            .into_response();
    }

    // NOR IS A STEP THAT DEMANDS EVIDENCE BORN RESOLVED. Completing a
    // step is judged at the step PUT — the assurance guard, the
    // human-only check — and the sign-off door is where required stamps
    // come from; a step posted straight in as completed or skipped
    // passes through none of them. So a step that declares a demand is
    // born open and resolved through the door that judges it. A skip
    // counts, as it does on the PUT: `ready_when` reads `steps.x.done`,
    // which a skipped step satisfies. A step that demands nothing may
    // still be born completed, with the server's stamps below.
    if matches!(step.status, StepStatus::Completed | StepStatus::Skipped) {
        let floor = state
            .step_registry
            .get(&step.kind)
            .map(|t| t.assurance_floor)
            .unwrap_or_default();
        let required = step.assurance_required.unwrap_or_default().max(floor);
        let mut demands = Vec::new();
        if !step.sign_offs_required.is_empty() {
            demands.push(format!(
                "sign-offs by {}",
                step.sign_offs_required.join(", ")
            ));
        }
        if required > boss_core::job::Assurance::Session {
            demands.push(format!("{required:?} assurance").to_lowercase());
        }
        if crate::human_only::declared(&step.metadata) {
            demands.push("completion by a person (human_only)".to_string());
        }
        if !demands.is_empty() {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "a step that demands evidence cannot be posted already resolved",
                    "demands": demands,
                    "detail": "post it open (pending, ready or active), then complete it \
                               through PUT /api/jobs/{id}/steps/{step_id}, which judges \
                               what it demands.",
                })),
            )
                .into_response();
        }
    }

    // Ensure the step belongs to this job.
    step.job_id = job_id;

    // A step born human-only with a non-human assignee is the same
    // assignment the PUT refuses (c17871fe), one door earlier.
    if crate::human_only::declared(&step.metadata)
        && let Some(assignee) = step.assignee_id.as_deref()
        && let Err(why) = crate::human_only::person_check(state.roster.as_deref(), assignee).await
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(crate::human_only::refusal_body(
                &step.id.to_string(),
                &step.title,
                step.metadata.get("authority_role").and_then(|v| v.as_str()),
                assignee,
                &why,
            )),
        )
            .into_response();
    }

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
    let mut stamp = state.publisher.stamp_with_actor(actor.clone()).await;
    // Step events inherit the parent packet's admission-fixed
    // partition (the packet, not the request's transport
    // context, is the source of truth). A step posted against a
    // missing Job keeps the chain default — Pg rejects it on the FK
    // anyway.
    if let Ok(Some(job)) = state.jobs.get_job(&job_id).await {
        stamp = stamp.with_partition(job.partition);
    }
    // The completion stamps are server-owned here as on the PUT
    // (c17871fe): a body cannot name who completed a step or when. A
    // step born `completed` is a completion, and carries the same
    // stamps the flip would have written; any other status has none.
    step.completed_by = None;
    step.completed_at = None;
    if step.status == StepStatus::Completed {
        step.completed_by = Some(actor);
        step.completed_at = Some(stamp.timestamp);
    }
    // The plugin version the row will be stored at, stamped before the
    // event is built so the event carries it (backlog aba364fe).
    if let Err(e) = stamp_step_plugin_version(state.jobs.as_ref(), &mut step).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
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
/// `spec` as the registry would hold it had it been published as
/// `row`: the fields the registry writes on a publish (`version`,
/// `status`, `created_at`, `authoring_job_id`) taken from `row`, every
/// authored field from `spec` — so `row == published_as(spec, row)`
/// says `row` IS the publish of `spec` (backlog 558396ff, SF3).
fn published_as(
    spec: &crate::registry::WorkflowSpec,
    row: &crate::registry::WorkflowSpec,
) -> crate::registry::WorkflowSpec {
    crate::registry::WorkflowSpec {
        version: row.version,
        status: row.status,
        created_at: row.created_at,
        authoring_job_id: row.authoring_job_id,
        ..spec.clone()
    }
}

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

    // PUBLISHED ONCE PER AUTHORING PACKET (backlog 558396ff). This runs
    // before the step write, and since car 88123ae0 that write can be
    // refused as stale — "nothing was written; send the same request
    // again" — AFTER this registry row was written. Each re-send then
    // published the same spec as one more version, retiring the last.
    // The row carries the packet that authored it, so a re-send finds
    // its own publish active and answers it instead of writing another —
    // but only a re-send of the SAME spec (the round-2 review of car
    // 983696b5, SF3). Keyed on the packet alone, a spec edited between
    // the refused attempt and its re-send was answered with the earlier
    // publish, and the step completed recording a spec the registry
    // never published. A different spec is a new publish.
    //
    // ITS OWN ROW, ACTIVE OR RETIRED (backlog 4bdb8150, the round-3
    // review of car 983696b5). Consulting only the ACTIVE row made a
    // concurrent publish last-writer-wins: another packet publishing the
    // same kind between this one's refused attempt and its re-send left
    // an active row that was not this packet's, so the re-send published
    // this spec again and retired the other packet's newer version. This
    // packet's publish happened once, and a later one superseding it does
    // not undo that; the re-send is answered with the row it wrote.
    let authored_here = Some(*job_id.inner().as_uuid());
    if let Ok(versions) = registry.list_versions(&spec.kind).await
        && let Some(own) = versions.into_iter().rev().find(|row| {
            row.authoring_job_id == authored_here
                && row.status != crate::registry::WorkflowStatus::Draft
                && *row == published_as(&spec, row)
        })
    {
        return Ok(own);
    }

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

/// What a step DEMANDS and what a request can actually PRODUCE.
///
/// ONE JUDGEMENT, CALLED FROM EVERY PATH THAT COMPLETES A STEP
/// (backlog 148549c5). This used to live only inside
/// `post_step_sign_off`, which made the control OPT-IN: a step reaches
/// that path only when it also declares a required sign-off role, and
/// everything else completes through the ordinary PUT below. Measured
/// on packet d5efbb3c, 2026-09-22 — the first presence-assured step in
/// the system was completed by a status flip with no ceremony, no
/// stamp and no refusal, and the host then ran the verb it was gating.
///
/// `assurance_required` is a property of the STEP, so the answer must
/// not depend on which door the caller used. §9a: one definition.
///
/// `Presence` is producible exactly one way — the gateway verified a
/// WebAuthn assertion over `sha256(shape_hash || ":" || nonce)` and
/// signed a ticket for it, and THIS SERVICE verifies that ticket's
/// signature (`boss_core::presence::PresenceTicket::decode`, the same
/// function the gateway checks it with) before reading a word of it.
/// Until backlog 72fe3640 (2026-09-24) the header held the ticket's
/// fields as plain JSON and was trusted because the gateway's edge strip
/// removes inbound `x-boss-*` — but the machine door (:7900) is
/// reachable without the gateway, so every machine-token holder could
/// stamp presence on any step as any person. The binding is then
/// re-checked against the step's CURRENT shape: a stale hash means the
/// content moved after the ceremony, and an approval must not survive
/// an edit it never saw.
pub(super) struct Assured {
    pub required: boss_core::job::Assurance,
    pub produced: boss_core::job::Assurance,
    pub presence_nonce: Option<String>,
    /// What to tell a caller that fell short, or "" when it did not.
    pub detail: &'static str,
    /// A presence claim was made and did not verify. Refused on every
    /// judged write, whatever the step requires: a claim this service
    /// cannot check is a forgery or a fault, and either one said aloud
    /// beats a stamp quietly downgraded to Session.
    pub unverified: bool,
}

impl Assured {
    pub fn falls_short(&self) -> bool {
        self.unverified || self.required > self.produced
    }

    /// The refusal both doors return, in one shape so a caller cannot
    /// tell which door it knocked on.
    pub fn refusal(&self) -> Response {
        if self.unverified {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "the presence claim on this request did not verify",
                    "required": self.required,
                    "produced": self.produced,
                    "detail": self.detail,
                })),
            )
                .into_response();
        }
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "step requires stronger assurance than this request carries",
                "required": self.required,
                "produced": self.produced,
                "detail": format!(
                    "this step requires proof of presence — a passkey assertion bound \
                     to the step's shape hash.{}",
                    self.detail
                ),
            })),
        )
            .into_response()
    }
}

/// What the presence-content refusal in `update_step` tells its caller
/// (backlog c0b56fd9).
const PRESENCE_CONTENT_HINT: &str = "write the content first through the merge door \
     (PATCH .../metadata), run the passkey ceremony over the step as it then stands, and \
     complete with {\"status\":\"completed\"} alone and the ticket that ceremony issued. \
     A presence-assured step is not skipped: leave it open, or cancel the packet.";

pub(super) fn judge_assurance(
    floor: boss_core::job::Assurance,
    step: &boss_core::job::Step,
    step_id_str: &str,
    user_id: &str,
    headers: &axum::http::HeaderMap,
    // The gateway's key, or `None` when this service has none — and then
    // nothing verifies (http/presence.rs).
    key: Option<&[u8]>,
) -> Assured {
    use boss_core::presence::{HEADER, PresenceTicket, now_epoch};
    // The step's own requirement wins when it is stronger than the
    // kind's floor; a Workflow may raise, never lower.
    let required = step.assurance_required.unwrap_or_default().max(floor);
    let shape = boss_core::job::step_shape_hash(&step.title, &step.metadata);
    // Absent → no claim. Present → it verifies against the key, or it is
    // refused; there is no third reading of a header anyone at the
    // machine door can write.
    let claim = headers.get(HEADER).map(|v| {
        v.to_str()
            .ok()
            .zip(key)
            .and_then(|(value, key)| PresenceTicket::decode(value, key, now_epoch()))
    });
    let (produced, presence_nonce, detail, unverified) = match &claim {
        Some(Some(t)) if t.s == step_id_str && t.h == shape && t.i == user_id => (
            boss_core::job::Assurance::Presence,
            Some(t.n.clone()),
            "",
            false,
        ),
        Some(Some(_)) => (
            boss_core::job::Assurance::Session,
            None,
            " A presence ticket WAS presented but did not match: either the step's \
             content changed after the ceremony (stale shape hash — re-run it against \
             the current content) or it was minted for a different step or actor.",
            false,
        ),
        Some(None) => (
            boss_core::job::Assurance::Session,
            None,
            "An x-boss-presence header was presented and did not verify: it is not a \
             ticket the gateway signed, it has expired, or this service holds no key to \
             check it with. Presence is granted only on a signed ticket, verified here \
             (backlog 72fe3640) — run the passkey ceremony through the gateway and \
             present the ticket it issues.",
            true,
        ),
        None => (
            boss_core::job::Assurance::Session,
            None,
            " Complete the passkey ceremony for this step \
             (POST /api/auth/passkey/assert/begin, then .../finish) and retry with \
             the issued ticket.",
            false,
        ),
    };
    Assured {
        required,
        produced,
        presence_nonce,
        detail,
        unverified,
    }
}

pub(super) async fn update_step<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path((id, step_id_str)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    // The presence claim rides here, exactly as it does on the sign-off
    // door: `x-boss-presence`, the gateway's signed ticket, verified by
    // `judge_assurance` before it is believed, so this handler judges
    // the same way (backlog 148549c5; verification 72fe3640).
    headers: axum::http::HeaderMap,
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
    // Simulator traffic is allowed by the SimBypassPolicyClient on a sim
    // instance, for a sim caller only (backlog 85e7f10f; the write is
    // still stamped `_simulated`), so this gate never stalls a regen.
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
            return e.into_response();
        }
    }

    // PATCH semantics: fetch the current step, then overlay the caller's
    // body on top. Any field the caller omits keeps its current value,
    // so clients can send `{"status": "done"}` without having to round-
    // trip the whole Step. Full replacements still work — a body that
    // includes every field just overwrites everything.
    //
    // Read WITH its version: the write at the foot lands only over the
    // row as this read saw it (backlog 6ec22d71).
    let (old, old_version) = match state.jobs.get_step_versioned(&step_id).await {
        Ok(Some(read)) => read,
        Ok(None) => return (StatusCode::NOT_FOUND, "step not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Same containment rule as the metadata PATCH and the claim route,
    // which have carried it all along — this handler did not, and it is
    // the most-used write of the three (packet 730ea77e). The step is
    // found by its OWN id, so without this the path's job id is
    // decorative: a fabricated one was accepted live on 2026-09-21, the
    // step flipped on the real packet, and the caller got 204. What it
    // costs is below this line, not here — a job id naming nothing
    // makes `parent_job` `None`, the event's subject and workflow fall
    // back to empty strings, and the `if let Some(job)` at the foot
    // skips BOTH the re-evaluator and the terminal close. The packet is
    // left wedged with no error anywhere.
    if old.job_id != job_id {
        return (StatusCode::NOT_FOUND, "step not on this job").into_response();
    }

    // The parent packet, fetched ONCE: the event stamp inherits its
    // partition, the step.done / step.assigned markers read
    // its Subject identity, and the re-evaluator runs against it.
    // The step write below never touches the jobs row, so this read
    // stays current through all of those. (The auto-close pass at
    // the bottom re-fetches — close_job_on_terminal may have closed
    // the Job in between.)
    //
    // A READ THAT FAILS IS REFUSED, NOT READ AS "NO PACKET" (backlog
    // 5186c5e1). This was `.ok().flatten()`, so a storage error became
    // `None`: the abort exemption and the predicate gate below both
    // read no protocol (`protocol_reading` answers `Unpaired` for no
    // packet), a terminal waiting on a job marker completed without it,
    // and the close at the foot was skipped — a completed terminal on
    // an open packet, answered 204. A gate that cannot read its
    // protocol does not open. `Ok(None)` — a read that answered, with
    // no row — stays `None` and is judged as it always was; only the
    // read that did not answer is refused.
    let parent_job = match state.jobs.get_job(&job_id).await {
        Ok(job) => job,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("reading packet {job_id} failed, so its step is not written: {e}"),
            )
                .into_response();
        }
    };

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

    // A METADATA BODY THAT DROPS A STORED KEY IS REFUSED (backlog
    // e39a9d2a, design baf738b7 answered 2026-09-23 — the one-car rule).
    //
    // The overlay above replaces `metadata` WHOLESALE, so a body that
    // omits a key deletes it. Three keys were hand-carried past that
    // replace — `authority_role`, `human_only`, `agent_run` — each
    // after someone lost it in production (the run edge: a completer
    // erased it and the run died four hours later on the silence clock,
    // b91a2103), and two more step-level keys were in flight. The list
    // grew by incident; this removes the class instead.
    //
    // WHY REFUSE AND NOT MERGE, since merging looks obviously nicer:
    // merging silently changes the meaning of EVERY existing call at
    // once — a caller that clears a key by omitting it today would stop
    // clearing it, invisibly and retroactively. A refusal is loud and
    // arrives at the one call site that must change, which is the shape
    // the terminal-step refusal below already has. A read-merge-write
    // caller sends every stored key and is untouched; a PUT with no
    // `metadata` key (a status-only flip) is not judged; a caller whose
    // read went stale while a concurrent writer added a key is now
    // caught instead of erasing that key. Clearing on purpose is the
    // merge door's job, with the key sent as `null`.
    //
    // NOT ON A TERMINAL STEP. The merge door refuses a terminal step
    // too, so routing the caller there would send it from one 409 to
    // another; the terminal refusal below speaks instead (an omitting
    // body changes the metadata, so it fires), and its hint names the
    // doors that work on a record. An unchanged re-send is not an
    // omission and stays the no-op the freeze lets through.
    //
    // STAGE 1 of design 93d2bddb (decided_2026_09_24b on e39a9d2a): the
    // decided end state refuses ANY metadata body; this omission rule
    // removes the silent wipe now, and the tighten is this one block
    // once the read-merge-write writers have moved to the merge door.
    let is_terminal = matches!(old.status, StepStatus::Completed | StepStatus::Skipped);
    if let Some(sent) = body_obj.get("metadata")
        && !is_terminal
    {
        let missing = crate::step_metadata_write::omitted_keys(&old.metadata, sent);
        if !missing.is_empty() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "metadata body omits stored keys — a step PUT replaces \
                              metadata wholesale, so an omitted key would be deleted",
                    "step_id": step_id.to_string(),
                    "missing_keys": missing,
                    "merge_door": format!("/api/jobs/{job_id}/steps/{step_id}/metadata"),
                    "hint": crate::step_metadata_write::OMITTED_KEYS_HINT,
                })),
            )
                .into_response();
        }
    }

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
    // A BLANK HOLDER IS STORED AS NOBODY (backlog 6ef4a36b, the review
    // of car 781b9209). `{"status":"ready","assignee_id":""}` is a
    // release, and it stored `Some("")` — which the claim CAS reads as
    // a holder, so the freed step could never be claimed. Before every
    // judgement below, so each one reads the value that will be
    // written. Not on a terminal row: its holder is frozen, and a
    // re-send of a legacy blank must stay the no-op the freeze allows.
    if !is_terminal {
        step.assignee_id = crate::active_holder::stored(step.assignee_id);
    }

    // A STEP'S PLACE IN ITS PROTOCOL DOES NOT MOVE (backlog b433bdf3).
    // The overlay above took every field from the body, so a writer
    // could pick which terminal closes the packet (`sort_order` is the
    // index the close pairs a completed step to its spec by), complete
    // out of order (`blocked_by: []` emptied the list the gate reads),
    // or complete without the evidence the protocol asks for (`fields:
    // []`, or a `kind` whose bundle requires less and whose assurance
    // floor is lower). Refused, like the `human_only` change below and
    // for its reason — the caller learns the rule at the call; a body
    // sending each back as read is not a move. Every read of these
    // below is from `old` as well, so the refusal is not the only
    // thing standing between a body and the gate.
    let reshaped = crate::step_metadata_write::reshaped_fields(&old, &step);
    if !reshaped.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "a step's place in its protocol is fixed",
                "step_id": step_id.to_string(),
                "refused_fields": reshaped,
                "hint": crate::step_metadata_write::RESHAPED_FIELDS_HINT,
            })),
        )
            .into_response();
    }

    // Stamps are server-minted (POST .../sign-offs) and requirements
    // are materialization data — a PUT body controls neither. The
    // assurance requirement is one of those requirements: the guard
    // below judges `old`, so a body that lowered it would pass THIS
    // write and step over the bar on the next (afbf4f73).
    step.sign_offs = old.sign_offs.clone();
    step.sign_offs_required = old.sign_offs_required.clone();
    step.assurance_required = old.assurance_required;

    // So are the completion stamps: `completed_by` / `completed_at`
    // are the server's record of who flipped the step and when
    // (c17871fe). Whatever the body says, the stored values ride
    // through; the flip below overwrites them with the signing actor
    // and the clock. A client cannot choose its own provenance.
    step.completed_by = old.completed_by.clone();
    step.completed_at = old.completed_at;

    // `authority_role` is immutable across PUTs: the persisted value
    // wins, so a body can neither raise nor lower the required sign-off
    // authority — the sign-off gate reads `old.metadata` for its
    // decision, and this keeps the stored row consistent with it. The
    // merge door strips the key for the same reason. (Its OMISSION is
    // the drop refusal above; `agent_run`, which was carried past
    // omission beside it until e39a9d2a, needs nothing here now — a
    // body that omits it is refused, and it remains writable through
    // the merge door. `human_only` is not writable anywhere once the
    // row carries it: the change refusal below, adac8fa4.)
    if let Some(old_obj) = old.metadata.as_object()
        && let Some(auth) = old_obj.get("authority_role").cloned()
        && let Some(obj) = step.metadata.as_object_mut()
    {
        obj.insert("authority_role".into(), auth);
    }

    // THE DECLARATION IS FROZEN ON THE STEP (adac8fa4). The completion
    // check below reads the STORED row, so a body that set `human_only`
    // to false — in the completing PUT itself, or in an earlier one —
    // would otherwise walk round it. Refused, not silently kept like
    // `authority_role` above, so the caller learns the rule at the call.
    if crate::human_only::declaration_changed(&old.metadata, &step.metadata) && !is_terminal {
        return (
            StatusCode::FORBIDDEN,
            Json(crate::human_only::change_refusal_body(
                &step_id.to_string(),
                &old.title,
                &old.metadata,
            )),
        )
            .into_response();
    }

    // `outcome_kind` IS THE PROTOCOL'S (b433bdf3). The abort exemption
    // below reads the stored value, so a PUT that stored `aborted` on
    // an ordinary terminal opened the gate for the next, bare, PUT.
    // Same scope as the `human_only` refusal above: a terminal row's
    // metadata is refused by the freeze below, which says it better.
    let protocol_keys =
        crate::step_metadata_write::protocol_keys_changed(&old.metadata, &step.metadata);
    if !protocol_keys.is_empty() && !is_terminal {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "metadata body changes a key the protocol owns",
                "step_id": step_id.to_string(),
                "refused_keys": protocol_keys,
                "hint": crate::step_metadata_write::PROTOCOL_KEYS_HINT,
            })),
        )
            .into_response();
    }

    // AN ACTIVE STEP KEEPS ITS HOLDER (backlog 650ebd0c, the review of
    // car e341f7cd). A claim is the Ready→Active CAS; nothing else may
    // take the step off the actor who won it. The dispatcher nominates
    // with a bare `PUT {assignee_id}`: when a claim landed between its
    // read and its write the version compare refused it (STEP_CHANGED),
    // JetStream redelivered, and the redelivered PUT read the fresh
    // row — active, held by the claimant — passed the compare it had
    // just read for itself, and wrote the dispatcher's pick over the
    // claimant about a second after the claim. So the boundary is here,
    // for every caller, as the human-only one below is: a PUT naming a
    // DIFFERENT holder of an ACTIVE, held step is refused, naming the
    // holder, in the terminal freeze's shape (`step_status` +
    // `refused_fields`) so the dispatcher reads both refusals as
    // "nothing to hold".
    //
    // What still moves: a re-send of the stored holder is not a change;
    // a READY step's nomination is not a claim, so it can be moved; and
    // a RELEASE (`assignee_id: null`, the abandoned-step reclaim's
    // body) frees the step — freeing it and then claiming it through
    // the CAS is how an active step changes hands, two writes, each on
    // the record. Exact spelling: an alias of the holder is refused
    // too, which is harmless, since the claim door already rewrote the
    // holder to the registered id (d7fef617).
    //
    // AND A CLEAR IS A RELEASE ONLY WITH THE STATUS BESIDE IT (backlog
    // 0f42efa0, the review of car fb3e9424). A bare `{assignee_id:
    // null}` — or `""`, which this guard read as nobody — answered 204
    // and left the step Active with no holder, so the next PUT naming
    // anyone met no holder to keep and installed them: this refusal's
    // defect in two writes. A null/blank holder now passes only when the
    // same body moves the step out of Active. The rule and the refusal
    // body live in `crate::active_holder`, which the dispatcher's test
    // reads as well.
    if crate::active_holder::refuses(
        old.status,
        old.assignee_id.as_deref(),
        step.status,
        step.assignee_id.as_deref(),
    ) {
        return (
            StatusCode::CONFLICT,
            Json(crate::active_holder::refusal_body(
                &step_id.to_string(),
                old.assignee_id.as_deref().unwrap_or_default(),
            )),
        )
            .into_response();
    }

    // A HUMAN-ONLY STEP REFUSES A NON-HUMAN ASSIGNEE (c17871fe). Checked
    // when the assignee actually changes — an idempotent re-send of the
    // same assignee is not an assignment — against the employee
    // registry, because the actor model cannot tell a login from a
    // person and the roster can. The dispatcher no longer nominates
    // such a step at all; this is the boundary for every other caller.
    if crate::human_only::declared(&step.metadata)
        && let Some(assignee) = step.assignee_id.as_deref()
        && old.assignee_id.as_deref() != Some(assignee)
        && let Err(why) = crate::human_only::person_check(state.roster.as_deref(), assignee).await
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(crate::human_only::refusal_body(
                &step_id.to_string(),
                &step.title,
                step.metadata.get("authority_role").and_then(|v| v.as_str()),
                assignee,
                &why,
            )),
        )
            .into_response();
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
    // a vaguer one. This block covers the fields the row freezes that
    // were still being dropped in silence.
    //
    // AND WHAT WAS COMPLETED: title, holder and notes (backlog
    // 42e7c6b9, the review of car 52ad60e6). None of the three was
    // checked here or frozen at the row, so after a ticketed completion
    // a bare `PUT {"title": ...}` answered 204 and the completed
    // presence step's stored title changed: the ops runner fails closed
    // on its stamp, but the record then shows a passkey approval of a
    // title no passkey saw. A correction to a finished step is `boss
    // correct` (the hint below), never a rewrite of what was signed.
    if is_terminal {
        let mut frozen: Vec<&str> = Vec::new();
        if step.completed_on != old.completed_on {
            frozen.push("completed_on");
        }
        if step.metadata != old.metadata {
            frozen.push("metadata");
        }
        if step.title != old.title {
            frozen.push("title");
        }
        if step.assignee_id != old.assignee_id {
            frozen.push("assignee_id");
        }
        if step.notes != old.notes {
            frozen.push("notes");
        }
        if !frozen.is_empty() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "step is terminal — these fields are immutable",
                    "step_id": step_id.to_string(),
                    "step_status": status_word(old.status),
                    "refused_fields": frozen,
                    // Names the corrections door (design 4105b020):
                    // this hint was the only guidance a correcting
                    // author got, and pointing at free-form job
                    // metadata is where 25 invented key names came from.
                    "hint": crate::corrections::TERMINAL_STEP_HINT,
                })),
            )
                .into_response();
        }

        // AN UNCHANGED RE-SEND OF A TERMINAL STEP WRITES NOTHING (backlog
        // 29a7ea09). The freeze above lets it through so a racing writer
        // stays harmless — and it was not: it still rewrote the step row,
        // re-ran the re-evaluator and ran the all-steps-terminal catch-all
        // close below. Measured on car 6b23d135, 2026-09-24T22:18:10Z: a
        // direct PUT completed the `disproved` terminal and its close
        // stamped `outcome=disproved` (.556307); the dispatcher's
        // complete-marker-on-step-ready re-sent the completion (.576623, a
        // bare STEP_UPDATED — the step was already completed), and its
        // catch-all, having read the Job before that close committed,
        // wrote the whole row back closed with no outcome (.586476). A
        // write that changes nothing cannot have made a packet closable,
        // so it has nothing to close: answer 204 and touch nothing.
        // (A writer that DOES change a step can still race another
        // close; both step-driven closes go through `close_job_at`, which
        // merges only the keys a close owns into a row still open, and
        // the job PUT's hand close lands only on a row still in the
        // status it read — the rest of 29a7ea09.)
        if step == old {
            return StatusCode::NO_CONTENT.into_response();
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

    // THE ASSURANCE GUARD, on the path that completes almost every step
    // in the system (backlog 148549c5). It used to live only in the
    // sign-off endpoint, which made the control OPT-IN — a step reaches
    // that door only when it also declares a required sign-off role.
    // Measured live on packet d5efbb3c: the first presence-assured step
    // was completed by a status flip, with `sign_offs: []` and no
    // ceremony, and the host then ran the verb it was gating.
    //
    // SCOPED TO LEAVING THE OPEN STATES, not to every write. A metadata
    // write to a step that stays ready needs no ceremony — the plan is
    // put onto an approve step that way before anyone signs it, and
    // refusing that would make a guarded step unusable rather than
    // guarded. A SKIP counts: `ready_when` predicates read
    // `steps.x.done`, which a skipped step satisfies, so skipping is
    // completing by another name.
    let is_leaving_open = !matches!(old.status, StepStatus::Completed | StepStatus::Skipped)
        && matches!(step.status, StepStatus::Completed | StepStatus::Skipped);
    // A HUMAN-ONLY STEP IS COMPLETED BY A PERSON (backlog adac8fa4). The
    // assignment and claim checks above guard who may HOLD the step; an
    // unheld one could still be flipped by any caller the policy lets
    // write steps, and on the in-memory API an agent's bare
    // `{"status":"completed"}` answered 204 and stamped the agent as
    // `completed_by`. Every completion path in the estate — the UI, the
    // merge door followed by this PUT, `boss step complete`, a
    // dispatcher handler — lands here, so this is the one boundary.
    //
    // Judged on the actor that SIGNED the write, not on the one the
    // event will name: an automation's body `completed_by` proxy (the
    // sim's attribution, below) names a person who did not make this
    // call, and the declaration's whole claim is that a person did.
    // Read from `old.metadata`, the protocol's materialised row, never
    // from the body (the change refusal above keeps them equal). The
    // same scope as the assurance guard: a skip satisfies `steps.x.done`
    // exactly as a completion does.
    if is_leaving_open
        && crate::human_only::declared(&old.metadata)
        && let Err(why) = crate::human_only::person_check(state.roster.as_deref(), &user.id).await
    {
        return (
            StatusCode::FORBIDDEN,
            Json(crate::human_only::completion_refusal_body(
                &step_id.to_string(),
                &old.title,
                old.metadata.get("authority_role").and_then(|v| v.as_str()),
                &user.id,
                &why,
            )),
        )
            .into_response();
    }
    // ...AND ITS RECORD IS WRITTEN BY A PERSON (backlog 50f012ed): a PUT
    // that stops short of completing must not carry the person's fields
    // past the check above either. The merge door's rule, on the same
    // stored row; context for the person stays writable. A status-only
    // or assignment PUT changes no key and asks nothing of the roster.
    if !is_terminal && crate::human_only::declared(&old.metadata) {
        let refused =
            crate::human_only::record_keys_changed(&old.metadata, &step.metadata, &old.fields);
        if !refused.is_empty()
            && let Err(why) =
                crate::human_only::person_check(state.roster.as_deref(), &user.id).await
        {
            return (
                StatusCode::FORBIDDEN,
                Json(crate::human_only::write_refusal_body(
                    &step_id.to_string(),
                    &old.title,
                    &format!("PUT /api/jobs/{job_id}/steps/{step_id}"),
                    &user.id,
                    &why,
                    &refused,
                )),
            )
                .into_response();
        }
    }
    if is_leaving_open {
        let floor = state
            .step_registry
            .get(&old.kind)
            .map(|t| t.assurance_floor)
            .unwrap_or_default();
        let key = super::presence::key_for(state.presence_key.as_deref(), &headers).await;
        let assured = judge_assurance(floor, &old, &step_id_str, &user.id, &headers, key);
        if assured.falls_short() {
            return assured.refusal();
        }
        // THE CONTENT JUDGED IS THE CONTENT COMPLETED (backlog c0b56fd9,
        // review of car 5b30ccf9). A ticket binds the step's shape hash,
        // and the judgement above reads `old` — so a PUT that changed
        // the title or metadata in the same write completed bytes no
        // passkey ever saw: measured 204, stored plan replaced, on a
        // presence step without sign-off roles. The PUT that leaves the
        // open states therefore changes nothing the hash covers; the
        // content goes through the merge door first, and the ceremony
        // is then run over it. The status-only completion the surfaces
        // send (car 5b30ccf9) and a read-merge-write re-send of the
        // stored keys change nothing, and pass.
        //
        // AND IT IS NOT SKIPPED. A skip satisfies `steps.<slug>.done`
        // as a completion does, but the sign-off contract and the
        // required-at-done fields below are judged on `completed` only.
        // The ticket carries no verb, so no ticket can mean "skip"
        // rather than "approve" — the safe skip is none. Declining is
        // leaving the step open or cancelling the packet.
        //
        // After the judgement, so a caller carrying no presence still
        // gets the 422 that tells it to run the ceremony (the contract
        // an_assurance_holds_on_every_path pins); this refusal speaks
        // only to one that did.
        //
        // AND NOTHING BESIDE IT (backlog 42e7c6b9, the review of car
        // 52ad60e6). The hash covers title and metadata only, so the
        // completing PUT could still carry `notes` and `assignee_id`:
        // 204, and the step finished holding words and a holder no
        // passkey saw. Those two ride the same refusal, named in
        // `refused_fields`. (Who dates the completion is f3e78bdf.)
        if assured.required >= boss_core::job::Assurance::Presence {
            let judged = boss_core::job::step_shape_hash(&old.title, &old.metadata);
            let completing = boss_core::job::step_shape_hash(&step.title, &step.metadata);
            let mut moved: Vec<&str> = Vec::new();
            if judged != completing {
                if step.title != old.title {
                    moved.push("title");
                }
                if step.metadata != old.metadata || moved.is_empty() {
                    moved.push("metadata");
                }
            }
            if step.notes != old.notes {
                moved.push("notes");
            }
            if step.assignee_id != old.assignee_id {
                moved.push("assignee_id");
            }
            let skipping = step.status == StepStatus::Skipped;
            if !moved.is_empty() || skipping {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": if skipping {
                            "a presence-assured step is completed, never skipped"
                        } else {
                            "a presence-assured step completes the content its ceremony saw \
                             — this PUT also changes its title, metadata, notes or holder"
                        },
                        "refused_fields": moved,
                        "step_id": step_id.to_string(),
                        "merge_door": format!("/api/jobs/{job_id}/steps/{step_id}/metadata"),
                        "hint": PRESENCE_CONTENT_HINT,
                    })),
                )
                    .into_response();
            }
        }
    }

    if is_flipping_to_done && step.completed_on.is_none() {
        step.completed_on = Some(boss_clock_client::now_from(&state.clock).await.date_naive());
    }

    // A STAMP DIES WHEN THE SHAPE IT SIGNED LEAVES THE STEP (design
    // 87329a13, backlog c085256d). This write's content is final for the
    // hash from here on — the server keys stamped below it (the decision
    // record, `aborted_from`) come after, and are not edits — so if it
    // moves the shape, every stamp still alive dies now: on this copy,
    // which the write persists and the STEP_UPDATED below carries, and on
    // the record through the invalidation event listing them, recorded in
    // the same transaction. Before the sign-off contract, so a write that
    // both moves the content and completes is judged on the dead stamps.
    // Nothing is written if this PUT is refused, and a refused PUT moves
    // no shape either.
    let void_event_id = uuid::Uuid::new_v4();
    let voided = step.void_stamps_if_moved(
        &old.shape_hash(),
        boss_clock_client::wall_now(),
        void_event_id,
    );

    // Sign-off contract: a step completes only when every required role
    // holds a LIVE stamp — never voided, on the current shape
    // (`Step::live_stamps`, the one rule).
    if is_flipping_to_done && !step.sign_offs_satisfied() {
        let missing = step.roles_without_a_live_stamp();
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
            .validate_metadata(&old.kind, &step.metadata)
            .and_then(|()| {
                // Inline authoring: the completion contract is
                // the union of the kind bundle's fields and the step's
                // own authored fields.
                crate::step_registry::StepRegistry::validate_authored_fields(
                    &old.fields,
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

    // THE STANDING REFUSALS, ON A PUT THAT DOES NOT COMPLETE (backlog
    // 73ef81fa, the gap car 1 left). The merge door judges a repeated
    // anchor, a value over an inline bound, a binding to an anchor the
    // step lacks and two of a one-of as the write lands; completion
    // judges them again above. A PUT carrying metadata WITHOUT completing
    // was judged by neither, so sending the whole bag here walked round
    // the door. Judged for the keys this write CHANGES — a PUT must
    // resend every stored key (the omission refusal above), so "sent"
    // would be every field, and a reviewer's resolutions save would be
    // refused over an exhibit an author wrote before these rules existed.
    if step.status != StepStatus::Completed && body_obj.contains_key("metadata") {
        let changed = |k: &str| step.metadata.get(k) != old.metadata.get(k);
        // Against the fields the step STANDS with, as the merge door
        // judges — a body that also rewrote `fields` does not get to
        // choose the contract its own metadata is judged by.
        let refusals = crate::step_registry::StepRegistry::standing_refusals(
            &old.fields,
            &step.metadata,
            changed,
        );
        if !refusals.is_empty() {
            let msg = refusals
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid step metadata: {msg}"),
            )
                .into_response();
        }
    }

    // Blocker gate (invariant I-4 — preconditions enforced). When the
    // caller is flipping this step to `done`, every step in
    // `blocked_by` must already be in a terminal state. Otherwise the
    // machine is firing a transition whose upstream data dependencies
    // aren't satisfied.
    //
    // AND WHEN A WRITER OPENS A PENDING STEP BY HAND (backlog 36352452).
    // This comment used to say moving a step to `active` was fine with
    // open blockers — a tech starting prep work before a sign-off lands
    // — and the gate fired only at `done`. That stopped being safe when
    // the gate learned to trust a step stored Ready or Active as one the
    // ENGINE opened (below): a hand PUT of a Pending terminal to `ready`
    // or `active`, then a bare completion, walked past the gate, and the
    // review of car 9392b8b5 closed a packet `done` whose work was never
    // done. So the promotion out of Pending is judged by this same gate,
    // which makes a Ready or Active step one that was opened either by
    // the engine or past its blockers — both of which the trust below is
    // entitled to. The engine's own promotion never comes through here.
    //
    // Judged rather than refused outright: the web's Start and Save
    // surfaces and four step plugins (checklist, review-design,
    // sr-triage, diagnostic-call) move a Pending step to `active` by
    // PUT, and a refusal of every one would lose those writes — two of
    // the plugins do not read the answer. A step with nothing blocking
    // it still starts; one with open blockers is refused naming them.
    // (What this gate reads is the edge list, not the predicate; the
    // predicate is read by the gate after this one, 570e72bd, so a
    // Pending step whose blockers are resolved but whose `ready_when`
    // also waits on job metadata is refused there.)
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
    let is_opening_by_hand = old.status == StepStatus::Pending
        && matches!(step.status, StepStatus::Ready | StepStatus::Active);

    // AN ABORT COMPLETES FROM ANY OPEN STATE (fd0f92ae, 2026-09-15).
    //
    // The gate keeps steps in order, and order is the right thing to
    // enforce on every step whose meaning is "the work up to here is
    // done". It is the wrong thing to enforce on exactly one: a
    // terminal whose materialised `outcome_kind` is `aborted`, whose
    // meaning is "stop here, wherever here is". Abandonment is not
    // forward progress, and a blocker is a data dependency of the
    // forward path — an abort has none.
    //
    // Measured when this was written: all 37 aborted terminals in
    // infra/platform/workflows/*.toml carry a `ready_when` that
    // references an upstream step (`steps.scope.done AND
    // job.metadata.abandoned = "true"`), so every one of them is
    // Pending with a live blocker for most of its Job's life, and the
    // gate below answered 409 to the abort the accepted design
    // (c6f9fb3e) allows whenever the Job is open. The control was
    // enabled exactly where the server refused.
    //
    // Read from `old.metadata` — the protocol's materialised row —
    // and never from the merged body, so a caller cannot claim
    // `aborted` on the way in to slip past the gate. "Open" is the
    // same set `close_job_on_terminal` will act on, so an abort that
    // passes here is one the close can honour. Everything else about
    // the completion is unchanged: the required-at-done fields
    // (`reason`) were validated above, the actor cleared policy at
    // the top, the frozen-terminal refusals still apply, and the
    // completion is evented as any other. The terminal's `ready_when`
    // stays as data: it still describes the machine's own path to it.
    let abort_from_any_state = is_flipping_to_done
        && old.metadata.get("outcome_kind").and_then(|v| v.as_str()) == Some("aborted")
        && parent_job.as_ref().is_some_and(|j| {
            !matches!(
                j.status,
                JobStatus::Closed | JobStatus::Cancelled | JobStatus::Draft
            )
        });
    if abort_from_any_state {
        tracing::info!(
            job_id = %job_id,
            step_id = %step_id,
            from = status_word(old.status),
            "abort-from-any-state: aborted terminal completing past its blockers",
        );
    } else if (is_flipping_to_done || is_opening_by_hand)
        && !old.blocked_by.is_empty()
        // A STEP THE ENGINE HAS ALREADY OPENED IS NOT BLOCKED.
        //
        // `blocked_by` is a predicate-derived denormalised edge list
        // FOR DAG RENDERING (CLAUDE.md §Steps), and it cannot express a
        // disjunction: it lists every step the predicate REFERENCES,
        // not the ones its truth actually required. So a `ready_when`
        // of the shape `A OR B` lists both, and completing the step
        // while only A holds was refused as "unresolved blockers".
        //
        // Measured 2026-09-22: the ops-request protocol gated `execute`
        // on `steps.filed.done AND (NOT job.metadata.requires_approval
        // OR steps.approve.done)`. With the flag absent the engine made
        // execute READY on the `NOT` arm, and this guard refused the
        // runner's completion over the pending approve step — 16
        // requests jammed on the forge, the oldest 68 minutes,
        // including a converge; `PUT failed … 409` once a minute for
        // over an hour, answered=0.
        //
        // TWO READINGS OF ONE EDGE, AND THE PREDICATE IS THE AUTHORITY.
        // The engine evaluates it; this list only draws it. Where they
        // disagreed the drawing was winning.
        //
        // THE GUARD KEEPS ITS REAL JOB, which is a step completed OUT
        // OF ORDER — one the engine has never opened. That is still
        // refused below, and `a_pending_step_with_an_unresolved_blocker
        // _is_still_refused` holds it.
        && !matches!(old.status, StepStatus::Ready | StepStatus::Active)
    {
        match state.jobs.resolve_blockers(&old.blocked_by).await {
            Ok(statuses) => {
                // Missing blockers (returned-length < asked-length) are
                // treated as unresolved — a step we can't find is
                // definitely not terminal.
                let resolved_by_id: std::collections::HashMap<_, _> =
                    statuses.into_iter().collect();
                let unresolved: Vec<String> = old
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

    // THE STEP'S OWN PREDICATE DECIDES, NOT ONLY ITS EDGE LIST (backlog
    // 570e72bd, road 4). The gate above reads `blocked_by`, which is
    // drawn FROM `ready_when` and cannot carry a `job.metadata` clause:
    // ship-a-change's `merged` terminal waits on `steps.review.done AND
    // job.metadata.merged = "true"`, and with review done a hand PUT
    // completed it — closing the packet `merged` — with no marker. So a
    // step the engine has not opened is opened or completed by hand
    // only where its own `ready_when`, read against the packet as
    // stored, holds: exactly the predicate the engine opens it by. A
    // step the engine opened (Ready / Active) keeps the trust the gate
    // above gives it, and the abort keeps its exemption.
    //
    // Every in-tree closer of a marker-gated terminal writes the marker
    // FIRST — the conductor's `merged`, `boss car retire` / `boss
    // disprove`, the sweep judge's `action_needed` — and both metadata
    // doors re-evaluate, so the terminal is Ready by the completion.
    let is_skipping_by_hand = !is_terminal && step.status == StepStatus::Skipped;
    let judged_by_its_predicate = (is_flipping_to_done || is_opening_by_hand)
        && old.status == StepStatus::Pending
        && !abort_from_any_state;
    if judged_by_its_predicate || is_skipping_by_hand {
        let reading = match protocol_reading(&state, parent_job.as_ref(), &old).await {
            Ok(reading) => reading,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("reading the step's protocol failed: {e}"),
                )
                    .into_response();
            }
        };
        match reading {
            // A HAND SKIP OF A PROTOCOL STEP (road 1). Skipped reads as
            // resolved to the blocker gate, and a skip passes no gate of
            // its own — no blockers, no required-at-done fields — so
            // `PUT work {status: skipped}` and a completion of the
            // terminal behind it closed a packet `merged` with its
            // work, its review and its marker all absent (measured on
            // the in-memory router, 2026-09-25). Skipped is the
            // protocol's word: the engine writes it for a step whose
            // predicate can no longer hold, and the terminal close for
            // what is left. Callers measured before refusing: nothing
            // in the tree — CLI, dispatcher, conductor, web, step
            // plugins, sim — PUTs a step to skipped; the abort is a
            // COMPLETION of the aborted terminal, untouched here. A
            // step no protocol describes (a kind with none) still
            // skips by hand, as before.
            ProtocolReading::Holds { .. } | ProtocolReading::Waits { .. }
                if is_skipping_by_hand =>
            {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "a protocol step is skipped by its protocol, not by hand",
                        "step_id": step_id.to_string(),
                        "step_status": status_word(old.status),
                        "hint": crate::job_outcome::HAND_SKIP_HINT,
                    })),
                )
                    .into_response();
            }
            ProtocolReading::Waits { ready_when } => {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "step's ready_when does not hold",
                        "step_id": step_id.to_string(),
                        "ready_when": ready_when,
                        "hint": crate::job_outcome::READY_WHEN_HINT,
                    })),
                )
                    .into_response();
            }
            ProtocolReading::Holds { .. } | ProtocolReading::Unpaired => {}
        }
    }

    // HOW an answer-question was decided (c17871fe): at the flip the
    // server compares the answer to the packet's `proposed` text and
    // stamps `accepted_as_proposed`; off the flip the stored value
    // rides through and a body's own claim is dropped. After the
    // shape-hash reads above so the stamp neither stales a sign-off
    // nor counts as an edit; the required-at-done validation already
    // passed on the same metadata.
    if step.kind == crate::decision_record::KIND {
        let proposed = parent_job
            .as_ref()
            .and_then(|j| j.metadata.get("proposed"))
            .and_then(|v| v.as_str());
        crate::decision_record::stamp(
            &mut step.metadata,
            &old.metadata,
            is_flipping_to_done,
            proposed,
        );
    }

    // WHERE THE ABORT FIRED FROM (fd0f92ae). Closure says every Job
    // reaches a terminal; provenance says the record shows how. An
    // abort past open blockers is the one completion that skips
    // work, so the terminal names the steps that were still open when
    // it fired — the same set `close_job_on_terminal` is about to mark
    // Skipped, each with its own STEP_UPDATED. Nothing is skipped
    // silently: the step.done marker carries this list in `metadata`,
    // and the row keeps it. Slugs in sort order, so the same log
    // replays to the same list. After the shape-hash read for the
    // same reason the decision-record stamp is.
    if abort_from_any_state
        && let Ok(siblings) = state.jobs.list_steps(&job_id).await
        && let Some(obj) = step.metadata.as_object_mut()
    {
        let mut open: Vec<&Step> = siblings
            .iter()
            .filter(|s| {
                s.id != step_id
                    && matches!(
                        s.status,
                        StepStatus::Pending | StepStatus::Ready | StepStatus::Active
                    )
            })
            .collect();
        open.sort_by_key(|s| s.sort_order);
        let slugs: Vec<serde_json::Value> = open
            .into_iter()
            .map(|s| {
                serde_json::Value::String(s.spec_slug.clone().unwrap_or_else(|| s.title.clone()))
            })
            .collect();
        obj.insert("aborted_from".into(), serde_json::Value::Array(slugs));
    }

    // Calendar reservation hook — runs BEFORE the persistence write so
    // a hard-conflict 409 doesn't leave the step in the new in-progress
    // state without a reservation. The same function the claim door
    // calls (design 611fbffd); its outcome is settled after the write.
    let hold = match start_hold(&state, &old, &step, &user.id).await {
        Ok(hold) => hold,
        Err(refused) => return refused,
    };

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

    // A COMPLETION NAMES ITS ACTOR (c17871fe). The step carries the
    // same actor the event is signed with, and the same instant — so
    // the row and the log agree, and a reader of the step does not
    // have to go to the log to learn who flipped it. Stamped at the
    // flip only; the carry-forward above keeps a finished step's
    // stamps through every later write, and the adapters freeze them
    // with the row.
    if is_flipping_to_done {
        step.completed_by = Some(actor.clone());
        step.completed_at = Some(now);
    }

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
        stamp = stamp.with_partition(j.partition);
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

        // Dispatcher routing: rules in infra/dispatcher/rules/
        // listen on `step.done.<kind>` so each StepType's side
        // effects can be declared as a rule without a giant `match`
        // in the subscriber. Payload mirrors the simulator's
        // in-process SimEventBus shape so handlers don't fork by
        // source — subject_kind / subject_id come from the parent
        // Job so every handler has the Subject identity without an
        // extra fetch. (Read before the write; the step update
        // doesn't touch the job row.)
        if !step.kind.is_empty() {
            let (subject_kind, subject_id, workflow_kind, job_owner_id) =
                if let Some(job) = &parent_job {
                    (
                        boss_core::primitives::Subject::kind(&job.subject).to_string(),
                        boss_core::primitives::Subject::id(&job.subject).to_string(),
                        job.kind.clone(),
                        job.owner_id.clone(),
                    )
                } else {
                    (String::new(), String::new(), String::new(), String::new())
                };
            step_events.push(stamp.event(
                &format!("step.done.{}", step.kind),
                serde_json::json!({
                    "job_id": job_id.to_string(),
                    "step_id": step_id.to_string(),
                    "kind": step.kind,
                    "subject_kind": subject_kind,
                    "subject_id": subject_id,
                    // The parent job's kind, always present ("" when the
                    // job could not be read, like the subject fields): a
                    // `spec_slug` repeats across workflows, and the
                    // ledger's projection `when` picks ONE workflow's step
                    // out of `step.done.task` by this (backlog a40541cb).
                    "workflow_kind": workflow_kind,
                    // The parent job's owner ("" when unread): the
                    // person a `notify_on_done` step's wait-is-over
                    // signal reaches when the step names no role. The
                    // step's own audience says who EXECUTES it — for a
                    // pr-train that is the conductor since v2 — and the
                    // one waiting on the packet is its owner. Without
                    // this, v2 silenced every train's done: signal for
                    // a week (backlog 58f0b536).
                    "job_owner_id": job_owner_id,
                    "completed_on": step.completed_on,
                    "metadata": step.metadata,
                    // `notify_on_done` and `spec_slug` are BOTH hoisted
                    // to the payload root, and BOTH always present, for
                    // one reason: the dispatcher expr binder resolves
                    // flat top-level identifiers only, and an absent
                    // identifier is a PredicateFailed → Retry →
                    // dead-letter storm, not a quiet false.
                    //
                    // `notify_on_done` defaults false; the
                    // notify-on-step-done-marked rule (migration 106)
                    // matches it.
                    //
                    // `spec_slug` is the step's stable slug, defaulting
                    // to "" when the step has none (an ad-hoc step, or
                    // one materialized before the column existed). It is
                    // WHICH step of a kind completed: every step of a
                    // pr-train is `kind = "task"`, so `step.done.task`
                    // alone cannot tell `merged` from `deployed` from
                    // `ci`, and a rule that must — `spec_slug ==
                    // "merged"` — reads it here rather than digging into
                    // the nested `metadata` the binder cannot reach.
                    "notify_on_done": step
                        .metadata
                        .get("notify_on_done")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    "spec_slug": step.spec_slug.clone().unwrap_or_default(),
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

    // The stamps this write killed, recorded with it — after its
    // STEP_UPDATED, which already carries them dead, so a replay lands
    // the row and then applies the void to it (a no-op by then).
    if let Some(event) = events::stamps_invalidated(&stamp, void_event_id, &step, &voided) {
        step_events.push(event);
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

    // WRITTEN ONLY OVER THE ROW IT WAS COMPUTED FROM (backlog e381689d).
    // Everything above judged, and every event above describes, `old`
    // overlaid with the body — so if another writer moved the row's
    // metadata since `old` was read, writing this copy would erase that
    // write while both answered success. A status-only PUT is exactly
    // that shape: the dispatcher's assignment PUT erased a merged
    // `prompt_bytes` 2 ms after the merge door stored it (run 6b6fe011,
    // 2026-09-25). Refused by name instead; a re-send reads the row
    // afresh and lands. Reproduced live on run 9aa88562 at 11:26:01Z the
    // same day, same 2 ms gap: it is the ordinary timing of a dispatch,
    // whose merge lands while the dispatcher is assigning the new run's
    // ready step, not a one-off.
    //
    // Judged on the row's VERSION, not its metadata (backlog 6ec22d71):
    // a claim moves status and holder and no metadata, and this write
    // computed before it wrote `ready` and its own holder back over it.
    let written = state
        .jobs
        .update_step_if_unchanged_at(&step, old_version, stamp.timestamp, &step_events)
        .await;
    // Landed over the very row `old` was read as, so `step` is what is
    // stored — or refused, and nothing was written, which must be true
    // of the calendar too (558396ff). `settle_start_hold` says how.
    settle_start_hold(&state, &stamp, hold, written.is_ok(), &old, &step, &user.id).await;
    match written {
        Ok(()) => {}
        Err(crate::port::JobsError::StepChanged { .. }) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": crate::step_metadata_write::STEP_CHANGED_ERROR,
                    "step_id": step_id.to_string(),
                    "hint": "nothing was written; send the same request again — the \
                             handler reads the row afresh",
                })),
            )
                .into_response();
        }
        // A terminal row this handler read afresh, and a write that
        // would move a column it freezes past the checks above (the
        // authored `fields`, most plausibly): the row would keep its
        // value while the event recorded the write's, so the adapter
        // refuses and this says so rather than asking for a re-send
        // that would be refused the same way.
        Err(crate::port::JobsError::TerminalStep { status, .. }) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": format!(
                        "step is {status}: what a finished step recorded does not move"
                    ),
                    "step_id": step_id.to_string(),
                    "step_status": status,
                    "hint": crate::corrections::TERMINAL_STEP_HINT,
                })),
            )
                .into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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
        // contract) — the STORED index, never the body's (b433bdf3).
        // Resolve the pinned version — same rule as the re-evaluator.
        let just_completed =
            old.status != StepStatus::Completed && step.status == StepStatus::Completed;
        if just_completed
            && let Ok(spec) = reg.get_version(&job.kind, job.workflow_version).await
            && let Some(outcome) = spec.terminal_outcome_at(old.sort_order)
            && let Err(e) = close_job_on_terminal(&state, &job_id, outcome, &actor, now).await
        {
            return close_not_written(&job_id, &e);
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
                stamp_close_instant(&mut job, &job_now);
                // A catch-all close that FOLLOWS a completed declared
                // terminal names that terminal's outcome. Two completers
                // of one marker (a verb's PUT and the dispatcher's
                // `complete-marker-on-step-ready`) race here: the second
                // reads the terminal completed and the rest skipped while
                // the first's close is not yet written, closes the Job
                // itself, and its whole-row write landed last — so car
                // 6b23d135 closed through `disproved` with no outcome
                // (228c9a7d). Deriving it here makes both writers say
                // the same thing, whichever lands last.
                if let Some(reg) = &state.kind_registry
                    && let Ok(spec) = reg.get_version(&job.kind, job.workflow_version).await
                    && let Some(outcome) = spec.completed_terminal_outcome(
                        steps
                            .iter()
                            .map(|s| (s.sort_order, s.status == StepStatus::Completed)),
                    )
                    && let serde_json::Value::Object(map) = &mut job.metadata
                {
                    map.insert("outcome".to_string(), outcome.into());
                }
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
                .with_partition(job.partition);
            // `compute_job_status` answers only Open or Closed, and an
            // Open Job is the only one that reaches here, so this
            // transition is always a close — and it goes through the
            // close door, never a whole-row write (backlog 29a7ea09:
            // on car 6b23d135 this site wrote back a copy read before
            // the terminal close committed, and erased its outcome).
            // The adapter builds the state event from the post-close
            // row; these markers are built from that row too.
            let markers = |job: &Job| {
                vec![
                    close_stamp.event(
                        events::JOB_STATUS_CHANGED,
                        serde_json::json!({
                            "id": job.id.to_string(),
                            "old_status": old_status,
                            "new_status": new_status,
                        }),
                    ),
                    close_stamp.event(
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
                    ),
                ]
            };
            // A close that loses the compare-and-set (another closer
            // got there first) writes nothing and is not a failure; a
            // close the store REFUSED is, and it is answered, never
            // dropped: this site discarded it with `let _ =` and said
            // 204 over a packet left open with every step terminal.
            let closed_on = job.closed_on.unwrap_or_else(|| now.date_naive());
            if let Err(e) = state
                .jobs
                .close_job_at(
                    &job_id,
                    closed_on,
                    &close_owned_fields(&job),
                    &close_stamp,
                    &markers,
                )
                .await
            {
                return close_not_written(&job_id, &e);
            }
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

/// The answer to a step write whose close did not land: a 500 NAMING
/// THE PACKET and saying which half committed. The step row is already
/// written, so the caller must not read this as "nothing happened" —
/// nor, as the 204 it replaced let them, as "the packet closed"
/// (backlog 29a7ea09).
fn close_not_written(job_id: &boss_core::job::JobId, e: &crate::port::JobsError) -> Response {
    tracing::error!(job_id = %job_id, error = %e, "step written, but its job close failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("the step was written, but closing job {job_id} failed: {e}"),
    )
        .into_response()
}

/// `PATCH /api/jobs/{id}/steps/{step_id}/metadata` — merge top-level
/// metadata keys into the Step, atomically, server-side. The step-side
/// twin of `PATCH /api/jobs/{id}/metadata`, and the same contract: the
/// body is a JSON object of top-level keys, a `null` value REMOVES the
/// key, every other value replaces that key wholesale. Status,
/// assignee, and every other step field are untouchable through this
/// route — a `status` key in the body is just a metadata key named
/// "status", exactly as it is on the job merge.
///
/// WHY IT EXISTS: the same lost-update race the job merge retired.
/// With only the overlay PUT, every caller that wanted to set one
/// metadata key ran GET → spread → PUT client-side, and PUT metadata
/// is replaced wholesale — so a concurrent writer's keys were erased
/// by whichever write landed second. The merge now happens inside one
/// adapter transaction against the row as it stands.
///
/// A terminal step is refused with the PUT's own 409 shape (job
/// 903e6b90: the caller is TOLD, never 204'd into believing a frozen
/// write landed), and the hint names the doors that work, because a
/// completed step is a record of what happened: the corrections door
/// for a correction, the job metadata merge for an annotation. A patch
/// that CHANGES NOTHING is the one exception, answered 204 like the
/// PUT's idempotent re-send: this door said "nothing redelivers through
/// this route" until e39a9d2a moved the gate verdict, auto-park, boss
/// park, prove and design onto it, each of which re-sends on a retry
/// ([`crate::step_metadata_write::patch_is_noop`]).
///
/// Policy: the same coarse `(Update, step)` gate as the step PUT.
pub(super) async fn patch_step_metadata<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path((id, step_id_str)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(patch): Json<serde_json::Value>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    let step_id = match parse_step_id(&step_id_str) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid step id").into_response(),
    };
    let serde_json::Value::Object(mut patch) = patch else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "metadata patch must be a JSON object of top-level keys",
        )
            .into_response();
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
            return e.into_response();
        }
    }

    let old = match state.jobs.get_step(&step_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, "step not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Same containment rule as the claim route: a step addressed
    // through a job it does not belong to is not found THERE.
    if old.job_id != job_id {
        return (StatusCode::NOT_FOUND, "step not on this job").into_response();
    }

    // `authority_role` is immutable across writes, same rule as the
    // PUT: the persisted value wins, so a body can neither raise nor
    // lower the required sign-off authority — nor shed it with null.
    patch.remove("authority_role");

    // THE STANDING REFUSALS, AS THE WRITE LANDS (design 26a89f11,
    // exhibits). A repeated anchor, a value over the field's inline
    // bound, or a binding to an anchor the step does not carry is what a
    // record may never say, so it is refused here, where the writer is on
    // the line — not at done, where it would land on the reviewer. Judged
    // against the row AS IT WOULD STAND (the patch overlaid, null
    // removing), and only for the fields this write touches or that bind
    // one it touches. A step whose fields declare none of these answers
    // exactly as before.
    let merged_view = {
        let mut md = old.metadata.as_object().cloned().unwrap_or_default();
        for (k, v) in &patch {
            if v.is_null() {
                md.remove(k);
            } else {
                md.insert(k.clone(), v.clone());
            }
        }
        serde_json::Value::Object(md)
    };
    // `human_only` is the protocol's, same rule as the PUT (adac8fa4):
    // deleting it here with `null`, or flipping it, and then PUTting the
    // status alone was the second road round the completion check.
    if crate::human_only::declaration_changed(&old.metadata, &merged_view) {
        return (
            StatusCode::FORBIDDEN,
            Json(crate::human_only::change_refusal_body(
                &step_id.to_string(),
                &old.title,
                &old.metadata,
            )),
        )
            .into_response();
    }
    // THE PERSON'S RECORD (backlog 50f012ed). Completions write their
    // fields here and then PUT the status (e39a9d2a), and the PUT's
    // completion check refuses only the flip — so an agent completing a
    // person's step landed its fields and was refused the status, and
    // the record kept a write the step reserves for a person. The same
    // person check as the PUT, on the actor that SIGNED this write, over
    // every key it would change that is not context for the person
    // (`human_only::record_keys_changed` holds the rule and its why).
    // Open steps only: a terminal row is the adapter's refusal below.
    if !matches!(old.status, StepStatus::Completed | StepStatus::Skipped)
        && crate::human_only::declared(&old.metadata)
    {
        let refused =
            crate::human_only::record_keys_changed(&old.metadata, &merged_view, &old.fields);
        if !refused.is_empty()
            && let Err(why) =
                crate::human_only::person_check(state.roster.as_deref(), &user.id).await
        {
            return (
                StatusCode::FORBIDDEN,
                Json(crate::human_only::write_refusal_body(
                    &step_id.to_string(),
                    &old.title,
                    &format!("PATCH /api/jobs/{job_id}/steps/{step_id}/metadata"),
                    &user.id,
                    &why,
                    &refused,
                )),
            )
                .into_response();
        }
    }
    // `outcome_kind` too (b433bdf3): the PUT's abort exemption reads the
    // stored value, so a merge of `aborted` onto an ordinary terminal
    // followed by a bare completing PUT walked past the blocker gate.
    // Refused rather than stripped like `authority_role`, as
    // `human_only` is, so the caller is told; an unchanged re-send is
    // not a change and lands. On a terminal row the adapter's refusal
    // below speaks instead, as it does for every other key.
    let protocol_keys =
        crate::step_metadata_write::protocol_keys_changed(&old.metadata, &merged_view);
    if !protocol_keys.is_empty()
        && !matches!(old.status, StepStatus::Completed | StepStatus::Skipped)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "metadata patch changes a key the protocol owns",
                "step_id": step_id.to_string(),
                "refused_keys": protocol_keys,
                "hint": crate::step_metadata_write::PROTOCOL_KEYS_HINT,
            })),
        )
            .into_response();
    }
    let refusals =
        crate::step_registry::StepRegistry::standing_refusals(&old.fields, &merged_view, |k| {
            patch.contains_key(k)
        });
    if !refusals.is_empty() {
        let msg = refusals
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid step metadata: {msg}"),
        )
            .into_response();
    }

    // The parent packet: the event stamp inherits its admission-fixed
    // partition, and the re-evaluator runs against it.
    let parent_job = state.jobs.get_job(&job_id).await.ok().flatten();

    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let mut stamp = state.publisher.stamp_with_actor(actor.clone()).await;
    if let Some(j) = &parent_job {
        stamp = stamp.with_partition(j.partition);
    }
    let stamp = stamp;

    // A merge that moves the shape voids the stamps it leaves behind
    // INSIDE the adapter's transaction, judged against the row under its
    // lock, and records the invalidation event with the row (design
    // 87329a13). It was judged here, against `old` — a read taken before
    // the write — and recorded best-effort in a transaction of its own:
    // a void the log could lose, which the rebuild now depends on.
    match state
        .jobs
        .merge_step_metadata_at(&step_id, &patch, &stamp)
        .await
    {
        Ok(_) => {}
        // The adapter's row-riding check wins over our `old` fetch —
        // it saw the step at write time — so both the pre-known and
        // the raced terminal case land here, in the PUT's 409 shape.
        Err(crate::port::JobsError::TerminalStep { status, .. }) => {
            // THE IDEMPOTENT RE-SEND (e39a9d2a). A patch that would
            // leave the terminal row exactly as it is changes nothing,
            // and is answered as the no-op it is — the PUT's freeze has
            // the same carve-out, and the writers moved from that PUT
            // onto this door (the gate verdict, auto-park, boss park,
            // prove, design) re-send on a redelivery or a retry. Judged
            // against a FRESH read, not `old`: in the raced case the
            // step went terminal after `old` was taken, and what the
            // patch must match is the row as it now stands.
            if let Ok(Some(now)) = state.jobs.get_step(&step_id).await
                && crate::step_metadata_write::patch_is_noop(&now.metadata, &patch)
            {
                return StatusCode::NO_CONTENT.into_response();
            }
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "step is terminal — these fields are immutable",
                    "step_id": step_id.to_string(),
                    "step_status": status,
                    "refused_fields": ["metadata"],
                    "hint": crate::corrections::TERMINAL_STEP_HINT,
                })),
            )
                .into_response();
        }
        Err(crate::port::JobsError::StepNotFound(_)) => {
            return (StatusCode::NOT_FOUND, "step not found").into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }

    // Same wake as the job metadata patch: a step-metadata write can
    // flip a metadata-gated `ready_when` (`steps.<slug>.metadata.<field>`
    // is in the predicate language — the backlog-item disposition
    // terminals are built on it). Without this, a fact recorded
    // through the merge would sit invisible to readiness until some
    // unrelated status write happened by. Closed/cancelled Jobs stay
    // untouched.
    if let Some(job) = &parent_job
        && job.status == JobStatus::Open
    {
        reevaluate_and_persist(&state, job, &actor).await;
    }

    StatusCode::NO_CONTENT.into_response()
}

/// `POST /api/jobs/{id}/steps/{step_id}/corrections` — append a
/// correction beside a completed or skipped step (design 4105b020,
/// backlog 56727f95). The ONLY writer of the job's reserved
/// `corrections` list; the rules are `crate::corrections`'.
///
/// Body: `{field, reads, should_read, why}`, or `{withdraws, why}` to
/// withdraw an earlier entry of this step by appending. 201 with
/// `{index, correction}`. Refuses: an open step (409 — it is still
/// editable), and with 422 a missing key, a `field` the step does not
/// hold, a `reads` excerpt not in that field's stored text, and a
/// withdrawal that names no live entry of this step. The step itself
/// and every event it produced are untouched; the entry is signed with
/// the caller and stamped with the write's time.
///
/// Policy: the job metadata merge's gate — `(Update, job)` plus the
/// scope check — because the write lands in the job's metadata.
pub(super) async fn post_step_correction<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path((id, step_id_str)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<crate::corrections::CorrectionRequest>,
) -> Response {
    let job_id = match super::jobs::resolve_path_job_id(&state, &id).await {
        Ok(job_id) => job_id,
        Err(refusal) => return refusal,
    };
    let step_id = match parse_step_id(&step_id_str) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid step id").into_response(),
    };
    let job = match state.jobs.get_job(&job_id).await {
        Ok(Some(j)) => j,
        Ok(None) => return (StatusCode::NOT_FOUND, "job not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let scope = match state
        .policy
        .check(&user, Action::Update, Resource::job())
        .await
    {
        Ok(Decision::Deny { reason }) => return (StatusCode::FORBIDDEN, reason).into_response(),
        Ok(Decision::Allow { scope }) => scope,
        Err(e) => {
            return e.into_response();
        }
    };
    if !scope_matches(&user, &scope, &job) {
        return (StatusCode::FORBIDDEN, "job is outside your scope").into_response();
    }
    let step = match state.jobs.get_step(&step_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, "step not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // Same containment rule as the claim and merge routes.
    if step.job_id != job_id {
        return (StatusCode::NOT_FOUND, "step not on this job").into_response();
    }

    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = state
        .publisher
        .stamp_with_actor(actor.clone())
        .await
        .with_partition(job.partition);
    let step_id_text = step_id.to_string();
    let entry = match crate::corrections::entry_for(
        crate::corrections::Target {
            step_id: &step_id_text,
            terminal: matches!(step.status, StepStatus::Completed | StepStatus::Skipped),
            status: status_word(step.status),
            metadata: &step.metadata,
        },
        crate::corrections::list(&job.metadata),
        &req,
        &actor.to_string(),
        &stamp.timestamp.to_rfc3339(),
    ) {
        Ok(entry) => entry,
        Err(refusal) => {
            let code = if refusal.is_conflict() {
                StatusCode::CONFLICT
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            return (
                code,
                Json(serde_json::json!({
                    "error": refusal.message(),
                    "step_id": step_id_text,
                    "step_status": status_word(step.status),
                })),
            )
                .into_response();
        }
    };

    match state
        .jobs
        .append_step_correction_at(&job_id, &entry, &stamp)
        .await
    {
        Ok((_, index)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "index": index, "correction": entry })),
        )
            .into_response(),
        Err(crate::port::JobsError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, "job not found").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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
    /// The holder a claim installs when it is not the caller (design
    /// 611fbffd, Q1 answered 2026-09-26). Only the executor the step's
    /// own audience names, or a holder of the `step-assign` authority,
    /// may name someone else; the event is signed by the caller and the
    /// assignment marker records both. Absent, blank, or the caller's
    /// own id: an ordinary claim for oneself.
    claimed_for: Option<String>,
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
            return e.into_response();
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

    // WHO WILL HOLD IT (design 611fbffd, Q1 answered 2026-09-26). The
    // claimant, unless the claim names someone else — which only the
    // executor the step's own audience declares (the automation a
    // Workflow row names, e.g. `automation:boss-step`) or a holder of
    // the `step-assign` authority (platform-admin today) may do. The
    // step PUT still installs a holder on a Ready step; this door is
    // where that act is judged and recorded, so it is the one the
    // surfaces' Start is moving to.
    let nominee = crate::active_holder::nominee(q.claimed_for.as_deref(), &user.id);
    if let Some(nominee) = nominee {
        // A station's capability judges the CLAIMANT's roles; a claim
        // for someone else would pass or fail on the wrong actor's.
        if let Some(station) = q.station.as_deref() {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "a claim for someone else names no station — a station's \
                              capability judges the claimant, not the nominee",
                    "station": station,
                    "claimed_for": nominee,
                })),
            )
                .into_response();
        }
        let is_executor = crate::active_holder::declared_executor(&old.metadata).as_deref()
            == Some(user.id.as_str());
        if !is_executor {
            match state
                .policy
                .check(&user, Action::Update, Resource::step_assign())
                .await
            {
                Ok(Decision::Deny { .. }) => {
                    return (
                        StatusCode::FORBIDDEN,
                        Json(crate::active_holder::claim_for_refusal_body(
                            &step_id.to_string(),
                            &user.id,
                            nominee,
                        )),
                    )
                        .into_response();
                }
                Ok(_) => {}
                Err(e) => return e.into_response(),
            }
        }
    }
    let holder: String = nominee.unwrap_or(user.id.as_str()).to_string();

    // A HUMAN-ONLY STEP REFUSES A NON-HUMAN HOLDER (c17871fe) — the
    // same rule the PUT enforces, at the other door an actor takes a
    // step through. Judged on who will HOLD the step: the claimant, or
    // the one it is claimed for. Before the station gate and the CAS: a
    // claim that is not allowed must not enter the race.
    if crate::human_only::declared(&old.metadata)
        && let Err(why) = crate::human_only::person_check(state.roster.as_deref(), &holder).await
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(crate::human_only::refusal_body(
                &step_id.to_string(),
                &old.title,
                old.metadata.get("authority_role").and_then(|v| v.as_str()),
                &holder,
                &why,
            )),
        )
            .into_response();
    }

    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    // The parent packet: the claim's events inherit its
    // admission-fixed partition, and the assignment marker
    // reads its Subject identity.
    let parent_job = state.jobs.get_job(&job_id).await.ok().flatten();

    // The step's agent block as every gate below reads it: the packet's
    // own projection, else its kind's ACTIVE row (backlog 51aef4dd) —
    // the resolution the station queue made, so the door an agent
    // claims through agrees with the queue it read. Read only when the
    // step carries no projection; best-effort, since a registry that
    // cannot answer leaves the step as recorded, which is how every
    // claim was judged before. `old` itself stays as recorded: it is
    // what the CAS writes back, and the resolution is never written.
    let active_row = match (&state.kind_registry, &parent_job) {
        (Some(reg), Some(job)) if crate::agent_spec::projected(&old.metadata).is_none() => {
            reg.get_active(&job.kind).await.ok()
        }
        _ => None,
    };
    let resolved = crate::agent_spec::resolved(&old, active_row.as_ref());

    // The holder's agents row, read once for the two gates below: the
    // station's model capability and the budget reservation. The holder
    // is the claimant unless the claim names someone else — whose run
    // it then is, and whose budget and concurrency it spends. `None` is
    // a person or an unregistered login — neither gate applies — and a
    // registry that cannot answer is a 500, not a silent pass (no
    // evidence is not a pass).
    let agent_row = match state.agent_budget.as_ref() {
        Some(door) => match door.agent_row(&holder).await {
            Ok(row) => row,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("agents registry could not answer for {holder}: {e}"),
                )
                    .into_response();
            }
        },
        None => None,
    };

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
        // Authored row first, then the projection — the SAME resolver
        // the queue read uses (923b6571). A derived station was 404
        // here and a queue there, so every `(role, model)` agent inbox
        // rendered and could not be claimed from.
        let row = match super::stations::station_by_name(&state, reg, station_name).await {
            Ok(s) => s,
            Err(r) => return r,
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
        // A failed steps read is a 500 naming the packet, not an empty
        // list: empty cannot match a step clause, so the claim was
        // refused 409 "packet is not at this station" — a confident
        // wrong answer to a question the door never read (f6c97006).
        let steps = if needs_steps {
            let steps = match state.jobs.list_steps(&job_id).await {
                Ok(steps) => steps,
                Err(e) => return steps_unreadable(&job_id, &e),
            };
            crate::agent_spec::resolved_steps(&steps, active_row.as_ref())
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
        // THE ROLE HALF, OVER BOTH VOCABULARIES (backlog 4b103f0f).
        // A station's `capability.roles` is whatever the step's role
        // selector spelled, and that is two vocabularies: a PLATFORM
        // role (`platform-admin`, which every named CLI caller asserts
        // about itself) or a Class code under `(employee, role)`
        // (`engineering-agent`, which the agents registry holds and
        // the dispatcher's roster nominates on). Judging only
        // `user.role` read one of them, so an agent routed here by the
        // role it HOLDS was refused by the role it ASSERTS — and the
        // registry's `role` was read by nothing at the claim although
        // the migration that added it says a role audience resolves to
        // its holders, agents included (20260918022311). Both
        // spellings, in the order they are decided: the request's
        // first, the row's second.
        let held_roles: Vec<&str> = std::iter::once(user.role.as_str())
            .chain(agent_row.as_ref().and_then(|a| a.role.as_deref()))
            .collect();
        if let Some(capability) = &row.capability
            && !capability.admits_roles(&held_roles)
        {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "role not admitted by station capability",
                    "station": station_name,
                    "role": user.role,
                    // Every spelling the door compared, not only the
                    // asserted one: a verdict an operator has to go
                    // re-derive is not a verdict.
                    "actor_roles": held_roles,
                    "allowed_roles": capability.roles,
                })),
            )
                .into_response();
        }
        // The MODEL half of the capability (c87fb59b car 3): a
        // `(role, model)` station admits an agent that runs one of its
        // models; a person runs none and is gated by the roles above.
        if let Some(capability) = &row.capability
            && !capability.models.is_empty()
        {
            let models: Vec<String> = agent_row.iter().map(|a| a.default_model.clone()).collect();
            if !capability.allows_model(&models) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": "model not admitted by station capability",
                        "station": station_name,
                        "actor": user.id,
                        "models": models,
                        "allowed_models": capability.models,
                    })),
                )
                    .into_response();
            }
        }
    }

    // THE BUDGET GATE (design c87fb59b car 3, backlog cb78818d). A step
    // with an agent block names what one run of it may spend
    // (`agent_budget_usd`, car 1's projection); an agent claiming it
    // reserves that much of its hour — the priced spend of its runs
    // finished in the last hour plus this budget must fit under the
    // row's `hourly_budget_usd_micros` — or is refused with the
    // numbers, BEFORE the CAS, so a claim the budget does not admit
    // never enters the race. A person, an unregistered login, and a
    // row with no cap reserve nothing (see `agent_budget`).
    //
    // A READING, NOT A REFUSAL, since backlog e6b2066f. Once a run is
    // priced from what it consumed — about five times the figure this
    // gate was calibrated against — the $40 hour would have refused
    // claims all day, and David's direction (2026-09-23) is that
    // budgets give protocols a cost signal and do not limit building.
    // So an over-cap reservation admits the claim and puts the reading
    // on the log beside it (`agents.claim.over_budget`, committed with
    // the claim), and a failed read of the hour is logged and admits
    // too: a gate that no longer refuses must not refuse on a hiccup.
    let mut over_budget: Option<serde_json::Value> = None;
    if let (Some(door), Some(row), Some(budget_usd)) = (
        state.agent_budget.as_ref(),
        agent_row.as_ref(),
        resolved
            .metadata
            .get(crate::agent_spec::BUDGET_KEY)
            .and_then(|v| v.as_f64()),
    ) {
        let now = boss_clock_client::now_from(&state.clock).await;
        match door.reserve(row, budget_usd, now).await {
            Ok(reservation) if reservation.decision.is_allowed() => {}
            Ok(reservation) => over_budget = Some(reservation.reading_body()),
            Err(e) => {
                tracing::warn!(
                    actor = %row.id,
                    "budget reading could not measure the actor's hour, claim admitted: {e}"
                );
            }
        }
    }

    // THE CONCURRENCY GATE (backlog 57c108c2, 2026-09-20). The
    // reservation above measures the hour from `agent_runs`, which is
    // written at FINISH — so a claimed-and-unreported run is priced at
    // nothing and the money gate admits the next claim, and the next.
    // A puller (`boss dispatch --next` on an interval) against the 47
    // ready packets measured at `a.platform-admin.opus-5-1m` would
    // have taken all 47 that way. This is the bound the money one
    // cannot be: the actor's OPEN `agent-run` packets against its
    // row's `max_concurrent_runs`. Under the cap nothing changes; a
    // row declaring no cap bounds nothing, like an undeclared budget.
    //
    // IT COUNTED CLAIMED STEPS UNTIL c314921e, AND THE PROXY DEADLOCKED
    // THE QUEUE. A backlog-item's `build` does not drain at the
    // handback — it drains when its car closes, and a `ship-a-change`
    // does not close until it is PROVEN in prod — so the bound measured
    // the proof backlog. Measured at the jam, 2026-09-20: 7 of 6 in
    // flight against an open-run population of ZERO, and one of the
    // seven was a car whose own probe wanted a fresh dispatch, so the
    // event that would have proven it was the event it forbade. The
    // run is what the cap is named after; it is now what the cap reads.
    // After the budget gate, because `BudgetDecision::decide` reports
    // money before concurrency and the two doors keep that order.
    if let Some(row) = agent_row.as_ref()
        && crate::agent_budget::declares_an_agent_run(&resolved.metadata)
        && let Some(cap) = row.max_concurrent_runs.and_then(|n| u32::try_from(n).ok())
    {
        // Every spelling of the actor: the registered id and the
        // aliases that resolve to it (design 6fda05ae). The CAS
        // rewrites an alias holder, but a step nominated before that
        // landed still carries one, and missing those under-refuses.
        let held_by: Vec<String> = std::iter::once(row.id.clone())
            .chain(row.aliases.iter().cloned())
            .collect();
        // One read of the population itself: the open `agent-run`
        // packets. A failed read REFUSES rather than admitting — an
        // uncounted bound is not a bound, and the money gate above
        // fails in the same direction.
        let filter = JobFilter {
            kind: Some(crate::agent_budget::RUN_KIND.to_string()),
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        let open_runs = match state.jobs.list_jobs(&filter, IN_FLIGHT_SCAN_LIMIT, 0).await {
            Ok((rows, _)) => rows,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "concurrency gate could not count {}'s runs in flight: {e}",
                        row.id
                    ),
                )
                    .into_response();
            }
        };
        let in_flight = crate::agent_budget::in_flight_runs(&open_runs, &held_by);
        let check = crate::agent_budget::Concurrency::measure(&row.id, Some(cap), in_flight);
        if !check.decision.is_allowed() {
            return (StatusCode::CONFLICT, Json(check.refusal_body())).into_response();
        }
    }

    let mut stamp = state.publisher.stamp_with_actor(actor).await;
    if let Some(j) = &parent_job {
        stamp = stamp.with_partition(j.partition);
    }
    let stamp = stamp;

    // Optimistic post-state for the events; the CAS makes it the
    // real post-state on success, and on conflict nothing records.
    let mut claimed = old.clone();
    claimed.assignee_id = Some(holder.clone());
    claimed.status = StepStatus::Active;
    // The CAS drops the previous run's edge when the holder changes
    // (9562f6df); the event says so too, or the log would go on naming
    // a run the row no longer names. The holder's aliases come from
    // its agents row — the same holder the CAS reads as `me`.
    let aliases = agent_row
        .as_ref()
        .map(|r| r.aliases.as_slice())
        .unwrap_or_default();
    if crate::agent_runs::claim_changes_holder(old.assignee_id.as_deref(), &holder, aliases) {
        claimed.metadata = crate::agent_runs::without_edge(&claimed.metadata);
    }

    let mut claim_events = vec![stamp.event(
        events::STEP_UPDATED,
        serde_json::to_value(&claimed).unwrap_or_default(),
    )];
    // The over-budget reading, on the log in the same commit as the
    // claim it describes (backlog e6b2066f): which step, which actor,
    // and every number the old refusal carried.
    if let Some(mut reading) = over_budget {
        if let Some(obj) = reading.as_object_mut() {
            obj.insert("job_id".into(), serde_json::json!(job_id.to_string()));
            obj.insert("step_id".into(), serde_json::json!(step_id.to_string()));
        }
        claim_events.push(stamp.event(crate::agent_budget::CLAIM_OVER_BUDGET, reading));
    }
    // An assignment marker only when the executor genuinely changed —
    // the same alias-aware answer the run edge took above, so a re-claim
    // and a respelled holder announce nothing (735ddc03). Payload
    // mirrors step.ready for messages.notify.
    //
    // AND ALWAYS FOR A CLAIM ON SOMEONE ELSE'S BEHALF (design 611fbffd):
    // the record names both — the event is signed by the caller, and
    // this marker carries `claimed_by` and `claimed_for` — even when the
    // nominee was already the step's nominated holder, because starting
    // someone's clock is an act of its own.
    if nominee.is_some()
        || crate::agent_runs::assignment_marker_due(
            old.assignee_id.as_deref(),
            &holder,
            aliases,
            &claimed.kind,
        )
    {
        let (subject_kind, subject_id) = if let Some(job) = &parent_job {
            (
                boss_core::primitives::Subject::kind(&job.subject).to_string(),
                boss_core::primitives::Subject::id(&job.subject).to_string(),
            )
        } else {
            (String::new(), String::new())
        };
        let mut marker = serde_json::json!({
            "job_id": job_id.to_string(),
            "step_id": step_id.to_string(),
            "kind": claimed.kind,
            "subject_kind": subject_kind,
            "subject_id": subject_id,
            "assignee_id": claimed.assignee_id,
            "metadata": claimed.metadata,
        });
        if let (Some(nominee), Some(obj)) = (nominee, marker.as_object_mut()) {
            obj.insert("claimed_by".into(), serde_json::json!(user.id));
            obj.insert("claimed_for".into(), serde_json::json!(nominee));
        }
        claim_events.push(stamp.event(&format!("step.assigned.{}", claimed.kind), marker));
    }

    // THE CLAIM DOOR RESERVES (design 611fbffd): the same start the step
    // PUT makes, through the same function, before the CAS — so moving a
    // Start here does not silently stop holding the holder's time. A
    // calendar conflict refuses the claim with nothing stored; a claim
    // the CAS then refuses hands its reservation back.
    let hold = match start_hold(&state, &old, &claimed, &user.id).await {
        Ok(hold) => hold,
        Err(refused) => return refused,
    };

    let claimed_row = state
        .jobs
        .claim_step_at(&step_id, &holder, &stamp, &claim_events)
        .await;
    let stored = claimed_row.as_ref().unwrap_or(&claimed);
    settle_start_hold(
        &state,
        &stamp,
        hold,
        claimed_row.is_ok(),
        &old,
        stored,
        &user.id,
    )
    .await;
    match claimed_row {
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
            return e.into_response();
        }
    };
    if let Decision::Deny { reason } = decision {
        return (StatusCode::FORBIDDEN, reason).into_response();
    }

    // ASSURANCE — the SAME judgement the ordinary step write makes, so
    // the answer cannot depend on which door the caller used (§9a,
    // backlog 148549c5).
    let floor = state
        .step_registry
        .get(&step.kind)
        .map(|t| t.assurance_floor)
        .unwrap_or_default();
    let key = super::presence::key_for(state.presence_key.as_deref(), &headers).await;
    let assured = judge_assurance(floor, &step, &step_id_str, &user.id, &headers, key);
    let produced = assured.produced;
    let presence_nonce = assured.presence_nonce.clone();
    if assured.falls_short() {
        return assured.refusal();
    }
    let shape = step.shape_hash();
    // Idempotent only over a LIVE stamp (`Step::live_stamps`, design
    // 87329a13): a dead stamp on this same shape — the content left and
    // came back — is not a signature of it, and answering "already
    // signed" would leave the approver no way to sign it at all.
    if step.live_stamps().any(|st| st.role == role) {
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
        voided_at: None,
        voided_by_event: None,
    };
    // OUTBOX (phase 2): the signed-off marker records in the SAME
    // transaction as the stamp append, after the STEP_UPDATED the
    // append builds from the row it wrote (backlog f146a13a).
    let actor = boss_core::actor::ActorId::human(&user.id);
    let mut event_stamp = state.publisher.stamp_with_actor(actor).await;
    // The signed-off marker inherits the packet's admission-fixed
    // flag, like every other event about the Job.
    if let Ok(Some(job)) = state.jobs.get_job(&job_id).await {
        event_stamp = event_stamp.with_partition(job.partition);
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
    match state
        .jobs
        .append_sign_off(&step_id, &stamp, &event_stamp, &[signed_off_event])
        .await
    {
        Ok(()) => {}
        // The step moved between this handler's read and the append's
        // lock (backlog 4174c4a9): the stamp signs a shape the row no
        // longer has, and nothing was written. The approver reads the
        // step again and signs what is there.
        Err(crate::port::JobsError::StampOffShape {
            signed, current, ..
        }) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "the step moved since it was read — sign it again as it stands",
                    "signed_shape": signed,
                    "current_shape": current,
                })),
            )
                .into_response();
        }
        // The ticket already stamped this step (backlog 3977b3d2): the
        // stamp it wrote was voided by an edit, and the content came
        // back to the shape the ticket was minted over. Presence is a
        // passkey touching THIS stamp, so the answer is a fresh
        // ceremony — `required` says so, the way a missing ticket does.
        Err(crate::port::JobsError::NonceSpent { .. }) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "this presence ticket has already stamped this step — sign again with a fresh passkey ceremony",
                    "required": boss_core::job::Assurance::Presence,
                    "produced": produced,
                })),
            )
                .into_response();
        }
        Err(crate::port::JobsError::StepNotFound(_)) => {
            return (StatusCode::NOT_FOUND, "no such step").into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
    step.sign_offs.push(stamp);
    Json(step).into_response()
}

/// Mark one still-open step `Skipped` for a terminal close, written only
/// over the row VERSION it was read at (backlogs e381689d, 6ec22d71). A
/// step another writer moved since the read — any column: a holder, a
/// note, metadata — is read again and skipped as it now stands, since a
/// skip decides nothing from the row's contents, so the fresh copy is
/// the same decision without erasing that write; and one that went
/// terminal in between needs no skip and records none, where the write
/// used to answer Ok over it while its `skipped` event was recorded
/// for a row that stayed completed. Bounded: a row that keeps moving
/// is left open and logged, which the catch-all close sees next pass.
async fn skip_open_step<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job_id: &boss_core::job::JobId,
    read: (Step, crate::port::StepVersion),
    terminal_stamp: &boss_core::publisher::EventStamp,
) {
    const ATTEMPTS: usize = 3;
    let (mut s, mut version) = read;
    for _ in 0..ATTEMPTS {
        if !matches!(
            s.status,
            StepStatus::Pending | StepStatus::Ready | StepStatus::Active
        ) {
            return;
        }
        s.status = StepStatus::Skipped;
        // OUTBOX (phase 2): the skip's state event records in the SAME
        // transaction as the row.
        let skip_event = terminal_stamp.event(events::STEP_UPDATED, events::step_state_payload(&s));
        match state
            .jobs
            .update_step_if_unchanged_at(&s, version, terminal_stamp.timestamp, &[skip_event])
            .await
        {
            Ok(()) => return,
            Err(crate::port::JobsError::StepChanged { .. }) => {
                match state.jobs.get_step_versioned(&s.id).await {
                    Ok(Some(fresh)) => (s, version) = fresh,
                    _ => break,
                }
            }
            Err(e) => {
                tracing::warn!(
                    job_id = %job_id,
                    step_id = %s.id,
                    error = %e,
                    "terminal close: failed to skip non-terminal step",
                );
                return;
            }
        }
    }
    tracing::warn!(
        job_id = %job_id,
        step_id = %s.id,
        "terminal close: step kept changing under the skip — left open",
    );
}

/// Close a Job because a *declared terminal* step reached
/// `Completed`. Mirrors the `compute_job_status`-driven close (same
/// JOB_UPDATED / JOB_STATUS_CHANGED / JOB_CLOSED events, same
/// closed_on anchoring) but is outcome-aware: it stamps
/// `metadata.outcome` and marks every still-non-terminal step
/// (`Pending` / `Ready` / `Active`) `Skipped` so the closed Job has no
/// dangling open work. No-ops if the Job is already terminal
/// (Cancelled / Draft) or already Closed.
///
/// The close itself is [`JobsRepository::close_job_at`]: it writes only
/// the fields a close owns and only while the row is still open
/// (backlog 29a7ea09), and its failure is returned rather than logged,
/// because the step write that called this has already committed and
/// its caller would otherwise be told the completion landed whole.
/// Record a hold a step lost, when the calendar hook reports one
/// (backlog 4bdb8150, the round-3 review of car 983696b5). The step
/// write already landed or was already refused, so there is no answer
/// left to change — the event is the record, stamped by this write
/// (its actor, instant and partition), and the dispatcher rule
/// `open-a-packet-when-a-step-loses-its-hold` turns it into a packet a
/// person reads. Until this, the loss was a `tracing::warn` and nothing
/// else. Any other outcome records nothing.
///
/// Best-effort, like the hook it reports on: a failure to record is
/// logged at ERROR, because it is the loss of the only record of a loss.
/// What a step's start did on the calendar before its write: the
/// reservation it made, which a refused write hands back by id
/// (backlog 558396ff), or a hold it found already standing, which is
/// not this write's and is re-asserted if the write lands (the round-2
/// review of car 983696b5).
#[derive(Default)]
struct StartHold {
    reserved: Option<boss_core::calendar::ReservationId>,
    took_held: bool,
}

/// THE CALENDAR HALF OF A STEP'S START, before its write — the ONE
/// function both doors call (design 611fbffd, answered 2026-09-26). It
/// ran only inside `update_step`, so a Start moved to the claim door
/// would silently have stopped reserving the holder's time; the pin
/// `both_doors_start_a_step_through_one_function` holds both doors to
/// it. `new` is the row the write would store (its holder is whose
/// time is held); `actor` is recorded as the reservation's creator.
///
/// A hard conflict refuses the start with nothing stored — the 409 is
/// the `Err`. A no-op when no calendar is configured, the transition is
/// not a start, or the step lacks a complete schedule; a calendar that
/// errors is logged and the start proceeds, as it always has.
async fn start_hold<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    old: &Step,
    new: &Step,
    actor: &str,
) -> Result<StartHold, Response> {
    match crate::calendar_hook::apply_step_transition(state.calendar.as_ref(), old, new, actor)
        .await
    {
        Ok(crate::calendar_hook::HookOutcome::Reserved(id)) => Ok(StartHold {
            reserved: Some(id),
            took_held: false,
        }),
        Ok(crate::calendar_hook::HookOutcome::AlreadyHeld) => Ok(StartHold {
            reserved: None,
            took_held: true,
        }),
        Ok(crate::calendar_hook::HookOutcome::Conflict { existing_rows }) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "calendar conflict",
                "step_id": new.id.to_string(),
                "existing": existing_rows,
            })),
        )
            .into_response()),
        Ok(_) => Ok(StartHold::default()),
        Err(e) => {
            tracing::warn!(error = %e, "calendar hook errored; proceeding with step write");
            Ok(StartHold::default())
        }
    }
}

/// The calendar half of a step write, AFTER it — `landed` says whether
/// the write stored `attempted` (both doors write only over the row as
/// they read it, so a write that landed stored exactly that).
///
/// REFUSED: "nothing was written" must be true of the calendar too, so
/// the reservation this attempt made is handed back — that one and no
/// other, and not even that one when the start that refused this write
/// is Active on it; the row as stored decides (558396ff, 983696b5).
///
/// LANDED: a start that landed on a hold it did not place re-asserts
/// it (the racer that placed it may be refused and hand it back), and a
/// skip releases the step's hold only now. A lost hold either way is
/// recorded (`jobs.step.hold_lost`, 4bdb8150).
async fn settle_start_hold<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    stamp: &boss_core::publisher::EventStamp,
    hold: StartHold,
    landed: bool,
    old: &Step,
    attempted: &Step,
    actor: &str,
) {
    if !landed {
        if let Some(id) = hold.reserved {
            let jobs = &state.jobs;
            let sid = attempted.id;
            let reheld = crate::calendar_hook::release_after_refused_write(
                state.calendar.as_ref(),
                id,
                attempted,
                actor,
                move || async move { jobs.get_step(&sid).await.ok().flatten() },
            )
            .await;
            record_lost_hold(state, stamp, reheld).await;
        }
        return;
    }
    if hold.took_held {
        let reheld =
            crate::calendar_hook::hold_for_landed_start(state.calendar.as_ref(), attempted, actor)
                .await;
        record_lost_hold(state, stamp, reheld).await;
    }
    crate::calendar_hook::after_step_written(state.calendar.as_ref(), old, attempted, actor).await;
}

async fn record_lost_hold<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    stamp: &boss_core::publisher::EventStamp,
    outcome: crate::calendar_hook::HookOutcome,
) {
    let crate::calendar_hook::HookOutcome::HoldLost(lost) = outcome else {
        return;
    };
    let event = stamp.event(
        events::STEP_HOLD_LOST,
        events::step_hold_lost_payload(&lost),
    );
    if let Err(e) = state.jobs.record_events(std::slice::from_ref(&event)).await {
        tracing::error!(
            error = %e,
            step_id = %lost.step_id,
            "calendar: a step lost its hold and the event recording it was not written"
        );
    }
}

async fn close_job_on_terminal<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job_id: &boss_core::job::JobId,
    outcome: &str,
    actor: &boss_core::actor::ActorId,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), crate::port::JobsError> {
    let Some(mut job) = state.jobs.get_job(job_id).await? else {
        return Ok(());
    };
    if matches!(
        job.status,
        JobStatus::Closed | JobStatus::Cancelled | JobStatus::Draft
    ) {
        // Already terminal / not-yet-open — nothing to close.
        return Ok(());
    }

    let terminal_stamp = state
        .publisher
        .stamp_with_actor(actor.clone())
        .await
        .with_partition(job.partition);

    // Skip every still-non-terminal step. The Job is closing on its
    // terminal outcome; any Pending/Ready/Active step is now moot.
    if let Ok(steps) = state.jobs.list_steps_versioned(job_id).await {
        for read in steps {
            skip_open_step(state, job_id, read, &terminal_stamp).await;
        }
    }

    let old_status = job.status;
    job.status = JobStatus::Closed;
    job.closed_on = Some(now.date_naive());
    // Stamp the terminal outcome onto the Job metadata so projections
    // / the SPA can render *why* the Job closed — and the precise
    // close instant beside the one-day-resolution `closed_on`
    // (`stamp_close_instant`, the one write shared by every close).
    if let serde_json::Value::Object(map) = &mut job.metadata {
        map.insert(
            "outcome".to_string(),
            serde_json::Value::String(outcome.to_string()),
        );
    } else {
        job.metadata = serde_json::json!({ "outcome": outcome });
    }
    stamp_close_instant(&mut job, &now);

    // OUTBOX (phase 2): the close's state event + markers record in
    // the SAME transaction as the row — the adapter builds the state
    // event from the post-close row, and these markers from it too.
    let markers = |job: &Job| {
        vec![
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
        ]
    };
    let closed_on = job.closed_on.unwrap_or_else(|| now.date_naive());
    state
        .jobs
        .close_job_at(
            job_id,
            closed_on,
            &close_owned_fields(&job),
            &terminal_stamp,
            &markers,
        )
        .await
        .map(|_| ())
}

/// The metadata keys a close OWNS, read off the closer's own copy of
/// the Job after it stamped them: the close instant (`closed_at`, from
/// [`stamp_close_instant`]) and the outcome, when the copy carries one.
/// Only these merge into the row ([`JobsRepository::close_job_at`]);
/// every other key stays as the row holds it at write time, so a key
/// another writer merged after this closer read the Job survives the
/// close (backlog 29a7ea09).
fn close_owned_fields(job: &Job) -> serde_json::Map<String, serde_json::Value> {
    ["closed_at", "outcome"]
        .into_iter()
        .filter_map(|key| {
            job.metadata
                .get(key)
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect()
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
            let stamp = state
                .publisher
                .stamp_with_actor(actor.clone())
                .await
                .with_partition(job.partition);
            // A pass whose promotion was refused because another writer
            // moved that row since the list read (backlog 6ec22d71) is
            // run again over the rows as they now stand: the promotion
            // is re-judged there — it may still hold, or the other
            // writer may have finished the step — rather than left to
            // a later pass that a claim, a merge or a sign-off never
            // runs. Promotions that landed are not repeated: the next
            // read shows them done. Bounded, and a pass still refused
            // at the bound is logged.
            const PASSES: usize = 3;
            for _ in 0..PASSES {
                if !reevaluate_pass(state, job, actor, &spec, &stamp).await {
                    return;
                }
            }
            tracing::warn!(
                job_id = %job.id,
                "re-eval: steps kept changing under the promotion — left for the next pass",
            );
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

/// One re-evaluation over the steps as read now: persist every
/// promotion, each only over the row version the read saw. Answers
/// whether a promotion was REFUSED because its row moved since the read
/// ([`crate::port::JobsError::StepChanged`]) — the caller's cue to run
/// another pass over the rows as they now stand (backlog 6ec22d71).
/// Every other failure is logged and not retried, as before.
async fn reevaluate_pass<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job: &Job,
    actor: &boss_core::actor::ActorId,
    spec: &crate::registry::WorkflowSpec,
    stamp: &boss_core::publisher::EventStamp,
) -> bool {
    let Ok(read) = state.jobs.list_steps_versioned(&job.id).await else {
        return false;
    };
    let (mut steps, versions): (Vec<Step>, Vec<crate::port::StepVersion>) =
        read.into_iter().unzip();
    // `reevaluate` requires steps in spec order (sort_order == index);
    // the list read returns them sorted by sort_order, so the invariant
    // holds. Invariant (expose, don't swallow): a Job's live step set
    // must match its active Workflow spec, or `reevaluate`'s
    // length-guard bails and the Job can no longer advance. With atomic
    // materialization this only fires on a genuine mid-flight republish
    // that changed the step count. Surface it loudly instead of
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
    let changed = crate::registry::reevaluate(spec, &mut steps, &job.subject, &job.metadata);
    let mut refused = false;
    for idx in changed {
        let changed_step = &steps[idx];
        // OUTBOX (phase 2): the promoted step's state event + D6 ready
        // marker (when it lands in `Ready` — lets dispatcher rules react
        // to a step *becoming eligible*, the delegate-subjob spawn fork
        // D7) record in the SAME transaction as the promotion.
        let mut reeval_events = vec![stamp.event(
            events::STEP_UPDATED,
            events::step_state_payload(changed_step),
        )];
        if changed_step.status == StepStatus::Ready && !changed_step.kind.is_empty() {
            reeval_events.push(build_step_ready_event(state, job, changed_step, actor).await);
        }
        // Judged against the row version the list read saw (backlogs
        // e381689d, 6ec22d71): a promotion computed before another
        // writer's claim, assignment or skip would otherwise write the
        // stale copy's holder back — or, over a row that went terminal,
        // record a `jobs.step.updated` the row refused.
        match state
            .jobs
            .update_step_if_unchanged_at(
                changed_step,
                versions[idx],
                stamp.timestamp,
                &reeval_events,
            )
            .await
        {
            Ok(()) => {}
            Err(crate::port::JobsError::StepChanged { .. }) => refused = true,
            Err(e) => {
                tracing::warn!(
                    job_id = %job.id,
                    step_id = %changed_step.id,
                    error = %e,
                    "re-eval: failed to persist promoted step",
                );
            }
        }
    }
    refused
}

/// What the packet's PINNED protocol says about `step`, read against the
/// packet as stored ([`crate::registry::read_step`]). `Unpaired` when
/// there is no protocol to read — no registry plumbed, no parent packet,
/// a kind the registry has no spec for — so such a step is judged
/// exactly as before (backlog 570e72bd). A failed read is an error, not
/// an `Unpaired`: a gate that cannot read its protocol does not open —
/// and that includes the packet itself, which `update_step` refuses to
/// go on without rather than handing this `None` (backlog 5186c5e1).
async fn protocol_reading<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job: Option<&Job>,
    step: &Step,
) -> Result<ProtocolReading, String> {
    let (Some(reg), Some(job)) = (&state.kind_registry, job) else {
        return Ok(ProtocolReading::Unpaired);
    };
    let spec = match reg.get_version(&job.kind, job.workflow_version).await {
        Ok(spec) => spec,
        Err(crate::registry::WorkflowError::NotFound(_)) => return Ok(ProtocolReading::Unpaired),
        Err(e) => return Err(e.to_string()),
    };
    let steps = state
        .jobs
        .list_steps(&job.id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(crate::registry::read_step(
        &spec,
        &steps,
        &job.subject,
        &job.metadata,
        &step.id,
    ))
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
        .with_partition(job.partition);
    stamp.event(
        &format!("step.ready.{}", step.kind),
        serde_json::json!({
            "job_id": step.job_id.to_string(),
            "step_id": step.id.to_string(),
            "kind": step.kind,
            "subject_kind": subject_kind,
            "subject_id": subject_id,
            // `workflow_kind` and `spec_slug` are hoisted to the
            // payload root, and BOTH always present, exactly as
            // `step.done.<kind>` hoists them and for the same reason:
            // the dispatcher expr binder resolves flat top-level
            // identifiers only, and an absent identifier is a
            // PredicateFailed → Retry → dead-letter storm, not a quiet
            // false.
            //
            // They are WHICH packet and WHICH step of it. The shared
            // `step.ready.task` topic carries every task step on the
            // board, so without these a "when this kind's step X
            // becomes ready" rule had no `when` it could write and had
            // to fetch the Job in a handler — which is what
            // `ops.file_tag_release` and `maintenance.chore.file_reds`
            // were written to do (backlog 4d53fae2, left by the
            // builder of 89c95245).
            //
            // `workflow_kind` is "" only when the parent Job could not
            // be read, like the subject fields above; `spec_slug` is
            // "" for a step that has none (ad-hoc, or materialized
            // before the column existed).
            "workflow_kind": job.kind,
            "spec_slug": step.spec_slug.clone().unwrap_or_default(),
            // A step assigned BEFORE it became ready notifies its
            // assignee, not the role's on-call member — the handler
            // prefers a named assignee when the payload carries one.
            "assignee_id": step.assignee_id,
            "metadata": step.metadata,
        }),
    )
}
