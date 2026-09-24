//! THE IT MAP'S STATE VOCABULARY AND ITS DECLARED BANDS (design
//! 62de32ae, "The IT map, round 3: meaning before drawing", decisions
//! 1, 2 and 5; car A of the plan on backlog c3105b2a).
//!
//! WHY THIS EXISTS. A fresh-eyes review on 2026-09-24 found six of the
//! ten regions painted the same amber BUSY, and the word meant three
//! different things: a healthy dock with a train due (the designed
//! state two minutes after any departure), gates at their bound, and a
//! receiving yard holding 325 packets a week old. A colour on most of
//! the map carries no information. The same review watched shop-floor
//! go clear -> troubled -> troubled -> clear across six reads twenty
//! seconds apart, because a run is "finished and not reported" for the
//! few seconds between its build ending and its handback landing — and
//! a red that comes and goes is a check nobody reads (CLAUDE.md
//! §Diagnosis).
//!
//! THREE WORDS, ONE MEANING EACH, EVERYWHERE — regions and borders:
//!
//!   * `clear`: flowing within its declared bounds. That INCLUDES busy
//!     and healthy — a dock with a train due, a full rail moving.
//!   * `attention`: a declared band crossed — a number past a line this
//!     table draws, which someone should look at.
//!   * `troubled`: ours, and not moving (the shed's rule, 3881f5c9,
//!     applied to every region), or a reading that could not be taken,
//!     which is refused like a failure rather than drawn as clear.
//!
//! EVERY NON-CLEAR STATE NAMES THE BAND THAT DECIDED IT, so the state
//! can always be checked against the number beside it ("oldest 7d > 3d
//! band"). The bands are declared ONCE, in [`BANDS`] below — the
//! thresholds the regions already enforce are referenced by their own
//! named constants, and the new numbers (the hold periods) live nowhere
//! else.
//!
//! HYSTERESIS, READ FROM THE RECORD. A band's condition changes the
//! state only after it has HELD for the band's declared period. How long
//! it has held is not remembered by this process — it is READ: every
//! condition carries the instant the record says it began (a run's
//! `building` completion, the moment the last bay filled, the day the
//! oldest packet crossed its triage band). So the judgement is a pure
//! function of the rows, identical across replicas, restarts and
//! readers with different scopes, and "troubled for 6m" means the
//! record shows six minutes of it — not that some process happened to
//! be polling. A remembered-state layer was the alternative and was
//! rejected for exactly those three reasons: it would answer
//! differently after a restart, per replica, and per policy scope.
//!
//! What that buys and what it does not, said plainly. A condition that
//! comes and goes faster than its hold never reaches the surface, and
//! one that returns must hold its full period again before it shows —
//! which is the flicker the review measured, damped. A condition that
//! CLEARS clears at once: the record holds when a condition began, and
//! for most conditions it does not hold when one last held, so a
//! "cleared for N minutes" rule would have to be remembered rather than
//! read. A condition whose onset the record does not hold (a train's
//! gate-wait marker carries no stamp) is stated at once, with no
//! duration — an unknown onset is not a pass.

use serde::{Deserialize, Serialize};

use crate::regions::{Instant, RegionState};

/// ONE DECLARED BAND: the line a region's number crosses, the state
/// crossing it means, and how long its condition must hold before the
/// state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Band {
    /// Stable, and what the payload names: `receiving-aging`.
    pub id: &'static str,
    /// The region whose state it decides; `*` for a band every region
    /// shares (an unread input, a failed machine).
    pub region: &'static str,
    pub state: RegionState,
    /// The band in words, as the header carries it after the reading:
    /// "the 3-day triage band".
    pub band: &'static str,
    /// How long the condition must hold, from the instant the record
    /// says it began, before the state changes. Zero where the band
    /// already carries its own time (a proof open past a day) or where
    /// the condition is a failure that must show at once.
    pub hold_minutes: i64,
}

const fn band(
    id: &'static str,
    region: &'static str,
    state: RegionState,
    band: &'static str,
    hold_minutes: i64,
) -> Band {
    Band {
        id,
        region,
        state,
        band,
        hold_minutes,
    }
}

use RegionState::{Attention, Troubled};

