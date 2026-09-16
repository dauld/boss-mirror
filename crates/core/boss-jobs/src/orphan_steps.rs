//! The no-orphan-steps check — design f5ebd2e1, resolution
//! `no-orphan-steps` (David, 2026-09-11): *"a check that every ready
//! step in an open packet is selected by at least one station predicate
//! or carries an assignee. It is computable from registry data alone."*
//! Car 1 (backlog 67a58840): a WARN over the platform bundle — the list
//! is printed and the count pinned by a test — so the next car can flip
//! it to a refusal at publish once the station set is complete.
//!
//! WHY A STATIC CHECK OVER REGISTRY DATA. The instance that forced the
//! design was a `backlog-item` routed to `design`, whose decision step
//! no station claimed: the authored `design-review` station admits
//! `kind = design-doc-review` at slug `review`, and the step was
//! `kind = backlog-item` at slug `design-review`. Nothing said so on the
//! day the disposition was introduced; it was found weeks later by an
//! operator looking for a decision that was in My Day's endpoint and
//! on no page. Every fact needed to say so — the protocol's steps, the
//! station rows, the projected `q.<role>.<kind>` queues — is registry
//! data, so the check runs over the registries with no packet in hand.
//!
//! WHAT "SELECTED" MEANS HERE. A step is COVERED when, materialised and
//! ready in an open packet of its protocol, it would be
//!   - born assigned (an `individual` audience — the assignment query's
//!     individual arm reads `assignee_id`), or
//!   - a member of at least one active station's queue, judged by the
//!     REAL predicate evaluator (`StationPredicate::matches`, one
//!     definition) against a synthetic packet whose Job-level facts
//!     satisfy the row's Job-level clauses. Job-level clauses
//!     (`metadata_present`, `tags_any`, ...) are runtime facts a static
//!     check cannot know, so they are granted; the STEP clause is what
//!     is judged. A station with NO step clause (`my-watchlist`) selects
//!     packets, not steps, and covers nothing here — it says who is
//!     watching, not who is doing. A station that binds `@me` is a
//!     per-viewer lens for the same reason.
//! Every other step — `completion = human` (or a kind the registry does
//! not know, conservatively) and not a structural marker
//! (`StepType::is_marker`) — that is neither is an ORPHAN: a person's
//! work that no queue holds and nobody is named for.
//!
//! The station set is the authored rows PLUS the projection
//! (`station_projection::derived_stations`), exactly as
//! `GET /api/stations` serves it — so a step with an `authority_role`
//! (or a `role` audience) is covered by its projected constraint queue,
//! and the orphans are the steps with none of the three.

use boss_core::job::{Job, JobId, JobStatus, Priority, StepId, StepStatus, Subject};
use boss_core::partition::Partition;
use chrono::NaiveDate;
use serde_json::{Map, Value};

use crate::registry::{StepSpec, WorkflowSpec, WorkflowStatus, materialize_steps};
use crate::station_queue::SELF;
use crate::stations::StationSpec;
use crate::step_registry::StepRegistry;

/// One step a person must do that no station holds and nobody is
/// named for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrphanStep {
    pub workflow: String,
    pub step: String,
    pub kind: String,
}

impl std::fmt::Display for OrphanStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{} ({})", self.workflow, self.step, self.kind)
    }
}

/// Is this step work an ACTOR performs — as opposed to a marker the
/// machine reaches, an agent's computation, a child Job's close, or an
/// external event? Only these can be orphans: the rest are never
/// waiting for a person to find them. An unknown kind is treated as a
/// person's, the way the dispatcher's `executor_for` treats one as
/// decision-shaped.
pub fn is_an_actors_work(step: &StepSpec, registry: &StepRegistry) -> bool {
    match registry.get(&step.kind) {
        None => true,
        Some(t) => t.completion == crate::step_registry::Completion::Human && !t.is_marker(),
    }
}

/// A synthetic open packet of `workflow`'s kind whose Job-level facts
/// satisfy `station`'s Job-level clauses — so that the only thing
/// `matches` can refuse on is the step clause, the half a static check
/// CAN judge. Presence keys are granted a value, equals keys their
/// value, absent keys are left absent, and the first `tags_any` tag is
/// set.
fn packet_satisfying(workflow: &WorkflowSpec, station: &StationSpec) -> Job {
    let p = &station.predicate;
    let mut metadata = Map::new();
    for key in &p.metadata_present {
        metadata.insert(key.clone(), Value::String("present".into()));
    }
    for (key, want) in &p.metadata_equals {
        metadata.insert(key.clone(), Value::String(want.clone()));
    }
    let subject_kind = workflow
        .subject_kinds
        .first()
        .map(String::as_str)
        .unwrap_or("custom");
    Job {
        id: JobId::new(),
        kind: workflow.kind.clone(),
        workflow_version: workflow.version,
        subject: Subject::new(subject_kind, "orphan-check"),
        title: workflow.kind.clone(),
        owner_id: "orphan-check".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 15).unwrap_or_default(),
        due_on: None,
        closed_on: None,
        metadata: Value::Object(metadata),
        tags: p.tags_any.first().cloned().into_iter().collect(),
        partition: Partition::Real,
    }
}

