//! Station flow — whether a queue is FORMING, not just how deep it is.
//!
//! `GET /api/stations/load` already answers depth and the age of the
//! oldest packet, and its own header says why that is not enough:
//! *"Depth is close to meaningless without a drain rate — ten role
//! queues each read exactly 48 the day this was written and none was a
//! bottleneck, they were a bug."* This module is the drain rate.
//!
//! ## The rate is already recorded — nothing new is written for it
//!
//! A station's membership is a predicate over packet state, and for
//! every station whose predicate waits on a step, the two transitions
//! that put a packet in the queue and take it out are exactly the two
//! the log already carries: `step.ready.<kind>` and `step.done.<kind>`
//! (`http/steps.rs`, `build_step_ready_event` and the `step.done`
//! marker). So arrivals and departures over a window are a read of
//! `audit_log`, not a new stamp on anything.
//!
//! WALL CLOCK, NOT EVENT TIME. The window is `audit_log.created_at`,
//! never `timestamp` — event time is sim-authoritative on a demo
//! deployment and a ten-minute triage reads as a week on it. The
//! doctrine and the incident behind it are stated once, in
//! `boss-views/src/flow.rs`; this obeys it.
//!
//! ## Why a cube, and why some stations report blind
//!
//! The log's step events do not carry the Job's kind, and only the
//! `done` half carries `spec_slug`, so the counts are grouped against
//! the `steps` / `jobs` projections into a small cube keyed by the
//! four coordinates a station predicate can name without replaying
//! packet state: `(job kind, step kind, spec slug, authority role)`.
//! [`station_flow`] then sums the cells one station's own predicate
//! selects — the station registry stays the definition, and no station
//! name appears in this file (CLAUDE.md §9).
//!
//! A predicate that reaches outside those four coordinates — a Job
//! metadata clause, a tag clause, a per-actor binding, a step
//! assignee — cannot be evaluated on the cube, and this returns
//! [`FlowBlind`] naming which clause blinded it rather than a number
//! it had to invent. The yard's rule holds upstream: **no figure this
//! surface makes up.** A station that reports depth plus "flow not
//! countable, and here is the clause" is the honest outcome.

use boss_core::job::StepStatus;
use serde::{Deserialize, Serialize};

use crate::station_queue::StationPredicate;

/// The step metadata key a constraint station matches on — the one
/// step-level clause the cube carries a coordinate for.
const AUTHORITY_ROLE: &str = "authority_role";

/// One cell of the flow cube: how many obligations of this exact
/// shape arrived and how many were served inside the window.
///
/// `spec_slug` and `authority_role` are the empty string when the step
/// declares none — a coordinate, not an `Option`, so a predicate that
/// does not name them matches every cell and one that names them
/// matches exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowCell {
    pub job_kind: String,
    pub step_kind: String,
    pub spec_slug: String,
    pub authority_role: String,
    /// Distinct steps that became `ready` inside the window.
    pub arrived: i64,
    /// Distinct steps that completed inside the window.
    pub served: i64,
}

/// Why one station's flow cannot be counted from the cube. Each
/// variant names the clause that blinded it, so the surface can say
/// what it would take to see the rate rather than showing a blank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlowBlind {
    /// No step clause at all: membership is the packet merely being
    /// open, so nothing arrives or leaves by a step transition.
    NoStepClause,
    /// The step clause does not wait on `ready`, so a `step.ready`
    /// event is not this queue's arrival.
    StatusIsNotAWait,
    /// A Job metadata clause — the cube holds no metadata coordinate,
    /// and a Job's metadata now is not its metadata when the step
    /// moved.
    JobMetadataClause,
    /// A Job tag clause, for the same reason.
    JobTagsClause,
    /// The queue is one actor's, bound at the read edge.
    SelfBound,
    /// A step assignee clause: assignment is not a transition the log
    /// counts as an arrival.
    StepAssigneeClause,
    /// A step metadata clause on some key other than `authority_role`.
    StepMetadataBeyondRole,
}

impl FlowBlind {
    /// A sentence an operator can read on the surface.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoStepClause => {
                "membership is the packet being open, not a step becoming ready — \
                 the log's step events cannot be attributed to this queue"
            }
            Self::StatusIsNotAWait => {
                "the queue does not hold steps waiting at `ready`, so a step becoming \
                 ready is not an arrival here"
            }
            Self::JobMetadataClause => {
                "membership turns on Job metadata, which the flow cube does not carry — \
                 and a packet's metadata now is not what it was when the step moved"
            }
            Self::JobTagsClause => {
                "membership turns on a Job tag, which the flow cube does not carry"
            }
            Self::SelfBound => "a per-actor queue: its flow is one person's, not the network's",
            Self::StepAssigneeClause => {
                "membership turns on who the step is assigned to; assignment is not a \
                 transition the log counts as an arrival"
            }
            Self::StepMetadataBeyondRole => {
                "membership turns on step metadata beyond `authority_role`, which the \
                 flow cube does not carry"
            }
        }
    }
}

