//! Stranded greens and the convergence-overdue alarm.

use super::*;

/// Why a green with no car is worth an alarm — which decides both the
/// window it is judged against and what the packet says failed
/// (CLAUDE.md §Diagnosis: "a verdict must name what failed").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrandCause {
    /// The gate carried a `--park-*` intent, so `jobs.auto-park` owed
    /// this green a car and recorded neither a car nor a `park_skipped`
    /// decision. The handler failed; the operator does not need to wait
    /// out a threshold to be told so.
    AutoParkFailed,
    /// A human gated without a park intent and never parked it. Nothing
    /// is broken — it is forgotten — so it is judged on elapsed time.
    NeverParked,
}

impl StrandCause {
    pub(super) fn from_intent(park_intent: bool) -> Self {
        if park_intent {
            Self::AutoParkFailed
        } else {
            Self::NeverParked
        }
    }
}

/// A stranded green worth an alarm: a gate-run that went GREEN, whose
/// branch no car ever claimed, that has sat that way past the window
/// its cause is judged on, and that no open alarm already names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StrandedGreen {
    pub branch: String,
    pub gate_run_id: String,
    pub age_mins: i64,
    pub cause: StrandCause,
}

/// The two windows this alarm runs on, and why there are two.
///
/// MEASURED 2026-09-09 against the system of record, over the 12 most
/// recent green gate-runs that carried a park intent: 11 had their car
/// filed 0.6 SECONDS after the verdict — `jobs.auto-park` fires on the
/// `step.done.gate-verdict` event, so the green-to-parked path is
/// sub-second, not minutes. A single 45-minute threshold was therefore
/// not "the ordinary gap plus margin"; it was a wait for something that
/// either happened instantly or was never going to happen.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StrandWindows {
    /// For [`StrandCause::NeverParked`] — a human's forgotten green.
    /// Elapsed time is the only signal there is, so this stays a
    /// threshold, now measured from the VERDICT rather than from the
    /// gate's start (see [`freshest_green`]).
    pub never_parked_mins: i64,
    /// For [`StrandCause::AutoParkFailed`] — grace for the handler,
    /// not a wait for the happy path: sized to cover a dispatcher
    /// restart or a redelivery, ~1000x the measured 0.6s latency.
    pub auto_park_grace_mins: i64,
}

impl StrandWindows {
    fn for_cause(&self, cause: StrandCause) -> i64 {
        match cause {
            StrandCause::AutoParkFailed => self.auto_park_grace_mins,
            StrandCause::NeverParked => self.never_parked_mins,
        }
    }
}

/// The FRESHEST green gate-run for a branch: its id, how long ago it
/// went green, and whether it carried a park intent.
///
/// AGE BASIS, in preference order — every one of them dates the
/// VERDICT, never the gate's start:
///   1. the verdict step's own `completed_at` column, which the jobs
///      API stamps on every completion (`automation:gate-runner`);
///   2. the same key inside the step's `metadata`, honoured for free in
///      case a runner ever writes one there;
///   3. the run's `metadata.closed_at` — the outcome rule closes the
///      packet ~0.2s after the verdict lands.
///
/// `opened_at` IS NOT A BASIS, and using it is the defect this
/// replaces. A gate-run opens BEFORE it goes green, so dating the green
/// from the open over-states its age by the whole gate duration —
/// measured 2026-09-09 over the 21 most recent green runs: median 18.9
/// min, max 26.1 min, and a run that waits for a concurrency slot
/// (`queued_at`) can be much longer. Against a 45-minute threshold that
/// left an effective post-green window of 45 − duration, sometimes
/// ZERO: the alarm could fire on a green the instant the verdict landed,
/// ahead of the auto-park handler it was supposedly waiting for. Four
/// false STRANDED GREEN packets on 2026-09-09 (e60398dc) are what that
/// looks like from the queue.
///
/// No dateable green on ANY of the branch's runs → `None`, and the
/// branch is left for a later pass rather than alarmed on a guessed age
/// (the `dead_gate_run_hours` idiom: absence of a stamp is not
/// evidence).
pub(super) fn freshest_green(
    gate_runs: &[Value],
    branch: &str,
    now: DateTime<Utc>,
) -> Option<(String, i64, bool)> {
    let mut best: Option<(String, DateTime<Utc>, bool)> = None;
    for g in gate_runs {
        let md = metadata_map(g);
        if md.get("branch").and_then(Value::as_str) != Some(branch) {
            continue;
        }
        // The same green flag the shared definition keys on: any step
        // whose metadata.verdict is green.
        let green_step = g
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| {
                s.get("metadata")
                    .and_then(|m| m.get("verdict"))
                    .and_then(Value::as_str)
                    == Some(boss_jobs::stranded::VERDICT_GREEN)
            });
        let Some(green_step) = green_step else {
            continue;
        };
        let Some(at) = green_step
            .get("completed_at")
            .and_then(Value::as_str)
            .or_else(|| {
                green_step
                    .get("metadata")
                    .and_then(|m| m.get("completed_at"))
                    .and_then(Value::as_str)
            })
            .or_else(|| md.get("closed_at").and_then(Value::as_str))
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
        else {
            continue;
        };
        let id = g
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let intent = boss_jobs::stranded::park_intent(g.get("metadata").unwrap_or(&Value::Null));
        match &best {
            Some((_, best_at, _)) if *best_at >= at => {}
            _ => best = Some((id, at, intent)),
        }
    }
    best.map(|(id, at, intent)| (id, (now - at).num_minutes(), intent))
}

