//! The machinery (design d2154293, car 5): the gate bays, the
//! conductor, the stations, the runners and the crews, each judged here
//! and hung on the region it works in — and the hosts that SHOULD have a
//! runner (backlog 49ed87b4), drawn from the estate registry rather than
//! from what answered, which stand in the plant.

use super::*;

/// The role a node declares when it answers ops-request packets — a
/// Class of `node`, joined through `node_roles` (202609120300), the
/// same vocabulary `cluster-operator` lives in. The tree declares it
/// per machine in infra/estate/estate.toml and the launcher publishes
/// it on every start; this constant is how the map asks the registry
/// which hosts SHOULD have a runner.
pub const OPS_RUNNER_ROLE: &str = "ops-runner";

/// THE CREWS' PACKET KIND (design 511fa7d4 car 2b). One open
/// `work-session` is one crew standing on the shop floor: the
/// SessionStart hook files it and the prompt hook heartbeats it.
pub const SESSION_KIND: &str = "work-session";

/// Silent this long and a crew is drawn IDLE rather than at work — the
/// crew board's own `IDLE_AFTER_MS` (`apps/web/src/it/crew/crew.ts`),
/// ported here so the map and the board stop calling a session
/// "working" at the same moment, and pinned equal by `crew.test.ts`
/// (CLAUDE.md §9a). The session's own silence rule ends it at six
/// hours; this is only where the floor stops crediting it with work.
pub const CREW_IDLE_HOURS: i64 = 1;

/// A host the registry expects an ops-runner on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerHost {
    /// The estate node id — what an ops-request carries as
    /// `metadata.host` and what the runner presents as `HOST_ID`.
    pub id: String,
    pub label: String,
}

/// The hosts a runner is EXPECTED on, from the estate registry's rows.
/// A retired machine is not expected to answer; a node declaring no
/// roles is not a runner host. One definition, read by the handler
/// that fetches the evidence and by the drawing below (CLAUDE.md §9a).
pub fn runner_hosts_of(nodes: &[crate::port::EstateNode]) -> Vec<RunnerHost> {
    nodes
        .iter()
        .filter(|n| !n.retired)
        .filter(|n| n.roles.iter().any(|r| r == OPS_RUNNER_ROLE))
        .map(|n| RunnerHost {
            id: n.id.clone(),
            label: n.label.clone(),
        })
        .collect()
}

/// The runners the map draws, one per region: the verb whose
/// ops-requests are the only evidence there is, the region it works
/// in, and what to call it. `http/regions.rs` reads the verbs from
/// [`RUNNER_VERBS`], derived from this — one list, so the read and the
/// drawing cannot drift (CLAUDE.md §9a).
const RUNNERS: [(&str, &str, &str); 2] = [
    ("converge", "arrivals", "converge runner"),
    ("run-car-probe", "shed", "probe runner"),
];

/// The ops-request verbs the handler fetches the newest request of.
pub fn runner_verbs() -> Vec<&'static str> {
    RUNNERS.iter().map(|(verb, _, _)| *verb).collect()
}

fn machine(id: String, name: &str, state: MachineState, why: String) -> Machine {
    Machine {
        id,
        name: name.to_string(),
        state,
        why,
    }
}

/// THE GATE BAYS. The policy declares how many there are and this
/// process counts what occupies them, so a bay nothing is in is
/// OBSERVED empty — the one machine here that can honestly be idle. A
/// bay whose run is `stale` holds a corpse, which is the yard's own
/// judgement (`yard::GATE_MAX_ACTIVE_HOURS`), read rather than redone.
fn gate_bays(inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let g = &inputs.status.gates;
    let capacity = usize::try_from(g.capacity).unwrap_or(0);
    // Never fewer bays than there are runs standing in them: a run the
    // policy has no slot for is still occupying something.
    (0..capacity.max(g.active.len()))
        .map(|i| {
            let id = format!("gate-bay-{}", i + 1);
            let name = format!("bay {}", i + 1);
            match g.active.get(i) {
                Some(a) if a.stale => machine(
                    id,
                    &name,
                    MachineState::Failed,
                    format!(
                        "{} has been active past the gate deadline — a corpse holding the bay",
                        a.branch
                    ),
                ),
                Some(a) => machine(
                    id,
                    &name,
                    MachineState::Running,
                    format!("gating {} since {}", a.branch, a.since),
                ),
                None => machine(id, &name, MachineState::Idle, "free".to_string()),
            }
        })
        .collect()
}

