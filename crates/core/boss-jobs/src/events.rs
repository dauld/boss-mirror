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

// Marker events — informational only; rebuild ignores them.
pub const JOB_STATUS_CHANGED: &str = "jobs.job.status_changed";
pub const STEP_COMPLETED: &str = "jobs.step.completed";
pub const STEP_SIGNED_OFF: &str = "jobs.step.signed_off";
/// Loud stamp invalidation (architecture-decisions.md §Step types
/// are property bundles): a stamped step's completion-relevant shape changed;
/// the listed stamps no longer attest the current content and the
/// named roles must re-sign before the step can complete.
pub const STEP_STAMPS_INVALIDATED: &str = "jobs.step.stamps_invalidated";
pub const JOB_CLOSED: &str = "jobs.job.closed";
/// Boot found an ACTIVE Workflow that fails the viability lint and
/// retired it so the service could start (`workflow_quarantine`).
/// The sibling state event is the registry's own
/// `jobs.kind.retired`; this marker is the loud one — it carries the
/// problems that condemned the row, so the log answers "why is this
/// kind gone?" without a re-lint. Rebuild ignores it.
pub const WORKFLOW_QUARANTINED: &str = "jobs.kind.quarantined";
/// Boot found an ACTIVE station that fails the viability lint
/// (`station_quarantine`) and retired it. Same contract as
/// [`WORKFLOW_QUARANTINED`]: the sibling state event is the
/// registry's own `jobs.station.retired`, and this marker is the loud
/// one carrying the problems that condemned the row. Rebuild ignores
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

/// The `jobs.kind.quarantined` marker: which Workflow row boot
/// retired, and the lint problems that condemned it. Payload keys
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

/// The loud marker for a station retired by the boot viability pass.
/// Carries the problems that condemned the row so the log answers
/// "why did this queue disappear?" without a re-lint.
pub fn station_quarantined_event(
    actor: &boss_core::actor::ActorId,
    spec: &crate::stations::StationSpec,
    problems: &[crate::station_lint::StationLintError],
) -> boss_core::event::Event {
    let payload = boss_core::publisher::inject_actor(
        serde_json::json!({
            "name": spec.name,
            "version": spec.version,
            "title": spec.title,
            "problems": crate::station_lint::problems_json(problems),
        }),
        actor,
    );
    boss_core::event::Event::new(
        "jobs",
        STATION_QUARANTINED,
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
