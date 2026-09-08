//! Shared types for the global-calendar primitive.
//!
//! A calendar reservation claims a time window on a **Subject** — a
//! reservation is always on a subject (the employee, asset, account,
//! … being scheduled). Which subject kinds may be reserved is data:
//! a `calendar_reservable` flag on the subject_kinds registry, not a
//! closed type here. The load-bearing "no two hard reservations
//! overlap on one subject" invariant is enforced by a Postgres GIST
//! exclusion constraint keyed on `(subject_kind, subject_id, window)`.
//!
//! Lives in `boss-core` because every domain crate needs to build a
//! `ReservationRequest` without taking a dep on `boss-calendar`.
//!
//! Decision record: `docs/architecture-decisions.md` §Calendar.

use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::define_id;
use crate::job::Subject;

define_id!(ReservationId);

/// Stable composite key for a reserved subject — `<subject_kind>:<id>`.
/// Postgres builds the same string for the exclusion-constraint key, so
/// in-memory adapters use this for their own collision checks. (The
/// "what can be reserved" question is data — the `calendar_reservable`
/// flag on the subject_kinds registry — not a closed type.)
pub fn reservation_key(subject: &Subject) -> String {
    format!("{}:{}", subject.kind, subject.id)
}

/// Conventional `reason_kind` tags — the reasons BOSS itself emits.
/// `reason_kind` is a free-form string on the reservation, so a tenant
/// can use its own reason without a core change (what the old `Custom`
/// variant existed to allow — now just "any other string"). These
/// consts keep the well-known set spelled one way across the callers,
/// the seed data, and the SPA's reason labels.
pub mod reason {
    pub const JOB_STEP: &str = "job-step";
    pub const PREVENTIVE_MAINTENANCE_VISIT: &str = "preventive-maintenance-visit";
    pub const TRAINING: &str = "training";
    pub const PTO: &str = "pto";
    pub const MEETING: &str = "meeting";
    pub const TRAVEL: &str = "travel";
}

/// Hard reservations participate in the exclusion constraint —
/// Postgres refuses a conflicting INSERT. Soft reservations can
/// overlap each other and overlap hards (warning at the UI, not a
/// 409). See Q2 in the design doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReservationStrength {
    Hard,
    Soft,
}

impl ReservationStrength {
    pub fn db_value(&self) -> &'static str {
        match self {
            ReservationStrength::Hard => "hard",
            ReservationStrength::Soft => "soft",
        }
    }
}

/// Half-open time window `[start, end)`. Stored UTC per Q1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeWindow {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, &'static str> {
        if end <= start {
            return Err("TimeWindow end must be strictly after start");
        }
        Ok(Self { start, end })
    }

    /// True iff the two windows share any point. Half-open means
    /// `[10:00, 11:00)` and `[11:00, 12:00)` do *not* overlap.
    pub fn overlaps(&self, other: &TimeWindow) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn duration_seconds(&self) -> i64 {
        (self.end - self.start).num_seconds()
    }
}

/// Input for `CalendarClient::reserve`. The implementation assigns
/// a new `ReservationId` and a `created_at` timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationRequest {
    /// The subject being reserved. Its kind must be
    /// `calendar_reservable` in the subject_kinds registry (enforced by
    /// the calendar on reserve); any individual subject can hold only
    /// one hard reservation per overlapping window.
    pub subject: Subject,
    pub window: TimeWindow,
    /// Free-form reason tag — see the `reason` module for the
    /// conventional values. Any string is valid (the old `Custom`
    /// escape hatch is now just "any other string").
    pub reason_kind: String,
    /// Stable identifier of the thing this reservation is for —
    /// a JobId, a PmScheduleId, a TrainingSessionId, etc. Used
    /// for cancellation cascade (delete every reservation whose
    /// `reason_ref_id` equals X) and for UI rendering ("this is
    /// blocking your tech because of Job-12345").
    pub reason_ref_id: String,
    pub strength: ReservationStrength,
    /// Free-form context shown to humans. Optional.
    #[serde(default)]
    pub notes: Option<String>,
    /// Actor making the reservation — employee id, "system-cron",
    /// "boss-jobs-api", etc. Recorded as `created_by`.
    pub created_by: String,
}