/// Which stranded greens are past their window AND not already
/// alarmed — the pure decision the reconcile alarm rides on. Detection
/// (green + no car + no spent marker + not held) is
/// `census::stranded_gate_runs`, which reads `boss_jobs::stranded`, the
/// ONE definition the yard read-model uses too (CLAUDE.md §9a); this
/// layers the cause-specific window (a just-gated green is not stranded
/// yet; a green whose auto-park never ran is stranded almost at once)
/// and the dedup (`already_alarmed` is the branches an open alarm
/// already names, so a persisting strand is ONE packet, not one every
/// ten minutes).
pub(crate) fn stranded_greens_to_alarm(
    gate_runs: &[Value],
    car_branches: &BTreeSet<String>,
    already_alarmed: &BTreeSet<String>,
    now: DateTime<Utc>,
    windows: StrandWindows,
) -> Vec<StrandedGreen> {
    let mut out: Vec<StrandedGreen> = Vec::new();
    for branch in crate::census::stranded_gate_runs(gate_runs, car_branches) {
        if already_alarmed.contains(&branch) {
            continue;
        }
        let Some((gate_run_id, age_mins, park_intent)) = freshest_green(gate_runs, &branch, now)
        else {
            continue;
        };
        let cause = StrandCause::from_intent(park_intent);
        if age_mins < windows.for_cause(cause) {
            continue;
        }
        out.push(StrandedGreen {
            branch,
            gate_run_id,
            age_mins,
            cause,
        });
    }
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// The stamp this alarm leaves when IT closes one of its own packets,
/// so a reader (and any future settled-suppression) can tell a machine
/// clear from a human's answer. The silence sweep's `cleared_by`
/// idiom, verbatim.
pub(crate) const STRANDED_CLEARED_BY: &str = "conductor.stranded-green-alarm";

/// Open alarms whose claim no longer holds: the branch parked, landed,
/// was re-railed, was held, or was superseded, so it is no longer in
/// `stranded_now`. Returns `(alarm id, branch)`.
///
/// THIS IS THE HALF THAT WAS MISSING. The alarm raised and nothing ever
/// revisited the claim, so a branch that parked a minute later left a
/// permanent false packet on the operator's queue — four of them on
/// 2026-09-09, every one closed by hand (e60398dc). An alarm that
/// cannot clear itself is a claim the system stops standing behind the
/// instant it stops being true.
pub(crate) fn stranded_alarms_to_clear(
    open_alarms: &[Value],
    stranded_now: &BTreeSet<String>,
) -> Vec<(String, String)> {
    open_alarms
        .iter()
        .filter_map(|j| {
            let branch = j
                .get("metadata")
                .and_then(|m| m.get("stranded_branch"))
                .and_then(Value::as_str)?;
            if stranded_now.contains(branch) {
                return None;
            }
            let id = j.get("id").and_then(Value::as_str)?;
            Some((id.to_string(), branch.to_string()))
        })
        .collect()
}

/// WHY a strand ended, named from the same data the detection reads —
/// so the clear says what happened rather than "no longer stranded".
pub(crate) fn stranded_clear_reason(
    gate_runs: &[Value],
    car_branches: &BTreeSet<String>,
    branch: &str,
) -> String {
    if car_branches.contains(branch) {
        return format!("a ship-a-change car now carries `{branch}`");
    }
    let run = gate_runs.iter().find(|g| {
        g.get("metadata")
            .and_then(|m| m.get("branch"))
            .and_then(Value::as_str)
            == Some(branch)
    });
    let md = run.and_then(|g| g.get("metadata")).unwrap_or(&Value::Null);
    if let Some(marker) = boss_jobs::stranded::spent_reason(md) {
        return format!("its gate-run is marked `{marker}` — the green is spent, not waiting");
    }
    if let Some(reason) = boss_jobs::stranded::hold_reason(md) {
        return format!("its gate-run is HELD on purpose: {reason}");
    }
    format!("no green gate-run for `{branch}` is unclaimed any more")
}

/// The measurement a STANDING alarm is refreshed with instead of being
/// twinned — `PATCH /api/jobs/{id}/metadata` merges top-level keys, so
/// this is exactly the fields that move between passes.
pub(crate) fn stranded_refresh_patch(a: &StrandedGreen, now: DateTime<Utc>) -> Value {
    json!({
        "gate_run_id": a.gate_run_id,
        "verdict_age_mins": a.age_mins,
        "stranded_cause": cause_key(a.cause),
        "last_measured_at": now.to_rfc3339(),
    })
}

/// The triage completion that CLOSES a standing alarm when the branch
/// stops being stranded. `disposition = "stale"` is the backlog-item
/// terminal titled "Closed — the claim no longer holds", which is
/// exactly the case. PUT on a step REPLACES top-level metadata, so the
/// step's existing keys are carried through.
pub(crate) fn stranded_clear_step_body(
    existing: &Map<String, Value>,
    branch: &str,
    why: &str,
) -> Value {
    let mut metadata = existing.clone();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "The conductor re-measured `{branch}` and it is no longer a stranded green: \
             {why}. The claim this alarm carried no longer holds; closed by machine, not \
             by judgement. A branch that strands again files a new packet."
        )),
    );
    metadata.insert("cleared_by".into(), json!(STRANDED_CLEARED_BY));
    json!({"status": "completed", "metadata": metadata})
}

