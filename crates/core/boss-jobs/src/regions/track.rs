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

/// THE TRACK: trains in transit. Troubled when the yard names a block
/// on any of them (`TrainStatus::block` — a red PR, a deploy refusal,
/// a converge overdue, a stall past the policy) or the conductor is
/// still waiting to file a train's gate ([`train_gate_troubled`]);
/// clear while a pre-merge train holds the single track (the track working). The trend is
/// the time at CI — the `pr` stamp to the `ci` verdict — for trains
/// that arrived in each window.
pub(super) fn track(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    let trains = &inputs.status.trains;
    let blocked: Vec<&str> = trains
        .iter()
        .filter(|t| t.block.is_some())
        .map(|t| t.title.as_str())
        .collect();
    let gate_waits: Vec<&str> = inputs
        .open_trains
        .iter()
        .filter(|(j, _)| train_gate_troubled(&j.metadata))
        .map(|(j, _)| j.title.as_str())
        .collect();
    // The single track's hold is the yard's own predicate — a train
    // before its merge; a merged one converging holds nothing.
    let on_track = inputs
        .open_trains
        .iter()
        .filter(|(_, s)| crate::yard::holds_the_track(s))
        .count();
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
    let mut findings = Vec::new();
    if !blocked.is_empty() {
        // A block that carries its own start (a deploy refusal, a stall)
        // says how long; the others are stated without one.
        let since = trains
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
    let fallback: Vec<&str> = inputs
        .open_trains
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
    // A pre-merge train holding the single track is the track WORKING —
    // clear, with the train named (decision 1).
    let clear_why = if on_track > 0 {
        format!(
            "{} — a train holds the track",
            plural(trains.len(), "train in transit", "trains in transit")
        )
    } else {
        plural(trains.len(), "train in transit", "trains in transit")
    };
    let settled = settle(findings, clear_why, inputs.now);
    // THE KPI: how long the oldest train has stood at the stage it is at
    // — from its last completed step (the stage's start), else from its
    // own opening.
    let at_stage = inputs
        .open_trains
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
        Some(trains.len()),
        Some((1, BoundKind::Capacity)),
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

    /// A train the yard already judges blocked (a red PR) troubles the
    /// track; one whose gate the conductor has not filed yet asks for
    /// attention, and one judged by CI alone troubles it — the yard's own
    /// predicate and the ported client one, split by what each means.
    #[test]
    fn a_blocked_train_or_an_unfiled_train_gate_is_read_on_the_track() {
        let train = job("pr-train", "train #470", JobStatus::Open, json!({}));
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
            {
                let mut s = step(
                    &train,
                    "ci",
                    StepStatus::Completed,
                    Some("2026-09-19T11:20:00Z"),
                );
                s.metadata = json!({ "result": "failing" });
                s
            },
            step(&train, "merged", StepStatus::Ready, None),
        ];
        let open = vec![(train, steps)];
        let status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &open, &[], &[], &[], Some(&[]), Some(&[])));
        let track = by_name(&out, "track");
        assert_eq!(track.count, Some(1));
        assert_eq!(track.state, RegionState::Troubled);
        assert!(track.why.contains("blocked: train #470"), "{}", track.why);

        // The same train, CI still pending, but the conductor waiting to
        // file its gate — trouble the client used to see alone.
        let waiting = job(
            "pr-train",
            "train #471",
            JobStatus::Open,
            json!({ TRAIN_GATE_WAIT_REASON: "bound: 3 running" }),
        );
        let steps = vec![
            step(
                &waiting,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            step(&waiting, "ci", StepStatus::Ready, None),
            step(&waiting, "merged", StepStatus::Pending, None),
        ];
        let open = vec![(waiting, steps)];
        let status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &open, &[], &[], &[], Some(&[]), Some(&[])));
        let track = by_name(&out, "track");
        // The conductor retrying against a full bound is a wait to
        // watch, not trouble (design 62de32ae, decision 1); the marker
        // carries no stamp, so it is stated at once with no duration.
        assert_eq!(track.state, RegionState::Attention);
        assert!(
            track.why.contains("gate not filed yet: train #471"),
            "{}",
            track.why
        );
        let band = track.band.as_ref().unwrap();
        assert_eq!(band.id, "track-gate-waiting");
        assert_eq!(band.held_minutes, None);

        // CI alone judged the train — the gate could not be filed at
        // all, a degraded verdict that is ours: trouble.
        let mut open = open;
        open[0].0.metadata = json!({ TRAIN_GATE_FALLBACK: "gate unavailable; CI alone" });
        let out = regions(&inputs(&status, &open, &[], &[], &[], Some(&[]), Some(&[])));
        let track = by_name(&out, "track");
        assert_eq!(track.state, RegionState::Troubled, "{}", track.why);
        assert!(track.why.contains("judged by CI alone"), "{}", track.why);
        // A blank marker is no marker (yard.ts `text()`).
        assert!(!train_gate_troubled(&json!({ TRAIN_GATE_WAIT_REASON: "" })));
        assert!(train_gate_troubled(
            &json!({ TRAIN_GATE_FALLBACK: "CI alone" })
        ));
    }

    /// A healthy pre-merge train holds the single track: CLEAR — the
    /// track working, which is what it is for (design 62de32ae, decision
    /// 1) — with the train named and its age at the stage as the KPI.
    #[test]
    fn a_pre_merge_train_holds_the_track_and_reads_clear() {
        let train = job("pr-train", "train #472", JobStatus::Open, json!({}));
        let steps = vec![
            step(
                &train,
                "pr",
                StepStatus::Completed,
                Some("2026-09-19T11:01:00Z"),
            ),
            step(&train, "ci", StepStatus::Ready, None),
            step(&train, "merged", StepStatus::Pending, None),
        ];
        let open = vec![(train, steps)];
        let status = build_status_for(
            YardInputs {
                open_trains: &open,
                now: Some(t(NOW)),
                ..Default::default()
            },
            Reading::Read,
            BoardingReadings::default(),
        );
        let out = regions(&inputs(&status, &open, &[], &[], &[], Some(&[]), Some(&[])));
        let track = by_name(&out, "track");
        assert_eq!(track.state, RegionState::Clear, "{}", track.why);
        assert!(
            track.why.contains("a train holds the track"),
            "{}",
            track.why
        );
        assert!(track.band.is_none());
        assert_eq!(track.bound, Some(1));
        assert_eq!(track.bound_kind, Some(BoundKind::Capacity));
        assert_eq!(track.unit, "trains in transit");
        // The `pr` step completed at 11:01 and NOW is 12:00.
        assert_eq!(track.kpi[0].value, Some(59.0));
        assert_eq!(track.kpi[0].text, "oldest train 59 minutes at its stage");
    }
}
