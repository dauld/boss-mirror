//! NATS event subjects for the jobs domain.
//!
//! Two layers:
//!
//! - **State events** carry full row state and are what the
//!   audit_log → projection rebuild path consumes. Every Job or
//!   Step mutation emits exactly one: `JOB_CREATED`, `JOB_UPDATED`,
//!   `STEP_CREATED`, `STEP_UPDATED`. The payload is a serialized
//!   `Job` / `Step` — the rebuilder reproduces the projection by
//!   replaying these in audit_log id order.
//! - **Marker events** are informational signals for downstream
//!   consumers (dispatcher rule registry, UI badges, integrations).
//!   They duplicate state already in the sibling state event but
//!   give consumers a topic to filter on without payload matching.
//!   Rebuilder ignores them.

// State events — projections rebuild from these.
pub const JOB_CREATED: &str = "jobs.job.created";
pub const JOB_UPDATED: &str = "jobs.job.updated";
pub const STEP_CREATED: &str = "jobs.step.created";
pub const STEP_UPDATED: &str = "jobs.step.updated";
/// A STAMP DIED (design 87329a13, option C decided 2026-09-25): an edit
/// moved a stamped step's completion-relevant shape, and every stamp
/// still alive on it was voided — kept on the step, marked `voided_at` /
/// `voided_by_event`, never to count again. Payload `{job_id, step_id,
/// stale_roles, required_roles, voided}`, `voided` being the stamps as
/// voided. Recorded in the SAME transaction as the edit, and APPLIED by
/// the rebuild, which voids exactly the listed stamps — or, for an
/// event written before `voided` existed, every stamp alive on the row
/// at that point, which is what the edit that emitted it left stale.
/// It was a marker the rebuild ignored, and the merge door wrote it
/// best-effort in a transaction of its own, so nothing on the step said
/// a stamp had died and an A-B-A edit revived a withdrawn approval
/// (backlog c085256d).
pub const STEP_STAMPS_INVALIDATED: &str = "jobs.step.stamps_invalidated";
/// A WorkflowSpec version went live: an author's publish, a
/// `workflow-publish` Step's `publish_authored` dispatch, or a
/// bootstrap reconcile inserting/republishing a platform default.
/// Recorded by the registry adapter atomically with the workflows
/// row. Payload is the full published `WorkflowSpec` (with
/// `authoring_job_id` set to the meta-Job's id when a Job authored
/// it), matching what `rebuild_workflows` consumes to reconstruct
/// the projection.
pub const WORKFLOW_PUBLISHED: &str = "jobs.kind.published";
/// A draft row was appended to the registry (author saved, not live).
pub const WORKFLOW_DRAFT_SAVED: &str = "jobs.kind.draft_saved";
/// The active row of a kind was retired with no successor.
pub const WORKFLOW_RETIRED: &str = "jobs.kind.retired";
/// A draft row deleted before it ever went live (ebd7bb70): the
/// armed-draft hazard removed, with the discard on the record.
pub const WORKFLOW_DRAFT_DISCARDED: &str = "jobs.kind.draft_discarded";
/// A draft StepPlugin row was appended to the registry (author
/// saved, not live). Recorded by the registry adapter atomically
/// with the step_plugins row; payload is the full `StepPluginSpec`.
pub const STEP_PLUGIN_DRAFT_SAVED: &str = "jobs.step_plugin.draft_saved";
/// A StepPluginSpec version went live: the latest draft flipped to
/// active, retiring any prior active row. Payload is the promoted
/// `StepPluginSpec`.
pub const STEP_PLUGIN_PUBLISHED: &str = "jobs.step_plugin.published";
/// The active StepPlugin version of a kind was retired with no
/// successor. Payload is the retired `StepPluginSpec`.
pub const STEP_PLUGIN_RETIRED: &str = "jobs.step_plugin.retired";
/// A draft station row was appended to the registry (author saved,
/// not live). Recorded by the registry adapter atomically with the
/// stations row; payload is the full `StationSpec`.
pub const STATION_DRAFT_SAVED: &str = "jobs.station.draft_saved";
/// A `StationSpec` version went live: the latest draft flipped to
/// active, retiring any prior active row. Payload is the promoted
/// `StationSpec`.
pub const STATION_PUBLISHED: &str = "jobs.station.published";
/// The active station version of a name was retired with no
/// successor. Payload is the retired `StationSpec`.
pub const STATION_RETIRED: &str = "jobs.station.retired";
/// A cadence rule version went live — the platform bundle's seed
/// landing a declared row, or an operator's `POST
/// /api/cadence/rules/{name}/publish` (backlog 13d1fff3): any prior
/// active row of the name retired, the declared version inserted
/// active, in one transaction. Payload is the row written, as a
/// `CadenceRuleSpec`. Until 2026-09-18 the only writer of
/// `cadence_rules` was a migration and no event ever recorded a
/// schedule change; this is the log witnessing one.
pub const CADENCE_PUBLISHED: &str = "jobs.cadence.published";
/// The active cadence rule of a name was retired with no successor —
/// the schedule switched off by a verb (`boss cadence retire`).
/// Payload is the retired `CadenceRuleSpec`.
pub const CADENCE_RETIRED: &str = "jobs.cadence.retired";