fn cause_key(cause: StrandCause) -> &'static str {
    match cause {
        StrandCause::AutoParkFailed => "auto-park-failed",
        StrandCause::NeverParked => "never-parked",
    }
}

/// The backlog-item an unrescued stranded green becomes. Mirrors
/// `estate.alarm`'s raise (a5adfb99): a `backlog-item` on the
/// operator's queue the overdue/watchlist machinery can see, with the
/// dedup key in metadata. The key is `stranded_branch` — the branch,
/// stable across a re-gate (a new gate-run id would let the same strand
/// re-alarm; the branch will not). Priority is `standard`, not
/// `urgent`: a strand decays over days, not minutes, and an alarm that
/// cries urgent over non-urgent things trains operators to ignore it
/// (estate.alarm's own calibration lesson). The packet NAMES ITS CAUSE:
/// an auto-park that never filed is a broken actor, and a human's
/// forgotten green is not, and they are not rescued the same way.
pub(crate) fn stranded_alarm_body(
    a: &StrandedGreen,
    windows: StrandWindows,
    now: DateTime<Utc>,
    owner: &str,
) -> Value {
    let window = windows.for_cause(a.cause);
    let title = match a.cause {
        StrandCause::AutoParkFailed => format!(
            "STRANDED GREEN: {} gated green {}min ago and auto-park filed no car",
            a.branch, a.age_mins
        ),
        StrandCause::NeverParked => format!(
            "STRANDED GREEN: {} gated green {}min ago, never parked",
            a.branch, a.age_mins
        ),
    };
    let cause_detail = match a.cause {
        StrandCause::AutoParkFailed => format!(
            "This green CARRIED a `--park-*` intent, so `jobs.auto-park` owed it a car and \
             recorded neither a car nor a `park_skipped` decision within {window}min — \
             measured 2026-09-09, the handler files in 0.6 SECONDS when it runs, so this is \
             the handler having failed, not a slow happy path. Check the dispatcher: the \
             rule on `step.done.gate-verdict`, and the handler's journal for this gate-run."
        ),
        StrandCause::NeverParked => format!(
            "This green carried NO park intent — a hand gate that was never parked — and has \
             sat that way for {}min (window {window}min).",
            a.age_mins
        ),
    };
    json!({
        "kind": "backlog-item",
        "status": "open",
        "title": title,
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        // The platform owner as the registry answers it (backlog
        // 3c23662d), or nobody for the jobs API to resolve from the
        // kind's owner_role — never a literal person. Same on every
        // alarm below.
        "owner_id": owner,
        "priority": "standard",
        // No `opened_on`: the create handler injects it off its clock
        // and stamps the filing instant as `metadata.opened_at` only
        // when it does — a body-sent date silenced the stamp on every
        // conductor alarm (dd3624a0, 2026-09-15; envelope: a7a07ffb).
        "tags": ["pipeline", "gate"],
        "metadata": {
            "area": "pipeline",
            "stranded_branch": a.branch,
            "gate_run_id": a.gate_run_id,
            "verdict_age_mins": a.age_mins,
            "stranded_cause": cause_key(a.cause),
            "last_measured_at": now.to_rfc3339(),
            "detail": format!(
                "Gate-run {} for `{}` went GREEN {}min ago but no ship-a-change car ever \
                 claimed the branch: it gated, was never parked, so it never reached the \
                 dock and cannot board. {} A stranded green DECAYS: gated yesterday, \
                 unmergeable today, and a later blind rescue reverts landed work (the decay \
                 jobs.auto-park was built to end, 2026-09-01). RESCUE = rebase the branch \
                 onto current origin/main + re-gate (its base has likely moved); never \
                 rebuild blind. If the change already landed via another branch, close this \
                 stale. The age is measured from the VERDICT, not from the gate's start. \
                 This packet CLOSES ITSELF when the branch parks, lands, is held or is \
                 re-railed — if it is still open, the claim still holds.",
                id8(&a.gate_run_id),
                a.branch,
                a.age_mins,
                cause_detail,
            ),
        },
    })
}

