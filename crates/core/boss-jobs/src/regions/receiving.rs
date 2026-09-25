//! RECEIVING: inbound packets standing until an actor takes them in —
//! with the receiving yard's pipeline set and age bands, the second of
//! the two client-side predicates ported here once (`receiving.ts`) and
//! pinned against the TypeScript.

use super::*;

/// The delivery pipeline's kinds — a packet of these is a car in
/// transit or the machinery moving it, never a request, so the
/// receiving yard leaves them to the train yard. `receiving.ts::
/// PIPELINE_KINDS`, ported; the test `the_pipeline_kinds_match_the_
/// receiving_yard` holds the two equal.
pub const PIPELINE_KINDS: [&str; 9] = [
    "pr-train",
    "gate-run",
    "ship-a-change",
    "ops-request",
    "park-a-job",
    "emergency-merge",
    "repair-a-train",
    "publish-request",
    "regenerate-deployment",
];

/// The receiving yard's age bands, in days from `opened_on`: triage
/// within 3 days, anything within 14 (`receiving.ts::AGE_THRESHOLDS`,
/// ported and pinned by the same test).
pub const AGING_DAYS: i64 = 3;
pub const STALE_DAYS: i64 = 14;

/// Which kinds are inbound: active platform-category kinds that are
/// neither chores (`maintenance-*`) nor the pipeline
/// (`receiving.ts::inboundKinds`). Derived from the registry, never
/// listed by kind.
pub fn inbound_kinds(workflows: &[WorkflowSpec]) -> Vec<String> {
    let mut kinds: Vec<String> = workflows
        .iter()
        .filter(|w| w.status == crate::registry::WorkflowStatus::Active)
        .filter(|w| w.category == "platform")
        .filter(|w| {
            !w.kind.starts_with("maintenance-") && !PIPELINE_KINDS.contains(&w.kind.as_str())
        })
        .map(|w| w.kind.clone())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

/// RECEIVING: inbound packets standing — feedback, alarms, findings,
/// design questions — from the moment they open until an actor takes
/// them in: until the intake step completes ([`taken_in`]), after which
/// the packet is marshalling's, never both ([`members`]). The receiving
/// yard's own bands decide the state: any
/// packet older than [`STALE_DAYS`] is troubled, older than
/// [`AGING_DAYS`] attention. The trend is inbound arrivals per day.
pub(super) fn receiving(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    const UNIT: &str = "packets standing";
    let Some(inbound) = inputs.inbound else {
        return region(
            "receiving",
            None,
            None,
            UNIT,
            unread_settled(
                "the workflow registry that names the inbound kinds could not be read",
                inputs.now,
            ),
            Trend {
                metric: "inbound".to_string(),
                unit: "per day".to_string(),
                current: None,
                previous: None,
                samples: 0,
                previous_samples: 0,
            },
            vec![measure("oldest untriaged", None, "days")],
        );
    };
    let (cur, prev) = count_split(w, inbound.iter().filter_map(|(j, _)| opened_at(j)));
    let trend = rate_trend("inbound", w, cur, prev);
    let today = inputs.now.date_naive();
    // THE PARTITION (design 62de32ae decision 4): standing here means not
    // yet taken in, and held by no region of its own.
    let open: Vec<&Job> = receiving_standing(inputs).unwrap_or_default();
    let oldest_day = open.iter().map(|j| j.opened_on).min();
    let oldest = oldest_day.map_or(0, |d| (today - d).num_days());
    let n = open.len();
    // A band in whole days from `opened_on` is crossed at the midnight
    // that makes the age exceed it — which IS its onset, read off the
    // packet's own date.
    let crossed = |band_days: i64| {
        oldest_day
            .and_then(|d| d.checked_add_days(chrono::Days::new(u64::try_from(band_days + 1).ok()?)))
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|t| chrono::DateTime::from_naive_utc_and_offset(t, chrono::Utc))
    };
    let mut findings = Vec::new();
    if oldest > STALE_DAYS {
        findings.push(Finding::new(
            bands::RECEIVING_STALE,
            crossed(STALE_DAYS),
            format!("oldest {oldest}d"),
            format!(
                "{} standing, the oldest {oldest} days (past the {STALE_DAYS}-day band)",
                plural(n, "packet", "packets")
            ),
        ));
    }
    if oldest > AGING_DAYS {
        findings.push(Finding::new(
            bands::RECEIVING_AGING,
            crossed(AGING_DAYS),
            format!("oldest {oldest}d"),
            format!(
                "{} standing, the oldest {oldest} days (past the {AGING_DAYS}-day triage band)",
                plural(n, "packet", "packets")
            ),
        ));
    }
    let settled = settle(
        findings,
        format!("{} standing", plural(n, "packet", "packets")),
        inputs.now,
    );
    // THE KPI (decision 9). Its second half — the share whose channel is
    // unrecorded — is not measured here yet: the channel rule is the
    // client's (`receiving.ts::channelOf`), and a second copy of it needs
    // its own equality pin (CLAUDE.md §9a), so it is left as named residue
    // on backlog c3105b2a rather than ported unpinned.
    #[allow(clippy::cast_precision_loss)]
    let kpi = vec![match oldest_day {
        Some(_) => measure("oldest untriaged", Some(oldest as f64), "days"),
        None => measure_said(
            "oldest untriaged",
            None,
            "days",
            "nothing untriaged".to_string(),
        ),
    }];
    region("receiving", Some(n), None, UNIT, settled, trend, kpi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// Receiving: the age bands from `receiving.ts`, and arrivals per
    /// day from the packets' own open stamps.
    #[test]
    fn receiving_reads_the_age_bands_and_inbound_arrivals() {
        let mut fresh = job(
            "user-feedback",
            "fresh",
            JobStatus::Open,
            json!({ "opened_at": "2026-09-19T09:00:00Z" }),
        );
        fresh.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap();
        let mut aging = job("backlog-item", "aging", JobStatus::Open, json!({}));
        aging.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let mut stale = job("backlog-item", "stale", JobStatus::Open, json!({}));
        stale.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        let mut yesterday = job(
            "user-feedback",
            "done",
            JobStatus::Closed,
            json!({ "opened_at": "2026-09-18T09:00:00Z" }),
        );
        yesterday.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let status = empty_status();
        let inbound = untaken(vec![fresh.clone(), aging.clone()]);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        assert_eq!(r.count, Some(2));
        // Past the triage band: ATTENTION, with the band named against
        // the number beside it (design 62de32ae, decision 1) — and the
        // band was crossed the midnight the oldest turned four days old.
        assert_eq!(r.state, RegionState::Attention, "{}", r.why);
        let band = r.band.as_ref().unwrap();
        assert_eq!(band.id, "receiving-aging");
        assert_eq!(band.reads, "oldest 5d > the 3-day triage band");
        assert_eq!(band.since.as_deref(), Some("2026-09-18T00:00:00+00:00"));
        assert_eq!(band.held_minutes, Some(36 * 60));
        assert_eq!(r.unit, "packets standing");
        assert_eq!(r.kpi[0].text, "oldest untriaged 5 days");
        assert_eq!(r.trend.current, Some(1.0));
        assert_eq!(r.trend.previous, Some(0.0));

        let inbound = untaken(vec![fresh, aging, stale, yesterday]);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        assert_eq!(r.count, Some(3));
        assert_eq!(r.state, RegionState::Troubled, "{}", r.why);
        assert!(r.why.contains("30 days"), "{}", r.why);
        assert_eq!(r.trend.previous, Some(1.0));
    }

    /// The two facts ported from the receiving yard's TypeScript live
    /// twice; this holds them equal (CLAUDE.md §9a). Reads the source
    /// the client is built from, so a kind added to one side and not
    /// the other names itself here.
    #[test]
    fn the_pipeline_kinds_and_age_bands_match_the_receiving_yard() {
        let src = std::fs::read_to_string(
            boss_testing::repo_root().join("apps/web/src/it/receiving/receiving.ts"),
        )
        .expect("receiving.ts is in the tree");
        let set = src
            .split("PIPELINE_KINDS: ReadonlySet<string> = new Set([")
            .nth(1)
            .and_then(|rest| rest.split("]);").next())
            .expect("receiving.ts declares PIPELINE_KINDS as a Set literal");
        let ts_kinds: Vec<&str> = set
            .split(',')
            .map(|s| s.trim().trim_matches('\''))
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            ts_kinds, PIPELINE_KINDS,
            "receiving.ts::PIPELINE_KINDS drifted from regions.rs::PIPELINE_KINDS"
        );
        assert!(
            src.contains(&format!(
                "AGE_THRESHOLDS = {{ aging: {AGING_DAYS}, stale: {STALE_DAYS} }}"
            )),
            "receiving.ts::AGE_THRESHOLDS drifted from regions.rs::AGING_DAYS / STALE_DAYS"
        );
        // The train-gate keys the yard's `readTrainGate` reads.
        let yard =
            std::fs::read_to_string(boss_testing::repo_root().join("apps/web/src/it/yard/yard.ts"))
                .expect("yard.ts is in the tree");
        for key in [TRAIN_GATE_WAIT_REASON, TRAIN_GATE_FALLBACK] {
            assert!(
                yard.contains(&format!("md.{key}")),
                "yard.ts no longer reads {key}"
            );
        }
    }

    #[test]
    fn inbound_kinds_are_active_platform_kinds_minus_chores_and_the_pipeline() {
        let spec = |kind: &str, category: &str| WorkflowSpec {
            kind: kind.into(),
            version: 1,
            status: crate::registry::WorkflowStatus::Active,
            label: kind.into(),
            description: None,
            category: category.into(),
            subject_kinds: vec![],
            steps: vec![],
            metadata_schema: json!({}),
            entitlements: json!({}),
            metadata: json!({}),
            on_complete_create: vec![],
            owning_team: "it".into(),
            authoring_job_id: None,
            created_at: t(NOW),
        };
        let specs = vec![
            spec("user-feedback", "platform"),
            spec("backlog-item", "platform"),
            spec("maintenance-nightly", "platform"),
            spec("pr-train", "platform"),
            spec("sale", "commerce"),
        ];
        assert_eq!(inbound_kinds(&specs), vec!["backlog-item", "user-feedback"]);
    }
}