// Marker events — informational only; rebuild ignores them.
pub const JOB_STATUS_CHANGED: &str = "jobs.job.status_changed";
pub const STEP_COMPLETED: &str = "jobs.step.completed";
pub const STEP_SIGNED_OFF: &str = "jobs.step.signed_off";
/// A correction was appended beside a completed or skipped step
/// (`corrections`, design 4105b020): payload `{job_id, step_id, index,
/// correction}`. The fact of the correction; the job's row state rides
/// the sibling JOB_UPDATED in the same transaction, which is what the
/// rebuild replays, so the rebuild ignores this marker.
pub const STEP_CORRECTED: &str = "jobs.step.corrected";
/// A packet was moved to another version of its protocol through the
/// re-pin door (`POST /api/jobs/{id}/convert`, design 7cf202a9 Q3):
/// payload `{job_id, from, to, by, at, reprojected: [{step, step_id,
/// changed, kept?}], inserted: [{step, step_id}]}` with the actor as
/// `_actor` — the same entry appended to the job's reserved `repins`
/// list. The fact of the move; the rows it changed ride the sibling
/// JOB_UPDATED / STEP_UPDATED / STEP_CREATED state events in the same
/// transaction, which is what the rebuild replays, so the rebuild
/// ignores this marker.
pub const JOB_REPINNED: &str = "jobs.job.repinned";
pub const JOB_CLOSED: &str = "jobs.job.closed";
/// A quarantine pass found an ACTIVE Workflow that fails the viability
/// lint and retired it. Boot no longer emits this: it checks and logs
/// but never retires (`workflow_quarantine`).
/// The sibling state event is the registry's own
/// `jobs.kind.retired`; this marker is the loud one — it carries the
/// problems that condemned the row, so the log answers "why is this
/// kind gone?" without a re-lint. Rebuild ignores it.
pub const WORKFLOW_QUARANTINED: &str = "jobs.kind.quarantined";
/// A quarantine pass found an ACTIVE station that fails the viability
/// lint and retired it. Boot no longer emits this: it checks and logs
/// but never retires (`station_quarantine`, 2026-09-08). Same contract
/// as [`WORKFLOW_QUARANTINED`]: the sibling state event is the
/// registry's own `jobs.station.retired`, and this marker is the loud
/// one carrying the problems that condemned the row. Declared in
/// migration 120; nothing in the tree emits it today. Rebuild ignores
/// it.
pub const STATION_QUARANTINED: &str = "jobs.station.quarantined";
/// One firing of the packet-loss census (packet-loss.md Q3): the
/// network's conservation counts as a measured series, one event per
/// firing. A marker in the strict sense — it duplicates nothing and
/// projects nothing; the payload IS the datum, and lenses read the
/// series from the log instead of recomputing it. Rebuild ignores it.
pub const NETWORK_CENSUS: &str = "jobs.network.census";
/// One observation of the estate: what machines were actually there
/// when someone looked. Paired with the `nodes` registry, which says
/// what we MEANT to have — the difference between the two is the
/// finding (59ef456a).
pub const ESTATE_OBSERVED: &str = "jobs.estate.observed";
/// One estate comparison: declared vs observed for one observation.
/// The compare handler computes it, the comparison door records it,
/// and neither writes the registry — the difference stays a finding,
/// never a silent correction (59ef456a).
pub const ESTATE_COMPARED: &str = "jobs.estate.compared";