// --- shared -----------------------------------------------------------

/// An input could not be read. Unknown is refused like a failure — a
/// region nobody could read must never draw as a quiet one.
pub const UNREAD: Band = band(
    "unread",
    "*",
    Troubled,
    "a reading that could not be taken",
    0,
);
/// A machine standing in the region failed (design d2154293, car 5): the
/// failure reads at world scale, not only when someone zooms in.
pub const MACHINE_FAILED: Band = band(
    "machine-failed",
    "*",
    Troubled,
    "a failed machine in the region",
    0,
);

// --- dock -------------------------------------------------------------

/// A parked car held behind a predecessor still in flight — it will
/// board by itself when that one lands, so it is a number to watch, not
/// trouble. The hold is a train's cycle (~40 min measured on the
/// conductor's clock), so a car waiting out ONE predecessor's train
/// never shows.
pub const DOCK_CANNOT_BOARD: Band = band(
    "dock-cannot-board",
    "dock",
    Attention,
    "a parked car that cannot board, held for 60m",
    60,
);
/// An edge that can NEVER clear — an abandoned or missing predecessor:
/// the conductor refuses it identically every pass until a person acts.
pub const DOCK_EDGE_NEVER_CLEARS: Band = band(
    "dock-edge-never-clears",
    "dock",
    Troubled,
    "an ordering edge that can never be satisfied",
    0,
);

// --- gates ------------------------------------------------------------

/// Every bay in use, or a run waiting for one. A gate takes ~11 minutes,
/// so bays full for half an hour is a queue longer than two gates — the
/// saturation the review said is "worth knowing", and nothing shorter.
pub const GATES_AT_BOUND: Band = band(
    "gates-at-bound",
    "gates",
    Attention,
    "every bay in use for 30m",
    30,
);
/// A run active past the gate Job's own deadline
/// (`yard::GATE_MAX_ACTIVE_HOURS`): a corpse holding a bay.
pub const GATES_CORPSE: Band = band("gates-corpse", "gates", Troubled, "the gate deadline", 0);

// --- track ------------------------------------------------------------

/// A train the yard names blocked (`TrainStatus::block`): a red PR, a
/// deploy refusal, a converge overdue, a stall past the policy. Each of
/// those carries its own threshold already.
pub const TRACK_BLOCKED: Band = band("track-blocked", "track", Troubled, "a blocked train", 0);
/// The gate could not be filed and CI alone judged the train — stamped
/// loudly on the train by the conductor.
pub const TRACK_GATE_FALLBACK: Band = band(
    "track-gate-fallback",
    "track",
    Troubled,
    "a train judged by CI alone",
    0,
);
/// The conductor has not filed a train's gate yet (the bound was full on
/// its last launch). It retries every pass, a minute apart, and clears
/// the marker the pass it files — so a wait of a few passes is the bound
/// working, and ten minutes of it is worth a look.
pub const TRACK_GATE_WAITING: Band = band(
    "track-gate-waiting",
    "track",
    Attention,
    "a train gate not filed for 10m",
    10,
);

// --- shed -------------------------------------------------------------

/// A landed car with no probe and no event: nothing mechanical can
/// settle it.
pub const SHED_UNPROVEN: Band = band(
    "shed-unproven",
    "shed",
    Troubled,
    "a landed car nothing can prove",
    0,
);
/// A landed car whose probe ran and failed.
pub const SHED_FAILING: Band = band("shed-failing", "shed", Troubled, "a failing probe", 0);
/// Open past [`PROOF_STALE_HOURS`] awaiting proof, and the next move is
/// ours (`regions::StaleProof`, every answer but `Theirs`).
pub const SHED_OURS_STALE: Band = band(
    "shed-ours-stale",
    "shed",
    Troubled,
    "the 24h proof band, ours to move",
    0,
);
/// Open past [`PROOF_STALE_HOURS`] awaiting proof, waiting on the owner
/// it declared and observes — not ours, but past the line.
pub const SHED_THEIRS_STALE: Band = band(
    "shed-theirs-stale",
    "shed",
    Attention,
    "the 24h proof band, waiting on its declared owner",
    0,
);

// --- arrivals ---------------------------------------------------------

