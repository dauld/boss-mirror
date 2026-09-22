//! **What a station's predicate cannot see** — backlog abda9ab4.
//!
//! [`crate::station_lint`] judges a station row on its own terms:
//! provable from the spec alone, with no reference to what packets
//! happen to exist. This file asks the other half of the question,
//! and it is a question only LIVE DATA can answer — which is why it
//! is not a lint and could never have been one.
//!
//! THE DEFECT. A station can omit a whole kind of work and answer a
//! correct-looking total. Measured 2026-09-22: the station
//! `a.platform-admin.opus-5-1m` answered its queue with 124 packets —
//! a complete, well-formed answer for what its predicate could see —
//! while 57 open packets carrying a ready step that the ACTIVE
//! protocol says belongs in that queue were absent from it. Forty-seven
//! of them were the page-audit initiative, which sat three days where
//! no agent could find it. It was found because a human noticed the
//! page march had never moved.
//!
//! THE MECHANISM. A station's step clause reads keys that are
//! PROJECTED onto a step at materialisation from the Workflow row it
//! was admitted under — `authority_role` and `station` from
//! [`crate::audience`], the four `agent_*` keys from
//! [`crate::agent_spec`]. A packet admitted under a version that did
//! not declare them carries none of them, and in-flight packets stay
//! pinned to the version they were admitted under. So the packet is
//! not deprioritised, it is ABSENT, and every number the station
//! reports is right about the rows it can see. There is no error, no
//! warning, and no figure that looks wrong — the only observable is a
//! count that is smaller than it should be, and nobody knows what it
//! should be.
//!
//! THE CONDITION IS DECIDABLE, which is the whole point. For each
//! packet the station's universe already holds: it is not a member,
//! but it WOULD be a member if the projected keys the clause reads
//! were relaxed, and its kind's ACTIVE Workflow row projects exactly
//! the values the clause demands onto that step. That needs no new
//! data — the registry, the packets and the predicate are all already
//! in the reader's hand — and it would have answered on day one.
//!
//! WHY A NUMBER AND NOT AN ALARM. Measured across all fifteen live
//! stations on 2026-09-22 before this was written: fourteen read zero
//! and one read 57. A surfaced figure is therefore not noise — it is
//! zero on a healthy network, which is the property a number beside
//! the depth has to have to be worth reading. An alarm packet would
//! have been the other honest shape and adds to a queue already 223
//! deep; a lint would be red for reasons no car caused, because the
//! drift happens to LIVE DATA long after any tree was judged. So the
//! finding rides where it is felt: `GET /api/stations/load`, beside
//! the depth it corrects, on the surface that draws the queue — the
//! rule that a troubled thing must LOOK troubled rather than have an
//! alarm existing elsewhere.

use std::collections::BTreeMap;

use boss_core::job::{Job, Step};

use crate::registry::{StepSpec, WorkflowSpec};
use crate::station_queue::StationPredicate;
use crate::stations::StationSpec;

/// The step-metadata keys a Workflow row PROJECTS at materialisation,
/// and so the keys a station clause can read that a pinned packet may
/// silently lack. Derived from the two projections themselves —
/// [`crate::audience::Selectors`]'s step-metadata keys and
/// [`crate::agent_spec::KEYS`] — never a third list beside them
/// (CLAUDE.md §9a).
fn projected_keys() -> Vec<&'static str> {
    let mut keys = vec![
        crate::audience::AUTHORITY_ROLE_KEY,
        crate::audience::STATION_KEY,
    ];
    keys.extend(crate::agent_spec::KEYS);
    keys
}

/// What a [`StepSpec`] would project onto a materialised step, as the
/// plain string values a station clause compares against. One reader's
/// door over both projections, so this file holds no copy of either
/// mapping.
fn projection_of(spec: &StepSpec) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let selectors = spec.selectors();
    if let Some(role) = selectors.authority_role {
        out.insert(crate::audience::AUTHORITY_ROLE_KEY.to_string(), role);
    }
    if let Some(station) = selectors.station {
        out.insert(crate::audience::STATION_KEY.to_string(), station);
    }
    if let Some(agent) = &spec.agent {
        for (key, value) in crate::agent_spec::projection(agent) {
            // The clause compares strings; a projected number (the
            // budget) is rendered the way the projection wrote it.
            let text = match value {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
            out.insert(key.to_string(), text);
        }
    }
    out
}