/// A station's counted flow over the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationFlow {
    pub arrived: i64,
    pub served: i64,
}

impl StationFlow {
    /// Arrivals minus departures: positive means the queue grew.
    pub fn net(self) -> i64 {
        self.arrived - self.served
    }
}

/// Sum the cells this station's predicate selects.
///
/// Pure. The Job `status` clause is deliberately IGNORED: this counts
/// transitions, and a step that completed inside the window may sit on
/// a packet that has closed since. Counting only still-open packets
/// would drop exactly the departures that prove a queue is draining.
pub fn station_flow(
    predicate: &StationPredicate,
    cells: &[FlowCell],
) -> Result<StationFlow, FlowBlind> {
    if predicate.binds_self() {
        return Err(FlowBlind::SelfBound);
    }
    if !predicate.tags_any.is_empty() {
        return Err(FlowBlind::JobTagsClause);
    }
    if !predicate.metadata_present.is_empty()
        || !predicate.metadata_absent.is_empty()
        || !predicate.metadata_equals.is_empty()
    {
        return Err(FlowBlind::JobMetadataClause);
    }
    let Some(step) = &predicate.step else {
        return Err(FlowBlind::NoStepClause);
    };
    if step.assignee_id.is_some() {
        return Err(FlowBlind::StepAssigneeClause);
    }
    if step.metadata_equals.keys().any(|k| k != AUTHORITY_ROLE) {
        return Err(FlowBlind::StepMetadataBeyondRole);
    }
    if !step.status_in.contains(&StepStatus::Ready) {
        return Err(FlowBlind::StatusIsNotAWait);
    }
    let want_role = step.metadata_equals.get(AUTHORITY_ROLE);
    let mut flow = StationFlow {
        arrived: 0,
        served: 0,
    };
    for cell in cells {
        if predicate
            .kind
            .as_deref()
            .is_some_and(|k| k != cell.job_kind)
        {
            continue;
        }
        if step.kind.as_deref().is_some_and(|k| k != cell.step_kind) {
            continue;
        }
        if step.slug.as_deref().is_some_and(|s| s != cell.spec_slug) {
            continue;
        }
        if want_role.is_some_and(|r| r != &cell.authority_role) {
            continue;
        }
        flow.arrived += cell.arrived;
        flow.served += cell.served;
    }
    Ok(flow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::station_queue::StepMatch;

    fn cell(job_kind: &str, step_kind: &str, slug: &str, role: &str, a: i64, s: i64) -> FlowCell {
        FlowCell {
            job_kind: job_kind.into(),
            step_kind: step_kind.into(),
            spec_slug: slug.into(),
            authority_role: role.into(),
            arrived: a,
            served: s,
        }
    }

    fn role_clause(role: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(AUTHORITY_ROLE.to_string(), role.to_string())])
    }

    /// The projected constraint station — `q.<role>.<step-kind>`, the
    /// shape 100 of the 105 active stations have — sums exactly the
    /// cells carrying that (step kind, role) pair and nothing else.
    #[test]
    fn a_constraint_station_sums_its_own_step_kind_and_role() {
        let predicate = StationPredicate {
            status: Some(boss_core::job::JobStatus::Open),
            step: Some(StepMatch {
                kind: Some("task".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                metadata_equals: role_clause("platform-admin"),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cells = vec![
            cell("backlog-item", "task", "triage", "platform-admin", 5, 2),
            cell("gate-run", "task", "run", "platform-admin", 3, 3),
            // Same step kind, different role — another station's queue.
            cell("wholesale-keg-order", "task", "pick", "brewer", 40, 39),
            // Same role, different step kind.
            cell(
                "design-doc",
                "answer-question",
                "review",
                "platform-admin",
                1,
                0,
            ),
        ];
        let flow = station_flow(&predicate, &cells).expect("countable");
        assert_eq!(flow.arrived, 8);
        assert_eq!(flow.served, 5);
        assert_eq!(flow.net(), 3);
    }

    /// An authored station keyed on the Job kind and the step's slug —
    /// `design-review` is the live one — is countable too: both
    /// coordinates are on the cube.
    #[test]
    fn an_authored_slug_station_is_countable() {
        let predicate = StationPredicate {
            kind: Some("design-doc".into()),
            status: Some(boss_core::job::JobStatus::Open),
            step: Some(StepMatch {
                slug: Some("review".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        };
        let cells = vec![
            cell(
                "design-doc",
                "answer-question",
                "review",
                "platform-admin",
                4,
                1,
            ),
            cell("design-doc", "task", "fold", "platform-admin", 9, 9),
            cell(
                "backlog-item",
                "answer-question",
                "review",
                "platform-admin",
                7,
                7,
            ),
        ];
        let flow = station_flow(&predicate, &cells).expect("countable");
        assert_eq!(flow.arrived, 4);
        assert_eq!(flow.served, 1);
    }

    /// The loading dock turns on Job metadata (`branch` present,
    /// `train` absent). A packet's metadata NOW is not what it was
    /// when its step moved, so the honest answer is the named clause,
    /// not a count.
    #[test]
    fn a_job_metadata_clause_reports_blind_rather_than_a_number() {
        let predicate = StationPredicate {
            kind: Some("ship-a-change".into()),
            metadata_present: vec!["branch".into()],
            metadata_absent: vec!["train".into()],
            step: Some(StepMatch {
                slug: Some("review".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = station_flow(&predicate, &[]).expect_err("blind");
        assert_eq!(err, FlowBlind::JobMetadataClause);
        assert!(err.reason().contains("metadata"));
    }

    /// A station with no step clause at all — the publish dock holds
    /// packets for being open — has no step transition to count.
    #[test]
    fn a_step_less_station_reports_blind() {
        let predicate = StationPredicate {
            kind: Some("publish-request".into()),
            status: Some(boss_core::job::JobStatus::Open),
            ..Default::default()
        };
        assert_eq!(
            station_flow(&predicate, &[]).expect_err("blind"),
            FlowBlind::NoStepClause
        );
    }

    /// A per-actor queue binds at the read edge; its flow is one
    /// person's, and the network board must not print it as a rate.
    #[test]
    fn a_self_bound_station_reports_blind() {
        let predicate = StationPredicate {
            metadata_equals: BTreeMap::from([(
                "submitted_by".to_string(),
                crate::station_queue::SELF.to_string(),
            )]),
            ..Default::default()
        };
        assert_eq!(
            station_flow(&predicate, &[]).expect_err("blind"),
            FlowBlind::SelfBound
        );
    }

    /// A queue that does not hold steps at `ready` is not fed by a
    /// `step.ready` event, so counting one as an arrival would be
    /// wrong rather than approximate.
    #[test]
    fn a_queue_that_does_not_wait_at_ready_reports_blind() {
        let predicate = StationPredicate {
            step: Some(StepMatch {
                kind: Some("task".into()),
                status_in: vec![StepStatus::Completed],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            station_flow(&predicate, &[]).expect_err("blind"),
            FlowBlind::StatusIsNotAWait
        );
    }

    /// A predicate naming neither step kind nor slug is a wide queue,
    /// and a wide queue's flow is every waiting step of its Job kind —
    /// `ad-hoc-desk` is the live one.
    #[test]
    fn a_kind_only_station_sums_every_step_of_that_job_kind() {
        let predicate = StationPredicate {
            kind: Some("ad-hoc".into()),
            status: Some(boss_core::job::JobStatus::Open),
            step: Some(StepMatch {
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        };
        let cells = vec![
            cell("ad-hoc", "task", "do", "platform-admin", 2, 2),
            cell("ad-hoc", "sign-off", "approve", "cto", 1, 0),
            cell("backlog-item", "task", "triage", "platform-admin", 30, 30),
        ];
        let flow = station_flow(&predicate, &cells).expect("countable");
        assert_eq!(flow.arrived, 3);
        assert_eq!(flow.served, 2);
    }

    /// An empty window is a fact: zero arrived, zero served — not an
    /// error, and not a missing rate.
    #[test]
    fn an_empty_window_counts_zero_rather_than_reporting_blind() {
        let predicate = StationPredicate {
            step: Some(StepMatch {
                kind: Some("task".into()),
                status_in: vec![StepStatus::Ready],
                ..Default::default()
            }),
            ..Default::default()
        };
        let flow = station_flow(&predicate, &[]).expect("countable");
        assert_eq!((flow.arrived, flow.served, flow.net()), (0, 0, 0));
    }
}