/// A train cancelled in the window with cars aboard, one still awaiting
/// the repair the red asked for (`regions::awaiting_repair`).
pub const ARRIVALS_RED_UNREPAIRED: Band = band(
    "arrivals-red-unrepaired",
    "arrivals",
    Troubled,
    "a cancelled train's car awaiting repair",
    0,
);

// --- garage -----------------------------------------------------------

/// A green gate no car claims, or a gate-run never judged. The
/// auto-park handler files a car within seconds of a green, so the hold
/// is the grace in which a green is still becoming a car.
pub const GARAGE_STRANDED: Band = band(
    "garage-stranded",
    "garage",
    Troubled,
    "a green or a gate-run with no car for 15m",
    15,
);
/// A car held on the dock, a green held before parking, or a red
/// awaiting rework — deliberate, and worked, until it has stood an hour.
pub const GARAGE_WAITING: Band = band(
    "garage-waiting",
    "garage",
    Attention,
    "held or red for 60m",
    60,
);

// --- receiving --------------------------------------------------------

/// The oldest untriaged packet past the triage band ([`AGING_DAYS`]).
pub const RECEIVING_AGING: Band = band(
    "receiving-aging",
    "receiving",
    Attention,
    "the 3-day triage band",
    0,
);
/// The oldest untriaged packet past [`STALE_DAYS`]: nothing has taken it
/// in for two weeks, which is ours and not moving.
pub const RECEIVING_STALE: Band = band(
    "receiving-stale",
    "receiving",
    Troubled,
    "the 14-day band",
    0,
);

// --- marshalling ------------------------------------------------------

/// A station holding more than its declared WIP limit.
pub const MARSHALLING_OVER_LIMIT: Band = band(
    "marshalling-over-limit",
    "marshalling",
    Troubled,
    "a station's WIP limit",
    0,
);
/// A station holding work that nothing left in the whole window.
pub const MARSHALLING_NOT_DRAINING: Band = band(
    "marshalling-not-draining",
    "marshalling",
    Troubled,
    "a station that served nothing in the window",
    0,
);

// --- shop floor -------------------------------------------------------

/// A run whose `building` is done and whose `reported` is still open:
/// finished, and still holding a slot. The handback lands within a
/// minute or two of the build ending, so ten minutes of it is a run
/// that will not report by itself — and the few seconds between the two
/// steps, which flapped the region on 2026-09-24, never show.
pub const SHOP_FLOOR_UNREPORTED: Band = band(
    "shop-floor-unreported",
    "shop-floor",
    Troubled,
    "a finished run unreported for 10m",
    10,
);
/// Runs in flight at the registry's cap: the next dispatch is refused.
pub const SHOP_FLOOR_AT_CAP: Band = band(
    "shop-floor-at-cap",
    "shop-floor",
    Attention,
    "the run cap, held for 15m",
    15,
);

// --- publish ----------------------------------------------------------

/// A scan read red and nobody recorded a disposition (design cb38d806
/// §2).
pub const PUBLISH_UNJUDGED_RED: Band = band(
    "publish-unjudged-red",
    "publish",
    Troubled,
    "a red scan nobody judged",
    0,
);
/// A mirror pull request open past [`STALLED_PUBLISH_HOURS`] (§4).
pub const PUBLISH_PR_STALLED: Band = band(
    "publish-pr-stalled",
    "publish",
    Troubled,
    "the 24h a publish may stand",
    0,
);
/// A publish packet held past [`STALLED_PUBLISH_HOURS`] with no pull
/// request (backlog f49ae66d).
pub const PUBLISH_HELD: Band = band(
    "publish-held",
    "publish",
    Troubled,
    "the 24h a publish may be held",
    0,
);

// --- every rail ---------------------------------------------------------

/// How many of a rail's own mean gaps it may stay quiet before it is
/// judged STILL — `flowing: false` on `GET /api/yard/borders` (design
/// 31bade8f, "The IT map moves", decision 8; car M1 on backlog
/// d220022f). The mean gap is the rail's measured window over its
/// crossings in it, so a rail crossed 24 times a day is still after four
/// quiet hours and one crossed 500 times after about eleven minutes. Four
/// is the design's number: at the mean rate, evenly spaced traffic is
/// never quiet past one gap, and bursty traffic seldom past two or three.
pub const STILL_AFTER_GAPS: i64 = 4;

