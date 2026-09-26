//! THE INPUT-LANE VOCABULARY — where a work-originating packet came
//! from, as data both filing doors read.
//!
//! WHY IT LIVES HERE (backlog b2d9b432). The lane was `boss-cli`'s
//! alone, so the only door that could stamp it was the CLI. Every
//! machine filer in `boss-dispatcher-handlers` — the estate alarms, the
//! cadence-silence arm, the chore's red lines, the ops judge, the
//! sensor poll, the DNS observer, the abandoned-step reclaim — filed
//! backlog-items with NO lane, though each one's origin is known
//! exactly, better than any human filer knows theirs.
//!
//! From `FIELD_LIVE_AT` (2026-09-21T00:00:00Z) a packet with no lane
//! reads as "the filer did not say". Left alone, the filed-without-a-lane
//! count would have been dominated by machines whose lane was never in
//! doubt, and the number would have stopped meaning "somebody forgot"
//! and started meaning "the machines do not participate". A count that
//! mixes those two is worse than no count.
//!
//! Stamping the sites would have spelled `"input_channel"` a second
//! time in a second crate, which is the duplication CLAUDE.md §9a
//! exists to refuse. So the vocabulary moved to the crate both sides
//! already depend on, and each side reads ONE definition. `boss-cli`
//! keeps what is genuinely its own: the inference for the pre-field
//! era, the provenance split, and the mix report.

/// The lane a work-originating job entered through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputChannel {
    UserFeedback,
    Roadmap,
    DesignResolution,
    Review,
    Telemetry,
    PipelineFailure,
    Discovery,
    PostMortem,
    Dependency,
    Scheduled,
    Unclassified,
}

impl InputChannel {
    pub fn label(self) -> &'static str {
        match self {
            InputChannel::UserFeedback => "user-feedback",
            InputChannel::Roadmap => "roadmap",
            InputChannel::DesignResolution => "design-resolution",
            InputChannel::Review => "review-finding",
            InputChannel::Telemetry => "telemetry/monitoring",
            InputChannel::PipelineFailure => "pipeline-failure",
            InputChannel::Discovery => "discovery-while-working",
            InputChannel::PostMortem => "post-mortem",
            InputChannel::Dependency => "dependency/external",
            InputChannel::Scheduled => "scheduled",
            InputChannel::Unclassified => "unclassified",
        }
    }

    /// Proactive lanes build what is wanted; the rest are the system
    /// responding to something that already happened. This split is the
    /// health reading, not a value judgement on the work itself.
    pub fn is_proactive(self) -> bool {
        matches!(
            self,
            InputChannel::UserFeedback | InputChannel::Roadmap | InputChannel::DesignResolution
        )
    }

    /// The inverse of [`InputChannel::label`] over the fileable lanes —
    /// derived from that one list, so a lane can never be nameable and
    /// unreadable at once. An unknown or misspelled label is `None`: a
    /// typo must not masquerade as a recorded fact.
    pub fn parse(label: &str) -> Option<InputChannel> {
        FILEABLE_LANES.into_iter().find(|c| c.label() == label)
    }
}

/// Every lane a FILER may name, in one list — the vocabulary `boss job
/// file --channel` parses and its refusal prints, so the door's help and
/// the report's labels cannot drift apart (CLAUDE.md §9a).
/// `Unclassified` is deliberately absent: it is what the ABSENCE of an
/// answer reads as, never an answer.
pub const FILEABLE_LANES: [InputChannel; 10] = [
    InputChannel::UserFeedback,
    InputChannel::Roadmap,
    InputChannel::DesignResolution,
    InputChannel::Review,
    InputChannel::Telemetry,
    InputChannel::PipelineFailure,
    InputChannel::Discovery,
    InputChannel::PostMortem,
    InputChannel::Dependency,
    InputChannel::Scheduled,
];

/// The metadata key the lane is RECORDED under, written at filing.
pub const RECORDED_KEY: &str = "input_channel";

