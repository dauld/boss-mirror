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
}
