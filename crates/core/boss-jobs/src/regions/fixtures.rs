//! The rows the regions' tests build a yard from, shared by every
//! module's tests (backlog 07addeed split them out of one test module).

use super::*;
pub(super) use crate::yard::{BoardingReadings, YardInputs, build_status_for};
pub(super) use boss_core::job::{JobId, JobStatus, Priority, StepId, Subject};
pub(super) use serde_json::json;

pub(super) const NOW: &str = "2026-09-19T12:00:00Z";

pub(super) fn t(s: &str) -> Instant {
    parse_instant(s).unwrap()
}

pub(super) fn job(kind: &str, title: &str, status: JobStatus, metadata: Value) -> Job {
    Job {
        id: JobId::new(),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("custom", "s"),
        title: title.into(),
        owner_id: "emp-david".into(),
        status,
        priority: Priority::Standard,
        opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

pub(super) fn step(job: &Job, slug: &str, status: StepStatus, done_at: Option<&str>) -> Step {
    let mut s = Step::new(job.id, "task", slug, 0);
    s.id = StepId::new();
    s.spec_slug = Some(slug.into());
    s.status = status;
    s.completed_at = done_at.map(t);
    s
}

/// Inbound rows as the handler hands them over with no steps read —
/// nothing has taken any of them in.
pub(super) fn untaken(jobs: Vec<Job>) -> Vec<(Job, Vec<Step>)> {
    jobs.into_iter().map(|j| (j, Vec::new())).collect()
}

/// An empty yard: every read answered, nothing in it.
pub(super) fn empty_status() -> YardStatus {
    build_status_for(
        YardInputs {
            now: Some(t(NOW)),
            ..Default::default()
        },
        Reading::Read,
        BoardingReadings::default(),
    )
}

pub(super) fn inputs<'a>(
    status: &'a YardStatus,
    open_trains: &'a [(Job, Vec<Step>)],
    closed_trains: &'a [(Job, Vec<Step>)],
    cars: &'a [(Job, Vec<Step>)],
    gate_runs: &'a [Job],
    inbound: Option<&'a [(Job, Vec<Step>)]>,
    stations: Option<&'a [StationReading]>,
) -> RegionInputs<'a> {
    RegionInputs {
        status,
        dock_reading: Reading::Read,
        open_trains,
        closed_trains,
        cars,
        gate_runs,
        inbound,
        stations,
        conductor: None,
        ops_requests: Some(&[]),
        runner_hosts: Some(&[]),
        agent_runs: Some(&[]),
        sessions: Some(&[]),
        publish_packets: Some(&[]),
        run_capacity: None,
        predecessors: &[],
        now: t(NOW),
        window_hours: 24,
    }
}

pub(super) fn machines_in<'a>(r: &'a Regions, region: &str) -> &'a [Machine] {
    &by_name(r, region).machines
}

pub(super) fn machine_state(r: &Regions, region: &str, id: &str) -> MachineState {
    machines_in(r, region)
        .iter()
        .find(|m| m.id == id)
        .unwrap_or_else(|| panic!("{region} has no machine {id}"))
        .state
}

/// A machine of the plant — the machinery that serves every region
/// (design 62de32ae, decision 11).
pub(super) fn plant_state(r: &Regions, id: &str) -> MachineState {
    r.plant
        .iter()
        .find(|m| m.id == id)
        .unwrap_or_else(|| panic!("the plant has no machine {id}"))
        .state
}

pub(super) fn by_name<'a>(r: &'a Regions, name: &str) -> &'a Region {
    r.regions.iter().find(|x| x.name == name).unwrap()
}

/// A parked car on the dock — open, review ready — with this metadata.
pub(super) fn parked(branch: &str, extra: Value) -> (Job, Vec<Step>) {
    let mut md = json!({ "branch": branch });
    if let (Some(dst), Some(src)) = (md.as_object_mut(), extra.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let j = job("ship-a-change", branch, JobStatus::Open, md);
    let s = vec![step(&j, crate::car::REVIEW_SLUG, StepStatus::Ready, None)];
    (j, s)
}

/// A status whose dock holds these cars, at the boarding depth.
pub(super) fn dock_at_depth(cars: &[(Job, Vec<Step>)]) -> YardStatus {
    let mut status = empty_status();
    status.dock = cars.iter().map(|(j, _)| crate::yard::dock_car(j)).collect();
    status.boarding.threshold_met = Some(true);
    status
}

/// A packet as the jobs API holds one: its trigger completed at
/// admission, then `next` — each `(slug, step kind, status)`.
pub(super) fn admitted(
    kind: &str,
    title: &str,
    next: &[(&str, &str, StepStatus)],
) -> (Job, Vec<Step>) {
    let j = job(kind, title, JobStatus::Open, json!({}));
    let mut trigger = step(
        &j,
        "filed",
        StepStatus::Completed,
        Some("2026-09-19T08:00:00Z"),
    );
    trigger.kind = TRIGGER_STEP_KIND.into();
    let rest = next.iter().map(|(slug, kind, status)| {
        let done = (*status == StepStatus::Completed).then_some("2026-09-19T09:00:00Z");
        let mut s = step(&j, slug, *status, done);
        s.kind = (*kind).into();
        s
    });
    let steps = std::iter::once(trigger).chain(rest).collect();
    (j, steps)
}

/// One station holding these packets, draining.
pub(super) fn holding(name: &str, members: &[&str]) -> StationReading {
    StationReading {
        name: name.into(),
        members: members.iter().map(|m| (*m).to_string()).collect(),
        served: Some(1),
        previous_served: Some(1),
        ..Default::default()
    }
}