/// Every orphan in `workflows` against `stations` (authored + derived,
/// the set the API serves). Sorted, so the printed list is stable.
pub fn orphan_steps(
    workflows: &[WorkflowSpec],
    stations: &[StationSpec],
    registry: &StepRegistry,
) -> Vec<OrphanStep> {
    let work_queues: Vec<&StationSpec> = stations
        .iter()
        .filter(|s| s.status == WorkflowStatus::Active)
        // A row with no step clause selects packets, not steps; a row
        // binding `@me` is a per-viewer lens. Neither says who does a
        // step, so neither covers one.
        .filter(|s| s.predicate.step.is_some() && !s.predicate.binds_self())
        .collect();

    let mut orphans = Vec::new();
    for workflow in workflows
        .iter()
        .filter(|w| w.status == WorkflowStatus::Active)
    {
        let subject_kind = workflow
            .subject_kinds
            .first()
            .map(String::as_str)
            .unwrap_or("custom");
        let subject = Subject::new(subject_kind, "orphan-check");
        // The REAL materialisation, so the step carries exactly the
        // projection a live packet's step would (assignee, the
        // audience-derived metadata keys, the defaults).
        let mut steps = materialize_steps(
            workflow,
            &subject,
            JobId::new(),
            &Value::Object(Map::new()),
            StepId::new,
        );
        for step in &mut steps {
            step.status = StepStatus::Ready;
        }
        for (spec, step) in workflow.steps.iter().zip(&steps) {
            if !is_an_actors_work(spec, registry) {
                continue;
            }
            if step
                .assignee_id
                .as_deref()
                .is_some_and(|a| !a.is_empty() && a != SELF)
            {
                continue;
            }
            let one_step = std::slice::from_ref(step);
            let held = work_queues.iter().any(|station| {
                let packet = packet_satisfying(workflow, station);
                station.predicate.matches(&packet, one_step)
            });
            if !held {
                orphans.push(OrphanStep {
                    workflow: workflow.kind.clone(),
                    step: spec.title.clone(),
                    kind: spec.kind.clone(),
                });
            }
        }
    }
    orphans.sort();
    orphans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audience::Audience;
    use crate::registry::Terminal;
    use crate::station_queue::{StationPredicate, StepMatch, default_discipline};
    use crate::stations::StationKind;
    use std::collections::BTreeMap;

    fn workflow(kind: &str, steps: Vec<StepSpec>) -> WorkflowSpec {
        WorkflowSpec::platform_seed(kind, kind, "platform", vec!["custom".into()], steps)
    }

    fn trigger() -> StepSpec {
        StepSpec {
            title: "filed".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            ..Default::default()
        }
    }

    fn work(title: &str, kind: &str) -> StepSpec {
        StepSpec {
            title: title.into(),
            kind: kind.into(),
            ready_when: "steps.filed.done".into(),
            terminal: Some(Terminal {
                outcome: "done".into(),
            }),
            ..Default::default()
        }
    }

    fn station(name: &str, predicate: StationPredicate) -> StationSpec {
        StationSpec {
            name: name.into(),
            version: 1,
            status: WorkflowStatus::Active,
            title: name.into(),
            kind: StationKind::Batch,
            predicate,
            discipline: default_discipline(),
            wip_limit: None,
            terminal_window_days: None,
            capability: None,
            rollup_parent: None,
            upstream: None,
            lens: None,
            created_at: chrono::DateTime::from_timestamp(1_760_000_000, 0).expect("fixed"),
        }
    }

    fn names(orphans: &[OrphanStep]) -> Vec<String> {
        orphans.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_persons_step_that_no_station_holds_and_nobody_is_named_for_is_an_orphan() {
        let reg = StepRegistry::v1();
        let wf = workflow("lonely", vec![trigger(), work("decide", "answer-question")]);
        let found = orphan_steps(&[wf], &[], &reg);
        assert_eq!(names(&found), vec!["lonely/decide (answer-question)"]);
    }

    #[test]
    fn markers_and_machine_completed_steps_are_never_orphans() {
        let reg = StepRegistry::v1();
        let mut outcome = work("closed", "outcome");
        outcome.ready_when = "steps.gate.done".into();
        let mut gate = work("gate", "demand-gate");
        gate.terminal = None;
        let wf = workflow("machine", vec![trigger(), gate, outcome]);
        assert!(orphan_steps(&[wf], &[], &reg).is_empty());
    }

    #[test]
    fn an_individual_audience_is_born_assigned_and_covered() {
        let reg = StepRegistry::v1();
        let mut decide = work("decide", "answer-question");
        decide.audience = Some(Audience::Individual("emp-david".into()));
        let wf = workflow("named", vec![trigger(), decide]);
        assert!(orphan_steps(&[wf], &[], &reg).is_empty());
    }

    #[test]
    fn a_station_whose_step_clause_selects_it_covers_it() {
        let reg = StepRegistry::v1();
        let wf = workflow("held", vec![trigger(), work("review", "task")]);
        let by_slug = station(
            "review-queue",
            StationPredicate {
                kind: Some("held".into()),
                status: Some(JobStatus::Open),
                // A Job-level fact the static check cannot know is
                // GRANTED, so the step clause is what decides.
                metadata_present: vec!["branch".into()],
                step: Some(StepMatch {
                    slug: Some("review".into()),
                    status_in: vec![StepStatus::Ready, StepStatus::Active],
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assert!(
            orphan_steps(
                std::slice::from_ref(&wf),
                std::slice::from_ref(&by_slug),
                &reg
            )
            .is_empty()
        );

        // The same row for ANOTHER kind does not cover it — the exact
        // miss that motivated the design (design-review admitted
        // design-doc-review at `review`; the backlog item was neither).
        let mut other_kind = by_slug.clone();
        other_kind.predicate.kind = Some("design-doc-review".into());
        assert_eq!(
            names(&orphan_steps(
                std::slice::from_ref(&wf),
                &[other_kind],
                &reg
            )),
            vec!["held/review (task)"]
        );
        // Nor does a retired row.
        let mut retired = by_slug;
        retired.status = WorkflowStatus::Retired;
        assert_eq!(orphan_steps(&[wf], &[retired], &reg).len(), 1);
    }

    #[test]
    fn a_station_with_no_step_clause_selects_packets_not_steps() {
        let reg = StepRegistry::v1();
        let wf = workflow("watched", vec![trigger(), work("review", "task")]);
        let watchlist = station(
            "my-watchlist",
            StationPredicate {
                metadata_equals: BTreeMap::from([("submitted_by".to_string(), SELF.to_string())]),
                ..Default::default()
            },
        );
        let everything = station("everything", StationPredicate::default());
        assert_eq!(
            orphan_steps(&[wf], &[watchlist, everything], &reg).len(),
            1,
            "a watchlist says who is watching, not who is doing"
        );
    }

    #[test]
    fn a_station_audience_is_covered_by_a_row_reading_the_station_key() {
        let reg = StepRegistry::v1();
        let mut review = work("review", "task");
        review.audience = Some(Audience::Station("design-review".into()));
        let wf = workflow("routed", vec![trigger(), review]);
        let by_station_key = station(
            "design-review",
            StationPredicate {
                status: Some(JobStatus::Open),
                step: Some(StepMatch {
                    metadata_equals: BTreeMap::from([(
                        "station".to_string(),
                        "design-review".to_string(),
                    )]),
                    status_in: vec![StepStatus::Ready, StepStatus::Active],
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assert!(orphan_steps(std::slice::from_ref(&wf), &[by_station_key], &reg).is_empty());
        // Naming a station whose row does not read the key is still an
        // orphan: the declaration alone routes nothing.
        assert_eq!(orphan_steps(&[wf], &[], &reg).len(), 1);
    }

    #[test]
    fn a_role_is_covered_through_its_projected_constraint_queue() {
        let reg = StepRegistry::v1();
        let mut decide = work("decide", "answer-question");
        decide.audience = Some(Audience::Role("platform-admin".into()));
        let wf = workflow("roled", vec![trigger(), decide]);
        let now = chrono::DateTime::from_timestamp(1_760_000_000, 0).expect("fixed");
        let derived =
            crate::station_projection::derived_stations(std::slice::from_ref(&wf), &[], now);
        assert_eq!(derived.len(), 1, "{derived:#?}");
        assert!(orphan_steps(std::slice::from_ref(&wf), &derived, &reg).is_empty());
        // Without the projection the role alone holds no queue — which
        // is why the check takes authored + derived, as the API serves.
        assert_eq!(orphan_steps(&[wf], &[], &reg).len(), 1);
    }
}