/// THE CONDUCTOR. It declares its heartbeat in the cadence registry,
/// so silence IS judgeable here — and only here. Without that declared
/// interval `silent` can never be true (`yard::conductor_health`), so
/// the machine says unknown rather than inheriting the permissive
/// answer.
fn conductor_machine(h: Option<&ConductorHealth>) -> Machine {
    let id = "conductor".to_string();
    let name = "conductor";
    let Some(h) = h else {
        return machine(
            id,
            name,
            MachineState::Unknown,
            "the conductor's firing record was not read".to_string(),
        );
    };
    if h.silent {
        let since = match h.silent_for_minutes {
            Some(m) => format!("{m}m since it last fired"),
            None => "past its declared heartbeat".to_string(),
        };
        return machine(
            id,
            name,
            MachineState::Failed,
            format!("SILENT — {since}; every train's truth is last-known-good"),
        );
    }
    if let Some(rc) = h.last_rc.filter(|rc| *rc != 0) {
        let verb = h.last_verb.as_deref().unwrap_or("its last pass");
        return machine(
            id,
            name,
            MachineState::Failed,
            format!("{verb} exited {rc} on its last pass"),
        );
    }
    if h.expected_every_minutes.is_none() {
        return machine(
            id,
            name,
            MachineState::Unknown,
            "no heartbeat is declared in the cadence registry, so silence cannot be judged"
                .to_string(),
        );
    }
    match h.silent_for_minutes {
        Some(m) => machine(
            id,
            name,
            MachineState::Running,
            format!(
                "{} {m}m ago, within its declared heartbeat",
                h.last_verb.as_deref().unwrap_or("fired")
            ),
        ),
        None => machine(
            id,
            name,
            MachineState::Unknown,
            "no firing on record to read a tick from".to_string(),
        ),
    }
}

/// A STATION THE SERVER CANNOT JUDGE: within its WIP limit, with flow
/// the cube is blind to — the `(false, None)` arm below, which draws the
/// `?` glyph. Marshalling's header counts these ("2 stations
/// unjudged", design 62de32ae decision 11) so that blindness is itself
/// a number; the test
/// `marshalling_counts_the_stations_it_cannot_judge_in_its_header` holds
/// the header's count to the glyphs'.
pub(super) fn unjudged(s: &StationReading) -> bool {
    !s.over_limit && s.served.is_none()
}

/// THE STATIONS. Each is a registry row this process evaluated, so its
/// presence is a fact and an empty one is genuinely idle. Flow the cube
/// is blind to is unknown, not zero: a station holding work whose
/// movement nobody can count is exactly the case an idle glyph would
/// lie about.
fn station_machines(inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let Some(rows) = inputs.stations else {
        return vec![machine(
            "stations".to_string(),
            "the stations",
            MachineState::Unknown,
            "the station registry could not be read".to_string(),
        )];
    };
    rows.iter()
        .map(|s| {
            let held = s.members.len();
            let id = format!("station:{}", s.name);
            let (state, why) = match (s.over_limit, s.served) {
                (true, _) => (
                    MachineState::Failed,
                    format!("{held} standing — over its WIP limit"),
                ),
                (false, None) => (
                    MachineState::Unknown,
                    format!(
                        "{held} standing — the flow cube is blind to this station's predicate, so whether it moves cannot be told"
                    ),
                ),
                (false, Some(0)) if held > 0 => (
                    MachineState::Failed,
                    format!("{held} standing and nothing served in the window — not draining"),
                ),
                (false, Some(n)) if n > 0 => (
                    MachineState::Running,
                    format!("{n} served in the window, {held} standing"),
                ),
                (false, Some(_)) => (
                    MachineState::Idle,
                    "nothing standing, nothing served in the window".to_string(),
                ),
            };
            machine(id, &s.name, state, why)
        })
        .collect()
}