/// One row from `calendar_reservations`. What `CalendarClient::list`
/// returns and what conflict errors carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: ReservationId,
    pub subject: Subject,
    pub window: TimeWindow,
    /// Free-form reason tag — see the `reason` module for the
    /// conventional values. Any string is valid (the old `Custom`
    /// escape hatch is now just "any other string").
    pub reason_kind: String,
    pub reason_ref_id: String,
    pub strength: ReservationStrength,
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub cancelled_at: Option<DateTime<Utc>>,
}

/// A named business calendar — as DATA, not code. The set of
/// non-business days for a calendar (`us-banking`, `us-tax`, …):
/// `weekend` weekdays plus concrete `closed` dates (federal holidays
/// + any closed windows expanded to individual days). The business-day
/// queries below are GENERIC over this data — there is no per-calendar
/// Rust. This is the type that makes "a tax calendar just data": the
/// rows are seeded into `boss-calendar` and fetched by callers (the
/// dispatcher's timing triggers, the simulator) via `boss-calendar-client`.
///
/// Lives in `boss-core` so the dispatcher + client + service share one
/// definition and one business-day implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessCalendar {
    /// Stable code, e.g. `us-banking` / `us-tax`.
    pub code: String,
    pub name: String,
    /// Weekdays that are non-business, as `Weekday::num_days_from_monday()`
    /// (Mon=0 … Sun=6). [`BusinessCalendar::new`] defaults to Sat+Sun.
    pub weekend: BTreeSet<u8>,
    /// Concrete non-business dates — holidays + closed windows expanded
    /// to individual days. The "just data" part of a tax calendar.
    pub closed: BTreeSet<NaiveDate>,
}

impl BusinessCalendar {
    /// A calendar with the conventional Sat+Sun weekend and no holidays.
    pub fn new(code: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            name: name.into(),
            weekend: [5, 6].into_iter().collect(), // Sat, Sun
            closed: BTreeSet::new(),
        }
    }

    /// Builder: add closed (non-business) dates.
    pub fn with_closed(mut self, dates: impl IntoIterator<Item = NaiveDate>) -> Self {
        self.closed.extend(dates);
        self
    }

    /// True iff `date` is a working day on this calendar — not a weekend
    /// weekday and not a `closed` day.
    pub fn is_business_day(&self, date: NaiveDate) -> bool {
        let wd = date.weekday().num_days_from_monday() as u8;
        !self.weekend.contains(&wd) && !self.closed.contains(&date)
    }

    /// The first business day strictly after `date`. Bounded so a
    /// pathological all-closed calendar can't loop forever; a sane
    /// calendar resolves within days.
    pub fn next_business_day(&self, date: NaiveDate) -> NaiveDate {
        let mut d = date;
        for _ in 0..366 {
            d = d.succ_opt().unwrap_or(d);
            if self.is_business_day(d) {
                return d;
            }
        }
        d
    }

    /// The first business day on or after `date` (returns `date` when it
    /// is already a business day). The postponement primitive for sparse
    /// cadences: a monthly/quarterly fire landing on a holiday pushes to
    /// the next business day.
    pub fn business_day_on_or_after(&self, date: NaiveDate) -> NaiveDate {
        if self.is_business_day(date) {
            date
        } else {
            self.next_business_day(date)
        }
    }

    /// `date` shifted by `n` business days (n>0 forward, n<0 back; n=0
    /// returns `date` unchanged even if it's non-business). Counts only
    /// business days crossed.
    pub fn add_business_days(&self, date: NaiveDate, n: i64) -> NaiveDate {
        if n == 0 {
            return date;
        }
        let forward = n > 0;
        let mut remaining = n.unsigned_abs();
        let mut d = date;
        for _ in 0..(366 * 4) {
            d = if forward {
                d.succ_opt().unwrap_or(d)
            } else {
                d.pred_opt().unwrap_or(d)
            };
            if self.is_business_day(d) {
                remaining -= 1;
                if remaining == 0 {
                    return d;
                }
            }
        }
        d
    }
}