/// A rail quiet past [`STILL_AFTER_GAPS`] of its own mean gap: nothing
/// is crossing where work has been crossing. It decides no region's
/// state and it does not change the border's own `state` either — the
/// design draws stillness as its own signal beside the state (a rail
/// stops moving), and folding it into the state word would repaint the
/// map on a number the review never asked to judge. So `state` here is
/// the reading it would be if a surface judged one: worth a look.
pub const BORDER_STILL: Band = band(
    "border-still",
    "*",
    Attention,
    "silence past 4× the rail's own mean gap",
    0,
);

/// EVERY BAND, once. A region names a band by referring to its constant,
/// so an undeclared band cannot be named at all; this list is what the
/// uniqueness and coverage pins read.
pub const BANDS: [Band; 26] = [
    UNREAD,
    MACHINE_FAILED,
    DOCK_CANNOT_BOARD,
    DOCK_EDGE_NEVER_CLEARS,
    GATES_AT_BOUND,
    GATES_CORPSE,
    TRACK_BLOCKED,
    TRACK_GATE_FALLBACK,
    TRACK_GATE_WAITING,
    SHED_UNPROVEN,
    SHED_FAILING,
    SHED_OURS_STALE,
    SHED_THEIRS_STALE,
    ARRIVALS_RED_UNREPAIRED,
    GARAGE_STRANDED,
    GARAGE_WAITING,
    RECEIVING_AGING,
    RECEIVING_STALE,
    MARSHALLING_OVER_LIMIT,
    MARSHALLING_NOT_DRAINING,
    SHOP_FLOOR_UNREPORTED,
    SHOP_FLOOR_AT_CAP,
    PUBLISH_UNJUDGED_RED,
    PUBLISH_PR_STALLED,
    PUBLISH_HELD,
    BORDER_STILL,
];

/// ONE CONDITION a region found true on this read.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub band: Band,
    /// The instant the record says the condition BEGAN holding. `None`
    /// where the record holds no onset — the condition is then stated at
    /// once, with no duration, because an unknown onset is not a pass.
    pub since: Option<Instant>,
    /// The measurement, in the words the header puts before the band:
    /// "oldest 7d". Empty where the band itself is the whole reading.
    pub measured: String,
    /// The region's sentence for this condition.
    pub why: String,
}

impl Finding {
    pub fn new(band: Band, since: Option<Instant>, measured: String, why: String) -> Self {
        Finding {
            band,
            since,
            measured,
            why,
        }
    }
}

/// THE BAND THAT DECIDED A REGION'S STATE, as the payload carries it —
/// the header's "oldest 7d > 3d band · troubled for 6m".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decided {
    /// The band's [`Band::id`].
    pub id: String,
    /// The measurement against the band: "oldest 7d > the 3-day triage
    /// band".
    pub reads: String,
    /// The period the condition had to hold before the state changed.
    pub hold_minutes: i64,
    /// When the record says the condition began (RFC 3339). `null` when
    /// it holds no onset.
    #[serde(default)]
    pub since: Option<String>,
    /// Minutes from `since` to the read's `now`. `null` with `since`.
    #[serde(default)]
    pub held_minutes: Option<i64>,
    /// The same, as every surface prints it — "6m", "3h", "2d"
    /// ([`duration_text`]) — so the map and `boss orient` say "troubled
    /// for 6m" in one voice rather than each formatting the minutes.
    #[serde(default)]
    pub held: Option<String>,
}

/// A region's settled state: what it says, why, and what decided it.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    pub state: RegionState,
    pub why: String,
    pub band: Option<Decided>,
}

fn rank(s: RegionState) -> u8 {
    match s {
        RegionState::Clear => 0,
        RegionState::Attention => 1,
        RegionState::Troubled => 2,
    }
}

fn held(f: &Finding, now: Instant) -> Option<i64> {
    f.since.map(|s| (now - s).num_minutes().max(0))
}

/// Has this condition held its band's period? A condition with no onset
/// on record has — it is stated at once (see the module note).
fn has_held(f: &Finding, now: Instant) -> bool {
    held(f, now).is_none_or(|m| m >= f.band.hold_minutes)
}