/// A RUNNER, from its ops-requests — and ONLY from them. A runner
/// polls; it declares no heartbeat anywhere this process can read, so
/// silence says nothing and is never a failure here.
///
/// The machine reports the RUNNER, not the verb's verdict. A verb that
/// answered with a non-zero exit ran fine — `run-car-probe` answers 75
/// ("not yet") on most passes, and a probe judged false is the car's
/// business, which the shed region already reads. What IS the runner's
/// failure: a refusal, and a request closed with no answer recorded at
/// all.
fn runner_machine(inputs: &RegionInputs<'_>, verb: &str, name: &str) -> Machine {
    let id = format!("runner:{verb}");
    let Some(rows) = inputs.ops_requests else {
        return machine(
            id,
            name,
            MachineState::Unknown,
            "the ops-request rows could not be read".to_string(),
        );
    };
    let newest = rows
        .iter()
        .filter(|(j, _)| j.metadata.get("verb").and_then(Value::as_str) == Some(verb))
        .max_by_key(|(j, _)| opened_at(j));
    let Some((job, steps)) = newest else {
        return machine(
            id,
            name,
            MachineState::Unknown,
            format!(
                "no {verb} request in the window read — this runner declares no heartbeat, so its silence cannot be judged"
            ),
        );
    };
    judge_request(id, name, verb, job, steps)
}

/// What ONE ops-request says about the runner that took it — shared by
/// the verb runners above and the per-host runners below, so a
/// disposition means the same thing whichever glyph reads it.
fn judge_request(id: String, name: &str, verb: &str, job: &Job, steps: &[Step]) -> Machine {
    if job.status != boss_core::job::JobStatus::Closed {
        return machine(
            id,
            name,
            MachineState::Running,
            format!("a {verb} request is in flight"),
        );
    }
    let execute = find_step(steps, "execute", "Execute the verb");
    let disposition = execute
        .map(|s| md_str(&s.metadata, "disposition"))
        .unwrap_or("");
    let exit = execute
        .map(|s| md_str(&s.metadata, "exit_code"))
        .unwrap_or("");
    match disposition {
        "answered" => machine(
            id,
            name,
            MachineState::Idle,
            format!(
                "last {verb} answered{}{}",
                if exit.is_empty() {
                    String::new()
                } else {
                    format!(" exit {exit}")
                },
                match closed_at(job) {
                    Some(at) => format!(" at {}", at.format("%H:%MZ")),
                    None => String::new(),
                }
            ),
        ),
        "refused" => machine(
            id,
            name,
            MachineState::Failed,
            format!("the last {verb} request was refused — outside the allowlist"),
        ),
        _ => machine(
            id,
            name,
            MachineState::Failed,
            format!("the last {verb} request closed with no answer recorded"),
        ),
    }
}