/// The state-event payload for a Step: the serialized struct plus a
/// top-level `step_id` — the same key every marker event uses.
///
/// The struct's own identity key serializes as `id`, the markers say
/// `step_id`, and the intersection of the two identifier sets over
/// the whole audit_log was measured EMPTY
/// (requirements-based-addressing.md, Constraints) — every
/// queue-drain metric joining creation to completion silently read
/// zero rows. One payload key ends the schism going forward;
/// historical rows stay as they were written.
/// The [`STEP_STAMPS_INVALIDATED`] event for stamps already voided on
/// `step` by [`boss_core::job::Step::void_stamps_if_moved`] under
/// `event_id` — built with that id, so every voided stamp's
/// `voided_by_event` names the event that lists it. `None` when nothing
/// was voided: the event means "these stamps died", not "someone wrote".
pub fn stamps_invalidated(
    stamp: &boss_core::publisher::EventStamp,
    event_id: uuid::Uuid,
    step: &boss_core::job::Step,
    voided: &[boss_core::job::SignOffStamp],
) -> Option<boss_core::event::Event> {
    if voided.is_empty() {
        return None;
    }
    let mut stale_roles: Vec<&str> = Vec::new();
    for st in voided {
        if !stale_roles.contains(&st.role.as_str()) {
            stale_roles.push(&st.role);
        }
    }
    let mut event = stamp.event(
        STEP_STAMPS_INVALIDATED,
        serde_json::json!({
            "job_id": step.job_id.to_string(),
            "step_id": step.id.to_string(),
            "stale_roles": stale_roles,
            "required_roles": step.sign_offs_required,
            "voided": voided,
        }),
    );
    event.id = event_id;
    Some(event)
}

/// Void the stamps an edit left behind and build the event that records
/// it, in one call — for a write that has its [`EventStamp`] in hand
/// when it learns the shape moved (the merge door and the re-pin, both
/// inside their adapter's transaction). The void is dated with the
/// stamp's instant, the instant of the edit's own state event.
///
/// [`EventStamp`]: boss_core::publisher::EventStamp
pub fn void_stamps_if_moved(
    stamp: &boss_core::publisher::EventStamp,
    shape_before: &str,
    step: &mut boss_core::job::Step,
) -> Option<boss_core::event::Event> {
    let id = uuid::Uuid::new_v4();
    let voided = step.void_stamps_if_moved(shape_before, stamp.timestamp, id);
    stamps_invalidated(stamp, id, step, &voided)
}

pub fn step_state_payload(step: &boss_core::job::Step) -> serde_json::Value {
    let mut v = serde_json::to_value(step).unwrap_or_default();
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "step_id".to_string(),
            serde_json::Value::String(step.id.to_string()),
        );
    }
    v
}

/// A workflow-registry event, built inside the adapter that owns the
/// row transaction. Under 3P a protocol edit IS a network
/// configuration change (protocol-policy-publish.md, Constraints):
/// the registry's writes were the one un-evented path in boss-jobs,
/// which made "protocols as data the log witnesses" false. The actor
/// rides as `_actor` exactly as EventStamp injects it, so consumers
/// and the rebuild read one shape.
///
/// The stamp is wall-clock, minted here — sim time is retired from
/// the record (David, 2026-08-22, packet a7a4cae5). Same for every
/// builder below.
pub fn workflow_registry_event(
    kind: &str,
    actor: &boss_core::actor::ActorId,
    spec: &crate::registry::WorkflowSpec,
) -> boss_core::event::Event {
    let payload =
        boss_core::publisher::inject_actor(serde_json::to_value(spec).unwrap_or_default(), actor);
    boss_core::event::Event::new("jobs", kind, payload, boss_clock_client::wall_now())
}

/// The `jobs.kind.quarantined` marker: which Workflow row a
/// quarantine retired, and the lint problems that condemned it. Payload keys
/// mirror the registry events (`kind`, `version`, `label`) plus a
/// `problems` list in the same `{step, reason, message}` wire shape
/// `POST /api/workflows/_validate` returns, so one reader parses
/// both. Actor rides as `_actor` exactly as EventStamp injects it.
pub fn workflow_quarantined_event(
    actor: &boss_core::actor::ActorId,
    spec: &crate::registry::WorkflowSpec,
    problems: &[crate::workflow_lint::WorkflowLintError],
) -> boss_core::event::Event {
    let payload = boss_core::publisher::inject_actor(
        serde_json::json!({
            "kind": spec.kind,
            "version": spec.version,
            "label": spec.label,
            "problems": crate::workflow_lint::problems_json(problems),
        }),
        actor,
    );
    boss_core::event::Event::new(
        "jobs",
        WORKFLOW_QUARANTINED,
        payload,
        boss_clock_client::wall_now(),
    )
}