// ---------------------------------------------------------------------------
// Recurrence — when a thing repeats.
// ---------------------------------------------------------------------------

/// How often something recurs, and whether it fires on a given day.
///
/// WHY THIS LIVES BESIDE `BusinessCalendar` (design packet a02b01e0,
/// decided 2026-08-28). Recurrence was defined TWICE — once in
/// `boss-dispatcher`, once in `boss-sim` — and the dispatcher's copy
/// existed only because "the dispatcher is Tier-1 and can't depend on
/// the sim", a tier constraint rather than a design decision. Both
/// already reached into this module for `BusinessCalendar` to answer
/// "is this a working day", so the rule was stranded in its consumers
/// while the concept it depends on sat here.
///
/// A third spelling lived in the cadence loop (`Wall { every_minutes }`
/// is `EveryNMinutes` under another name), and it could not express
/// "weekly" at all — which is what surfaced this.
///
/// David, 2026-08-28: *"I would be good having our IT workflows
/// essentially key off the same calendar infrastructure that everyone
/// uses."* A company has one calendar; a brewery shift, a PTO block, a
/// maintenance window and a protocol retro are all things that happen
/// on a schedule.
///
/// PURE ON PURPOSE. `fires_on` is a function of two dates: no clock, no
/// network, no state. That is what lets a scheduler decide whether to
/// fire without depending on the calendar SERVICE being up — a
/// scheduler that stops scheduling because another service is down
/// would be worse than the duplication this replaces.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Cadence {
    Daily,
    Weekly,
    Biweekly,
    Monthly,
    Quarterly,
    Annually,
    /// Fires once per hour. Day-granularity callers see "every day".
    Hourly,
    /// Fires every `n` minutes from midnight. Day-granularity callers
    /// see "every day"; sub-day resolution belongs to the caller that
    /// has a tick (see the note on [`Cadence::fires_on`]).
    EveryNMinutes(u32),
}

impl Cadence {
    /// Does this cadence fire on `day`, given its `anchor`?
    ///
    /// Pure calendar math — business-calendar postponement is applied
    /// *outside* this function by [`fires_on_with_calendar`].
    ///
    /// SUB-DAY CADENCES RETURN `true` FOR EVERY DAY on or after the
    /// anchor. Resolving *which tick within the day* needs a tick, and
    /// a tick is a property of whatever is driving time — the
    /// simulator has one, the wall-clock cadence loop has a different
    /// one. So that decision stays with the caller and does not come
    /// here. Day-anchored callers get the right yes/no directly.
    pub fn fires_on(&self, anchor: NaiveDate, day: NaiveDate) -> bool {
        if day < anchor {
            return false;
        }
        match self {
            Cadence::Daily => true,
            Cadence::Weekly => day.weekday() == anchor.weekday(),
            Cadence::Biweekly => {
                day.weekday() == anchor.weekday() && (day - anchor).num_days() % 14 == 0
            }
            Cadence::Monthly => {
                day.day() == clamp_anchor_day(anchor.day(), day.year(), day.month())
            }
            Cadence::Quarterly => {
                if day.day() != clamp_anchor_day(anchor.day(), day.year(), day.month()) {
                    return false;
                }
                let months_diff = ((day.year() as i64 - anchor.year() as i64) * 12)
                    + (day.month() as i64 - anchor.month() as i64);
                months_diff >= 0 && months_diff % 3 == 0
            }
            Cadence::Annually => {
                day.month() == anchor.month()
                    && day.day() == clamp_anchor_day(anchor.day(), day.year(), day.month())
            }
            // Sub-day: every day, resolved finer by the caller.
            Cadence::Hourly | Cadence::EveryNMinutes(_) => true,
        }
    }