/// THE HOSTS THE REGISTRY EXPECTS A RUNNER ON (backlog 49ed87b4).
///
/// The verb runners above are named by the requests they HAPPEN to
/// have answered, which is exactly the reading that cannot see a dead
/// host: a machine that answers nothing is drawn nowhere, and an
/// absent glyph is indistinguishable from a runner that does not
/// exist. The estate registry is the independent statement of which
/// hosts SHOULD be answering, so every declared host stands on the map
/// whether or not it has said anything — the false-empty class closed
/// at its most consequential point.
///
/// They are THE PLANT ([`Regions::plant`]; design 62de32ae, decision
/// 11). They stood in receiving until 2026-09-24, on the reasoning that
/// an ops-request is an inbound platform kind; the review read that as
/// arbitrary — a host runner moves nothing into receiving, and it
/// answers the converge for arrivals and the probe for the shed alike.
/// Machinery that serves every region belongs to none of them.
///
/// A declared host with no request in the window is UNKNOWN, never
/// idle: a runner still declares no poll interval anywhere the system
/// of record can read, so its silence remains unjudgeable. That is the
/// heartbeat half of 49ed87b4 and it is deliberately not claimed here
/// — this half makes the silence VISIBLE, not readable.
pub(super) fn host_runner_machines(inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let Some(hosts) = inputs.runner_hosts else {
        return vec![machine(
            "runner:hosts".to_string(),
            "the ops runners",
            MachineState::Unknown,
            "the estate registry could not be read, so which hosts should have a runner is unknown"
                .to_string(),
        )];
    };
    hosts
        .iter()
        .map(|host| {
            let id = format!("runner:host:{}", host.id);
            let name = format!("{} runner", host.label);
            let Some(rows) = inputs.ops_requests else {
                return machine(
                    id,
                    &name,
                    MachineState::Unknown,
                    "the ops-request rows could not be read".to_string(),
                );
            };
            // ANY verb it answered is evidence the runner polled — a
            // `df` answer proves the loop is alive exactly as a
            // `converge` does.
            let newest = rows
                .iter()
                .filter(|(j, _)| md_str(&j.metadata, "host") == host.id)
                .max_by_key(|(j, _)| opened_at(j));
            match newest {
                Some((job, steps)) => {
                    let verb = md_str(&job.metadata, "verb");
                    let verb = if verb.is_empty() { "ops" } else { verb };
                    judge_request(id, &name, verb, job, steps)
                }
                None => machine(
                    id,
                    &name,
                    MachineState::Unknown,
                    format!(
                        "the estate registry declares an ops-runner on {}, and no request it answered is in the window; a runner declares no poll interval the record can read, so its silence cannot be judged",
                        host.id
                    ),
                ),
            }
        })
        .collect()
}

/// The machinery of one region, by name. A region this answers nothing
/// for has no machine of ours in it — which is a fact, not a gap.
/// THE CREWS ON THE FLOOR — one machine per open session (design
/// 511fa7d4 car 2b, backlog 94c6ffd0). A crew is the only machinery on
/// the map that is mostly a HUMAN or an agent's own session rather than
/// a loop of ours, and it is read the same way: `running` while the
/// heartbeat is fresh, `idle` past [`CREW_IDLE_HOURS`] of silence, and
/// `unknown` where nothing measured it.
///
/// A session that has never prompted is UNKNOWN, not idle. Idle is a
/// reading — "it is here and it has no work" — and the only thing that
/// can take it is the heartbeat the prompt hook writes. Before the
/// first prompt there is no such reading, and a confident idle would
/// say the operator walked away when nothing asked.
fn crew_machines(inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let Some(sessions) = inputs.sessions else {
        return vec![machine(
            "crews".to_string(),
            "crews",
            MachineState::Unknown,
            "the work-session packets could not be read".to_string(),
        )];
    };
    let runs_of = |id: &str| {
        inputs
            .agent_runs
            .unwrap_or(&[])
            .iter()
            .filter(|(j, _)| j.status == JobStatus::Open)
            .filter(|(j, _)| md_str(&j.metadata, "session") == id)
            .count()
    };
    sessions
        .iter()
        .map(|s| {
            let id = s.id.to_string();
            let name = {
                let actor = md_str(&s.metadata, "actor");
                if actor.is_empty() {
                    s.title.clone()
                } else {
                    actor.to_string()
                }
            };
            let working = plural(runs_of(&id), "run in flight", "runs in flight");
            let (state, why) = match meta_instant(&s.metadata, "last_active_at") {
                None => (
                    MachineState::Unknown,
                    format!(
                        "no heartbeat on the packet — nothing says whether anyone is here; {working}"
                    ),
                ),
                Some(beat) => {
                    let silent = (inputs.now - beat).num_minutes().max(0);
                    if silent > CREW_IDLE_HOURS * 60 {
                        (
                            MachineState::Idle,
                            format!("silent for {silent} min; {working}"),
                        )
                    } else {
                        (
                            MachineState::Running,
                            format!("last prompt {silent} min ago; {working}"),
                        )
                    }
                }
            };
            machine(format!("session:{id}"), &name, state, why)
        })
        .collect()
}

