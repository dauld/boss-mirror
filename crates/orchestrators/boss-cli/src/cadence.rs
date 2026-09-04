//! `boss train cadence` — the conductor's cadence as protocol data.
//!
//! The train's scheduling knowledge used to live in two systemd
//! timers (06:00/18:00 boarding, 10-minute reconcile) — outside the
//! system, invisible to the log, changeable only by an operator with
//! sudo. Per docs/design/protocol-cadence.md (David, 2026-08-12,
//! bacca14e: "We want every protocol internalized so we can measure,
//! experiment, and update"), the schedule is now rows in the
//! `cadence_rules` registry (114-cadence-rules.sql): each rule names
//! the `boss train` verb it fires, its basis — `wall` (interval),
//! `clock` (times-of-day), `queue-depth` (parked ready cars) — and
//! the basis' parameters. This loop is the executor: each tick it
//! reads boss-clock time (never wall-clock — the no-wallclock lint's
//! invariant), evaluates the active rules, claims a deterministic
//! firing row (`cadence:<name>:<window-stamp>`), and runs the verb
//! as a child of this same binary. systemd is demoted to what an OS
//! is for: keeping this process alive (infra/train/boss-train.service).
//!
//! ALL OF THAT RIDES `/api/cadence/*` — the jobs API is the loop's
//! one door for rules, last-firings, claims and outcomes
//! (protocol-cadence.md, sequencing step 3; backlog a516f1f1). The
//! loop used to open its own sqlx pool, and BOSS_POSTGRES_URL on the
//! conductor's host named a DIFFERENT database than the system of
//! record — so `/api/cadence/rules/{name}/last-firing` answered
//! `null` ("never fired") for every rule while the loop fired on
//! schedule. One door, one database: the firings the loop records are
//! the firings the operator reads.
//!
//! Exactly-once, restated as data: the firing id is a pure function
//! of (rule, window), so a re-evaluated tick, a restarted loop, or a
//! second cadence instance all compute the same id and the
//! `cadence_firings` primary key dedupes the claim. Catch-up after
//! downtime fires at most the single most-recent missed window per
//! rule — a deliberate no-thundering-backfill choice matching the
//! conductor's one-window-at-a-time cadence (protocol-cadence Q3).
//!
//! **A verb runs beside the loop, never inside it.** Job 9c5871fa
//! (2026-08-13 10:00–10:20Z): a reconcile that deployed took 30+
//! minutes — a core-crate change forces a full workspace rebuild —
//! and because the tick awaited it inline, nothing else fired for
//! that whole window: no later reconcile, no queue-depth evaluation,
//! and no heartbeat. The scheduler went deaf exactly when an operator
//! most wanted to know it was alive. So a firing now spawns its verb
//! as a tracked task (`Runs`) and the tick returns; the task waits on
//! the child, journals the completion line, and merges rc + runtime
//! into the row it already claimed.
//!
//! What replaces the old "one verb at a time" serialization is a
//! guard scoped to what actually needs it: **one in-flight run per
//! rule**. A rule whose previous firing is still going skips its next
//! window loudly (`<rule> still running (<n>s) — not re-firing`) and
//! claims nothing. DIFFERENT rules may overlap on purpose — the
//! conductor's flock is the arbiter of whether two verbs can really
//! proceed, and its "another conductor run holds the lock — leaving"
//! is the correct, already-designed outcome for the loser. A second
//! lock in this loop would only re-implement it worse.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use boss_clock_client::{ClockClient, ReqwestClockClient};
use boss_jobs::cadence::{CadenceRuleRow, ClaimResult, LastFiring, NewFiring};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Timelike, Utc};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::train;
use boss_core::calendar::{BusinessCalendar, Cadence, fires_on_with_calendar};

/// The `boss train` verbs a cadence rule may fire — the same set the
/// CLI exposes. Pinned here so a hand-edited registry row cannot make
/// the loop spawn arbitrary arguments.
const VERBS: &[&str] = &["preflight", "reconcile", "board", "run"];

/// How far a calendar rule looks back for its most recent elapsed
/// firing day. Comfortably covers a month, so monthly rules resolve;
/// beyond that a rule that has not fired is simply waiting, and an
/// unbounded search would walk the calendar on every tick forever for
/// a rule anchored in the future.
const MAX_LOOKBACK_DAYS: u32 = 40;

/// What a rule fires. Two BOUNDED shapes, never arbitrary argv.
///
/// WHY THIS EXISTS. `cadence_rules` is a live, editable table and the
/// loader says so — "the registry is editable data". But until
/// 2026-08-28 every rule could only ever run `boss train <verb>`, so
/// the schedule was data and the thing being scheduled was not. All
/// three rules on record drove the conductor, and no other protocol
/// could be put on a schedule without a deploy.
///
/// That is the leak CLAUDE.md names: "a protocol that cannot be
/// replaced without a deploy has leaked into the substrate, and that
/// leak is the defect to hunt." The clock belongs in the substrate;
/// *what to run* is the operating model and belongs in data.
///
/// THE ALLOWLIST STAYS, in a different shape. The point of pinning
/// `VERBS` was that a hand-edited row must not spawn arbitrary
/// arguments — so `OpenPacket` does not spawn a process at all. It
/// files a packet through the jobs API, which means policy and the
/// audit log see it like any other write, and the worst a bad row can
/// do is name a workflow kind that does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// `boss train <verb>` — the conductor's verbs, allowlisted.
    Train(String),
    /// Open a packet of this workflow kind. Written `open:<kind>`.
    OpenPacket(String),
}

/// Read a registry row's `verb` column into what it will actually do.
///
/// Refuses rather than guesses, and the refusal names both shapes —
/// a row is edited by a person, and "unknown verb" without the
/// alternatives is the kind of message that sends someone to the source.
pub(crate) fn parse_action(verb: &str) -> Result<Action> {
    if let Some(kind) = verb.strip_prefix("open:") {
        let kind = kind.trim();
        if kind.is_empty() {
            bail!(
                "verb \"open:\" names no workflow kind — write open:<kind>, e.g. open:protocol-retro"
            );
        }
        // Kinds are kebab-case by convention everywhere in the
        // registry. Pinning that here keeps the value safe to put in a
        // URL query and a JSON body without escaping games.
        if !kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!(
                "workflow kind {kind:?} is not kebab-case (lowercase, digits, hyphens). \
                 This value goes into a query string and a JSON body; a kind that needs \
                 escaping is a kind that is wrong."
            );
        }
        return Ok(Action::OpenPacket(kind.to_string()));
    }
    if !VERBS.contains(&verb) {
        bail!(
            "unknown verb {verb:?}. A rule fires either a conductor verb ({}) or \
             `open:<kind>` to file a packet.",
            VERBS.join(" | ")
        );
    }
    Ok(Action::Train(verb.to_string()))
}

/// The packet a scheduled rule files.
///
/// Deliberately the SAME SHAPE `infra/boss-maintenance-wrap.sh` has
/// filed since the maintenance family existed — subject `infra/<kind>`,
/// the bootstrap admin as owner, one packet per day. Two mechanisms
/// filing the same kind with different shapes would be a fact living
/// twice (CLAUDE.md §9a), and the wrapper's shape is the one with
/// months of packets behind it.
pub(crate) fn packet_body(kind: &str, rule: &str, now: DateTime<Utc>) -> Value {
    json!({
        "kind": kind,
        "subject": {"subject_kind": "custom", "id": format!("infra/{kind}")},
        "title": format!("{kind} — {}", now.format("%Y-%m-%d")),
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        // trigger_kind/trigger_name is the convention the registry
        // already uses for "who opened this", so a scheduled packet is
        // attributable to its rule rather than to a mystery actor.
        "metadata": {"trigger_kind": "cadence", "trigger_name": rule, "chore": kind},
        "tags": ["cadence"],
    })
}

fn log(msg: impl std::fmt::Display) {
    println!("cadence: {msg}");
}

// ---------------------------------------------------------------------------
// Rules — the registry rows, typed.
// ---------------------------------------------------------------------------

/// One active cadence rule: fire `verb` on `basis`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CadenceRule {
    pub name: String,
    /// A `boss train` verb: preflight | reconcile | board | run.
    pub verb: String,
    pub basis: Basis,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Basis {
    /// Fire once per `every_minutes` bucket, buckets anchored at
    /// midnight UTC of the current boss-clock day.
    Wall { every_minutes: u32 },
    /// Fire once per time-of-day window (UTC), e.g. 06:00 and 18:00.
    Clock { at: Vec<NaiveTime> },
    /// Fire when the dock holds at least `min_depth` parked ready
    /// cars, at most once per `cooldown_minutes`.
    QueueDepth {
        min_depth: u32,
        cooldown_minutes: u32,
    },
    /// Fire on the days a CALENDAR recurrence selects, at `at`.
    ///
    /// The recurrence and its business-day handling are
    /// `boss_core::calendar`'s (design a02b01e0) — this basis owns only
    /// "which day, and at what time", never a second definition of what
    /// "weekly" means. Before it existed a week was inexpressible here:
    /// `Clock` fires every day, and `Wall` re-anchors at midnight so
    /// 10080 minutes floors to zero and fires daily too.
    Calendar {
        cadence: Cadence,
        anchor: NaiveDate,
        at: NaiveTime,
        /// Absent = every day is a business day. Per-schedule and not
        /// global, because deferring maintenance to the next working
        /// day is right and deferring a ten-minute reconcile is not.
        business_calendar: Option<BusinessCalendar>,
    },
}

impl Basis {
    fn as_str(&self) -> &'static str {
        match self {
            Basis::Wall { .. } => "wall",
            Basis::Clock { .. } => "clock",
            Basis::QueueDepth { .. } => "queue-depth",
            Basis::Calendar { .. } => "calendar",
        }
    }
}

// What evaluation compares a candidate window against is
// `boss_jobs::cadence::LastFiring` — the WIRE type, imported rather
// than restated. The loop reads it from the same surface an operator
// does, so a second local definition would be a fact living twice.

// ---------------------------------------------------------------------------
// Evaluation — pure functions of (rule, boss-clock now, last firing,
// dock depth). No I/O; the whole cadence semantic is testable here.
// ---------------------------------------------------------------------------

/// Minute-resolution window stamp — the deterministic half of a
/// firing id. Two evaluations of the same window always agree.
pub(crate) fn window_stamp(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%MZ").to_string()
}