/// One packet-loss census firing — same contract as the quarantine
/// markers: built where the write happens, actor riding as `_actor`
/// exactly as EventStamp injects it. `counts` is the census payload
/// verbatim (the dispatcher handler computed it; this side only
/// stamps and records), so the field list lives in ONE place — the
/// handler that measures — rather than being re-declared here.
pub fn network_census_event(
    actor: &boss_core::actor::ActorId,
    counts: serde_json::Value,
) -> boss_core::event::Event {
    let payload = boss_core::publisher::inject_actor(counts, actor);
    boss_core::event::Event::new(
        "jobs",
        NETWORK_CENSUS,
        payload,
        boss_clock_client::wall_now(),
    )
}

/// One estate observation, recorded verbatim.
///
/// DUMB ON PURPOSE, exactly like the census: the honesty lives in the
/// thing that looked, and a door that second-guesses its instrument is
/// a second instrument.
pub fn estate_observed_event(
    actor: &boss_core::actor::ActorId,
    observation: serde_json::Value,
) -> boss_core::event::Event {
    let payload = boss_core::publisher::inject_actor(observation, actor);
    boss_core::event::Event::new(
        "jobs",
        ESTATE_OBSERVED,
        payload,
        boss_clock_client::wall_now(),
    )
}

/// One estate comparison, recorded verbatim — the same dumb-door
/// contract as the observation above: the handler computed it, this
/// only records what it was handed.
pub fn estate_compared_event(
    actor: &boss_core::actor::ActorId,
    comparison: serde_json::Value,
) -> boss_core::event::Event {
    let payload = boss_core::publisher::inject_actor(comparison, actor);
    boss_core::event::Event::new(
        "jobs",
        ESTATE_COMPARED,
        payload,
        boss_clock_client::wall_now(),
    )
}

/// A step-plugin registry event — same contract as
/// [`workflow_registry_event`], for the `step_plugins` table: built
/// inside the adapter that owns the row transaction, payload is the
/// serialized `StepPluginSpec` with the actor riding as `_actor`
/// exactly as EventStamp injects it.
pub fn step_plugin_registry_event(
    kind: &str,
    actor: &boss_core::actor::ActorId,
    spec: &crate::step_plugins::StepPluginSpec,
) -> boss_core::event::Event {
    let payload =
        boss_core::publisher::inject_actor(serde_json::to_value(spec).unwrap_or_default(), actor);
    boss_core::event::Event::new("jobs", kind, payload, boss_clock_client::wall_now())
}

/// A station registry event — same contract as
/// [`workflow_registry_event`], for the `stations` table: built
/// inside the adapter that owns the row transaction, payload is the
/// serialized `StationSpec` with the actor riding as `_actor`
/// exactly as EventStamp injects it.
pub fn station_registry_event(
    kind: &str,
    actor: &boss_core::actor::ActorId,
    spec: &crate::stations::StationSpec,
) -> boss_core::event::Event {
    let payload =
        boss_core::publisher::inject_actor(serde_json::to_value(spec).unwrap_or_default(), actor);
    boss_core::event::Event::new("jobs", kind, payload, boss_clock_client::wall_now())
}

/// A cadence registry event — same contract as
/// [`workflow_registry_event`], for the `cadence_rules` table: built
/// inside the adapter that owns the row transaction, payload is the
/// serialized `CadenceRuleSpec` with the actor riding as `_actor`
/// exactly as EventStamp injects it.
pub fn cadence_registry_event(
    kind: &str,
    actor: &boss_core::actor::ActorId,
    spec: &crate::cadence::CadenceRuleSpec,
) -> boss_core::event::Event {
    let payload =
        boss_core::publisher::inject_actor(serde_json::to_value(spec).unwrap_or_default(), actor);
    boss_core::event::Event::new("jobs", kind, payload, boss_clock_client::wall_now())
}

#[cfg(test)]
mod payload_tests {
    use super::step_state_payload;
    use boss_core::job::{JobId, Step};

    #[test]
    fn state_payloads_carry_the_marker_key() {
        let step = Step::new(JobId::new(), "task", "Do it", 0);
        let p = step_state_payload(&step);
        assert_eq!(
            p["step_id"], p["id"],
            "state events and marker events must agree on the step's identity key"
        );
        assert_eq!(p["step_id"], step.id.to_string().as_str());
    }
}
