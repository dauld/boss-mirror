//! THE TRACK: trains in transit — and the train-gate wait, one of the
//! two client-side predicates ported here once (`yard.ts::
//! trainGateTroubled`) and pinned against the TypeScript.

use super::*;

/// WHY the conductor has not filed a train's gate yet, verbatim from
/// the last failed launch — cleared the pass it is filed (0d16df6f).
/// The conductor writes it (`boss-cli/src/train_gate.rs` names this
/// constant), the yard reads it (`yard.ts::readTrainGate`), and so does
/// the track region below.
pub const TRAIN_GATE_WAIT_REASON: &str = "train_gate_wait_reason";
/// The gate could not be filed and CI alone judged the train — stamped
/// loudly on the train. Same three readers.
pub const TRAIN_GATE_FALLBACK: &str = "train_gate_fallback";

/// `yard.ts::trainGateTroubled`, ported: a train whose gate could not
/// be filed (fallback) or is still waiting at the bound (wait reason)
/// wears the yard's trouble style. A blank string is no marker, as
/// `readTrainGate`'s `text()` reads it.
pub fn train_gate_troubled(md: &Value) -> bool {
    [TRAIN_GATE_FALLBACK, TRAIN_GATE_WAIT_REASON]
        .iter()
        .any(|k| {
            md.get(k)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        })
}

/// WHAT A TRAIN'S OWN TROUBLE SAYS, judged in the region the train
/// STANDS in (design e765b3fc §2a, car R1): a block the yard names
/// (`TrainStatus::block` — a red PR, a deploy refusal, a converge
/// overdue, a stall past the policy), a train judged by CI alone, a
/// train gate the conductor is still retrying against the bound
/// ([`train_gate_troubled`]). A red PR is a train at the gates, a deploy
/// refusal one on the track; a troubled packet must look troubled where
/// it is drawn, not in the region it used to be counted in.
pub(super) fn train_findings(
    inputs: &RegionInputs<'_>,
    trains: &[&(Job, Vec<Step>)],
) -> Vec<Finding> {
    let ids: Vec<String> = trains.iter().map(|(j, _)| j.id.to_string()).collect();
    let statuses: Vec<&crate::yard::TrainStatus> = inputs
        .status
        .trains
        .iter()
        .filter(|t| ids.contains(&t.id))
        .collect();
    let blocked: Vec<&str> = statuses
        .iter()
        .filter(|t| t.block.is_some())
        .map(|t| t.title.as_str())
        .collect();
    let gate_waits: Vec<&str> = trains
        .iter()
        .filter(|(j, _)| train_gate_troubled(&j.metadata))
        .map(|(j, _)| j.title.as_str())
        .collect();
    let mut findings = Vec::new();
    if !blocked.is_empty() {
        // A block that carries its own start (a deploy refusal, a stall)
        // says how long; the others are stated without one.
        let since = statuses
            .iter()
            .filter_map(|t| match &t.block {
                Some(crate::yard::TrainBlock::DeployBlocked { since, .. }) => {
                    since.as_deref().and_then(stamp_instant)
                }
                Some(crate::yard::TrainBlock::Stalled { since }) => stamp_instant(since),
                _ => None,
            })
            .min();
        findings.push(Finding::new(
            bands::TRACK_BLOCKED,
            since,
            String::new(),
            format!("blocked: {}", blocked.join(", ")),
        ));
    }
    // The two gate markers, split by what they mean (decision 1): CI
    // alone judged the train — degraded, ours — is trouble; a gate the
    // conductor is still retrying against the bound is a wait to watch.
    let fallback: Vec<&str> = trains
        .iter()
        .filter(|(j, _)| !md_str(&j.metadata, TRAIN_GATE_FALLBACK).is_empty())
        .map(|(j, _)| j.title.as_str())
        .collect();
    if !fallback.is_empty() {
        findings.push(Finding::new(
            bands::TRACK_GATE_FALLBACK,
            None,
            String::new(),
            format!(
                "judged by CI alone, the gate not filed: {}",
                fallback.join(", ")
            ),
        ));
    }
    let waiting: Vec<&str> = gate_waits
        .iter()
        .copied()
        .filter(|t| !fallback.contains(t))
        .collect();
    if !waiting.is_empty() {
        findings.push(Finding::new(
            bands::TRACK_GATE_WAITING,
            None,
            String::new(),
            format!("gate not filed yet: {}", waiting.join(", ")),
        ));
    }
    findings
}