/// The alarm a merged train whose commit never reaches the cluster
/// becomes (fdff316c / 7e5ee013). Pure so the shape is pinned by tests.
/// The packet a persistent non-departure files (backlog 6baabd43).
///
/// WHAT IT CARRIES IS THE POINT. The conductor already knows exactly
/// why — which branches conflicted, which edge cannot be satisfied,
/// what the consist check refused — and on 2026-09-19 it wrote all of
/// that to its own stdout every sixty seconds for nine and a half
/// hours while no train departed and nothing read a word of it. The
/// packet's own conclusion: prefer the conductor's signal over a
/// dock-depth alarm, "because it is the only one of the three that
/// knows WHY, and a stall alarm that carries the conflicting branches
/// and files is actionable on arrival, whereas a dock-depth alarm
/// sends someone to go and find out". So the refusal's own journal
/// line — the one a reader would otherwise have to go and grep — is
/// the message.
///
/// THE TRACK IS PROVABLY CLEAR when this fires: the conductor returns
/// early on `BOARDING HELD — track occupied`, so anything reaching a
/// refusal has already passed that check. This is a stopped pipeline,
/// not a busy one, and that is why no timer is needed to tell them
/// apart.
pub(crate) fn no_departure_alarm_body(line: &str, owner: &str, now: DateTime<Utc>) -> Value {
    json!({
        "kind": "user-feedback",
        "status": "open",
        "title": "Boarding stalled: the dock cannot depart and the track is clear",
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "tags": ["pipeline", "train"],
        "owner_id": owner,
        "priority": "urgent",
        "metadata": {
            "message": format!(
                "The conductor refused to board on a reason that will not clear itself, \
                 with no train on the track. Its own line for this window:\n\n{line}\n\n\
                 Filed once and deduplicated by this packet staying open — while it is \
                 open no twin is filed, so closing it is what re-arms the alarm. It was \
                 filed because the identical refusal repeats every sixty seconds until \
                 someone acts: on 2026-09-19 that ran from 04:27Z to 13:49Z, perfectly \
                 diagnosed in the journal and read by nobody (backlog 6baabd43)."
            ),
            "input_channel": "telemetry/monitoring",
            // WHEN the stall started, to the minute. The Job's own
            // `opened_on` is a DATE and cannot answer "has this been
            // unread for half an hour?", which is the question the
            // escalation ladder asks every window
            // (`boarding::stall_escalation`, backlog 94896e74). Staying
            // open is still the whole dedup; this is what lets the
            // number on the packet grow while it does.
            "stalled_since": now.to_rfc3339(),
            "escalation_level": 0,
        },
    })
}

pub(crate) fn convergence_overdue_alarm_body(
    tid: &str,
    merge_ref: &str,
    mins_since_merge: i64,
    reported: &str,
    threshold_mins: i64,
    owner: &str,
) -> Value {
    json!({
        "kind": "user-feedback",
        "status": "open",
        "title": format!(
            "Cluster convergence overdue: train {} merged {mins_since_merge} min ago",
            id8(tid)
        ),
        "subject": {"subject_kind": "custom", "id": "cluster-convergence"},
        "tags": ["deploy", "pipeline"],
        "owner_id": owner,
        "priority": "urgent",
        // No `opened_on`: see stranded_alarm_body (dd3624a0).
        "metadata": {
            "message": format!(
                "The train merged {merge_ref} {mins_since_merge} minutes ago and \
                 the cluster's running binary still reports {reported} — past the \
                 {threshold_mins}-minute threshold (BOSS_TRAIN_CONVERGE_ALARM_MINS). Filed by \
                 the conductor's converged step (fdff316c / 7e5ee013): the likely \
                 suspects are the deploy-runner timer on the forge host, the image \
                 build failing, or the rollout wedged — check \
                 cluster-deploy-runner's journal first. The train's arrival report \
                 will not fire until convergence verifies."
            ),
            "train": tid,
        },
    })
}

#[cfg(test)]
mod stranded_green_tests {
    use super::{
        StrandCause, StrandWindows, StrandedGreen, convergence_overdue_alarm_body,
        stranded_alarm_body, stranded_alarms_to_clear, stranded_clear_reason,
        stranded_clear_step_body, stranded_greens_to_alarm, stranded_refresh_patch,
    };
    use chrono::{TimeZone, Utc};
    use serde_json::{Map, json};
    use std::collections::BTreeSet;