    /// Sparse cadences, where a single dropped fire would lose a whole
    /// period rather than skipping one of many. These are the ones a
    /// business calendar POSTPONES instead of skipping.
    pub fn is_coarse(&self) -> bool {
        matches!(
            self,
            Cadence::Monthly | Cadence::Quarterly | Cadence::Annually
        )
    }

    /// String form stored in the `schedule_cadence` DB column /
    /// authored in TOML (kebab-case). Round-trips with [`Cadence::parse`].
    ///
    /// Returns `String`, not `&'static str`, because `EveryNMinutes`
    /// carries a number. The six day-cadence spellings are UNCHANGED —
    /// existing rows and TOML keep parsing exactly as before.
    pub fn token(&self) -> String {
        match self {
            Cadence::Daily => "daily".into(),
            Cadence::Weekly => "weekly".into(),
            Cadence::Biweekly => "biweekly".into(),
            Cadence::Monthly => "monthly".into(),
            Cadence::Quarterly => "quarterly".into(),
            Cadence::Annually => "annually".into(),
            Cadence::Hourly => "hourly".into(),
            Cadence::EveryNMinutes(n) => format!("every-{n}-minutes"),
        }
    }

    /// Parse the `schedule_cadence` DB value. Case-insensitive; accepts
    /// the kebab spelling used in TOML.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().to_ascii_lowercase();
        match raw.as_str() {
            "daily" => return Some(Cadence::Daily),
            "weekly" => return Some(Cadence::Weekly),
            "biweekly" => return Some(Cadence::Biweekly),
            "monthly" => return Some(Cadence::Monthly),
            "quarterly" => return Some(Cadence::Quarterly),
            "annually" => return Some(Cadence::Annually),
            "hourly" => return Some(Cadence::Hourly),
            _ => {}
        }
        // every-<n>-minutes. Zero is refused: a cadence that fires
        // every zero minutes is a busy loop, not a schedule.
        let n = raw.strip_prefix("every-")?.strip_suffix("-minutes")?;
        let n: u32 = n.parse().ok()?;
        if n == 0 || n > 1440 {
            return None;
        }
        Some(Cadence::EveryNMinutes(n))
    }
}

/// Last calendar day of `(year, month)` — 28–31 (29 in a leap February).
fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first_of_next| first_of_next.pred_opt())
        .map(|last| last.day())
        .unwrap_or(28)
}

/// Clamp a month-anchored day into a month that may be shorter — a
/// day-31 anchor fires on the 30th in April and the 28th/29th in
/// February. Without this a month-end anchor silently never matches the
/// short months, losing those periods entirely.
fn clamp_anchor_day(anchor_day: u32, year: i32, month: u32) -> u32 {
    anchor_day.min(last_day_of_month(year, month))
}

/// How many days a sparse-cadence fire may be carried forward to reach a
/// business day. Covers any realistic weekend + holiday closure while
/// staying well under the ~28-day minimum gap between monthly fires, so
/// the look-back can never reach the previous period's nominal day.
pub const MAX_POSTPONE_DAYS: u64 = 10;

