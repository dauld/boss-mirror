//! Stamps, windows and the shared judging (moved out of `regions.rs`,
//! backlog 07addeed). Every judge reads its instants through these
//! readers, measures this window against the previous one with
//! [`Windows`], and returns its card through [`region`] — so a stamp is
//! read one way, and a card is shaped one way, on every region.

use super::*;

pub(super) fn md_str<'a>(md: &'a Value, key: &str) -> &'a str {
    md.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(crate) type Instant = chrono::DateTime<chrono::Utc>;

pub(crate) fn parse_instant(s: &str) -> Option<Instant> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

pub(crate) fn meta_instant(md: &Value, key: &str) -> Option<Instant> {
    md.get(key).and_then(Value::as_str).and_then(parse_instant)
}

/// A step by slug, with the title fallback the conductor's own
/// addressing uses.
pub(crate) fn find_step<'a>(steps: &'a [Step], slug: &str, title: &str) -> Option<&'a Step> {
    steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug) || s.title == title)
}

/// When a step completed: the server's column first, then the
/// conductor's metadata stamp — the same two readers `yard.rs`'s
/// `verdict_instant` has, for the same reason (older rows carry only
/// the stamp).
pub(crate) fn step_done_at(step: Option<&Step>) -> Option<Instant> {
    let s = step?;
    if s.status != StepStatus::Completed {
        return None;
    }
    s.completed_at
        .or_else(|| meta_instant(&s.metadata, "completed_at"))
}

/// The instant a packet opened: the `opened_at` stamp every pipeline
/// packet carries, else its `opened_on` at midnight — coarser, and
/// still inside the right day.
pub(crate) fn opened_at(job: &Job) -> Option<Instant> {
    meta_instant(&job.metadata, "opened_at").or_else(|| {
        job.opened_on
            .and_hms_opt(0, 0, 0)
            .map(|t| chrono::DateTime::from_naive_utc_and_offset(t, chrono::Utc))
    })
}

/// The instant a packet closed: `closed_at`, which the server writes
/// when a declared terminal step completes.
pub(crate) fn closed_at(job: &Job) -> Option<Instant> {
    meta_instant(&job.metadata, "closed_at")
}

/// This window and the previous one, as half-open ranges ending at
/// `now`: `[since, now)` and `[before, since)`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Windows {
    before: Instant,
    since: Instant,
    now: Instant,
    pub(crate) hours: i64,
}

impl Windows {
    pub(crate) fn of(now: Instant, hours: i64) -> Self {
        let w = chrono::Duration::hours(hours);
        Windows {
            before: now - w - w,
            since: now - w,
            now,
            hours,
        }
    }
    pub(crate) fn current(&self, t: Instant) -> bool {
        t >= self.since && t <= self.now
    }
    pub(crate) fn previous(&self, t: Instant) -> bool {
        t >= self.before && t < self.since
    }
    /// A count as a per-day rate over one window.
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn per_day(&self, n: usize) -> f64 {
        n as f64 * 24.0 / self.hours as f64
    }
}

/// The middle observation of a sample — an observation, not an
/// interpolation, the convention the yard's `quantile` keeps so every
/// number on the map was read off a real packet.
fn median(mut v: Vec<i64>) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    v.get(v.len() / 2).copied()
}

#[allow(clippy::cast_precision_loss)]
fn seconds_as(unit: &str, s: i64) -> f64 {
    match unit {
        "hours" => s as f64 / 3600.0,
        "minutes" => s as f64 / 60.0,
        _ => s as f64,
    }
}

/// A median-duration trend: the samples that fall in each window,
/// reduced to their middle observation in `unit`.
pub(super) fn duration_trend(
    metric: &str,
    unit: &str,
    current: Vec<i64>,
    previous: Vec<i64>,
) -> Trend {
    Trend {
        metric: metric.to_string(),
        unit: unit.to_string(),
        samples: current.len(),
        previous_samples: previous.len(),
        current: median(current).map(|s| seconds_as(unit, s)),
        previous: median(previous).map(|s| seconds_as(unit, s)),
    }
}