/// The same predicate with the projected keys dropped from its step
/// clause — "everything this station asks for EXCEPT the keys a pinned
/// packet may be missing". Built by removing keys from the real
/// predicate rather than by re-implementing the match, so there is one
/// definition of membership ([`StationPredicate::matches`]) and this
/// file cannot drift from it.
fn relaxed(predicate: &StationPredicate) -> Option<StationPredicate> {
    let step = predicate.step.as_ref()?;
    let mut relaxed = predicate.clone();
    let clause = relaxed.step.as_mut()?;
    let mut dropped = false;
    for key in projected_keys() {
        if step.metadata_equals.contains_key(key) {
            clause.metadata_equals.remove(key);
            dropped = true;
        }
    }
    dropped.then_some(relaxed)
}

/// **The count a station's depth is missing.** How many packets of
/// this universe are absent from the queue only because a projected
/// key never reached their step.
///
/// `spec.predicate` must already be BOUND, exactly as
/// [`crate::station_queue::evaluate_station`] requires: an unbound
/// per-actor predicate matches nothing, so it would report its whole
/// universe as unreachable.
///
/// `active` is the ACTIVE Workflow row per kind — the protocol as it
/// is published TODAY, which is the thing the pinned packet has
/// drifted from. A kind with no active row contributes nothing: there
/// is no current protocol to disagree with.
///
/// Zero for every station that reads no projected key, which is most
/// of them, and zero on a network with no drift.
pub fn unreachable_packets(
    spec: &StationSpec,
    packets: &[(Job, Vec<Step>)],
    active: &BTreeMap<String, WorkflowSpec>,
) -> usize {
    let Some(relaxed_predicate) = relaxed(&spec.predicate) else {
        return 0;
    };
    let (Some(full_clause), Some(relaxed_clause)) = (
        spec.predicate.step.as_ref(),
        relaxed_predicate.step.as_ref(),
    ) else {
        return 0;
    };
    let wanted: BTreeMap<&String, &String> = full_clause
        .metadata_equals
        .iter()
        .filter(|(key, _)| !relaxed_clause.metadata_equals.contains_key(*key))
        .collect();

    packets
        .iter()
        .filter(|(job, steps)| {
            // A member is not omitted, however many other steps it
            // carries — the observable this repairs is a packet that is
            // ABSENT from the queue.
            !spec.predicate.matches(job, steps)
                && relaxed_predicate.matches(job, steps)
                && active.get(&job.kind).is_some_and(|row| {
                    steps.iter().any(|step| {
                        relaxed_clause.matches(step)
                            && !full_clause.matches(step)
                            && step_should_project(row, step, &wanted)
                    })
                })
        })
        .count()
}