/// THE TRACK: trains in transit — merged, deploying and converging
/// ([`train_region`]); a train being made up stands at the dock and one
/// under its train gate at the gates (design e765b3fc §2a, car R1).
/// Troubled or attention by the trains on it ([`train_findings`]). The
/// trend is the time at CI — the `pr` stamp to the `ci` verdict — for
/// trains that arrived in each window.
///
/// NO CAPACITY BOUND SINCE R1. The single track the bound of one drew is
/// the conductor's PRE-MERGE hold (`yard::holds_the_track`: a merged
/// train holds nothing, and the next consist merges on top of it) — and
/// R1 places a pre-merge train at the dock or the gates, where the
/// conductor has it. What is left on the track is merged trains in
/// transit, and two of those at once is the conductor working, not a
/// track over its bound; a "2 of 1" would read as trouble that is not
/// there. So the track counts, and a train on it is judged by its own
/// findings (a deploy refused, a converge overdue, a stall), not by a
/// capacity it no longer has. The gates stay the capacity station a full
/// line is read at (car F1).
pub(super) fn track(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let on_track = trains_in(inputs, "track");
    let ci_times = inputs.closed_trains.iter().filter_map(|(j, s)| {
        if j.metadata.get("outcome").and_then(Value::as_str) != Some("arrived") {
            return None;
        }
        let pr = step_done_at(find_step(s, "pr", "Open the batched PR"))?;
        let ci = step_done_at(find_step(s, "ci", "CI verdict"))?;
        let d = (ci - pr).num_seconds();
        (d >= 0).then_some((closed_at(j)?, d))
    });
    let (cur, prev) = split(w, ci_times);
    let trend = duration_trend("time at CI", "minutes", cur, prev);
    let count = count_in(inputs, "track").unwrap_or(on_track.len());
    let clear_why = plural(count, "train in transit", "trains in transit");
    let settled = settle(train_findings(inputs, &on_track), clear_why, inputs.now);
    // THE KPI: how long the oldest train in transit has stood at the
    // stage it is at — from its last completed step (the stage's start),
    // else from its own opening.
    let at_stage = on_track
        .iter()
        .filter_map(|(j, s)| {
            let start = s
                .iter()
                .filter_map(|st| step_done_at(Some(st)))
                .max()
                .or_else(|| opened_at(j))?;
            Some((inputs.now - start).num_minutes().max(0))
        })
        .max();
    #[allow(clippy::cast_precision_loss)]
    let kpi = vec![match at_stage {
        Some(m) => measure_said(
            "train age at its stage",
            Some(m as f64),
            "minutes",
            format!("oldest train {m} minutes at its stage"),
        ),
        None => measure_said(
            "train age at its stage",
            None,
            "minutes",
            "no train in transit".to_string(),
        ),
    }];
    region(
        "track",
        Some(count),
        None,
        "trains in transit",
        settled,
        trend,
        kpi,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    fn read(open: &[(Job, Vec<Step>)], closed: &[(Job, Vec<Step>)]) -> Regions {
        let status = build_status_for(
            YardInputs {
                open_trains: open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        regions(&inputs(
            &status,
            open,
            closed,
            &[],
            &[],
            Some(&[]),
            Some(&[]),
        ))
    }

    /// A train at `ci`: its PR open, its verdict not in.
    fn under_test(title: &str, metadata: Value, ci: StepStatus) -> (Job, Vec<Step>) {
        let train = job("pr-train", title, JobStatus::Open, metadata);
        let mut verdict = step(&train, "ci", ci, None);
        if ci == StepStatus::Completed {
            verdict.completed_at = Some(t("2026-09-19T11:20:00Z"));
            verdict.metadata = json!({ "result": "failing" });
        }
        let steps = vec![
            step(
                &train,
                "collect",
                StepStatus::Completed,
                Some("2026-09-19T11:00:00Z"),
            ),
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            verdict,
            step(&train, "merged", StepStatus::Ready, None),
        ];
        (train, steps)
    }

    /// A train merged at `merged`, deploying.
    fn in_transit(title: &str, merged: &str) -> (Job, Vec<Step>) {
        let train = job("pr-train", title, JobStatus::Open, json!({}));
        let steps = vec![
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T10:01:00Z"),
            ),
            step(
                &train,
                "ci",
                StepStatus::Completed,
                Some("2026-09-19T10:20:00Z"),
            ),
            step(&train, "merged", StepStatus::Completed, Some(merged)),
            step(&train, "deployed", StepStatus::Ready, None),
        ];
        (train, steps)
    }

    /// A TRAIN'S TROUBLE IS READ WHERE IT STANDS (design e765b3fc §2a,
    /// car R1). A red PR is a train at the gates, so it troubles the
    /// gates and the track stays clear with nothing on it; a gate the
    /// conductor has not filed yet asks for attention there; one judged
    /// by CI alone troubles it — the yard's own predicate and the ported
    /// client one, split by what each means.
    #[test]
    fn a_blocked_train_or_an_unfiled_train_gate_is_read_at_the_gates_where_it_stands() {
        let out = read(
            &[under_test("train #470", json!({}), StepStatus::Completed)],
            &[],
        );
        let gates = by_name(&out, "gates");
        assert_eq!(gates.count, Some(1), "the train under test");
        assert_eq!(gates.state, RegionState::Troubled, "{}", gates.why);
        assert!(gates.why.contains("blocked: train #470"), "{}", gates.why);
        let track = by_name(&out, "track");
        assert_eq!(
            (track.count, track.state),
            (Some(0), RegionState::Clear),
            "{}",
            track.why
        );

        // CI still pending, and the conductor waiting to file the gate.
        let waiting = under_test(
            "train #471",
            json!({ TRAIN_GATE_WAIT_REASON: "bound: 3 running" }),
            StepStatus::Ready,
        );
        let out = read(std::slice::from_ref(&waiting), &[]);
        let gates = by_name(&out, "gates");
        // The conductor retrying against a full bound is a wait to
        // watch, not trouble (design 62de32ae, decision 1); the marker
        // carries no stamp, so it is stated at once with no duration.
        assert_eq!(gates.state, RegionState::Attention, "{}", gates.why);
        assert!(
            gates.why.contains("gate not filed yet: train #471"),
            "{}",
            gates.why
        );
        let band = gates.band.as_ref().unwrap();
        assert_eq!(band.id, "track-gate-waiting");
        assert_eq!(band.held_minutes, None);

        // CI alone judged the train — the gate could not be filed at
        // all, a degraded verdict that is ours: trouble.
        let mut fallback = waiting;
        fallback.0.metadata = json!({ TRAIN_GATE_FALLBACK: "gate unavailable; CI alone" });
        let out = read(&[fallback], &[]);
        let gates = by_name(&out, "gates");
        assert_eq!(gates.state, RegionState::Troubled, "{}", gates.why);
        assert!(gates.why.contains("judged by CI alone"), "{}", gates.why);
        // A blank marker is no marker (yard.ts `text()`).
        assert!(!train_gate_troubled(&json!({ TRAIN_GATE_WAIT_REASON: "" })));
        assert!(train_gate_troubled(
            &json!({ TRAIN_GATE_FALLBACK: "CI alone" })
        ));
    }

    /// THE TRACK HOLDS MERGED TRAINS IN TRANSIT, and carries no capacity
    /// bound since R1: two merged trains converging at once is the
    /// conductor working, not a track over its bound — clear, counted,
    /// with the oldest train's age at its stage as the KPI. A pre-merge
    /// train is not on it.
    #[test]
    fn the_track_holds_merged_trains_in_transit_with_no_capacity_bound() {
        let out = read(
            &[
                in_transit("train #472", "2026-09-19T11:00:00Z"),
                in_transit("train #473", "2026-09-19T11:30:00Z"),
                under_test("train #474", json!({}), StepStatus::Ready),
            ],
            &[],
        );
        let track = by_name(&out, "track");
        assert_eq!(track.count, Some(2), "{}", track.why);
        assert_eq!(track.state, RegionState::Clear, "{}", track.why);
        assert_eq!((track.bound, track.bound_kind), (None, None));
        assert_eq!(track.why, "2 trains in transit");
        assert_eq!(track.unit, "trains in transit");
        // train #472 merged at 11:00 and NOW is 12:00.
        assert_eq!(track.kpi[0].value, Some(60.0));
        assert_eq!(track.kpi[0].text, "oldest train 60 minutes at its stage");

        let out = read(&[], &[]);
        assert_eq!(by_name(&out, "track").count, Some(0));
    }
}