pub(super) fn machines_of(name: &str, inputs: &RegionInputs<'_>) -> Vec<Machine> {
    let mut out = match name {
        "shop-floor" => crew_machines(inputs),
        "gates" => gate_bays(inputs),
        "track" => vec![conductor_machine(inputs.conductor)],
        "marshalling" => station_machines(inputs),
        _ => Vec::new(),
    };
    out.extend(
        RUNNERS
            .iter()
            .filter(|(_, region, _)| *region == name)
            .map(|(verb, _, label)| runner_machine(inputs, verb, label)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// An ops-request as the runner leaves it: the packet plus the
    /// `execute` step the runner completes in one write.
    fn ops_request(
        verb: &str,
        status: JobStatus,
        disposition: Option<&str>,
        exit: Option<&str>,
    ) -> (Job, Vec<Step>) {
        let job = job(
            "ops-request",
            verb,
            status,
            json!({ "verb": verb, "opened_at": "2026-09-19T11:00:00Z", "closed_at": "2026-09-19T11:01:00Z" }),
        );
        let mut execute = step(&job, "execute", StepStatus::Completed, None);
        execute.metadata = json!({
            "disposition": disposition.unwrap_or(""),
            "exit_code": exit.unwrap_or(""),
        });
        (job, vec![execute])
    }

    /// A machine nobody can read is UNKNOWN, and unknown is its own
    /// reading — never the idle one. This is the whole point of the
    /// fourth state: a default of `idle` would have the world map draw
    /// a calm, confident machinery hall for a system nothing is
    /// measuring.
    #[test]
    fn an_unreadable_machine_is_unknown_and_never_idle() {
        let status = empty_status();
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), None);
        i.conductor = None;
        i.ops_requests = None;
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "track", "conductor"),
            MachineState::Unknown
        );
        assert_eq!(
            machine_state(&out, "shed", "runner:run-car-probe"),
            MachineState::Unknown
        );
        assert_eq!(
            machine_state(&out, "arrivals", "runner:converge"),
            MachineState::Unknown
        );
        assert_eq!(
            machine_state(&out, "marshalling", "stations"),
            MachineState::Unknown
        );
        // And nothing unknown is ever drawn as idle anywhere on the map.
        for r in &out.regions {
            for m in &r.machines {
                assert_ne!(
                    (m.state, m.why.contains("could not be read")),
                    (MachineState::Idle, true),
                    "{}/{}: {}",
                    r.name,
                    m.id,
                    m.why
                );
            }
        }
    }

    /// A conductor that declares no heartbeat cannot be judged silent —
    /// `yard::conductor_health` leaves `silent` false — so the machine
    /// says unknown rather than inheriting that permissive answer.
    #[test]
    fn the_conductor_is_running_idle_never_and_unknown_without_a_declared_heartbeat() {
        let status = empty_status();
        let heard = crate::yard::conductor_health(
            Some(t("2026-09-19T11:55:00Z")),
            Some("reconcile"),
            Some(0),
            Some(10),
            Some(t(NOW)),
        );
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.conductor = Some(&heard);
        assert_eq!(
            machine_state(&regions(&i), "track", "conductor"),
            MachineState::Running
        );

        let undeclared = crate::yard::conductor_health(
            Some(t("2026-09-19T01:00:00Z")),
            Some("reconcile"),
            Some(0),
            None,
            Some(t(NOW)),
        );
        i.conductor = Some(&undeclared);
        assert_eq!(
            machine_state(&regions(&i), "track", "conductor"),
            MachineState::Unknown
        );
    }

    /// A failed machine troubles its territory at WORLD scale: the
    /// region turns troubled and the machine's own sentence leads the
    /// `why`, so the map names the failure without a zoom.
    #[test]
    fn a_silent_conductor_fails_its_machine_and_troubles_the_track() {
        let status = empty_status();
        let silent = crate::yard::conductor_health(
            Some(t("2026-09-19T10:00:00Z")),
            Some("reconcile"),
            Some(0),
            Some(10),
            Some(t(NOW)),
        );
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.conductor = Some(&silent);
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "track", "conductor"),
            MachineState::Failed
        );
        let track = by_name(&out, "track");
        assert_eq!(track.state, RegionState::Troubled);
        assert!(track.why.starts_with("conductor: SILENT"), "{}", track.why);
        // The region's own reading is kept behind the machine's, not
        // overwritten — a verdict adds to the record, it does not
        // replace it.
        assert!(track.why.contains("in transit"), "{}", track.why);
    }

    /// The runner machine reports the RUNNER, not the verb's verdict:
    /// `run-car-probe` answers 75 ("not yet") on most passes and the
    /// runner is fine. A refusal is its failure.
    #[test]
    fn a_runner_that_answered_is_idle_whatever_the_verb_exited_and_a_refusal_fails_it() {
        let status = empty_status();
        let answered = [ops_request(
            "run-car-probe",
            JobStatus::Closed,
            Some("answered"),
            Some("75"),
        )];
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.ops_requests = Some(&answered);
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "shed", "runner:run-car-probe"),
            MachineState::Idle
        );
        assert_eq!(by_name(&out, "shed").state, RegionState::Clear);

        let refused = [ops_request(
            "converge",
            JobStatus::Closed,
            Some("refused"),
            None,
        )];
        i.ops_requests = Some(&refused);
        let out = regions(&i);
        assert_eq!(
            machine_state(&out, "arrivals", "runner:converge"),
            MachineState::Failed
        );
        assert_eq!(by_name(&out, "arrivals").state, RegionState::Troubled);
        // The probe runner has no request in these rows at all — which
        // is unknown, not idle: it declares no heartbeat.
        assert_eq!(
            machine_state(&out, "shed", "runner:run-car-probe"),
            MachineState::Unknown
        );

        let in_flight = [ops_request("converge", JobStatus::Open, None, None)];
        i.ops_requests = Some(&in_flight);
        assert_eq!(
            machine_state(&regions(&i), "arrivals", "runner:converge"),
            MachineState::Running
        );
    }

    /// A gate bay is the one machine that can honestly be idle: the
    /// policy declares how many bays there are and this process counts
    /// what stands in them, so an empty bay is OBSERVED empty.
    #[test]
    fn every_gate_bay_the_policy_declares_is_a_machine_and_a_corpse_fails_one() {
        let run = job(
            "gate-run",
            "gate feat/x",
            JobStatus::Open,
            json!({ "branch": "feat/x", "opened_at": "2026-09-17T00:00:00Z" }),
        );
        let runs = [run];
        let status = build_status_for(
            YardInputs {
                gate_runs: &runs
                    .iter()
                    .map(|j| (j.clone(), Vec::new()))
                    .collect::<Vec<_>>(),
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let bays = machines_in(&out, "gates");
        assert_eq!(
            bays.len(),
            usize::try_from(status.gates.capacity).unwrap(),
            "one machine per declared bay"
        );
        // The run opened two days ago, so the yard calls it stale — a
        // corpse holding the bay, which fails that bay and troubles the
        // gates.
        assert_eq!(
            machine_state(&out, "gates", "gate-bay-1"),
            MachineState::Failed
        );
        assert_eq!(
            machine_state(&out, "gates", "gate-bay-2"),
            MachineState::Idle
        );
        assert_eq!(by_name(&out, "gates").state, RegionState::Troubled);
    }

    /// A station whose flow the cube cannot count is UNKNOWN even while
    /// it holds work — the reading an idle glyph would lie about.
    #[test]
    fn a_station_with_uncountable_flow_is_unknown_and_an_over_limit_one_fails() {
        let status = empty_status();
        let rows = [
            StationReading {
                name: "design-review".into(),
                over_limit: false,
                members: vec!["a".into(), "b".into()],
                served: None,
                previous_served: None,
                opened: Default::default(),
            },
            StationReading {
                name: "backlog".into(),
                over_limit: true,
                members: vec!["c".into()],
                served: Some(3),
                previous_served: Some(2),
                opened: Default::default(),
            },
            StationReading {
                name: "quiet".into(),
                over_limit: false,
                members: vec![],
                served: Some(0),
                previous_served: Some(0),
                opened: Default::default(),
            },
        ];
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&rows)));
        assert_eq!(
            machine_state(&out, "marshalling", "station:design-review"),
            MachineState::Unknown
        );
        assert_eq!(
            machine_state(&out, "marshalling", "station:backlog"),
            MachineState::Failed
        );
        assert_eq!(
            machine_state(&out, "marshalling", "station:quiet"),
            MachineState::Idle
        );
    }

    /// The verbs the handler fetches are DERIVED from the runner table
    /// the map draws from, so the read and the drawing cannot drift
    /// (CLAUDE.md §9a).
    #[test]
    fn the_runner_verbs_the_handler_reads_are_the_runners_the_map_draws() {
        assert_eq!(runner_verbs(), vec!["converge", "run-car-probe"]);
    }

    /// The same ops-request, filed against a host — what a runner reads
    /// to decide a packet is its own (`ops-runner.sh`: metadata.host
    /// equals HOST_ID).
    fn on_host(mut r: (Job, Vec<Step>), host: &str) -> (Job, Vec<Step>) {
        r.0.metadata
            .as_object_mut()
            .expect("job metadata is an object")
            .insert("host".to_string(), json!(host));
        r
    }

    fn host(id: &str) -> RunnerHost {
        RunnerHost {
            id: id.to_string(),
            label: id.to_string(),
        }
    }

    /// THE DEAD HOST IS DRAWN. A runner named only by what it answered
    /// is invisible the moment it stops answering, and an absent glyph
    /// is indistinguishable from a runner that does not exist. The
    /// registry says which hosts SHOULD have one, so the host with
    /// nothing in the window is a machine on the map — unknown, because
    /// a runner still declares no poll interval, never idle.
    #[test]
    fn a_declared_runner_host_with_nothing_in_the_window_is_drawn_unknown_not_absent() {
        let status = empty_status();
        let hosts = [host("forge"), host("boss-gcp")];
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.runner_hosts = Some(&hosts);
        let out = regions(&i);
        let drawn: Vec<&str> = out.plant.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(drawn, vec!["runner:host:forge", "runner:host:boss-gcp"]);
        let m = out
            .plant
            .iter()
            .find(|m| m.id == "runner:host:boss-gcp")
            .expect("the declared host is drawn");
        assert_eq!(m.state, MachineState::Unknown);
        assert!(
            m.why.contains("declares an ops-runner"),
            "the why names the registry that expects it: {}",
            m.why
        );
    }

    /// A host's runner is judged from ANY verb it answered — the
    /// evidence is the runner polling, not what the verb decided. A
    /// refusal and an answerless close are the runner's own failures.
    #[test]
    fn a_runner_host_is_judged_from_any_verb_it_answered() {
        let status = empty_status();
        let hosts = [host("boss-gcp")];
        let rows = [on_host(
            ops_request("uptime", JobStatus::Closed, Some("answered"), Some("0")),
            "boss-gcp",
        )];
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.runner_hosts = Some(&hosts);
        i.ops_requests = Some(&rows);
        assert_eq!(
            plant_state(&regions(&i), "runner:host:boss-gcp"),
            MachineState::Idle
        );

        let refused = [on_host(
            ops_request("converge", JobStatus::Closed, Some("refused"), None),
            "boss-gcp",
        )];
        i.ops_requests = Some(&refused);
        let out = regions(&i);
        assert_eq!(
            plant_state(&out, "runner:host:boss-gcp"),
            MachineState::Failed
        );
        assert_eq!(by_name(&out, "receiving").state, RegionState::Clear);

        let in_flight = [on_host(
            ops_request("df", JobStatus::Open, None, None),
            "boss-gcp",
        )];
        i.ops_requests = Some(&in_flight);
        assert_eq!(
            plant_state(&regions(&i), "runner:host:boss-gcp"),
            MachineState::Running
        );

        // Another host's request is not this host's evidence.
        let elsewhere = [on_host(
            ops_request("uptime", JobStatus::Closed, Some("answered"), Some("0")),
            "forge",
        )];
        i.ops_requests = Some(&elsewhere);
        assert_eq!(
            plant_state(&regions(&i), "runner:host:boss-gcp"),
            MachineState::Unknown
        );
    }

    /// An unread estate registry is ONE unknown machine, never an
    /// estate with no runners in it — the false-empty class the whole
    /// machinery reading exists to refuse.
    #[test]
    fn an_unread_estate_registry_is_unknown_and_never_an_empty_estate() {
        let status = empty_status();
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.runner_hosts = None;
        let out = regions(&i);
        let m = out
            .plant
            .iter()
            .find(|m| m.id == "runner:hosts")
            .expect("an unread registry is still a machine");
        assert_eq!(m.state, MachineState::Unknown);
        assert!(
            m.why.contains("could not be read"),
            "the why names the read that failed: {}",
            m.why
        );
    }

    /// The hosts the handler reads are DERIVED from the same role the
    /// map draws on, and a retired machine is not expected to answer
    /// (CLAUDE.md §9a: one definition, not two lists).
    #[test]
    fn the_runner_hosts_are_the_nodes_declaring_the_role_and_never_a_retired_one() {
        let node = |id: &str, roles: &[&str], retired: bool| crate::port::EstateNode {
            id: id.to_string(),
            label: format!("{id} label"),
            address: "10.0.0.1".to_string(),
            role: "forge".to_string(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            cpu: None,
            memory_gb: None,
            disk_gb: None,
            notes: None,
            retired,
        };
        let nodes = [
            node("forge", &[OPS_RUNNER_ROLE, "cluster-operator"], false),
            node("w-1", &[], false),
            node("old", &[OPS_RUNNER_ROLE], true),
        ];
        assert_eq!(
            runner_hosts_of(&nodes),
            vec![RunnerHost {
                id: "forge".to_string(),
                label: "forge label".to_string()
            }]
        );
    }

    /// THE HOST RUNNERS ARE THE PLANT (decision 11). They serve every
    /// region, so they stand in none: receiving carries no host runner,
    /// and a failed one troubles no region — it blinks on the plant
    /// strip, where the review asked for it.
    #[test]
    fn the_host_runners_stand_in_the_plant_and_in_no_region() {
        let status = empty_status();
        let hosts = [host("forge")];
        let refused = [on_host(
            ops_request("converge", JobStatus::Closed, Some("refused"), None),
            "forge",
        )];
        let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
        i.runner_hosts = Some(&hosts);
        i.ops_requests = Some(&refused);
        let out = regions(&i);
        assert!(
            out.regions
                .iter()
                .all(|r| r.machines.iter().all(|m| !m.id.starts_with("runner:host"))),
            "no region carries a host runner"
        );
        assert_eq!(
            out.plant.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["runner:host:forge"]
        );
        assert_eq!(out.plant[0].state, MachineState::Failed);
        assert_eq!(by_name(&out, "receiving").state, RegionState::Clear);
        // The HUD's machine cell (design 00774ca8 decision 3) still
        // counts it — ONCE, under the plant, neither dropped with its
        // move out of receiving nor counted in a region as well.
        let cell = out.machines.as_ref().expect("the cell is answered");
        let in_regions: usize = out.regions.iter().map(|r| r.machines.len()).sum();
        assert_eq!(cell.total, in_regions + out.plant.len());
        let named: Vec<(&str, &str)> = cell
            .failed_or_unknown
            .iter()
            .filter(|m| m.id.starts_with("runner:host"))
            .map(|m| (m.region.as_str(), m.id.as_str()))
            .collect();
        assert_eq!(named, vec![("plant", "runner:host:forge")]);
    }
}