/// THE ONE JUDGEMENT every region's state passes through. The findings
/// arrive in the region's own priority order; the worst state among
/// those that have HELD wins, the first of equal rank naming it. A
/// finding still inside its hold changes nothing, and is said after the
/// clear sentence as `settling`, with how long it has held of how long it
/// must — an honest reading, never a hidden one.
pub fn settle(findings: Vec<Finding>, clear_why: String, now: Instant) -> Settled {
    let (settled, settling): (Vec<Finding>, Vec<Finding>) =
        findings.into_iter().partition(|f| has_held(f, now));
    let decided = settled
        .iter()
        .max_by(|a, b| {
            // max_by keeps the LAST of equals; reversing the tie keeps the
            // first, which is the region's own priority.
            rank(a.band.state)
                .cmp(&rank(b.band.state))
                .then(std::cmp::Ordering::Greater)
        })
        .cloned();
    match decided {
        Some(f) => Settled {
            state: f.band.state,
            band: Some(decide(&f, now)),
            why: f.why,
        },
        None => {
            let pending: Vec<String> = settling
                .iter()
                .map(|f| {
                    format!(
                        "settling: {} — held {}m of the {}m it must hold",
                        f.why,
                        held(f, now).unwrap_or(0),
                        f.band.hold_minutes
                    )
                })
                .collect();
            Settled {
                state: RegionState::Clear,
                why: std::iter::once(clear_why)
                    .chain(pending)
                    .collect::<Vec<_>>()
                    .join(" · "),
                band: None,
            }
        }
    }
}

/// The payload's form of the finding that decided a state.
pub fn decide(f: &Finding, now: Instant) -> Decided {
    Decided {
        id: f.band.id.to_string(),
        reads: if f.measured.is_empty() {
            f.band.band.to_string()
        } else {
            format!("{} > {}", f.measured, f.band.band)
        },
        hold_minutes: f.band.hold_minutes,
        since: f.since.map(|s| s.to_rfc3339()),
        held_minutes: held(f, now),
        held: held(f, now).map(duration_text),
    }
}