/// A per-day rate trend from two counts. A count is a measurement even
/// when it is zero — an empty window is a rate of 0, not "unknown" —
/// so both halves are always `Some`.
pub(crate) fn rate_trend(metric: &str, w: &Windows, current: usize, previous: usize) -> Trend {
    Trend {
        metric: metric.to_string(),
        unit: "per day".to_string(),
        current: Some(w.per_day(current)),
        previous: Some(w.per_day(previous)),
        samples: current,
        previous_samples: previous,
    }
}

/// Split instants into (current, previous) samples.
pub(super) fn split<I>(w: &Windows, items: I) -> (Vec<i64>, Vec<i64>)
where
    I: IntoIterator<Item = (Instant, i64)>,
{
    let mut cur = Vec::new();
    let mut prev = Vec::new();
    for (at, sample) in items {
        if w.current(at) {
            cur.push(sample);
        } else if w.previous(at) {
            prev.push(sample);
        }
    }
    (cur, prev)
}

pub(crate) fn count_split<I>(w: &Windows, items: I) -> (usize, usize)
where
    I: IntoIterator<Item = Instant>,
{
    let (c, p) = split(w, items.into_iter().map(|t| (t, 0)));
    (c.len(), p.len())
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// One region's card, from its count, its unit, its settled state (every
/// state passes through [`crate::region_states::settle`]) and its KPI.
pub(super) fn region(
    name: &str,
    count: Option<usize>,
    bound: Option<(usize, BoundKind)>,
    unit: &str,
    settled: Settled,
    trend: Trend,
    kpi: Vec<Measure>,
) -> Region {
    Region {
        name: name.to_string(),
        count,
        bound: bound.map(|(b, _)| b),
        bound_kind: bound.map(|(_, k)| k),
        unit: unit.to_string(),
        state: settled.state,
        why: settled.why,
        band: settled.band,
        trend,
        kpi,
        // Attached in one pass in `regions` below, so each region
        // function stays about its own count and trend.
        machines: Vec::new(),
        // Set by the two regions that have places ([`Place`]).
        places: Vec::new(),
        // Folded in by the regions read from the moves record
        // (`crate::moves::with_undeclared`); unread until then.
        undeclared: None,
    }
}

/// A region whose input could not be read: troubled on the shared
/// [`bands::UNREAD`] band, at once, with the read named — never an empty
/// region that reads as clear.
pub(super) fn unread_settled(why: &str, now: Instant) -> Settled {
    settle(
        vec![Finding::new(
            bands::UNREAD,
            None,
            String::new(),
            why.to_string(),
        )],
        String::new(),
        now,
    )
}

/// An instant from a yard stamp: RFC 3339, or a bare date read at its
/// midnight (several yard rows carry `opened_on` where no instant was
/// stamped) — coarser, and never later than the truth.
pub(super) fn stamp_instant(stamp: &str) -> Option<Instant> {
    parse_instant(stamp).or_else(|| {
        chrono::NaiveDate::parse_from_str(stamp, "%Y-%m-%d")
            .ok()?
            .and_hms_opt(0, 0, 0)
            .map(|t| chrono::DateTime::from_naive_utc_and_offset(t, chrono::Utc))
    })
}

/// WHEN "AT LEAST `bound` OF THESE HELD" BEGAN, from the members' own
/// start instants: with them sorted, the count of current members
/// already started at `t` reaches `bound` exactly when the `bound`-th
/// oldest started — so the condition has held at least since then. A
/// member that left in between only makes the true onset EARLIER, never
/// later, so this never over-states a hold. `None` when fewer than
/// `bound` starts are known.
pub(super) fn onset_of_count(mut starts: Vec<Instant>, bound: usize) -> Option<Instant> {
    if bound == 0 {
        return None;
    }
    starts.sort_unstable();
    starts.get(bound - 1).copied()
}