/// THE day-firing decision: does a cadence anchored at `anchor` fire on
/// `day`, given an optional business calendar?
///
/// - DENSE cadences (daily/weekly/biweekly, and the sub-day ones) SKIP
///   non-business days. Losing one of many is the intended behaviour.
/// - SPARSE cadences (monthly and coarser) POSTPONE onto the next
///   business day, walking back up to [`MAX_POSTPONE_DAYS`] — a
///   non-business nominal day is carried forward, not dropped, so a
///   whole period is never lost.
/// - With no calendar, a cadence fires exactly on its nominal day.
///
/// Pure: no I/O, deterministic in `(cadence, anchor, cal, day)`.
pub fn fires_on_with_calendar(
    cadence: Cadence,
    anchor: NaiveDate,
    cal: Option<&BusinessCalendar>,
    day: NaiveDate,
) -> bool {
    if cadence.is_coarse() {
        match cal {
            None => cadence.fires_on(anchor, day),
            Some(cal) => {
                if !cal.is_business_day(day) {
                    return false;
                }
                for back in 0..=MAX_POSTPONE_DAYS {
                    let Some(nominal) = day.checked_sub_days(Days::new(back)) else {
                        break;
                    };
                    // `business_day_on_or_after` (not `next_business_day`):
                    // a nominal day that is itself a business day must map
                    // to itself (back==0 fires on the day); a non-business
                    // nominal carries forward to the first business day on
                    // or after it.
                    if cadence.fires_on(anchor, nominal)
                        && cal.business_day_on_or_after(nominal) == day
                    {
                        return true;
                    }
                }
                false
            }
        }
    } else {
        // Dense: a non-business day SKIPS — "every Tuesday" means Tuesday.
        cadence.fires_on(anchor, day) && cal.map(|c| c.is_business_day(day)).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, h, m, 0).unwrap()
    }

    #[test]
    fn reservation_key_is_subject_kind_colon_id() {
        assert_eq!(
            reservation_key(&Subject::new("employee", "emp-001")),
            "employee:emp-001"
        );
        assert_eq!(
            reservation_key(&Subject::new("asset", "sys-001")),
            "asset:sys-001"
        );
        assert_eq!(
            reservation_key(&Subject::new("account", "acc-mercy")),
            "account:acc-mercy"
        );
    }

    #[test]
    fn time_window_rejects_zero_or_negative_duration() {
        assert!(TimeWindow::new(t(10, 0), t(10, 0)).is_err());
        assert!(TimeWindow::new(t(11, 0), t(10, 0)).is_err());
        assert!(TimeWindow::new(t(10, 0), t(11, 0)).is_ok());
    }

    #[test]
    fn time_window_overlap_is_strict_half_open() {
        let a = TimeWindow::new(t(10, 0), t(11, 0)).unwrap();
        let b = TimeWindow::new(t(11, 0), t(12, 0)).unwrap();
        // [10, 11) and [11, 12) touch but don't overlap.
        assert!(!a.overlaps(&b));
        assert!(!b.overlaps(&a));
        // [10:30, 11:30) and [11, 12) do overlap.
        let c = TimeWindow::new(t(10, 30), t(11, 30)).unwrap();
        assert!(c.overlaps(&b));
        assert!(b.overlaps(&c));
        // Containment is overlap.
        let outer = TimeWindow::new(t(9, 0), t(13, 0)).unwrap();
        let inner = TimeWindow::new(t(10, 0), t(11, 0)).unwrap();
        assert!(outer.overlaps(&inner));
        assert!(inner.overlaps(&outer));
    }

    #[test]
    fn reservation_request_round_trips_through_json() {
        let req = ReservationRequest {
            subject: Subject::new("employee", "emp-042"),
            window: TimeWindow::new(t(14, 0), t(16, 0)).unwrap(),
            reason_kind: reason::JOB_STEP.to_string(),
            reason_ref_id: "stp-xyz".into(),
            strength: ReservationStrength::Hard,
            notes: Some("urgent repair".into()),
            created_by: "emp-svc-mgr".into(),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: ReservationRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.reason_ref_id, "stp-xyz");
        assert_eq!(back.subject.kind, "employee");
    }

    // --- business calendars: data-driven business-day logic ---------------
    // These port the assertions that used to live in the simulator's
    // hardcoded `us_banking.rs` / `us_tax.rs` — now expressed over DATA.

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn weekend_and_holidays_are_non_business() {
        // us-banking-like: Sat+Sun weekend + a weekday federal holiday.
        // 2026-01-01 (New Year's Day) is a Thursday.
        let cal = BusinessCalendar::new("us-banking", "US Banking").with_closed([day(2026, 1, 1)]);
        assert!(!cal.is_business_day(day(2026, 1, 1))); // Thu holiday
        assert!(cal.is_business_day(day(2026, 1, 2))); // Fri
        assert!(!cal.is_business_day(day(2026, 1, 3))); // Sat
        assert!(!cal.is_business_day(day(2026, 1, 4))); // Sun
        assert!(cal.is_business_day(day(2026, 1, 5))); // Mon
    }

    #[test]
    fn tax_surge_window_as_data_is_non_business() {
        // Ports us_tax.rs: the Apr 12-19 filing-surge window, now DATA
        // (the window expanded to concrete closed dates).
        let window = (12..=19).map(|d| day(2026, 4, d));
        let cal = BusinessCalendar::new("us-tax", "US Tax").with_closed(window);
        assert!(!cal.is_business_day(day(2026, 4, 13))); // Mon in surge
        assert!(!cal.is_business_day(day(2026, 4, 15))); // Apr 15 itself (Wed)
        assert!(!cal.is_business_day(day(2026, 4, 17))); // Fri in surge
        assert!(cal.is_business_day(day(2026, 4, 27))); // Mon, outside surge
    }

    #[test]
    fn next_business_day_skips_weekend_and_holiday() {
        let cal = BusinessCalendar::new("c", "c").with_closed([day(2026, 1, 1)]);
        // Wed 12/31 → Thu 1/1 is a holiday → Fri 1/2.
        assert_eq!(cal.next_business_day(day(2025, 12, 31)), day(2026, 1, 2));
        // Fri 1/2 → Sat/Sun skipped → Mon 1/5.
        assert_eq!(cal.next_business_day(day(2026, 1, 2)), day(2026, 1, 5));
    }

    #[test]
    fn business_day_on_or_after_is_identity_on_a_business_day() {
        let cal = BusinessCalendar::new("c", "c");
        assert_eq!(
            cal.business_day_on_or_after(day(2026, 1, 2)),
            day(2026, 1, 2)
        ); // Fri
        assert_eq!(
            cal.business_day_on_or_after(day(2026, 1, 3)),
            day(2026, 1, 5)
        ); // Sat → Mon
    }

    #[test]
    fn add_business_days_counts_only_business_days() {
        let cal = BusinessCalendar::new("c", "c");
        assert_eq!(cal.add_business_days(day(2026, 1, 2), 1), day(2026, 1, 5)); // Fri +1 → Mon
        assert_eq!(cal.add_business_days(day(2026, 1, 5), -1), day(2026, 1, 2)); // Mon -1 → Fri
        assert_eq!(cal.add_business_days(day(2026, 1, 3), 0), day(2026, 1, 3)); // 0 = identity
    }

    #[test]
    fn business_calendar_round_trips_through_json() {
        let cal = BusinessCalendar::new("us-tax", "US Tax").with_closed([day(2026, 4, 15)]);
        let s = serde_json::to_string(&cal).unwrap();
        let back: BusinessCalendar = serde_json::from_str(&s).unwrap();
        assert_eq!(back, cal);
        assert!(!back.is_business_day(day(2026, 4, 15)));
    }

    // ----- recurrence (moved here with the code, design a02b01e0) -----

    fn d(y: i32, m: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, dd).unwrap()
    }

    #[test]
    fn clamp_anchor_day_handles_short_months() {
        assert_eq!(clamp_anchor_day(31, 2025, 4), 30); // April -> 30
        assert_eq!(clamp_anchor_day(31, 2025, 2), 28); // Feb, non-leap
        assert_eq!(clamp_anchor_day(31, 2024, 2), 29); // Feb, leap
        assert_eq!(clamp_anchor_day(31, 2025, 1), 31); // Jan, unchanged
        assert_eq!(clamp_anchor_day(15, 2025, 2), 15); // below month length
    }

    /// The six day-cadence tokens are DB values and TOML spellings.
    /// They must round-trip byte-for-byte after the move, or existing
    /// dispatcher_rules rows stop parsing.
    #[test]
    fn the_six_day_cadence_tokens_are_unchanged() {
        for (c, want) in [
            (Cadence::Daily, "daily"),
            (Cadence::Weekly, "weekly"),
            (Cadence::Biweekly, "biweekly"),
            (Cadence::Monthly, "monthly"),
            (Cadence::Quarterly, "quarterly"),
            (Cadence::Annually, "annually"),
        ] {
            assert_eq!(c.token(), want);
            assert_eq!(Cadence::parse(want), Some(c));
        }
        assert_eq!(Cadence::parse("MONTHLY"), Some(Cadence::Monthly));
        assert_eq!(Cadence::parse("  weekly "), Some(Cadence::Weekly));
        assert_eq!(Cadence::parse("never"), None);
    }

    #[test]
    fn sub_day_cadences_round_trip_too() {
        assert_eq!(Cadence::Hourly.token(), "hourly");
        assert_eq!(Cadence::parse("hourly"), Some(Cadence::Hourly));
        let every = Cadence::EveryNMinutes(15);
        assert_eq!(every.token(), "every-15-minutes");
        assert_eq!(Cadence::parse("every-15-minutes"), Some(every));
        // A cadence firing every zero minutes is a busy loop, and more
        // than a day is not a sub-day cadence.
        assert_eq!(Cadence::parse("every-0-minutes"), None);
        assert_eq!(Cadence::parse("every-1441-minutes"), None);
        assert_eq!(Cadence::parse("every-x-minutes"), None);
    }

    /// Sub-day cadences answer "every day" at day granularity — the
    /// which-tick decision belongs to whatever drives time.
    #[test]
    fn sub_day_cadences_fire_every_day_from_the_anchor() {
        let anchor = d(2026, 8, 28);
        for c in [Cadence::Hourly, Cadence::EveryNMinutes(10)] {
            assert!(c.fires_on(anchor, anchor));
            assert!(c.fires_on(anchor, d(2026, 8, 29)));
            assert!(
                !c.fires_on(anchor, d(2026, 8, 27)),
                "never before the anchor"
            );
        }
        assert!(
            !Cadence::Hourly.is_coarse(),
            "sub-day is dense, so it skips rather than postpones"
        );
    }

    /// Weekly is the case that started this: it was inexpressible in
    /// the cadence loop, which is why protocol-retro could not be
    /// scheduled.
    #[test]
    fn weekly_fires_on_the_anchors_weekday() {
        let anchor = d(2026, 8, 28); // a Friday
        assert!(Cadence::Weekly.fires_on(anchor, d(2026, 9, 4)));
        assert!(!Cadence::Weekly.fires_on(anchor, d(2026, 9, 3)));
    }

    /// Dense skips a closed day; sparse carries forward onto the next
    /// business day so a whole period is never lost.
    #[test]
    fn a_calendar_skips_dense_fires_and_postpones_sparse_ones() {
        let cal = BusinessCalendar::new("weekends-off", "Weekends Off")
            .with_closed([d(2026, 9, 5), d(2026, 9, 6)]);
        // 2026-09-05 is a Saturday; a Saturday-anchored weekly fire is skipped.
        let sat_anchor = d(2026, 9, 5);
        assert!(!fires_on_with_calendar(
            Cadence::Weekly,
            sat_anchor,
            Some(&cal),
            d(2026, 9, 5)
        ));
        // Monthly anchored on the 5th carries forward to Monday the 7th.
        assert!(fires_on_with_calendar(
            Cadence::Monthly,
            sat_anchor,
            Some(&cal),
            d(2026, 9, 7)
        ));
        assert!(!fires_on_with_calendar(
            Cadence::Monthly,
            sat_anchor,
            Some(&cal),
            d(2026, 9, 5)
        ));
        // With no calendar, both fire on their nominal day.
        assert!(fires_on_with_calendar(
            Cadence::Monthly,
            sat_anchor,
            None,
            d(2026, 9, 5)
        ));
    }
}