/// How long, as a header says it: `45m`, `6h`, `3d` — the largest unit
/// that is whole, so a reading never claims more precision than it
/// has. Written once, here, and carried on the payload as
/// [`Decided::held`].
pub fn duration_text(minutes: i64) -> String {
    if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 48 * 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", minutes / (24 * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Instant {
        crate::regions::parse_instant(s).unwrap()
    }

    const NOW: &str = "2026-09-24T12:00:00Z";

    #[test]
    fn every_band_is_declared_once_with_a_known_region_and_a_sane_hold() {
        let mut ids: Vec<&str> = BANDS.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "a band id is declared twice");
        for b in BANDS {
            assert!(
                b.region == "*" || crate::regions::REGIONS.contains(&b.region),
                "{} names no region the map draws: {}",
                b.id,
                b.region
            );
            assert!(b.hold_minutes >= 0, "{}", b.id);
            assert_ne!(
                b.state,
                RegionState::Clear,
                "{}: a band decides a non-clear state; clear is the absence of one",
                b.id
            );
        }
    }

    /// A band whose line is an existing named constant says that
    /// constant's value in its words — the const fn cannot format, so
    /// the two are held equal here (CLAUDE.md §9a) rather than by a
    /// comment.
    #[test]
    fn a_band_that_names_a_threshold_names_the_constant_that_decides_it() {
        use crate::regions::{AGING_DAYS, PROOF_STALE_HOURS, STALE_DAYS, STALLED_PUBLISH_HOURS};
        for (b, says) in [
            (RECEIVING_AGING, format!("{AGING_DAYS}-day")),
            (RECEIVING_STALE, format!("{STALE_DAYS}-day")),
            (SHED_OURS_STALE, format!("{PROOF_STALE_HOURS}h")),
            (SHED_THEIRS_STALE, format!("{PROOF_STALE_HOURS}h")),
            (PUBLISH_PR_STALLED, format!("{STALLED_PUBLISH_HOURS}h")),
            (PUBLISH_HELD, format!("{STALLED_PUBLISH_HOURS}h")),
            (
                BORDER_STILL,
                format!("{STILL_AFTER_GAPS}× the rail's own mean gap"),
            ),
        ] {
            assert!(
                b.band.contains(&says),
                "{}: {:?} lacks {says}",
                b.id,
                b.band
            );
        }
        // And a band that names its hold says the hold it declares.
        for b in BANDS.iter().filter(|b| b.band.contains(" for ")) {
            assert!(
                b.band.contains(&format!("{}m", b.hold_minutes)),
                "{}: {:?} does not say its {}m hold",
                b.id,
                b.band,
                b.hold_minutes
            );
        }
    }

    #[test]
    fn a_condition_inside_its_hold_is_settling_and_changes_nothing() {
        let f = Finding::new(
            SHOP_FLOOR_UNREPORTED,
            Some(t("2026-09-24T11:55:00Z")),
            String::new(),
            "1 run finished and not reported".into(),
        );
        let s = settle(vec![f], "2 runs in flight".into(), t(NOW));
        assert_eq!(s.state, RegionState::Clear, "{}", s.why);
        assert!(s.band.is_none());
        assert!(
            s.why.starts_with("2 runs in flight")
                && s.why.contains("settling: 1 run finished and not reported")
                && s.why.contains("held 5m of the 10m"),
            "{}",
            s.why
        );
    }

    #[test]
    fn a_condition_past_its_hold_decides_the_state_and_says_for_how_long() {
        let f = Finding::new(
            SHOP_FLOOR_UNREPORTED,
            Some(t("2026-09-24T11:44:00Z")),
            String::new(),
            "1 run finished and not reported".into(),
        );
        let s = settle(vec![f], "clear".into(), t(NOW));
        assert_eq!(s.state, RegionState::Troubled);
        let band = s.band.expect("a troubled state names its band");
        assert_eq!(band.id, "shop-floor-unreported");
        assert_eq!(band.held_minutes, Some(16));
        assert_eq!(band.held.as_deref(), Some("16m"));
        assert_eq!(band.hold_minutes, 10);
        assert_eq!(band.since.as_deref(), Some("2026-09-24T11:44:00+00:00"));
        assert_eq!(s.why, "1 run finished and not reported");
    }

    #[test]
    fn an_onset_the_record_does_not_hold_is_stated_at_once_with_no_duration() {
        let f = Finding::new(
            TRACK_GATE_WAITING,
            None,
            String::new(),
            "gate not filed yet: train #471".into(),
        );
        let s = settle(vec![f], "clear".into(), t(NOW));
        assert_eq!(s.state, RegionState::Attention);
        let band = s.band.unwrap();
        assert_eq!(band.held_minutes, None);
        assert_eq!(band.reads, TRACK_GATE_WAITING.band);
    }

    #[test]
    fn the_worst_held_state_wins_and_the_first_of_equals_names_it() {
        let at = Some(t("2026-09-24T10:00:00Z"));
        let s = settle(
            vec![
                Finding::new(RECEIVING_AGING, at, "oldest 5d".into(), "aging".into()),
                Finding::new(SHED_UNPROVEN, at, String::new(), "first trouble".into()),
                Finding::new(SHED_FAILING, at, String::new(), "second trouble".into()),
            ],
            "clear".into(),
            t(NOW),
        );
        assert_eq!(s.state, RegionState::Troubled);
        assert_eq!(s.why, "first trouble");
        let s = settle(
            vec![Finding::new(
                RECEIVING_AGING,
                at,
                "oldest 5d".into(),
                "aging".into(),
            )],
            "clear".into(),
            t(NOW),
        );
        assert_eq!(s.state, RegionState::Attention);
        assert_eq!(
            s.band.unwrap().reads,
            "oldest 5d > the 3-day triage band",
            "the header reads the measurement against the band"
        );
    }

    #[test]
    fn a_duration_is_said_in_its_largest_whole_unit() {
        assert_eq!(duration_text(6), "6m");
        assert_eq!(duration_text(59), "59m");
        assert_eq!(duration_text(60), "1h");
        assert_eq!(duration_text(47 * 60 + 59), "47h");
        assert_eq!(duration_text(48 * 60), "2d");
    }
}