/// Where a lane reading came from: the filer's own [`RECORDED_KEY`], or
/// nothing it said — never a guess from the kind or the reporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaneBasis {
    Recorded,
    Unclassified,
}

/// A packet's lane as a reader draws it: the label, and its basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LaneReading {
    pub lane: &'static str,
    pub basis: LaneBasis,
}

/// THE LANE A READER DRAWS (backlog 1eea4554, decided 2026-09-24): the
/// label the filer recorded under [`RECORDED_KEY`], read through
/// [`InputChannel::parse`], or `unclassified` when the filer said
/// nothing this vocabulary can read. ONE rule, on the server: the
/// receiving board kept a second, six-lane vocabulary of its own and
/// read it off `metadata.channel`, a key no filer writes, so 0 of 1,626
/// arrivals read as recorded while 492 of them carried this key. Both
/// the jobs list (`lane=true`) and the receiving region read this.
///
/// No inference: the pre-field keyword guess is `boss-cli`'s report and
/// says so there; a surface that draws a lane draws what was recorded.
pub fn lane_of(metadata: &serde_json::Value) -> LaneReading {
    match metadata
        .get(RECORDED_KEY)
        .and_then(serde_json::Value::as_str)
        .and_then(InputChannel::parse)
    {
        Some(lane) => LaneReading {
            lane: lane.label(),
            basis: LaneBasis::Recorded,
        },
        None => LaneReading {
            lane: InputChannel::Unclassified.label(),
            basis: LaneBasis::Unclassified,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fileable lane round-trips through its own label, and
    /// `Unclassified` is not fileable — it is what silence reads as.
    #[test]
    fn every_fileable_lane_parses_back_to_itself_and_unclassified_is_not_one() {
        for lane in FILEABLE_LANES {
            assert_eq!(
                InputChannel::parse(lane.label()),
                Some(lane),
                "{} must parse back",
                lane.label()
            );
        }
        assert_eq!(
            InputChannel::parse(InputChannel::Unclassified.label()),
            None,
            "`unclassified` is the absence of an answer, never one a filer may name"
        );
        assert_eq!(
            InputChannel::parse("telemetry"),
            None,
            "a near miss is None"
        );
        assert_eq!(InputChannel::parse(""), None);
    }

    /// THE LANE A READER DRAWS (backlog 1eea4554): the recorded key,
    /// read through the one vocabulary, and nothing else. The receiving
    /// board read `metadata.channel` — a key no filer writes — so 0 of
    /// 1,626 arrivals read as recorded while 492 carried this one.
    #[test]
    fn a_lane_is_read_off_the_recorded_key_or_is_unclassified() {
        let recorded = lane_of(&serde_json::json!({ "input_channel": "review-finding" }));
        assert_eq!(recorded.lane, "review-finding");
        assert_eq!(recorded.basis, LaneBasis::Recorded);

        // The filer said nothing: unclassified, never guessed into a lane.
        for md in [
            serde_json::json!({}),
            serde_json::json!(null),
            serde_json::json!({ "channel": "monitoring" }),
            serde_json::json!({ "reporter": "automation:cluster-watchdog" }),
            serde_json::json!({ "input_channel": "" }),
            serde_json::json!({ "input_channel": 7 }),
            // A near miss is not a recorded fact.
            serde_json::json!({ "input_channel": "telemetry" }),
            // `unclassified` is the absence of an answer, never one.
            serde_json::json!({ "input_channel": "unclassified" }),
        ] {
            let read = lane_of(&md);
            assert_eq!(read.lane, "unclassified", "{md}");
            assert_eq!(read.basis, LaneBasis::Unclassified, "{md}");
        }

        // The wire shape a reader parses.
        assert_eq!(
            serde_json::to_value(recorded).unwrap(),
            serde_json::json!({ "lane": "review-finding", "basis": "recorded" })
        );
        assert_eq!(
            serde_json::to_value(lane_of(&serde_json::json!({}))).unwrap(),
            serde_json::json!({ "lane": "unclassified", "basis": "unclassified" })
        );
    }
}