/// `cadence:<rule>:<window-stamp>` — the exactly-once key
/// (protocol-cadence Q3).
pub(crate) fn firing_id(rule: &str, window: DateTime<Utc>) -> String {
    format!("cadence:{rule}:{}", window_stamp(window))
}

/// Evaluate one rule against boss-clock `now`: `Some(window)` means
/// the rule is due for that window and no firing for it is recorded
/// yet. `dock_depth` is the parked-ready-car count when the tick
/// probed it (`None` = not probed or probe failed — queue-depth
/// rules hold rather than fire blind).
pub(crate) fn due_window(
    rule: &CadenceRule,
    now: DateTime<Utc>,
    last: Option<&LastFiring>,
    dock_depth: Option<u32>,
) -> Option<DateTime<Utc>> {
    let window = match &rule.basis {
        Basis::Wall { every_minutes } => {
            // The bucket of `now` on the interval grid anchored at
            // midnight UTC of the boss-clock day. Older elapsed
            // buckets never fire — catch-up is one window at most.
            let every = i64::from(*every_minutes);
            if every == 0 {
                return None;
            }
            let midnight = now.date_naive().and_hms_opt(0, 0, 0)?.and_utc();
            let elapsed_min = (now - midnight).num_minutes();
            midnight + Duration::minutes((elapsed_min / every) * every)
        }
        Basis::Clock { at } => {
            // The most recent elapsed time-of-day window: today's
            // where already reached, else yesterday's. Anything
            // older was missed and stays missed — no backfill.
            let today = now.date_naive();
            let yesterday = today.pred_opt()?;
            at.iter()
                .map(|t| {
                    let w = today.and_time(*t).and_utc();
                    if w <= now {
                        w
                    } else {
                        yesterday.and_time(*t).and_utc()
                    }
                })
                .filter(|w| *w <= now)
                .max()?
        }
        Basis::Calendar {
            cadence,
            anchor,
            at,
            business_calendar,
        } => {
            // The most recent elapsed firing day, at `at`. Same shape as
            // Clock — "today's window if reached, else the previous
            // one" — except which days qualify is the calendar's
            // decision, not every day.
            //
            // BOUNDED LOOK-BACK, not "search until found". A rule whose
            // anchor is in the future, or an annual rule ten months
            // from its day, must not walk the calendar forever on every
            // tick. MAX_LOOKBACK_DAYS covers a month comfortably; past
            // that the honest answer is "no elapsed window", and the
            // rule simply waits. Catch-up is at most one window, which
            // matches every other basis here.
            let today = now.date_naive();
            let mut day = today;
            let mut found = None;
            for _ in 0..=MAX_LOOKBACK_DAYS {
                if fires_on_with_calendar(*cadence, *anchor, business_calendar.as_ref(), day) {
                    let w = day.and_time(*at).and_utc();
                    if w <= now {
                        found = Some(w);
                        break;
                    }
                }
                day = day.pred_opt()?;
            }
            found?
        }
        Basis::QueueDepth {
            min_depth,
            cooldown_minutes,
        } => {
            // Hold below the threshold, and hold when depth is
            // unknown — a failed probe must never board a train.
            if dock_depth? < *min_depth {
                return None;
            }
            // The cooldown is the re-fire guard: a dock that stays
            // deep (cars skipped on conflicts) re-fires at most once
            // per cooldown instead of every tick.
            if let Some(last) = last
                && now - last.fired_at < Duration::minutes(i64::from(*cooldown_minutes))
            {
                return None;
            }
            // The window is the evaluation minute — deterministic,
            // so two instances in the same minute claim one id.
            now.with_second(0)?.with_nanosecond(0)?
        }
    };
    // Fired already? The recorded id for this rule + window says so —
    // across ticks, restarts, and instances alike.
    if last.is_some_and(|l| l.firing_id == firing_id(&rule.name, window)) {
        return None;
    }
    Some(window)
}

/// One run this loop spawned and has not yet reaped: which rule it
/// belongs to, and how long it has been going. Plain data because
/// both readers — the per-rule guard and the heartbeat — are pure
/// functions of it, testable without a child process or a clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunSnapshot {
    pub rule: String,
    pub elapsed: std::time::Duration,
}

/// What a tick decided about one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Not due this tick.
    Hold,
    /// Due for this window: claim it, then spawn the verb.
    Fire(DateTime<Utc>),
    /// Due, but this rule's previous firing is still running. One
    /// in-flight run per rule — skip the window, claim nothing, say
    /// so in the journal. The elapsed time is the run's, not the
    /// window's: it is the number an operator wants.
    StillRunning(std::time::Duration),
}

/// The whole per-tick scheduling decision for one rule, as a pure
/// function of (rule, boss-clock now, last firing, dock depth,
/// what this loop already has in flight).
pub(crate) fn decide(
    rule: &CadenceRule,
    now: DateTime<Utc>,
    last: Option<&LastFiring>,
    dock_depth: Option<u32>,
    running: &[RunSnapshot],
) -> Decision {
    let Some(window) = due_window(rule, now, last, dock_depth) else {
        return Decision::Hold;
    };
    // Per rule, deliberately: a long reconcile must not gag the
    // boarding window or the queue-depth board. Overlap between
    // rules is the conductor's flock to arbitrate, not this loop's.
    match running.iter().find(|r| r.rule == rule.name) {
        Some(run) => Decision::StillRunning(run.elapsed),
        None => Decision::Fire(window),
    }
}

/// Whole elapsed seconds — the one conversion the completion line,
/// the still-running line, the heartbeat, and the firing row's
/// `runtime_secs` all read, so none of them can disagree.
pub(crate) fn runtime_secs(elapsed: std::time::Duration) -> u64 {
    elapsed.as_secs()
}

/// `<rule> verb=<verb> rc=<n> in <t>s` — a firing's obituary,
/// journalled by the spawned task with its true elapsed time.
pub(crate) fn completion_line(rule: &str, verb: &str, rc: i32, runtime_secs: u64) -> String {
    format!("{rule} verb={verb} rc={rc} in {runtime_secs}s")
}

/// The skipped-window line. A window that does not fire must say why;
/// silence here would look exactly like the deafness this fixes.
pub(crate) fn still_running_line(rule: &str, elapsed: std::time::Duration) -> String {
    format!(
        "{rule} still running ({}s) — not re-firing",
        runtime_secs(elapsed)
    )
}