/// Does the ACTIVE row say this step carries exactly the values the
/// clause demands? Matched by `spec_slug`, the stable identity a
/// materialised step keeps across versions.
fn step_should_project(
    row: &WorkflowSpec,
    step: &Step,
    wanted: &BTreeMap<&String, &String>,
) -> bool {
    let Some(slug) = step.spec_slug.as_deref() else {
        return false;
    };
    let Some(spec) = row.steps.iter().find(|s| s.title == slug) else {
        return false;
    };
    let projection = projection_of(spec);
    wanted
        .iter()
        .all(|(key, want)| projection.get(key.as_str()) == Some(&(*want).clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_spec::{AgentSpec, Effort};
    use crate::station_queue::{StationPredicate, StepMatch};
    use crate::stations::{StationKind, StationSpec};
    use boss_core::job::{Priority, StepStatus, Subject};
    use chrono::NaiveDate;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 22).unwrap()
    }

    /// The live `a.platform-admin.opus-5-1m` row, reduced to the two
    /// projected keys it reads.
    fn station() -> StationSpec {
        StationSpec::draft(
            "a.platform-admin.opus-5-1m",
            "Agent queue",
            StationKind::Constraint,
            StationPredicate {
                status: Some(boss_core::job::JobStatus::Open),
                step: Some(StepMatch {
                    status_in: vec![StepStatus::Ready, StepStatus::Active],
                    metadata_equals: [
                        ("agent_model".to_string(), "opus-5[1m]".to_string()),
                        ("authority_role".to_string(), "platform-admin".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            chrono::Utc::now(),
        )
    }

    /// The ACTIVE row: a `build` step declaring both the role and the
    /// agent block, so a materialised step SHOULD carry both keys.
    fn active_row(kind: &str) -> BTreeMap<String, WorkflowSpec> {
        let spec = WorkflowSpec::platform_seed(
            kind,
            "Backlog item",
            "it",
            vec!["custom".to_string()],
            vec![StepSpec {
                title: "build".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                authority_role: Some("platform-admin".into()),
                agent: Some(AgentSpec {
                    profile: "builder".into(),
                    model: "opus-5[1m]".into(),
                    budget_usd: 5.0,
                    effort: Effort::High,
                }),
                ..Default::default()
            }],
        );
        [(kind.to_string(), spec)].into_iter().collect()
    }

    fn packet(kind: &str, metadata: serde_json::Value) -> (Job, Vec<Step>) {
        let mut job = Job::new(
            kind,
            Subject::new("custom", "x"),
            "t",
            "emp-david",
            Priority::Standard,
            day(),
        );
        job.status = boss_core::job::JobStatus::Open;
        let mut step = Step::new(job.id, "task", "Build the change", 0);
        step.spec_slug = Some("build".into());
        step.status = StepStatus::Ready;
        step.metadata = metadata;
        (job, vec![step])
    }

    /// The measured defect: a packet pinned to a version with no agent
    /// block carries neither key, so the station's own predicate cannot
    /// see it — and this is the number that says so.
    #[test]
    fn a_pinned_packet_missing_the_projected_keys_is_counted() {
        let packets = vec![packet("backlog-item", serde_json::json!({}))];
        assert_eq!(
            unreachable_packets(&station(), &packets, &active_row("backlog-item")),
            1
        );
    }

    /// A packet the station CAN see is not omitted from it.
    #[test]
    fn a_member_is_not_unreachable() {
        let packets = vec![packet(
            "backlog-item",
            serde_json::json!({ "agent_model": "opus-5[1m]", "authority_role": "platform-admin" }),
        )];
        assert_eq!(
            unreachable_packets(&station(), &packets, &active_row("backlog-item")),
            0
        );
    }

    /// Half the keys is still omitted — the clause is a conjunction, so
    /// one missing projection is enough to make the packet absent.
    #[test]
    fn a_partial_projection_is_counted() {
        let packets = vec![packet(
            "backlog-item",
            serde_json::json!({ "authority_role": "platform-admin" }),
        )];
        assert_eq!(
            unreachable_packets(&station(), &packets, &active_row("backlog-item")),
            1
        );
    }

    /// The active row is the authority. A step whose CURRENT protocol
    /// does not put it in this queue is not omitted from it — it simply
    /// does not belong, and counting it would make the figure noise.
    #[test]
    fn a_step_the_active_row_does_not_route_here_is_not_counted() {
        let mut active = active_row("backlog-item");
        if let Some(row) = active.get_mut("backlog-item") {
            row.steps[0].agent = None;
        }
        let packets = vec![packet("backlog-item", serde_json::json!({}))];
        assert_eq!(unreachable_packets(&station(), &packets, &active), 0);
    }

    /// A kind with no active row has no current protocol to disagree
    /// with, so it contributes nothing rather than everything.
    #[test]
    fn a_kind_with_no_active_row_contributes_nothing() {
        let packets = vec![packet("backlog-item", serde_json::json!({}))];
        assert_eq!(
            unreachable_packets(&station(), &packets, &BTreeMap::new()),
            0
        );
    }

    /// Every clause that is NOT a projected key still applies. A step
    /// in the wrong status is not in this queue's universe at all, and
    /// no amount of projection would put it there.
    #[test]
    fn a_step_failing_a_non_projected_clause_is_not_counted() {
        let (job, mut steps) = packet("backlog-item", serde_json::json!({}));
        steps[0].status = StepStatus::Pending;
        assert_eq!(
            unreachable_packets(&station(), &[(job, steps)], &active_row("backlog-item")),
            0
        );
    }

    /// The ten `q.<role>.<kind>` stations read one projected key and
    /// measured zero on the live network; a station that reads NONE
    /// cannot have this defect at all and pays nothing to say so.
    #[test]
    fn a_station_reading_no_projected_key_is_zero() {
        let mut spec = station();
        if let Some(clause) = spec.predicate.step.as_mut() {
            clause.metadata_equals.clear();
            clause
                .metadata_equals
                .insert("result".into(), "failing".into());
        }
        let packets = vec![packet("backlog-item", serde_json::json!({}))];
        assert_eq!(
            unreachable_packets(&spec, &packets, &active_row("backlog-item")),
            0
        );
    }

    /// An UNBOUND per-actor predicate matches nothing, so a naive
    /// reading would report its whole universe unreachable. It reports
    /// zero instead — the same fail-closed posture
    /// `StationPredicate::matches` takes for the same reason.
    #[test]
    fn an_unbound_self_predicate_reports_zero() {
        let mut spec = station();
        if let Some(clause) = spec.predicate.step.as_mut() {
            clause.assignee_id = Some(crate::station_queue::SELF.into());
        }
        let packets = vec![packet("backlog-item", serde_json::json!({}))];
        assert_eq!(
            unreachable_packets(&spec, &packets, &active_row("backlog-item")),
            0
        );
    }
}