    /// The windows the live conductor runs on: 45 min for a hand-gated
    /// green, 10 min of grace for auto-park.
    fn windows() -> StrandWindows {
        StrandWindows {
            never_parked_mins: 45,
            auto_park_grace_mins: 10,
        }
    }

    /// A green gate-run for `branch`, opened `opened` (RFC3339), with an
    /// optional verdict-step `completed_at` and optional extra metadata
    /// (e.g. `superseded`, `park_summary`). Mirrors a real gate-run: the
    /// verdict lives on the `record-verdict` step, and `completed_at` is
    /// the STEP'S OWN column, which the jobs API stamps on every
    /// completion — the same place a live packet carries it.
    fn green_run(
        id: &str,
        branch: &str,
        opened: &str,
        completed_at: Option<&str>,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut md = json!({"branch": branch, "opened_at": opened});
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                md[k] = v.clone();
            }
        }
        let mut step = json!({
            "title": "Record the gate verdict", "spec_slug": "record-verdict",
            "status": "completed", "metadata": {"verdict": "green"}
        });
        if let Some(c) = completed_at {
            step["completed_at"] = json!(c);
        }
        json!({
            "id": id, "kind": "gate-run", "status": "closed", "metadata": md,
            "steps": [step]
        })
    }

    /// The same run with a `--park-*` intent stamped: the shape
    /// `boss gate --park-summary …` leaves, and the flag
    /// `jobs.auto-park` keys on.
    fn green_run_with_intent(
        id: &str,
        branch: &str,
        opened: &str,
        completed_at: Option<&str>,
    ) -> serde_json::Value {
        green_run(
            id,
            branch,
            opened,
            completed_at,
            json!({"park_summary": "does the thing"}),
        )
    }

    fn branches(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The reason this alarm exists: a hand-gated green with no car,
    /// aged past its window, no open alarm — selected, carrying its
    /// branch, gate-run id, verdict age and CAUSE.
    #[test]
    fn a_stranded_green_past_threshold_is_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        // Went green 90 min ago.
        let runs = [green_run(
            "gr-1",
            "fix/stranded",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:30:00Z"),
            json!({}),
        )];
        let got = stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows());
        assert_eq!(
            got,
            vec![StrandedGreen {
                branch: "fix/stranded".into(),
                gate_run_id: "gr-1".into(),
                age_mins: 90,
                cause: StrandCause::NeverParked,
            }]
        );
    }

    /// THE FALSE-ALARM REGRESSION (e60398dc). The age was read off the
    /// gate-run's `opened_at` — when the GATE STARTED — so a gate that
    /// ran 30 minutes and went green 1 minute ago read as 31 minutes
    /// stranded, and a slower one crossed the threshold before auto-park
    /// could possibly have filed. Measured 2026-09-09: green gates run a
    /// median 18.9 min. Age is now dated from the VERDICT, so a green
    /// that just landed is fresh no matter how long its gate took.
    #[test]
    fn a_long_gate_that_just_went_green_is_not_stranded() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-slow",
            "fix/slow-gate",
            // Opened 3h ago — under the old opened_at basis this alarms
            // instantly; the verdict landed 1 minute ago.
            "2026-09-07T09:00:00Z",
            Some("2026-09-07T11:59:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty(),
            "a green one minute old is not stranded, however long its gate ran"
        );
    }

    /// A green with NO dateable verdict is left for a later pass rather
    /// than alarmed on the gate's start time: absence of a stamp is not
    /// evidence.
    #[test]
    fn a_green_with_no_verdict_stamp_is_left_alone() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-nostamp",
            "fix/nostamp",
            "2026-09-07T06:00:00Z",
            None,
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// The run's own `closed_at` dates the verdict when a step stamp is
    /// missing — the outcome rule closes the packet a fraction of a
    /// second after the verdict lands.
    #[test]
    fn closed_at_dates_the_verdict_when_the_step_has_no_stamp() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-closed",
            "fix/closed",
            "2026-09-07T06:00:00Z",
            None,
            json!({"closed_at": "2026-09-07T10:00:00Z"}),
        )];
        let got = stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].age_mins, 120);
    }

    /// A just-gated green is NOT stranded — its car is moments away
    /// (jobs.auto-park files it on the green event).
    #[test]
    fn a_fresh_green_inside_the_window_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-2",
            "fix/fresh",
            "2026-09-07T09:00:00Z",
            Some("2026-09-07T11:50:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// A green that CARRIED a park intent is judged on the auto-park
    /// grace, not the 45-minute human window: the handler files in 0.6
    /// SECONDS (measured 2026-09-09), so 20 minutes with no car is the
    /// handler having failed, and the packet says so.
    #[test]
    fn an_intent_carrying_green_alarms_on_the_grace_and_names_auto_park() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run_with_intent(
            "gr-intent",
            "fix/auto",
            "2026-09-07T11:20:00Z",
            Some("2026-09-07T11:40:00Z"),
        )];
        let got = stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows());
        assert_eq!(got.len(), 1, "20 min with no car is past the 10-min grace");
        assert_eq!(got[0].cause, StrandCause::AutoParkFailed);
        let body = stranded_alarm_body(&got[0], windows(), now, "emp-owner");
        assert_eq!(body["metadata"]["stranded_cause"], "auto-park-failed");
        let title = body["title"].as_str().unwrap();
        assert!(title.contains("auto-park"), "the title names it: {title}");
    }

    /// Inside the grace, an intent-carrying green is not alarmed: a
    /// dispatcher restart or one redelivery must not read as a failure.
    #[test]
    fn an_intent_carrying_green_inside_the_grace_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run_with_intent(
            "gr-intent-fresh",
            "fix/auto",
            "2026-09-07T11:20:00Z",
            Some("2026-09-07T11:55:00Z"),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// The other side of the same split: a HAND-gated green 20 minutes
    /// old is not alarmed — nothing is broken, and a human's
    /// gate-then-park gap is not a defect.
    #[test]
    fn a_hand_gated_green_gets_the_longer_window() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-hand",
            "fix/hand",
            "2026-09-07T11:20:00Z",
            Some("2026-09-07T11:40:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// A green whose branch became a car is not stranded (the shared
    /// definition filters it) — proves the reuse of
    /// `census::stranded_gate_runs`.
    #[test]
    fn a_green_with_a_car_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-4",
            "fix/parked",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:10:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(
                &runs,
                &branches(&["fix/parked"]),
                &branches(&[]),
                now,
                windows()
            )
            .is_empty()
        );
    }

    /// A green `jobs.auto-park` deliberately SKIPPED — the branch had
    /// landed, or its car was already aboard a train — is a recorded
    /// decision, not a strand. It never alarms.
    #[test]
    fn an_auto_park_skipped_green_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-skipped",
            "fix/landed",
            "2026-09-07T09:00:00Z",
            Some("2026-09-07T09:10:00Z"),
            json!({"park_summary": "does the thing", "park_skipped": "landed"}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// A branch an open alarm already names is skipped — the dedup that
    /// stops a flood of one packet every ten minutes.
    #[test]
    fn an_already_alarmed_branch_is_deduped() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-5",
            "fix/stranded",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:10:00Z"),
            json!({}),
        )];
        assert_eq!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows()).len(),
            1
        );
        assert!(
            stranded_greens_to_alarm(
                &runs,
                &branches(&[]),
                &branches(&["fix/stranded"]),
                now,
                windows()
            )
            .is_empty()
        );
    }

    /// TRUNCATION REGRESSION at the stranded-green dedup. The existing
    /// alarm for a strand can sit beyond a single page of open
    /// backlog-items; a bare `limit=200` read that treats the truncated
    /// page as the whole set misses it and re-files every pass (the
    /// notification flood). Building the dedup set through
    /// `list_all_pages` — as `alarm_stranded_greens` does — gathers the
    /// alarm on page three, so the strand is deduped.
    #[tokio::test]
    async fn a_dedup_alarm_beyond_the_first_page_still_dedups() {
        use super::{PAGE_LIMIT, list_all_pages};
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let mut open: Vec<serde_json::Value> = (0..250)
            .map(|i| {
                json!({"id": format!("bi-{i}"),
                       "metadata": {"stranded_branch": format!("other/{i}")}})
            })
            .collect();
        open[220] = json!({"id": "bi-220", "metadata": {"stranded_branch": "fix/stranded"}});
        let open_ref = &open;
        let gathered = list_all_pages(|offset| async move {
            let page: Vec<serde_json::Value> = open_ref
                .iter()
                .skip(offset)
                .take(PAGE_LIMIT)
                .cloned()
                .collect();
            anyhow::Ok(Some(json!({
                "data": page, "total": open_ref.len(), "offset": offset, "limit": PAGE_LIMIT,
            })))
        })
        .await
        .unwrap();
        let already_alarmed: BTreeSet<String> = gathered
            .iter()
            .filter_map(|j| {
                j.get("metadata")?
                    .get("stranded_branch")?
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        let runs = [green_run(
            "gr-trunc",
            "fix/stranded",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:10:00Z"),
            json!({}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &already_alarmed, now, windows())
                .is_empty(),
            "the strand's alarm sits on page three; the paginated read must find it and dedup"
        );
        let truncated: BTreeSet<String> = open
            .iter()
            .take(200)
            .filter_map(|j| {
                j.get("metadata")?
                    .get("stranded_branch")?
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &truncated, now, windows()).len(),
            1,
            "a truncated 200-row read misses the alarm and re-files — the defect this fixes"
        );
    }

    /// A superseded green is dead, not waiting — it never alarms
    /// (rescue guidance pointing at a deleted branch is worse than
    /// none).
    #[test]
    fn a_superseded_green_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [green_run(
            "gr-6",
            "fix/superseded",
            "2026-09-07T10:00:00Z",
            Some("2026-09-07T10:10:00Z"),
            json!({"superseded": "by hand"}),
        )];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty()
        );
    }

    /// A HELD green is deliberately waiting, and a RE-RAILED one is
    /// spent — neither alarms; the unheld green beside them still does.
    /// This is the drift that filed four false packets on 2026-09-09:
    /// the yard excluded both and the alarm did not.
    #[test]
    fn a_held_or_rerailed_green_is_not_selected() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [
            green_run(
                "gr-7",
                "fix/held",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({"hold": "lands at the next restart"}),
            ),
            green_run(
                "gr-9",
                "fix/old",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({"rerailed_to": "fix/old-rerail"}),
            ),
            green_run(
                "gr-8",
                "fix/free",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({}),
            ),
        ];
        let out = stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows());
        assert_eq!(
            out.iter().map(|s| s.branch.as_str()).collect::<Vec<_>>(),
            vec!["fix/free"]
        );
    }

    /// The FRESHEST green among a branch's several runs decides age: a
    /// re-gate that just went green makes the branch fresh even if an
    /// older run for the same branch went green long ago.
    #[test]
    fn the_freshest_green_run_decides_age() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let runs = [
            green_run(
                "gr-old",
                "fix/regated",
                "2026-09-06T00:00:00Z",
                Some("2026-09-06T01:00:00Z"),
                json!({}),
            ),
            green_run(
                "gr-new",
                "fix/regated",
                "2026-09-07T11:40:00Z",
                Some("2026-09-07T11:50:00Z"),
                json!({}),
            ),
        ];
        assert!(
            stranded_greens_to_alarm(&runs, &branches(&[]), &branches(&[]), now, windows())
                .is_empty(),
            "the branch was re-gated 10 min ago — fresh, not stranded"
        );
    }

    /// The alarm packet a strand becomes: a backlog-item carrying the
    /// dedup key `stranded_branch`, a stable title, and the rescue
    /// guidance an operator acts on.
    #[test]
    fn the_alarm_packet_carries_the_dedup_key_and_rescue() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let a = StrandedGreen {
            branch: "fix/stranded".into(),
            gate_run_id: "gr-1".into(),
            age_mins: 90,
            cause: StrandCause::NeverParked,
        };
        let b = stranded_alarm_body(&a, windows(), now, "emp-owner");
        assert_eq!(b["kind"], "backlog-item");
        assert_eq!(b["status"], "open");
        assert_eq!(b["priority"], "standard");
        assert_eq!(b["owner_id"], "emp-owner", "the owner is the one handed in");
        assert_eq!(b["metadata"]["stranded_branch"], "fix/stranded");
        assert_eq!(b["metadata"]["gate_run_id"], "gr-1");
        assert_eq!(b["metadata"]["verdict_age_mins"], 90);
        assert_eq!(b["metadata"]["stranded_cause"], "never-parked");
        let title = b["title"].as_str().unwrap();
        assert!(
            title.contains("fix/stranded"),
            "title names the branch: {title}"
        );
        let detail = b["metadata"]["detail"].as_str().unwrap();
        assert!(
            detail.contains("rebase") && detail.contains("re-gate"),
            "detail carries the orient rescue guidance: {detail}"
        );
    }

    /// The create handler stamps `metadata.opened_at` — the precise
    /// filing instant behind the one-day `opened_on` — ONLY when its
    /// clock owns the date, i.e. when the body carries no `opened_on`
    /// (boss-jobs http/jobs.rs). Every conductor alarm sent
    /// `now.date_naive()`, so a stranded green, a blocked deploy and an
    /// overdue convergence all filed packets with no filing instant
    /// (backlog dd3624a0, 2026-09-15; the shared envelope was fixed the
    /// same way under a7a07ffb). `now` is the conductor's wall clock,
    /// never a backdated date, so the clock owns it.
    #[test]
    fn the_stranded_alarm_leaves_the_open_date_to_the_api_clock() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let a = StrandedGreen {
            branch: "fix/stranded".into(),
            gate_run_id: "gr-1".into(),
            age_mins: 90,
            cause: StrandCause::NeverParked,
        };
        let b = stranded_alarm_body(&a, windows(), now, "emp-owner");
        assert!(
            b.get("opened_on").is_none(),
            "`opened_on` must be left to the create handler's clock, \
             or the packet gets no `opened_at`: {b}"
        );
        assert_eq!(b["metadata"]["last_measured_at"], now.to_rfc3339());
    }

    /// Same rule, the convergence-overdue alarm (dd3624a0).
    #[test]
    fn the_convergence_overdue_alarm_leaves_the_open_date_to_the_api_clock() {
        let b = convergence_overdue_alarm_body(
            "abcdef12-0000-0000-0000-000000000000",
            "5c6ca038",
            50,
            "nothing",
            30,
            "emp-owner",
        );
        assert!(
            b.get("opened_on").is_none(),
            "`opened_on` must be left to the create handler's clock: {b}"
        );
        assert_eq!(b["kind"], "user-feedback");
        assert_eq!(
            b["title"],
            "Cluster convergence overdue: train abcdef12 merged 50 min ago"
        );
        assert_eq!(
            b["metadata"]["train"],
            "abcdef12-0000-0000-0000-000000000000"
        );
        let msg = b["metadata"]["message"].as_str().unwrap();
        assert!(
            msg.contains("5c6ca038") && msg.contains("nothing") && msg.contains("30-minute"),
            "{msg}"
        );
    }

    /// THE HALF THAT WAS MISSING (e60398dc). An alarm whose branch
    /// stopped being stranded — it parked, landed, was held or was
    /// re-railed — is closed by the conductor itself. The alarm for a
    /// strand that still holds is left alone.
    #[test]
    fn an_alarm_whose_strand_ended_is_cleared_and_a_live_one_is_not() {
        let open = [
            json!({"id": "bi-gone", "metadata": {"stranded_branch": "fix/parked-since"}}),
            json!({"id": "bi-live", "metadata": {"stranded_branch": "fix/still-stranded"}}),
            // Not one of ours: no dedup key, so not this alarm's packet.
            json!({"id": "bi-other", "metadata": {"area": "pipeline"}}),
        ];
        let got = stranded_alarms_to_clear(&open, &branches(&["fix/still-stranded"]));
        assert_eq!(
            got,
            vec![("bi-gone".to_string(), "fix/parked-since".to_string())]
        );
    }

    /// The clear NAMES WHAT CHANGED rather than saying "no longer
    /// stranded" — a car now carries it, a marker spent it, or an
    /// operator held it.
    #[test]
    fn the_clear_names_what_ended_the_strand() {
        let runs = [
            green_run(
                "gr-a",
                "fix/held",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({"hold": "lands at the next restart"}),
            ),
            green_run(
                "gr-b",
                "fix/rerailed",
                "2026-09-07T10:00:00Z",
                Some("2026-09-07T10:10:00Z"),
                json!({"rerailed_to": "fix/rerailed-rerail"}),
            ),
        ];
        let cars = branches(&["fix/parked"]);
        assert!(stranded_clear_reason(&runs, &cars, "fix/parked").contains("car now carries"));
        assert!(stranded_clear_reason(&runs, &cars, "fix/rerailed").contains("rerailed_to"));
        assert!(stranded_clear_reason(&runs, &cars, "fix/held").contains("HELD"));
    }

    /// The clear completes the triage step as `stale` — the
    /// backlog-item terminal titled "Closed — the claim no longer
    /// holds" — carries the step's existing keys through the PUT, and
    /// STAMPS ITSELF, so a machine clear is distinguishable from a
    /// human's answer and can never be read as one.
    #[test]
    fn the_clear_step_body_closes_stale_and_stamps_itself() {
        let mut existing = Map::new();
        existing.insert("authority_role".into(), json!("platform-admin"));
        let body = stranded_clear_step_body(&existing, "fix/parked", "a car now carries it");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["disposition"], "stale");
        assert_eq!(body["metadata"]["authority_role"], "platform-admin");
        assert_eq!(
            body["metadata"]["cleared_by"],
            json!(super::STRANDED_CLEARED_BY)
        );
        let evidence = body["metadata"]["evidence"].as_str().unwrap();
        assert!(
            evidence.contains("fix/parked") && evidence.contains("a car now carries it"),
            "the evidence names the branch and what changed: {evidence}"
        );
    }

    /// A STANDING alarm is refreshed with today's measurement rather
    /// than twinned — the metadata PATCH merges top-level keys.
    #[test]
    fn a_standing_alarm_is_refreshed_with_the_fresh_measurement() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let a = StrandedGreen {
            branch: "fix/stranded".into(),
            gate_run_id: "gr-2".into(),
            age_mins: 300,
            cause: StrandCause::AutoParkFailed,
        };
        let patch = stranded_refresh_patch(&a, now);
        assert_eq!(patch["verdict_age_mins"], 300);
        assert_eq!(patch["gate_run_id"], "gr-2");
        assert_eq!(patch["stranded_cause"], "auto-park-failed");
        assert!(
            patch["last_measured_at"]
                .as_str()
                .unwrap()
                .starts_with("2026-09-07")
        );
        assert!(
            patch.get("stranded_branch").is_none(),
            "the dedup key never moves — a refresh must not repoint the packet"
        );
    }
}