/// `<rule> <n>s, ...` — in-flight work, named and aged.
fn running_list(running: &[RunSnapshot]) -> String {
    running
        .iter()
        .map(|r| format!("{} {}s", r.rule, runtime_secs(r.elapsed)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The heartbeat: one line every N ticks saying the loop is alive,
/// what it is waiting for, and — the point of this fix — what it is
/// currently running. A 30-minute deploy now prints heartbeats
/// throughout instead of 30 minutes of silence.
pub(crate) fn heartbeat_line(
    tick_n: u64,
    rules: usize,
    next_due: Option<DateTime<Utc>>,
    running: &[RunSnapshot],
) -> String {
    let due = next_due.map_or_else(|| "?".to_string(), window_stamp);
    let work = if running.is_empty() {
        String::new()
    } else {
        format!(", running: {}", running_list(running))
    };
    format!("alive (tick {tick_n}, {rules} rules, next due {due}{work})")
}

/// The soonest scheduled window strictly after `now` across the
/// rules — the heartbeat's "next due". Wall rules promise their next
/// grid bucket; clock rules today's next time-of-day or tomorrow's
/// first; queue-depth rules fire on dock state, not on time, and
/// promise nothing. None when no rule carries a schedule.
pub(crate) fn next_due(rules: &[CadenceRule], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    rules
        .iter()
        .filter_map(|rule| match &rule.basis {
            Basis::Wall { every_minutes } => {
                let every = i64::from(*every_minutes);
                if every == 0 {
                    return None;
                }
                let midnight = now.date_naive().and_hms_opt(0, 0, 0)?.and_utc();
                let elapsed_min = (now - midnight).num_minutes();
                Some(midnight + Duration::minutes((elapsed_min / every + 1) * every))
            }
            Basis::Clock { at } => {
                let today = now.date_naive();
                let tomorrow = today.succ_opt()?;
                at.iter()
                    .map(|t| {
                        let w = today.and_time(*t).and_utc();
                        if w > now {
                            w
                        } else {
                            tomorrow.and_time(*t).and_utc()
                        }
                    })
                    .min()
            }
            Basis::Calendar {
                cadence,
                anchor,
                at,
                business_calendar,
            } => {
                // The NEXT firing day at or after today whose window is
                // still ahead. Same bounded walk as `due_window`, in
                // the other direction — the heartbeat's "next due" is a
                // convenience, so a rule with nothing in range reports
                // None rather than guessing.
                let mut day = now.date_naive();
                for _ in 0..=MAX_LOOKBACK_DAYS {
                    if fires_on_with_calendar(*cadence, *anchor, business_calendar.as_ref(), day) {
                        let w = day.and_time(*at).and_utc();
                        if w > now {
                            return Some(w);
                        }
                    }
                    day = day.succ_opt()?;
                }
                None
            }
            // A queue-depth rule has no clock: it fires when the dock
            // fills, which no schedule can predict.
            Basis::QueueDepth { .. } => None,
        })
        .min()
}

/// Parse the registry's `at_times` JSONB (`["06:00","18:00"]`) into
/// times-of-day. Rejects empty lists and non-"HH:MM" entries loudly —
/// a rule that cannot be read must be skipped visibly, not fire never
/// and silently.
pub(crate) fn parse_at_times(v: &Value) -> Result<Vec<NaiveTime>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("at_times must be a JSON array"))?;
    if arr.is_empty() {
        bail!("at_times must name at least one time");
    }
    arr.iter()
        .map(|e| {
            let s = e
                .as_str()
                .ok_or_else(|| anyhow!("at_times entries are \"HH:MM\" strings"))?;
            NaiveTime::parse_from_str(s, "%H:%M")
                .with_context(|| format!("parsing at_times entry {s:?}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Registry + measurement I/O — the four cadence calls, over the jobs
// API's /api/cadence/* door (protocol-cadence.md, sequencing step 3).
//
// The loop used to open its own sqlx pool here, and that pool was a
// recorded defect: BOSS_POSTGRES_URL on the conductor's host is NOT
// the database behind the system of record, so every firing the loop
// recorded was invisible to /api/cadence/rules/{name}/last-firing —
// the surface answered "never fired" for every rule while the loop
// fired on schedule (backlog a516f1f1; 123-cadence-registry-
// reconcile.sql measured the same split for rules: 244 firing rows
// local, 0 on the cluster). One door, one database: what the loop
// obeys is what the operator reads. Timestamps are still bound from
// boss-clock time — the API stores the caller's fired_at, never NOW().
// ---------------------------------------------------------------------------

/// The jobs API base the whole loop talks to — rules, firings and the
/// dock probe alike. **Unset is a refusal, not a default.** A default
/// that is right on one host and silently wrong on another is exactly
/// how the old pool's firings spent weeks invisible to the last-firing
/// surface; a loop that cannot reach the right instance must reach
/// none. The box that really does schedule against its local stack
/// says so explicitly in its unit drop-in.
fn jobs_base() -> Result<String> {
    let raw = std::env::var("BOSS_JOBS_URL").unwrap_or_default();
    let base = raw.trim().trim_end_matches('/');
    if base.is_empty() {
        bail!(
            "BOSS_JOBS_URL is unset, so there is no system of record to schedule against. \
             Refusing rather than defaulting: the loop's rules and firings must live in the \
             database operators read, and a local fallback is how every cadence firing spent \
             weeks answering `null` from /api/cadence/rules/{{name}}/last-firing. Set it in \
             the boss-train unit drop-in (jobs-sor.conf), e.g. \
             BOSS_JOBS_URL=http://10.20.0.34:7900."
        );
    }
    Ok(base.to_string())
}

fn rule_from_row(row: &CadenceRuleRow) -> Result<CadenceRule> {
    // Validate at LOAD, not at fire. A malformed row is skipped loudly
    // every tick (see `load_rules`); discovering it only when the rule
    // is due would hide a typo until the moment it matters.
    parse_action(&row.verb)?;
    let basis = &row.basis;
    let positive = |field: &str, v: Option<i32>| -> Result<u32> {
        v.ok_or_else(|| anyhow!("{field} is required for basis {basis:?}"))?
            .try_into()
            .with_context(|| format!("{field} must be positive"))
    };
    let basis = match row.basis.as_str() {
        "wall" => Basis::Wall {
            every_minutes: positive("every_minutes", row.every_minutes)?,
        },
        "clock" => {
            let at = row
                .at_times
                .as_ref()
                .ok_or_else(|| anyhow!("at_times is required for basis \"clock\""))?;
            Basis::Clock {
                at: parse_at_times(at)?,
            }
        }
        "queue-depth" => Basis::QueueDepth {
            min_depth: positive("min_dock_depth", row.min_dock_depth)?,
            cooldown_minutes: positive("cooldown_minutes", row.cooldown_minutes)?,
        },
        "calendar" => {
            let raw = row
                .cadence
                .as_deref()
                .ok_or_else(|| anyhow!("cadence is required for basis \"calendar\""))?;
            // Parsed by boss_core::calendar, not re-implemented here —
            // the whole point of the move (design a02b01e0) is that
            // "weekly" has one definition in this tree.
            let cadence = Cadence::parse(raw)
                .ok_or_else(|| anyhow!("unknown cadence {raw:?} — see boss_core::calendar"))?;
            let anchor = row
                .anchor_date
                .ok_or_else(|| anyhow!("anchor_date is required for basis \"calendar\""))?;
            let at = row
                .at_times
                .as_ref()
                .ok_or_else(|| anyhow!("at_times is required for basis \"calendar\""))?;
            let times = parse_at_times(at)?;
            // The DB check pins exactly one, but a reader that trusts a
            // constraint it cannot see is how a silent wrong-window bug
            // gets in: a weekly rule with two times is a clock rule
            // that was mislabelled.
            let [at] = times[..] else {
                bail!(
                    "basis \"calendar\" takes exactly one time-of-day, got {} — the cadence \
                     chooses the DAYS and at_times chooses WHEN on them",
                    times.len()
                );
            };
            // BUSINESS CALENDARS ARE NOT RESOLVABLE HERE YET, and this
            // REFUSES rather than pretending.
            //
            // The column exists because design a02b01e0 Q3 decided the
            // calendar is per-schedule, and the firing math already
            // takes an Option<&BusinessCalendar>. What is missing is the
            // resolution from a CODE ("us-banking") to the closed days,
            // which lives in the calendar service — and this loop must
            // not acquire a network dependency to decide whether to
            // fire, or a scheduler stops scheduling when another service
            // is down.
            //
            // Constructing an empty BusinessCalendar from the code would
            // compile and be WORSE than refusing: a rule naming a
            // calendar would fire on every holiday while its row claimed
            // otherwise. A silent half-feature is the failure this same
            // car found in the verb CHECK an hour earlier.
            if let Some(code) = &row.business_calendar {
                bail!(
                    "business_calendar {code:?} cannot be resolved by the cadence loop yet — \
                     the code-to-closed-days lookup lives in the calendar service, and this \
                     loop deliberately holds no dependency on it. Leave the column NULL until \
                     that resolution exists; a rule that names a calendar it cannot read would \
                     fire on holidays while claiming not to."
                );
            }
            Basis::Calendar {
                cadence,
                anchor,
                at,
                business_calendar: None,
            }
        }
        other => bail!("unknown basis {other:?}"),
    };
    Ok(CadenceRule {
        name: row.name.clone(),
        verb: row.verb.clone(),
        basis,
    })
}

async fn load_rules(http: &reqwest::Client, base: &str) -> Result<Vec<CadenceRule>> {
    // EVERY COLUMN rule_from_row READS MUST BE SERVED. The columns
    // live behind the API now (PgCadence::active_rules carries the
    // widening scar: the calendar basis landed without its columns
    // being selected, and the loop skipped protocol-retro-daily on
    // every tick — the rule was in the table the whole time).
    let v = api(http, reqwest::Method::GET, base, "/api/cadence/rules", None)
        .await
        .context("loading cadence rules")?
        .unwrap_or(Value::Null);
    let rows: Vec<CadenceRuleRow> =
        serde_json::from_value(v).context("parsing /api/cadence/rules")?;
    let mut out = Vec::new();
    for row in &rows {
        match rule_from_row(row) {
            Ok(rule) => out.push(rule),
            // A malformed row is skipped LOUDLY every tick, not
            // dropped once at startup: the registry is editable data.
            Err(e) => log(format!("skipping unreadable rule {}: {e:#}", row.name)),
        }
    }
    Ok(out)
}

async fn last_firing(http: &reqwest::Client, base: &str, rule: &str) -> Result<Option<LastFiring>> {
    // The endpoint answers `null` for "never fired" — an ANSWER, not
    // an absence: it means every window is a candidate.
    let v = api(
        http,
        reqwest::Method::GET,
        base,
        &format!("/api/cadence/rules/{rule}/last-firing"),
        None,
    )
    .await
    .context("reading the last cadence firing")?;
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(
            serde_json::from_value(v).context("parsing the last cadence firing")?,
        )),
    }
}

/// Claim a firing id. `false` = the window was already claimed (a
/// concurrent instance, or a re-run after a crash mid-verb) — the
/// caller must not run the verb. Exactly-once still rests on the
/// firing_id primary key; the API reports a losing claim as 200 +
/// `{"claimed": false}` so a race never looks like a failure.
async fn claim_firing(
    http: &reqwest::Client,
    base: &str,
    id: &str,
    rule: &CadenceRule,
    now: DateTime<Utc>,
    dock_depth: Option<u32>,
) -> Result<bool> {
    let detail = match (&rule.basis, dock_depth) {
        (Basis::QueueDepth { .. }, Some(d)) => json!({"dock_depth": d}),
        _ => json!({}),
    };
    let new = NewFiring {
        firing_id: id.to_string(),
        rule_name: rule.name.clone(),
        verb: rule.verb.clone(),
        basis: rule.basis.as_str().to_string(),
        fired_at: now, // boss-clock time, bound — never the DB's wallclock
        detail,
    };
    let v = api(
        http,
        reqwest::Method::POST,
        base,
        "/api/cadence/firings",
        Some(&serde_json::to_value(&new)?),
    )
    .await
    .context("claiming the cadence firing")?
    .ok_or_else(|| anyhow!("POST /api/cadence/firings returned no body"))?;
    let res: ClaimResult = serde_json::from_value(v).context("parsing the claim result")?;
    Ok(res.claimed)
}

/// Merge the verb's outcome into the firing row — the runtime and
/// exit code are what make "what did the cadence cost" a query.
async fn record_outcome(
    http: &reqwest::Client,
    base: &str,
    id: &str,
    rc: i32,
    runtime_secs: u64,
) -> Result<()> {
    api(
        http,
        reqwest::Method::POST,
        base,
        &format!("/api/cadence/firings/{id}/outcome"),
        Some(&json!({"rc": rc, "runtime_secs": runtime_secs})),
    )
    .await
    .context("recording the cadence outcome")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The jobs-API door itself, and the dock probe — parked ready cars,
// counted with the same predicate boarding itself collects by
// (train::parked_ready).
// ---------------------------------------------------------------------------

/// Every call the loop makes reads the same system of record the
/// conductor does, and the same pod roll hits it: on 2026-08-13 a
/// `Connection refused` here held the queue-depth rules for a tick.
/// Same blip guard, same classifier — journalled in this loop's idiom
/// (`cadence: `). A POST re-sends only on a refused connection
/// (nothing was received); an ambiguous claim is settled by the next
/// tick re-evaluating the window, never by re-sending blind.
async fn api(
    http: &reqwest::Client,
    method: reqwest::Method,
    base: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Option<Value>> {
    train::retrying(
        &train::JOBS_API_RETRY,
        &method,
        // The cadence loop is not a train and resolves no delivery
        // policy — it decides only WHEN to spawn a verb. Its journal
        // keeps the compiled cause budget, which is the same number the
        // registry seeds; if the loop ever needs to read policy, this is
        // the line that changes.
        crate::delivery_policy::COMPILED_BLIP_CAUSE_BUDGET,
        &|m| log(m),
        || api_once(http, &method, base, path, body),
    )
    .await
}

async fn api_once(
    http: &reqwest::Client,
    method: &reqwest::Method,
    base: &str,
    path: &str,
    body: Option<&Value>,
) -> std::result::Result<Option<Value>, train::ApiFailure> {
    let mut req = http
        .request(method.clone(), format!("{base}{path}"))
        .header("content-type", "application/json")
        .header("x-boss-user", train::boss_user());
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| train::ApiFailure::transport(e, format!("{method} {path}")))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        train::ApiFailure::transport(e, format!("reading {method} {path} response"))
    })?;
    if !status.is_success() {
        return Err(train::ApiFailure {
            kind: train::Failure::Http(status.as_u16()),
            cause: anyhow!("{method} {path}: HTTP {status}: {}", text.trim()),
        });
    }
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| train::ApiFailure {
            kind: train::Failure::Malformed,
            cause: anyhow::Error::new(e).context(format!("parsing {method} {path} response")),
        })
}

async fn probe_dock_depth(http: &reqwest::Client, base: &str) -> Result<u32> {
    let listed = train::rows(
        api(
            http,
            reqwest::Method::GET,
            base,
            "/api/jobs?kind=ship-a-change&status=open&limit=100",
            None,
        )
        .await?,
    )?;
    let mut depth = 0u32;
    for j in listed {
        let Some(id) = j.get("id").and_then(Value::as_str) else {
            continue;
        };
        let job = api(
            http,
            reqwest::Method::GET,
            base,
            &format!("/api/jobs/{id}"),
            None,
        )
        .await?
        .ok_or_else(|| anyhow!("job {id} came back empty"))?;
        if train::parked_ready(&job) {
            depth += 1;
        }
    }
    Ok(depth)
}

// ---------------------------------------------------------------------------
// The executor — evaluate, claim, run the verb, record what happened.
// ---------------------------------------------------------------------------

/// Run one `boss train <verb>` as a child of this same binary and
/// return its exit code. The conductor's own flock makes an overlap
/// with a manually-started run exit clean, and a preflight exit 3
/// lands here as data instead of killing the loop.
async fn run_verb(verb: &str, rule: &str, now: DateTime<Utc>) -> Result<i32> {
    match parse_action(verb)? {
        Action::Train(v) => {
            let exe = std::env::current_exe().context("resolving the boss binary path")?;
            let status = tokio::process::Command::new(exe)
                .args(["train", &v])
                .status()
                .await
                .with_context(|| format!("spawning boss train {v}"))?;
            Ok(status.code().unwrap_or(-1))
        }
        Action::OpenPacket(kind) => open_packet(&kind, rule, now).await,
    }
}

/// File one packet of `kind`, or reuse the open one.
///
/// SINGLE-OPEN, the same contract `boss-maintenance-wrap.sh` keeps: if
/// an open packet of this kind already exists, today's firing does not
/// add a second. A failed run leaves its packet open on purpose — the
/// timer is the executor, the Job is the visibility — and piling up a
/// packet per firing would turn one unfinished chore into a wall of
/// them.
async fn open_packet(kind: &str, rule: &str, now: DateTime<Utc>) -> Result<i32> {
    // WHERE THE PACKET GOES IS NOT A DEFAULT, IT IS A DECISION.
    //
    // `boss-maintenance-wrap.sh` learned this the expensive way: it
    // read `${BOSS_JOBS_URL:-http://127.0.0.1:7900}`, and on a box
    // whose local instance is not the system of record that fallback
    // is a silent redirect. It ran for weeks — the backup,
    // audit-integrity and ledger-replay timers each left 7 packets on
    // boss-gcp's demo instance and ZERO on the cluster, while firing
    // exactly on schedule and passing every check. So: no fallback
    // here either. An unset variable is a refusal.
    let base = std::env::var("BOSS_JOBS_URL").unwrap_or_default();
    if base.trim().is_empty() {
        bail!(
            "BOSS_JOBS_URL is unset, so there is no deployment to file a {kind} packet with. \
             Refusing rather than defaulting: a default here is a silent redirect to whichever \
             instance happens to be local, and that has already cost weeks of packets landing \
             on the wrong one."
        );
    }

    let http = reqwest::Client::new();
    let open = crate::gate::rows(
        crate::gate::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/jobs?kind={kind}&status=open&limit=2"),
            None,
        )
        .await?,
    );
    if !open.is_empty() {
        log(format!(
            "{rule}: an open {kind} packet exists — leaving it to be completed rather than \
             filing a second"
        ));
        return Ok(0);
    }

    crate::gate::api(
        &http,
        reqwest::Method::POST,
        "/api/jobs",
        Some(packet_body(kind, rule, now)),
    )
    .await?;
    log(format!("{rule}: filed a {kind} packet"));
    Ok(0)
}

/// A spawned verb the loop is still tracking.
struct Run {
    started: Instant,
    handle: JoinHandle<()>,
}

/// The verbs this loop has in flight, at most one per rule. A
/// `BTreeMap` and not a `HashMap` so the heartbeat's "running: ..."
/// reads the same way every time — a journal line that shuffles its
/// own fields is a line operators stop trusting.
#[derive(Default)]
struct Runs {
    inner: BTreeMap<String, Run>,
}

impl Runs {
    /// Adopt a spawned task under `rule`'s guard. `started` comes
    /// from the caller so the heartbeat's elapsed and the firing
    /// row's `runtime_secs` measure from one instant, not two.
    fn track(&mut self, rule: &str, started: Instant, handle: JoinHandle<()>) {
        self.inner.insert(rule.to_string(), Run { started, handle });
    }

    /// Release the guards of runs that have finished. Called at the
    /// top of every tick: a run that ended since the last tick must
    /// not cost its rule an extra window.
    fn reap(&mut self) {
        self.inner.retain(|_, run| !run.handle.is_finished());
    }

    fn snapshot(&self) -> Vec<RunSnapshot> {
        self.inner
            .iter()
            .map(|(rule, run)| RunSnapshot {
                rule: rule.clone(),
                elapsed: run.started.elapsed(),
            })
            .collect()
    }

    /// Wait for the in-flight runs to finish, up to `budget`
    /// (`None` = as long as it takes). Returns whatever is still
    /// running when the budget expires — the loop reports those
    /// rather than pretending they ended.
    async fn drain(&mut self, budget: Option<std::time::Duration>) -> Vec<RunSnapshot> {
        let waiting_since = Instant::now();
        loop {
            self.reap();
            if self.inner.is_empty() {
                return Vec::new();
            }
            if budget.is_some_and(|b| waiting_since.elapsed() >= b) {
                return self.snapshot();
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Spawn a claimed firing's verb beside the loop. The task owns
    /// the whole tail of the firing — waiting on the child, merging
    /// rc + runtime into the claimed row, and journalling the
    /// completion line — so none of it depends on the loop being
    /// free, and the loop is free immediately.
    fn spawn_verb(
        &mut self,
        http: &reqwest::Client,
        base: &str,
        rule: &CadenceRule,
        firing_id: String,
        now: DateTime<Utc>,
    ) {
        let http = http.clone();
        let base = base.to_string();
        let name = rule.name.clone();
        let verb = rule.verb.clone();
        let rule_name = rule.name.clone();
        let started = Instant::now();
        let handle = tokio::spawn(async move {
            let rc = match run_verb(&verb, &rule_name, now).await {
                Ok(rc) => rc,
                Err(e) => {
                    // The verb never started. Say so, then record it
                    // like any other failure: an unfinished claim with
                    // no rc would look like a loop that lost the run.
                    log(format!("{name} verb={verb} could not start: {e:#}"));
                    -1
                }
            };
            let secs = runtime_secs(started.elapsed());
            if let Err(e) = record_outcome(&http, &base, &firing_id, rc, secs).await {
                log(format!(
                    "{name}: recording the firing outcome failed: {e:#}"
                ));
            }
            log(completion_line(&name, &verb, rc, secs));
        });
        self.track(&rule.name, started, handle);
    }
}

/// What a tick saw — fodder for the heartbeat line.
#[derive(Default)]
struct TickSummary {
    rules: usize,
    next_due: Option<DateTime<Utc>>,
}

async fn tick(
    http: &reqwest::Client,
    base: &str,
    clock: &dyn ClockClient,
    dry: bool,
    runs: &mut Runs,
) -> Result<TickSummary> {
    // Boss-clock time is the only "now" in this loop (clock-as-service;
    // the no-wallclock invariant). In the wall-mode production deploy
    // it IS wall time — served by the one authoritative clock.
    let now = clock.now().await.now;
    let rules = load_rules(http, base).await?;
    // Release the guards of runs that finished since the last tick,
    // then read the survivors once — every rule this tick is judged
    // against the same picture of what is in flight.
    runs.reap();
    let running = runs.snapshot();
    // One dock probe per tick, and only when a queue-depth rule is
    // active. A failed probe holds those rules; it never fires blind.
    let mut dock_depth: Option<u32> = None;
    if rules
        .iter()
        .any(|r| matches!(r.basis, Basis::QueueDepth { .. }))
    {
        match probe_dock_depth(http, base).await {
            Ok(d) => dock_depth = Some(d),
            Err(e) => log(format!("dock probe failed — queue-depth rules hold: {e:#}")),
        }
    }
    for rule in &rules {
        let last = last_firing(http, base, &rule.name).await?;
        let window = match decide(rule, now, last.as_ref(), dock_depth, &running) {
            Decision::Hold => continue,
            Decision::StillRunning(elapsed) => {
                log(still_running_line(&rule.name, elapsed));
                continue;
            }
            Decision::Fire(window) => window,
        };
        let id = firing_id(&rule.name, window);
        if dry {
            log(format!(
                "DRY: would fire {} ({id}) verb={}",
                rule.name, rule.verb
            ));
            continue;
        }
        if !claim_firing(http, base, &id, rule, now, dock_depth).await? {
            continue; // someone else holds this window
        }
        let depth_note = match (&rule.basis, dock_depth) {
            (Basis::QueueDepth { .. }, Some(d)) => format!(" dock_depth={d}"),
            _ => String::new(),
        };
        log(format!(
            "fired {} ({id}) verb={} basis={}{depth_note}",
            rule.name,
            rule.verb,
            rule.basis.as_str()
        ));
        // Spawn and move on. The tick that fires a 30-minute deploy
        // ends in milliseconds like any other.
        runs.spawn_verb(http, base, rule, id, now);
    }
    Ok(TickSummary {
        rules: rules.len(),
        next_due: next_due(&rules, now),
    })
}

/// Stop evaluating and settle up with whatever is in flight. A
/// running verb is a real child process doing real work (a merge, a
/// deploy); the loop owes the journal an honest account of it either
/// way. Wait up to `budget` for it to finish — the common case, since
/// systemd's default `KillMode=control-group` SIGTERMs the child too
/// and it exits promptly — and name whatever outlasts the budget.
async fn shut_down(runs: &mut Runs, signal: &str, budget: std::time::Duration) {
    runs.reap();
    let in_flight = runs.snapshot();
    if in_flight.is_empty() {
        log(format!("{signal} — nothing in flight, leaving"));
        return;
    }
    log(format!(
        "{signal} — waiting up to {}s for {}",
        budget.as_secs(),
        running_list(&in_flight)
    ));
    for left in runs.drain(Some(budget)).await {
        // Its claim stands with no rc: the window is not re-fired,
        // and the missing outcome IS the record that the run was cut
        // short rather than completed.
        log(format!(
            "{signal} — {} still running ({}s), leaving it without an outcome",
            left.rule,
            runtime_secs(left.elapsed)
        ));
    }
}

/// The `boss train cadence` entry: the supervised loop
/// (infra/train/boss-train.service) or, with `once`, a single
/// evaluated tick for an operator or a test.
pub async fn run(once: bool, dry: bool) -> Result<()> {
    // One address does the loop's whole job — rules, last-firings,
    // claims, outcomes and the dock probe all ride the jobs API
    // (protocol-cadence.md, sequencing step 3). The private sqlx pool
    // that used to live here is gone WITH the split-brain it carried:
    // it wrote firings to whatever database BOSS_POSTGRES_URL named,
    // which on the conductor's host was not the system of record.
    let base = jobs_base()?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("building the jobs API client")?;
    let clock_url = train::env_or("BOSS_CLOCK_URL", &boss_ports::url("clock"));
    let clock: Arc<dyn ClockClient> = Arc::new(ReqwestClockClient::new(clock_url.clone()));
    let tick_secs: u64 = train::env_or("BOSS_TRAIN_CADENCE_TICK_SECONDS", "60")
        .parse()
        .context("parsing BOSS_TRAIN_CADENCE_TICK_SECONDS")?;
    // The heartbeat cadence: one "alive" line every N ticks (~30 min
    // at the 60s default). Silence must be diagnosable — a hung loop
    // and a quiet loop cannot be allowed to look identical in the
    // journal; this makes "is it hung?" a one-line grep.
    let heartbeat_ticks = train::env_or("BOSS_TRAIN_HEARTBEAT_TICKS", "30")
        .parse::<u64>()
        .ok()
        .filter(|&n| n > 0)
        .unwrap_or(30);
    // How long a SIGTERM waits for an in-flight verb before naming it
    // and leaving. Long enough for a child that got the same SIGTERM
    // to wind down; far short of a full deploy, which no restart
    // should be made to sit through.
    let drain = std::time::Duration::from_secs(
        train::env_or("BOSS_TRAIN_CADENCE_DRAIN_SECONDS", "30")
            .parse::<u64>()
            .unwrap_or(30),
    );
    log(format!(
        "loop starting — rules from {base}/api/cadence/rules, clock at {clock_url}, tick {tick_secs}s{}",
        if dry { ", DRY" } else { "" }
    ));
    let mut runs = Runs::default();
    if once {
        // One evaluated tick — but an operator (or a test) asking for
        // it wants the verb's result, and a detached child would die
        // with this process. Wait it out; only the supervised loop
        // gets to move on while a verb runs.
        let outcome = tick(&http, &base, clock.as_ref(), dry, &mut runs).await;
        runs.drain(None).await;
        return outcome.map(|_| ());
    }
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("installing the SIGTERM handler")?;
    let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .context("installing the SIGINT handler")?;
    let mut tick_n: u64 = 0;
    // The last tick that got as far as reading the registry. A failed
    // tick must not cost the heartbeat its content — "alive" with a
    // stale rule count still beats silence.
    let mut seen = TickSummary::default();
    loop {
        tick_n += 1;
        match tick(&http, &base, clock.as_ref(), dry, &mut runs).await {
            // The loop survives a bad tick — supervision is systemd's
            // job, coordination is this loop's; a transient jobs-api
            // or Postgres outage must not kill the schedule.
            Err(e) => log(format!("tick failed: {e:#}")),
            Ok(summary) => seen = summary,
        }
        if tick_n.is_multiple_of(heartbeat_ticks) {
            // Unconditional: the heartbeat is the aliveness signal,
            // so neither a long verb nor a failing tick may suppress
            // it. That silence is the whole bug being fixed.
            log(heartbeat_line(
                tick_n,
                seen.rules,
                seen.next_due,
                &runs.snapshot(),
            ));
        }
        // The signal handlers are installed once, above, so a signal
        // that arrives mid-tick is still waiting here when the tick
        // ends — it is never dropped for landing at a busy moment.
        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_secs(tick_secs)) => {}
            _ = term.recv() => {
                shut_down(&mut runs, "SIGTERM", drain).await;
                return Ok(());
            }
            _ = int.recv() => {
                shut_down(&mut runs, "SIGINT", drain).await;
                return Ok(());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — the cadence semantics, pinned before the implementation.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    // -- what a rule may fire ------------------------------------------

    #[test]
    fn a_conductor_verb_still_parses() {
        for v in VERBS {
            assert_eq!(parse_action(v).unwrap(), Action::Train((*v).to_string()));
        }
    }

    #[test]
    fn open_names_a_workflow_kind() {
        assert_eq!(
            parse_action("open:protocol-retro").unwrap(),
            Action::OpenPacket("protocol-retro".into())
        );
    }

    /// THE SAFETY PROPERTY THE ALLOWLIST EXISTED FOR: a hand-edited row
    /// must not be able to spawn arbitrary arguments. `OpenPacket`
    /// keeps it by not spawning a process at all, and the kind is
    /// pinned to kebab-case so it is safe in a query string and a JSON
    /// body without escaping.
    #[test]
    fn a_kind_that_would_need_escaping_is_refused() {
        for bad in [
            "open:foo bar",
            "open:foo/../bar",
            "open:foo;rm -rf /",
            "open:Foo",
            "open:foo?status=open",
            "open:foo&x=1",
            "open:",
            "open:   ",
        ] {
            assert!(parse_action(bad).is_err(), "should refuse {bad:?}");
        }
    }

    #[test]
    fn an_unknown_verb_names_both_shapes_in_its_refusal() {
        let e = parse_action("deploy").unwrap_err().to_string();
        assert!(e.contains("unknown verb"), "{e}");
        assert!(
            e.contains("open:<kind>") && e.contains("reconcile"),
            "the refusal must show a person editing a row what IS allowed: {e}"
        );
    }

    /// The packet a scheduled rule files must match what
    /// `boss-maintenance-wrap.sh` has always filed — two mechanisms
    /// filing one kind with different shapes is a fact living twice.
    #[test]
    fn a_scheduled_packet_matches_the_wrappers_shape() {
        let now = utc(2026, 8, 28, 6, 5, 0);
        let b = packet_body("protocol-retro", "retro-weekly", now);
        assert_eq!(b["kind"], "protocol-retro");
        assert_eq!(b["subject"]["subject_kind"], "custom");
        assert_eq!(b["subject"]["id"], "infra/protocol-retro");
        assert_eq!(b["owner_id"], "emp-bootstrap-admin");
        assert_eq!(b["status"], "open");
        assert!(
            b["title"].as_str().unwrap().contains("2026-08-28"),
            "the title carries the firing day so two packets are distinguishable: {}",
            b["title"]
        );
        // Attributable to its rule, not to a mystery actor.
        assert_eq!(b["metadata"]["trigger_kind"], "cadence");
        assert_eq!(b["metadata"]["trigger_name"], "retro-weekly");
    }

    /// The title must come from the clock that was passed in — the
    /// no-wallclock invariant. A sim deploy advancing its clock must
    /// see the sim date here, not the host's.
    #[test]
    fn the_packet_title_uses_the_clock_it_was_given() {
        let a = packet_body("x-kind", "r", utc(2020, 1, 2, 0, 0, 0));
        let b = packet_body("x-kind", "r", utc(2031, 12, 25, 0, 0, 0));
        assert!(a["title"].as_str().unwrap().contains("2020-01-02"));
        assert!(b["title"].as_str().unwrap().contains("2031-12-25"));
    }

    // -- the calendar basis: the reason this whole thread exists -------

    fn cal_rule(c: Cadence, anchor: (i32, u32, u32), at_h: u32, at_m: u32) -> CadenceRule {
        CadenceRule {
            name: "protocol-retro-daily".into(),
            verb: "open:protocol-retro".into(),
            basis: Basis::Calendar {
                cadence: c,
                anchor: NaiveDate::from_ymd_opt(anchor.0, anchor.1, anchor.2).unwrap(),
                at: at(at_h, at_m),
                business_calendar: None,
            },
        }
    }

    /// WEEKLY IS THE CASE THAT WAS INEXPRESSIBLE. `Clock` fires every
    /// day and `Wall` re-anchors at midnight, so a week could not be
    /// written as a row at all.
    #[test]
    fn a_weekly_rule_fires_on_the_anchors_weekday_and_not_the_others() {
        // 2026-08-28 is a Friday.
        let rule = cal_rule(Cadence::Weekly, (2026, 8, 28), 6, 10);
        // The following Friday, after the window: due.
        let friday = utc(2026, 9, 4, 6, 30, 0);
        assert_eq!(
            due_window(&rule, friday, None, None),
            Some(utc(2026, 9, 4, 6, 10, 0)),
            "a weekly rule must fire on its anchor weekday"
        );
        // Thursday: the most recent elapsed window is the PREVIOUS
        // Friday, not today — and if that already fired, nothing is due.
        let thursday = utc(2026, 9, 3, 23, 0, 0);
        assert_eq!(
            due_window(&rule, thursday, None, None),
            Some(utc(2026, 8, 28, 6, 10, 0))
        );
    }

    /// Before the window on a firing day, the due window is the PREVIOUS
    /// occurrence — never today's, which has not happened yet.
    #[test]
    fn a_window_not_yet_reached_today_does_not_count_as_elapsed() {
        let rule = cal_rule(Cadence::Weekly, (2026, 8, 28), 6, 10);
        let early = utc(2026, 9, 4, 5, 59, 0); // Friday, before 06:10
        assert_eq!(
            due_window(&rule, early, None, None),
            Some(utc(2026, 8, 28, 6, 10, 0))
        );
    }

    /// Once a window has fired, it is not due again — the same
    /// exactly-once guard every other basis gets.
    #[test]
    fn a_calendar_window_fires_once() {
        let rule = cal_rule(Cadence::Weekly, (2026, 8, 28), 6, 10);
        let now = utc(2026, 9, 4, 6, 30, 0);
        let w = due_window(&rule, now, None, None).unwrap();
        assert_eq!(due_window(&rule, now, Some(&fired(&rule, w)), None), None);
    }

    /// A rule anchored in the FUTURE has no elapsed window, and the
    /// bounded look-back must return None rather than walking the
    /// calendar forever on every tick.
    #[test]
    fn a_future_anchor_is_not_due_and_terminates() {
        let rule = cal_rule(Cadence::Weekly, (2027, 1, 1), 6, 10);
        assert_eq!(
            due_window(&rule, utc(2026, 9, 4, 12, 0, 0), None, None),
            None
        );
    }

    /// Monthly resolves within the look-back; the anchor day is clamped
    /// into short months by boss_core::calendar, which is why this
    /// basis borrows that math instead of restating it.
    #[test]
    fn a_monthly_rule_resolves_and_clamps_short_months() {
        let rule = cal_rule(Cadence::Monthly, (2026, 1, 31), 6, 10);
        // April has 30 days: the fire lands on the 30th.
        assert_eq!(
            due_window(&rule, utc(2026, 4, 30, 7, 0, 0), None, None),
            Some(utc(2026, 4, 30, 6, 10, 0))
        );
    }

    /// The heartbeat's "next due" must look FORWARD, and a queue-depth
    /// rule still has no predictable next.
    #[test]
    fn next_due_reports_the_coming_calendar_window() {
        let rule = cal_rule(Cadence::Weekly, (2026, 8, 28), 6, 10);
        assert_eq!(
            next_due(&[rule], utc(2026, 9, 4, 6, 30, 0)),
            Some(utc(2026, 9, 11, 6, 10, 0)),
            "after today's window has passed, the next is a week out"
        );
    }

    /// A calendar rule names its basis in the journal like any other.
    #[test]
    fn the_calendar_basis_names_itself() {
        let rule = cal_rule(Cadence::Weekly, (2026, 8, 28), 6, 10);
        assert_eq!(rule.basis.as_str(), "calendar");
    }

    fn wall_rule(every: u32) -> CadenceRule {
        CadenceRule {
            name: "train-reconcile".into(),
            verb: "reconcile".into(),
            basis: Basis::Wall {
                every_minutes: every,
            },
        }
    }

    fn clock_rule() -> CadenceRule {
        CadenceRule {
            name: "train-window".into(),
            verb: "run".into(),
            basis: Basis::Clock {
                at: vec![at(6, 0), at(18, 0)],
            },
        }
    }

    fn depth_rule(min: u32, cooldown: u32) -> CadenceRule {
        CadenceRule {
            name: "train-board-on-dock-depth".into(),
            verb: "board".into(),
            basis: Basis::QueueDepth {
                min_depth: min,
                cooldown_minutes: cooldown,
            },
        }
    }

    fn fired(rule: &CadenceRule, window: DateTime<Utc>) -> LastFiring {
        LastFiring {
            firing_id: firing_id(&rule.name, window),
            fired_at: window,
        }
    }

    // -- wall basis: interval buckets ------------------------------------

    #[test]
    fn wall_first_start_fires_current_bucket() {
        let rule = wall_rule(10);
        // 06:07 sits in the 06:00 bucket of the 10-minute grid.
        let now = utc(2026, 8, 12, 6, 7, 30);
        assert_eq!(
            due_window(&rule, now, None, None),
            Some(utc(2026, 8, 12, 6, 0, 0))
        );
    }

    #[test]
    fn wall_holds_within_a_fired_bucket() {
        let rule = wall_rule(10);
        let window = utc(2026, 8, 12, 6, 0, 0);
        let last = fired(&rule, window);
        // Re-evaluated later in the same bucket: idempotent, no re-fire.
        for min in [0u32, 3, 9] {
            let now = utc(2026, 8, 12, 6, min, 59);
            assert_eq!(due_window(&rule, now, Some(&last), None), None);
        }
    }

    #[test]
    fn wall_fires_the_next_bucket() {
        let rule = wall_rule(10);
        let last = fired(&rule, utc(2026, 8, 12, 6, 0, 0));
        let now = utc(2026, 8, 12, 6, 10, 0);
        assert_eq!(
            due_window(&rule, now, Some(&last), None),
            Some(utc(2026, 8, 12, 6, 10, 0))
        );
    }

    #[test]
    fn wall_downtime_catches_up_one_window_only() {
        let rule = wall_rule(10);
        // Last fired 06:00; the loop was down through 06:10..06:40.
        let last = fired(&rule, utc(2026, 8, 12, 6, 0, 0));
        let now = utc(2026, 8, 12, 6, 47, 12);
        // Only the CURRENT bucket fires — no thundering backfill.
        assert_eq!(
            due_window(&rule, now, Some(&last), None),
            Some(utc(2026, 8, 12, 6, 40, 0))
        );
    }

    // -- clock basis: times-of-day ---------------------------------------

    #[test]
    fn clock_fires_the_most_recent_elapsed_window_only() {
        let rule = clock_rule();
        // 19:00 with yesterday's 18:00 recorded: today's 06:00 was
        // missed too, but only today's 18:00 (the most recent) fires.
        let last = fired(&rule, utc(2026, 8, 11, 18, 0, 0));
        let now = utc(2026, 8, 12, 19, 0, 0);
        assert_eq!(
            due_window(&rule, now, Some(&last), None),
            Some(utc(2026, 8, 12, 18, 0, 0))
        );
    }

    #[test]
    fn clock_holds_between_windows_once_fired() {
        let rule = clock_rule();
        let last = fired(&rule, utc(2026, 8, 12, 6, 0, 0));
        // 17:59 — the most recent elapsed window is still 06:00.
        let now = utc(2026, 8, 12, 17, 59, 0);
        assert_eq!(due_window(&rule, now, Some(&last), None), None);
    }

    #[test]
    fn clock_reaches_back_across_midnight() {
        let rule = clock_rule();
        // 01:00 with nothing recorded: yesterday 18:00 is the most
        // recent elapsed window — fire it (Persistent=true semantics).
        let now = utc(2026, 8, 12, 1, 0, 0);
        assert_eq!(
            due_window(&rule, now, None, None),
            Some(utc(2026, 8, 11, 18, 0, 0))
        );
        // ... and once recorded, 01:00 holds.
        let last = fired(&rule, utc(2026, 8, 11, 18, 0, 0));
        assert_eq!(due_window(&rule, now, Some(&last), None), None);
    }

    #[test]
    fn clock_fires_exactly_at_the_window_instant() {
        let rule = clock_rule();
        let now = utc(2026, 8, 12, 6, 0, 0);
        assert_eq!(due_window(&rule, now, None, None), Some(now));
    }

    #[test]
    fn clock_with_no_times_never_fires() {
        let rule = CadenceRule {
            name: "empty".into(),
            verb: "run".into(),
            basis: Basis::Clock { at: vec![] },
        };
        assert_eq!(
            due_window(&rule, utc(2026, 8, 12, 12, 0, 0), None, None),
            None
        );
    }

    // -- queue-depth basis: dock pressure --------------------------------

    #[test]
    fn queue_depth_threshold_edge() {
        let rule = depth_rule(4, 120);
        let now = utc(2026, 8, 12, 12, 0, 30);
        // Below threshold: hold. At and above: fire (window = the
        // evaluation minute — the id is still deterministic per minute).
        assert_eq!(due_window(&rule, now, None, Some(3)), None);
        assert_eq!(
            due_window(&rule, now, None, Some(4)),
            Some(utc(2026, 8, 12, 12, 0, 0))
        );
        assert_eq!(
            due_window(&rule, now, None, Some(9)),
            Some(utc(2026, 8, 12, 12, 0, 0))
        );
    }

    #[test]
    fn queue_depth_respects_the_cooldown() {
        let rule = depth_rule(4, 120);
        let last = fired(&rule, utc(2026, 8, 12, 11, 0, 0));
        // 30 minutes after a firing, a deep dock still holds...
        assert_eq!(
            due_window(&rule, utc(2026, 8, 12, 11, 30, 0), Some(&last), Some(8)),
            None
        );
        // ... and fires again once the cooldown has fully elapsed.
        assert_eq!(
            due_window(&rule, utc(2026, 8, 12, 13, 0, 0), Some(&last), Some(8)),
            Some(utc(2026, 8, 12, 13, 0, 0))
        );
    }

    #[test]
    fn queue_depth_never_fires_blind() {
        // Depth unknown (probe failed / not probed): hold, never fire.
        let rule = depth_rule(1, 1);
        assert_eq!(
            due_window(&rule, utc(2026, 8, 12, 12, 0, 0), None, None),
            None
        );
    }

    // -- registry parsing --------------------------------------------------

    #[test]
    fn at_times_parse_and_reject() {
        assert_eq!(
            parse_at_times(&serde_json::json!(["06:00", "18:30"])).unwrap(),
            vec![at(6, 0), at(18, 30)]
        );
        for bad in [
            serde_json::json!([]),
            serde_json::json!(["6am"]),
            serde_json::json!([600]),
            serde_json::json!("06:00"),
        ] {
            assert!(parse_at_times(&bad).is_err(), "accepted {bad}");
        }
    }

    // -- the heartbeat's next-due -----------------------------------------
    //
    // One journal line every N ticks says the loop is alive and what
    // it is waiting for — a hung loop and a quiet loop must not look
    // identical. `next_due` is the schedule half of that line.

    #[test]
    fn next_due_is_the_soonest_scheduled_window() {
        let rules = vec![wall_rule(10), clock_rule()];
        // 06:07: the wall grid's next bucket (06:10) beats 18:00.
        assert_eq!(
            next_due(&rules, utc(2026, 8, 12, 6, 7, 30)),
            Some(utc(2026, 8, 12, 6, 10, 0))
        );
    }

    #[test]
    fn next_due_rolls_a_clock_rule_to_tomorrow() {
        // 19:00 — both of today's windows (06:00, 18:00) are behind;
        // the promise is tomorrow 06:00.
        assert_eq!(
            next_due(&[clock_rule()], utc(2026, 8, 12, 19, 0, 0)),
            Some(utc(2026, 8, 13, 6, 0, 0))
        );
    }

    #[test]
    fn queue_depth_rules_promise_no_window() {
        // Depth rules fire on dock state, not on time — a registry of
        // only depth rules gives the heartbeat nothing to promise.
        assert_eq!(
            next_due(&[depth_rule(3, 120)], utc(2026, 8, 12, 6, 0, 0)),
            None
        );
        assert_eq!(next_due(&[], utc(2026, 8, 12, 6, 0, 0)), None);
    }

    // -- firing ids -------------------------------------------------------

    #[test]
    fn firing_id_is_deterministic_per_window() {
        let w = utc(2026, 8, 12, 6, 0, 0);
        assert_eq!(firing_id("train-window", w), firing_id("train-window", w));
        assert_eq!(
            firing_id("train-window", w),
            "cadence:train-window:2026-08-12T06:00Z"
        );
        // Seconds within the minute collapse to one id.
        assert_eq!(
            firing_id("r", utc(2026, 8, 12, 6, 0, 59)),
            firing_id("r", utc(2026, 8, 12, 6, 0, 1))
        );
    }

    // -- the per-rule in-flight guard --------------------------------------
    //
    // Job 9c5871fa, 2026-08-13 10:00–10:20Z: a reconcile that deployed
    // ran 30+ minutes INSIDE the tick, and for that whole window no
    // rule fired and no heartbeat printed — the scheduler went deaf
    // exactly when an operator wanted to know it was alive. Verbs now
    // spawn; these pin what the loop may decide while one is running.

    fn running(rule: &str, secs: u64) -> RunSnapshot {
        RunSnapshot {
            rule: rule.into(),
            elapsed: std::time::Duration::from_secs(secs),
        }
    }

    #[test]
    fn a_due_rule_with_nothing_in_flight_fires() {
        let rule = wall_rule(10);
        let last = fired(&rule, utc(2026, 8, 13, 10, 0, 0));
        assert_eq!(
            decide(&rule, utc(2026, 8, 13, 10, 10, 0), Some(&last), None, &[]),
            Decision::Fire(utc(2026, 8, 13, 10, 10, 0))
        );
    }

    #[test]
    fn the_same_rule_still_running_does_not_re_fire() {
        let rule = wall_rule(10);
        // 10:00 fired and is still deploying at 10:10. The next bucket
        // is due, but this rule already holds a verb — skip the window
        // and claim nothing, so the firing row stays honest.
        let last = fired(&rule, utc(2026, 8, 13, 10, 0, 0));
        assert_eq!(
            decide(
                &rule,
                utc(2026, 8, 13, 10, 10, 0),
                Some(&last),
                None,
                &[running("train-reconcile", 612)],
            ),
            Decision::StillRunning(std::time::Duration::from_secs(612))
        );
    }

    #[test]
    fn a_different_rules_run_does_not_block_this_one() {
        // The guard is per rule, never global: the conductor's flock
        // arbitrates whether two verbs may actually proceed, and its
        // "another conductor run holds the lock — leaving" is the
        // right outcome for the loser. This loop adds no second lock.
        let rule = clock_rule(); // train-window
        let now = utc(2026, 8, 13, 18, 0, 0);
        assert_eq!(
            decide(&rule, now, None, None, &[running("train-reconcile", 1_800)]),
            Decision::Fire(now)
        );
    }

    #[test]
    fn a_rule_that_is_not_due_holds_whatever_else_runs() {
        let rule = wall_rule(10);
        let last = fired(&rule, utc(2026, 8, 13, 10, 0, 0));
        let mid_bucket = utc(2026, 8, 13, 10, 5, 0);
        for r in [
            Vec::new(),
            vec![running("train-reconcile", 300)],
            vec![running("train-window", 300)],
        ] {
            assert_eq!(
                decide(&rule, mid_bucket, Some(&last), None, &r),
                Decision::Hold
            );
        }
    }

    #[test]
    fn the_still_running_line_names_the_rule_and_its_elapsed() {
        assert_eq!(
            still_running_line("train-reconcile", std::time::Duration::from_secs(612)),
            "train-reconcile still running (612s) — not re-firing"
        );
    }

    // -- the heartbeat under load ------------------------------------------

    #[test]
    fn the_heartbeat_reports_in_flight_work() {
        assert_eq!(
            heartbeat_line(
                30,
                3,
                Some(utc(2026, 8, 13, 10, 10, 0)),
                &[running("train-reconcile", 412)],
            ),
            "alive (tick 30, 3 rules, next due 2026-08-13T10:10Z, \
             running: train-reconcile 412s)"
        );
    }

    #[test]
    fn the_heartbeat_with_nothing_running_is_the_line_it_always_was() {
        assert_eq!(
            heartbeat_line(30, 3, Some(utc(2026, 8, 13, 10, 10, 0)), &[]),
            "alive (tick 30, 3 rules, next due 2026-08-13T10:10Z)"
        );
        // No rule carries a schedule (or the last tick failed): the
        // loop still says it is alive, with an honest "?".
        assert_eq!(
            heartbeat_line(60, 1, None, &[]),
            "alive (tick 60, 1 rules, next due ?)"
        );
    }

    #[test]
    fn the_heartbeat_lists_every_in_flight_run_in_a_stable_order() {
        assert_eq!(
            heartbeat_line(
                90,
                3,
                None,
                &[running("train-reconcile", 412), running("train-window", 7)],
            ),
            "alive (tick 90, 3 rules, next due ?, running: train-reconcile 412s, train-window 7s)"
        );
    }

    // -- elapsed-time accounting -------------------------------------------

    #[test]
    fn elapsed_truncates_to_whole_seconds_everywhere() {
        // The journal line and the firing row's runtime_secs are the
        // same number by construction — one conversion, read twice.
        let e = std::time::Duration::from_millis(412_900);
        assert_eq!(runtime_secs(e), 412);
        assert_eq!(
            completion_line("train-reconcile", "reconcile", 0, runtime_secs(e)),
            "train-reconcile verb=reconcile rc=0 in 412s"
        );
        assert_eq!(
            still_running_line("train-reconcile", e),
            "train-reconcile still running (412s) — not re-firing"
        );
    }

    #[test]
    fn a_long_deploy_reports_its_true_elapsed_time() {
        // The 9c5871fa shape: a core-crate change forces a full
        // workspace rebuild. Spawning must not cost the true runtime.
        let e = std::time::Duration::from_secs(31 * 60 + 7);
        assert_eq!(
            completion_line("train-reconcile", "reconcile", 0, runtime_secs(e)),
            "train-reconcile verb=reconcile rc=0 in 1867s"
        );
    }

    // -- the tracked-run registry ------------------------------------------

    #[tokio::test]
    async fn a_finished_run_stops_holding_its_rules_guard() {
        let mut runs = Runs::default();
        let (done, wait) = tokio::sync::oneshot::channel::<()>();
        runs.track(
            "train-reconcile",
            Instant::now(),
            tokio::spawn(async move {
                let _ = wait.await;
            }),
        );
        assert_eq!(runs.snapshot().len(), 1);
        runs.reap();
        assert_eq!(
            runs.snapshot().len(),
            1,
            "an unfinished run keeps its guard"
        );
        done.send(()).unwrap();
        assert!(runs.drain(None).await.is_empty());
        assert!(
            runs.snapshot().is_empty(),
            "a finished run releases its guard"
        );
    }

    #[tokio::test]
    async fn drain_reports_what_is_still_running_when_the_budget_expires() {
        // Shutdown semantics: the loop never claims a verb finished
        // that did not. Budget zero = report immediately, no wait.
        let mut runs = Runs::default();
        let (_hold, wait) = tokio::sync::oneshot::channel::<()>();
        runs.track(
            "train-reconcile",
            Instant::now(),
            tokio::spawn(async move {
                let _ = wait.await;
            }),
        );
        let left = runs.drain(Some(std::time::Duration::ZERO)).await;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].rule, "train-reconcile");
    }
}

// ---------------------------------------------------------------------------
// DB-backed tests — the registry seed and the exactly-once claim,
// pinned against real Postgres (boss_testing::TestDb).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod db_tests {
    use super::*;
    use chrono::TimeZone;

    /// The 114 seed loads through the same reader the loop uses: the
    /// two retired timers as data, plus the queue-depth rule — as
    /// reconciled by 123 to the row the conductor actually evaluates.
    ///
    /// This test is the reason 123 exists, from the other side. The
    /// boarding threshold was raised 4 -> 8 live on the running
    /// instance on 2026-08-13 and never migrated, so the seed said 4
    /// while production ran 8, and reading the system of record gave a
    /// confident wrong answer about why a train had not boarded. The
    /// assertion below is now the same number in both places, and it
    /// fails if they diverge again.
    ///
    /// Loading rides the real `/api/cadence/*` router here, so this
    /// test is also the cross-crate pin that the API serves every
    /// column the parser reads: an unserved calendar column makes
    /// `by_name("protocol-retro-daily")` panic "seed rule missing",
    /// which is the skipping-unreadable-rule scar made loud in CI.
    #[tokio::test(flavor = "multi_thread")]
    async fn seeded_rules_load_and_parse() {
        let db = boss_testing::TestDb::new().await;
        let base = serve_cadence_api(db.pool.clone()).await;
        let http = reqwest::Client::new();
        let rules = load_rules(&http, &base).await.unwrap();
        let by_name = |n: &str| {
            rules
                .iter()
                .find(|r| r.name == n)
                .unwrap_or_else(|| panic!("seed rule {n} missing"))
        };
        let reconcile = by_name("train-reconcile");
        assert_eq!(reconcile.verb, "reconcile");
        assert_eq!(reconcile.basis, Basis::Wall { every_minutes: 10 });
        let window = by_name("train-window");
        assert_eq!(window.verb, "run");
        // :05, not :00, since 134-cadence-window-off-grid.sql. The
        // reconcile rule fires every ten minutes on a grid anchored at
        // midnight, so a window at :00 lands in the same tick and loses
        // the conductor's flock — which is why this rule had never once
        // boarded a train. Five past is off that grid.
        assert_eq!(
            window.basis,
            Basis::Clock {
                at: vec![
                    NaiveTime::from_hms_opt(6, 5, 0).unwrap(),
                    NaiveTime::from_hms_opt(18, 5, 0).unwrap(),
                ],
            }
        );
        // THE CALENDAR RULE, AND THE REASON THIS ASSERTION EXISTS.
        //
        // This test passed while the loop could not read a calendar
        // rule at all. Every seeded rule was wall / clock / queue-depth,
        // so `rule_from_row`'s new branch was never exercised against a
        // real row — and `load_rules`' SELECT had not been widened to
        // fetch `cadence`, `anchor_date` or `business_calendar`. In
        // production the loader logged, every tick:
        //
        //     skipping unreadable rule protocol-retro-daily:
        //     no column found for name: cadence
        //
        // The rule was in the table and served over the API the whole
        // time; only the loop could not see it. A DB-backed test that
        // covers three of four bases is a test that covers the three
        // that already worked.
        let retro = by_name("protocol-retro-daily");
        assert_eq!(retro.verb, "open:protocol-retro");
        assert_eq!(
            retro.basis,
            Basis::Calendar {
                cadence: boss_core::calendar::Cadence::Daily,
                anchor: chrono::NaiveDate::from_ymd_opt(2026, 8, 28).unwrap(),
                at: NaiveTime::from_hms_opt(6, 10, 0).unwrap(),
                business_calendar: None,
            }
        );

        let depth = by_name("train-board-on-dock-depth");
        assert_eq!(depth.verb, "board");
        // 3 since 202609032030-cadence-supersede-by-name.sql. The
        // history: 114 started at 4, raised to 8 then 12 when a train
        // was expensive, dropped back to 4 (147) when a dock that never
        // exceeds 3-5 made 12 unreachable, and now 3 — finished work
        // should not wait for eleven friends (David 2026-09-03).
        //
        // WHY THIS ASSERTION MOVED TWICE-REMOVED. board-on-three
        // (202609031515) tried to set 3 and SILENTLY NO-OP'd: its
        // version-keyed retire missed the real active row against a
        // diverged version history (123), so the live value stayed 4
        // and this test kept asserting 4 — documenting the breakage
        // rather than catching it. The supersede-by-name migration
        // retires the active row BY NAME and this pin now asserts the
        // value that actually took: 3.
        //
        // This is why the number lives in exactly two places that must
        // move together — the migration seeds the row, this test pins
        // it (§9a). Note `--auto` gates a schema-only change with
        // "fixture + lints only" and SKIPS the tests, so editing the
        // migration alone leaves this red until a crate change drags
        // boss-cli back into scope. CAVEAT still live: this is the
        // CLUSTER SoR value; whether the boarding loop reads it depends
        // on resolving the conductor's cadence source (the-cluster-is-
        // the-system Q2) — the conductor move makes it moot.
        assert_eq!(
            depth.basis,
            Basis::QueueDepth {
                min_depth: 3,
                cooldown_minutes: 120,
            }
        );
    }

    /// One window, one firing: the second claim of the same id loses,
    /// and the recorded firing holds the window on re-evaluation —
    /// the restart / second-instance idempotence contract end to end,
    /// through the same door the deployed loop uses.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_window_claims_exactly_once() {
        let db = boss_testing::TestDb::new().await;
        let base = serve_cadence_api(db.pool.clone()).await;
        let http = reqwest::Client::new();
        let rule = CadenceRule {
            name: "train-window".into(),
            verb: "run".into(),
            basis: Basis::Clock {
                at: vec![NaiveTime::from_hms_opt(6, 0, 0).unwrap()],
            },
        };
        let now = Utc.with_ymd_and_hms(2026, 8, 12, 6, 0, 30).unwrap();
        let window = due_window(&rule, now, None, None).expect("window due");
        let id = firing_id(&rule.name, window);

        assert!(
            claim_firing(&http, &base, &id, &rule, now, None)
                .await
                .unwrap()
        );
        // A concurrent instance (or a restart mid-verb) computes the
        // same id and must lose the claim.
        assert!(
            !claim_firing(&http, &base, &id, &rule, now, None)
                .await
                .unwrap()
        );

        // The recorded firing is what evaluation sees next tick.
        let last = last_firing(&http, &base, &rule.name)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(last.firing_id, id);
        assert_eq!(last.fired_at, now);
        assert_eq!(due_window(&rule, now, Some(&last), None), None);

        // The outcome merges into the claim's detail row.
        record_outcome(&http, &base, &id, 0, 42).await.unwrap();
        let detail: Value =
            sqlx::query_scalar("SELECT detail FROM cadence_firings WHERE firing_id = $1")
                .bind(&id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(detail.get("rc"), Some(&json!(0)));
        assert_eq!(detail.get("runtime_secs"), Some(&json!(42)));
    }

    /// The registry is append-only with one live row per name: a
    /// second 'active' version of a seeded rule must be refused by
    /// the partial unique index (supersede = retire + insert).
    #[tokio::test(flavor = "multi_thread")]
    async fn one_active_row_per_rule_name() {
        let db = boss_testing::TestDb::new().await;
        let dup = sqlx::query(
            "INSERT INTO cadence_rules (name, version, status, verb, basis, every_minutes) \
             VALUES ('train-reconcile', 2, 'active', 'reconcile', 'wall', 5)",
        )
        .execute(&db.pool)
        .await;
        assert!(dup.is_err(), "second active train-reconcile row accepted");
    }

    /// The SAFE supersede idiom (202609032030): retire the active row
    /// BY NAME, insert the next version as MAX(version)+1. This must
    /// land the new depth from a DIVERGENT state — an active row whose
    /// version is NOT the one a version-keyed migration would name.
    /// That divergence (measured in 123) is exactly why board-on-three
    /// silently no-op'd: its `retire WHERE version = 3` missed the real
    /// active row, so depth 3 never took and the API kept serving 4.
    #[tokio::test(flavor = "multi_thread")]
    async fn supersede_by_name_lands_the_new_depth_from_a_divergent_state() {
        let db = boss_testing::TestDb::new().await;
        let name = "test-supersede-divergent";
        // The divergent state: the active row is version 7 — a version
        // NOT the one a version-keyed migration would try to retire.
        // This is the shape that silently no-op'd board-on-three: a
        // `retire WHERE version = 3` would miss version 7, leave it
        // active, and the new insert would be refused or skipped.
        sqlx::query(
            "INSERT INTO cadence_rules \
             (name, version, status, verb, basis, min_dock_depth, cooldown_minutes) \
             VALUES ($1, 7, 'active', 'board', 'queue-depth', 4, 120)",
        )
        .bind(name)
        .execute(&db.pool)
        .await
        .unwrap();

        // The SAFE idiom — retire by name, insert at MAX+1 — the exact
        // shape of migration 202609032030.
        sqlx::query(
            "UPDATE cadence_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
        )
        .bind(name)
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO cadence_rules \
             (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes) \
             SELECT $1, COALESCE(MAX(version),0)+1, 'active', 'board', 'queue-depth', NULL, NULL, 3, 120 \
             FROM cadence_rules WHERE name = $1",
        )
        .bind(name)
        .execute(&db.pool)
        .await
        .expect("safe supersede must not be refused by the partial index");

        // Exactly one active row, version 8, depth 3 — the change took
        // from a state a version-keyed retire would have missed.
        let rows: Vec<(i32, i32)> = sqlx::query_as(
            "SELECT version, min_dock_depth FROM cadence_rules WHERE name = $1 AND status = 'active'",
        )
        .bind(name)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "exactly one active row after supersede");
        assert_eq!(rows[0].0, 8, "new version is MAX(7)+1");
        assert_eq!(rows[0].1, 3, "the live depth is the new 3, not the stale 4");
    }

    /// Serve the REAL `/api/cadence/*` router over a TestDb — the same
    /// wire, handlers and Pg adapter production mounts, on an
    /// ephemeral port. What these tests call "the surface" is not a
    /// lookalike.
    async fn serve_cadence_api(pool: sqlx::PgPool) -> String {
        let repo: std::sync::Arc<dyn boss_jobs::cadence::CadenceRepository> =
            std::sync::Arc::new(boss_jobs::cadence::PgCadence::new(pool));
        let app =
            boss_jobs::cadence::http::router(boss_jobs::cadence::http::CadenceApiState { repo });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// THE PACKET'S CLAIM, AS A TEST (backlog a516f1f1): the loop
    /// fires on schedule, and `/api/cadence/rules/{name}/last-firing`
    /// on the system of record must report that firing — not `null`.
    ///
    /// `null` from that surface means "never fired", and the conductor,
    /// an operator, and every "why has the train not boarded" question
    /// read it exactly that way. A firing the surface cannot see is a
    /// firing recorded somewhere the system of record is not.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_observability_surface_reports_what_the_loop_records() {
        // The system of record: the database behind /api/cadence/*.
        let sor = boss_testing::TestDb::new().await;
        let base = serve_cadence_api(sor.pool.clone()).await;
        let http = reqwest::Client::new();

        // The loop fires a wall rule on schedule and records the
        // firing THE WAY THE LOOP RECORDS IT.
        let rule = CadenceRule {
            name: "train-reconcile".into(),
            verb: "reconcile".into(),
            basis: Basis::Wall { every_minutes: 10 },
        };
        let now = Utc.with_ymd_and_hms(2026, 8, 31, 6, 7, 0).unwrap();
        let window = due_window(&rule, now, None, None).expect("window due");
        let id = firing_id(&rule.name, window);
        // The RED run of this test recorded the firing the way the
        // loop did at origin/main — through its own BOSS_POSTGRES_URL
        // pool, a DIFFERENT database than the one behind the surface
        // (123-cadence-registry-reconcile.sql measured it: 244 firing
        // rows local, 0 on the system of record) — and the surface
        // answered null. The loop now has exactly one way to record a
        // firing: the same door the surface serves.
        assert!(
            claim_firing(&http, &base, &id, &rule, now, None)
                .await
                .unwrap()
        );

        // The operator's read: the public observability surface.
        let body: Value = http
            .get(format!(
                "{base}/api/cadence/rules/{}/last-firing",
                rule.name
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            body.get("firing_id").and_then(Value::as_str),
            Some(id.as_str()),
            "the surface answered {body} for a rule that just fired — null here \
             reads as 'never fired' while the loop fires on schedule"
        );
    }

    /// The other half of honesty: a rule that truly never fired stays
    /// `null`. The fix must make the surface see real firings, never
    /// invent one.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rule_that_never_fired_stays_null() {
        let sor = boss_testing::TestDb::new().await;
        let base = serve_cadence_api(sor.pool.clone()).await;
        let body = reqwest::Client::new()
            .get(format!("{base}/api/cadence/rules/train-window/last-firing"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "null", "never-fired must stay null");
    }
}
