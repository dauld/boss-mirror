//! Clock-driven schedule runner — the timing-trigger counterpart of the
//! event-driven [`RulesRunner`](super::runner::RulesRunner).
//!
//! The event runner fires a rule's `do_steps` when a NATS event matches
//! its `on_event` topic. This runner fires a rule's `do_steps` when the
//! streaming clock crosses a sim-DAY boundary the rule's `schedule`
//! selects. Same handler-dispatch machinery (`handler::dispatch`); the
//! only difference is what decides "fire now" — a calendar cadence
//! instead of an inbound event.
//!
//! The design splits into three pure decisions (heavily unit-tested) and
//! one thin async shell:
//!
//! 1. [`super::registry::schedule_fires_on`] — does ONE schedule fire on
//!    ONE day? (calendar postponement; lives in registry.rs next to the
//!    cadence math.)
//! 2. [`advance_cursor`] — given the last-fired sim-day, an incoming
//!    `ClockNow`, and a catch-up cap, which days must fire now and what
//!    is the new cursor? (pause / restart / backward-jump / first-obs /
//!    cap edge cases live here.)
//! 3. [`run`] — the shell: fetch + cache calendars, load + persist the
//!    cursor, consume the clock stream, and for each day-to-fire dispatch
//!    every schedule rule's `do_steps`.
//!
//! SUB-DAY CADENCES RIDE THE TICK, NOT THE DAY (backlog 2d33e111,
//! 2026-09-17). `Cadence::Hourly` / `EveryNMinutes` existed in core with
//! the note "sub-day resolution belongs to the caller that has a tick",
//! and this runner has one — the clock stream ticks every second — but
//! until the first sensor poll (`sensors-poll`, every five minutes)
//! nothing in the dispatcher was allowed to run more often than daily.
//! A sub-day rule is excluded from the midnight path and fires from
//! [`tick_bucket`] / [`advance_bucket`] instead: the tick's instant is
//! bucketed by the period, and the rule fires once when its bucket
//! changes. The bucket cursor is in-memory per rule — a restart
//! baselines and fires on the next boundary, which for a poll is the
//! right posture (there is nothing to catch up: it reads the world as
//! it is now). The anchor date and business calendar still apply
//! through `Schedule::fires_on` for the tick's day.
//!
//! EVERY FIRING IS RECORDED (backlog 4b175523) in `dispatcher_firings`,
//! through the event runner's own sink and its reading of "fired" (every
//! handler succeeded): `clock.day` rows keyed by the sim-day, `clock.tick`
//! rows keyed by the tick. See [`ScheduleRunner::record_firings`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_calendar_client::CalendarClient;
use boss_clock_client::ClockNow;
use boss_core::calendar::BusinessCalendar;
use chrono::{DateTime, Days, NaiveDate, Utc};
use futures::StreamExt;
use serde_json::json;
use tracing::{debug, info, warn};

use super::firings::{Firing, FiringSink, Outcome, fired_rules, firing_id};
use super::handler::{self, HandlerRegistry};
use super::registry::{MatchedInvocation, MatchedRule, Registry};

/// Catch-up cap: the maximum number of past sim-days the runner will fire
/// in a single clock observation. A cold start or a huge warp gap can
/// surface a multi-day (or multi-year) jump; without a cap that would
/// fire thousands of daily rules at once. We fire only the most recent
/// `N` days and report the rest as `skipped` (logged). 90 days covers a
/// quarter of catch-up — enough that a brief dispatcher outage during a
/// warp run still fires every monthly/quarterly schedule, while bounding
/// a pathological gap.
pub const DEFAULT_CATCHUP_CAP: u64 = 90;

/// The outcome of advancing the sim-day cursor against one `ClockNow`.
/// Pure data; [`advance_cursor`] computes it and the caller acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAdvance {
    /// Sim-days that must fire now, oldest first. Empty when nothing
    /// advanced (paused / restart / first-observation / backward-jump /
    /// same-day).
    pub days_to_fire: Vec<NaiveDate>,
    /// The cursor to persist after handling this observation. `None`
    /// only in the degenerate case where there was no prior cursor AND
    /// the clock gave nothing usable (never happens for a normal
    /// `ClockNow`, which always carries a date).
    pub new_cursor: Option<NaiveDate>,
    /// Days dropped by the catch-up cap (older than the most recent
    /// `N`). Reported so the caller can log "skipped M days".
    pub skipped: u64,
}

/// THE sim-day cursor decision — pure, total, deterministic.
///
/// Given the last-processed sim-day `cursor`, an incoming `now`, and a
/// catch-up cap `cap`, decide which days to fire and the new cursor.
/// Rules (in order):
///
/// - **Paused / restart-in-progress** → fire nothing, cursor unchanged.
///   (A paused sim isn't advancing time; a mid-flight epoch restart is
///   trimming the log + rebuilding — firing into either is wrong.)
/// - **First observation** (`cursor == None`) → adopt today as the
///   baseline, fire nothing. We do NOT backfill history: the runner only
///   fires days it actually observes the clock cross, so a fresh deploy
///   doesn't dump a year of past schedules.
/// - **Backward jump** (`d < cursor`, e.g. an epoch restart reset the
///   clock to a past date) → reset the cursor to `d`, fire nothing. The
///   new epoch re-establishes its own baseline.
/// - **Same day** (`d == cursor`) → nothing.
/// - **Forward** (`d > cursor`) → fire the half-open range `(cursor, d]`.
///   If that exceeds `cap`, fire only the most recent `cap` days and
///   report the remainder as `skipped`. New cursor is `d`.
pub fn advance_cursor(cursor: Option<NaiveDate>, now: &ClockNow, cap: u64) -> CursorAdvance {
    // Frozen states: don't move the cursor, don't fire.
    if now.paused || now.restart_in_progress {
        return CursorAdvance {
            days_to_fire: vec![],
            new_cursor: cursor,
            skipped: 0,
        };
    }
    let d = now.now.date_naive();
    match cursor {
        // First observation: establish the baseline, fire nothing.
        None => CursorAdvance {
            days_to_fire: vec![],
            new_cursor: Some(d),
            skipped: 0,
        },
        Some(c) if d <= c => {
            // Same day OR a backward jump. Same-day is a no-op (cursor
            // unchanged); a backward jump resets the cursor to the new
            // (earlier) day so the next forward tick fires from there.
            CursorAdvance {
                days_to_fire: vec![],
                new_cursor: Some(d.min(c)),
                skipped: 0,
            }
        }
        Some(c) => {
            // Forward: fire (c, d]. Count = number of days strictly after
            // c through d inclusive.
            let total = (d - c).num_days().max(0) as u64;
            let (start_after, skipped) = if total > cap {
                // Fire only the most recent `cap` days: (d - cap, d].
                let first_fired = d - chrono::Duration::days(cap as i64);
                (first_fired, total - cap)
            } else {
                (c, 0)
            };
            // Walk forward from the day AFTER `start_after` through `d`.
            // The count is known: `d - start_after` days.
            let n_fire = (d - start_after).num_days().max(0);
            let mut days = Vec::with_capacity(n_fire as usize);
            let mut day = start_after;
            while let Some(next) = day.checked_add_days(Days::new(1)) {
                if next > d {
                    break;
                }
                day = next;
                days.push(day);
            }
            CursorAdvance {
                days_to_fire: days,
                new_cursor: Some(d),
                skipped,
            }
        }
    }
}

/// The sub-day bucket a tick falls in: the instant's unix seconds
/// divided by the period. Buckets are anchored at the epoch, so two
/// dispatcher instances (or a restarted one) compute the same bucket
/// for the same tick.
pub fn tick_bucket(now: DateTime<Utc>, every_minutes: u32) -> i64 {
    now.timestamp().div_euclid(i64::from(every_minutes) * 60)
}

/// THE sub-day cursor decision — pure. `(fire, new_cursor)`: a first
/// observation adopts the bucket and fires nothing (the day path's
/// posture); the same bucket is silent; a LATER bucket fires once and
/// becomes the cursor however many buckets were skipped — a poll never
/// backfills; a backward jump re-baselines without firing.
pub fn advance_bucket(cursor: Option<i64>, bucket: i64) -> (bool, i64) {
    match cursor {
        Some(c) if bucket > c => (true, bucket),
        Some(c) if bucket == c => (false, c),
        _ => (false, bucket),
    }
}

/// What the schedule runner needs to operate.
pub struct ScheduleRunner {
    pub registry: Registry,
    pub handlers: HandlerRegistry,
    /// Helper resolver for `when` guards and args — the SAME resolver
    /// the event runner uses, so a predicate means one thing regardless
    /// of what triggered its rule.
    pub helpers: Arc<dyn super::expr::HelperResolver + Send + Sync>,
    /// Clock SSE base URL (e.g. `http://127.0.0.1:7060`).
    pub clock_url: String,
    /// Postgres pool — the single-row `dispatcher_clock_cursor` lives here.
    pub pool: sqlx::PgPool,
    /// Calendar client used at startup to fetch the business calendars
    /// the schedule rules reference.
    pub calendar: Arc<dyn CalendarClient>,
    /// Catch-up cap (days). [`DEFAULT_CATCHUP_CAP`] in production.
    pub catchup_cap: u64,
    /// Where a scheduled rule's firing is recorded — the same sink the
    /// event runner writes (`super::firings`, backlog 4b175523). `None`
    /// leaves the log as the only trace, which is what every scheduled
    /// rule had until the rules list could only say "not recorded".
    pub firings: Option<Arc<dyn FiringSink>>,
}

/// The distinct business-calendar codes referenced by the schedule rules
/// in `registry`. Drives the startup calendar fetch. Pure — no pool, no
/// I/O — so it's unit-testable without a runtime.
pub fn referenced_calendar_codes(registry: &Registry) -> HashSet<String> {
    registry
        .rules()
        .iter()
        .filter_map(|r| r.schedule())
        .filter_map(|s| s.business_calendar.clone())
        .collect()
}

/// True iff `registry` has any schedule-triggered rule. When false the
/// schedule runner has nothing to do and `run` returns immediately. Pure.
pub fn has_schedule_rules(registry: &Registry) -> bool {
    registry.rules().iter().any(|r| r.schedule().is_some())
}

impl ScheduleRunner {
    /// The distinct business-calendar codes referenced by this runner's
    /// schedule rules. (Thin wrapper over [`referenced_calendar_codes`].)
    pub fn calendar_codes(&self) -> HashSet<String> {
        referenced_calendar_codes(&self.registry)
    }

    /// True iff this runner has any schedule-triggered rule.
    pub fn has_schedule_rules(&self) -> bool {
        has_schedule_rules(&self.registry)
    }

    /// Fetch every referenced calendar into a code→calendar map. A
    /// calendar that is absent (404) or errors is logged and SKIPPED —
    /// the day-firing logic treats a missing calendar as "all days are
    /// business days" (the `cal: None` branch of `schedule_fires_on`),
    /// which degrades to firing on the nominal cadence day without
    /// weekend/holiday postponement. That is a deliberate
    /// fail-OPEN: a calendar outage must not silently stop scheduled
    /// work (payroll, tax, sweeps) from firing.
    pub async fn load_calendars(&self) -> HashMap<String, BusinessCalendar> {
        let mut out = HashMap::new();
        for code in self.calendar_codes() {
            match self.calendar.get_business_calendar(&code).await {
                Ok(Some(cal)) => {
                    debug!(calendar = %code, "schedule runner: loaded business calendar");
                    out.insert(code, cal);
                }
                Ok(None) => {
                    warn!(
                        calendar = %code,
                        "schedule runner: business calendar not found; \
                         scheduled rules using it will fire on the nominal cadence day \
                         (no business-day postponement)"
                    );
                }
                Err(e) => {
                    warn!(
                        calendar = %code, error = %e,
                        "schedule runner: failed to fetch business calendar; \
                         scheduled rules using it will fire on the nominal cadence day"
                    );
                }
            }
        }
        out
    }

    /// Load the persisted cursor (the last sim-day already fired), or
    /// `None` if the runner has never run / the row is absent.
    pub async fn load_cursor(&self) -> Result<Option<NaiveDate>> {
        let row: Option<(Option<NaiveDate>,)> =
            sqlx::query_as("SELECT last_sim_day FROM dispatcher_clock_cursor WHERE id = 1")
                .fetch_optional(&self.pool)
                .await
                .context("loading dispatcher_clock_cursor")?;
        Ok(row.and_then(|(d,)| d))
    }

    /// Persist the cursor (upsert the single row id=1).
    pub async fn save_cursor(&self, day: NaiveDate) -> Result<()> {
        sqlx::query(
            "INSERT INTO dispatcher_clock_cursor (id, last_sim_day) VALUES (1, $1) \
             ON CONFLICT (id) DO UPDATE SET last_sim_day = EXCLUDED.last_sim_day",
        )
        .bind(day)
        .execute(&self.pool)
        .await
        .context("persisting dispatcher_clock_cursor")?;
        Ok(())
    }

    /// Build the [`MatchedRule`] set for one sim-day: every schedule rule
    /// that fires on `day` (using the cached calendar), with its `when`
    /// guard evaluated and its `do_steps` args evaluated against the
    /// synthetic clock-day payload.
    ///
    /// Pure given `(registry, calendars, day, helpers)` — no I/O of its
    /// own, though a helper may do some. The synthetic payload is
    /// `{"_day": "YYYY-MM-DD"}` so a `jobs.spawn` arg like
    /// `subject = "_day"` (or a `metadata.x` field) can reference the
    /// firing day. (Mirrors the sim PeriodicEngine's `inject_day`.)
    ///
    /// `when` on a schedule rule is the same predicate language the
    /// event runner evaluates, helpers included — this is what lets a
    /// daily spawner carry a dedup guard like
    /// `NOT open_publish_exists("github-mirror")` (defect 9f0c566a: the
    /// daily publish packet was minted every day its predecessor sat at
    /// an approval, because scheduled rules had no way to ask). Until
    /// this landed, `when` on a schedule rule was silently IGNORED —
    /// parsed, stored, never consulted — which is worse than either
    /// honoring or rejecting it.
    ///
    /// Error posture: a guard that fails to evaluate SKIPS the rule for
    /// the day, with a warning — the same posture as the arg-eval
    /// failure directly below, one rule per function. The event runner
    /// instead NAKs toward a dead-letter, but it has redelivery to lean
    /// on; a clock day fires once. The cost is a quiet day when the
    /// helper's backing API is down, which the warn line makes findable.
    pub fn matched_for_day(
        registry: &Registry,
        calendars: &HashMap<String, BusinessCalendar>,
        day: NaiveDate,
        helpers: &dyn super::expr::HelperResolver,
    ) -> (Vec<MatchedRule>, serde_json::Value) {
        let payload = json!({ "_day": day.format("%Y-%m-%d").to_string() });
        // Day rules only: a sub-day rule fires from the tick path.
        let matched = Self::matched(registry, calendars, day, &payload, helpers, &|s| {
            s.cadence.sub_day_minutes().is_none()
        });
        (matched, payload)
    }

    /// The tick-path twin of [`Self::matched_for_day`]: every SUB-DAY
    /// schedule rule that fires on the tick's day (anchor + calendar,
    /// the same decision) and that `due` says has crossed a bucket
    /// boundary — the runner's in-memory cursor, asked by rule name.
    /// The payload carries `_day` and the tick's instant as `_at`.
    pub fn matched_for_tick(
        registry: &Registry,
        calendars: &HashMap<String, BusinessCalendar>,
        now: DateTime<Utc>,
        due: &dyn Fn(&str) -> bool,
        helpers: &dyn super::expr::HelperResolver,
    ) -> (Vec<MatchedRule>, serde_json::Value) {
        let day = now.date_naive();
        let payload = json!({
            "_day": day.format("%Y-%m-%d").to_string(),
            "_at": now.to_rfc3339(),
        });
        let matched = Self::matched(registry, calendars, day, &payload, helpers, &|s| {
            s.cadence.sub_day_minutes().is_some()
        });
        (
            matched.into_iter().filter(|m| due(&m.rule_name)).collect(),
            payload,
        )
    }

    /// The shared evaluation: every schedule rule `selects` admits that
    /// fires on `day`, with its `when` guard and args evaluated against
    /// `payload`.
    fn matched(
        registry: &Registry,
        calendars: &HashMap<String, BusinessCalendar>,
        day: NaiveDate,
        payload: &serde_json::Value,
        helpers: &dyn super::expr::HelperResolver,
        selects: &dyn Fn(&super::registry::Schedule) -> bool,
    ) -> Vec<MatchedRule> {
        let mut matched = Vec::new();
        for rule in registry.rules() {
            let Some(sched) = rule.schedule() else {
                continue;
            };
            if !selects(sched) {
                continue;
            }
            let cal = sched
                .business_calendar
                .as_deref()
                .and_then(|c| calendars.get(c));
            if !sched.fires_on(cal, day) {
                continue;
            }
            let ctx = super::expr::Context { payload, helpers };
            if let Some(when) = &rule.when {
                match super::expr::eval(when, &ctx).map(|v| v.as_bool()) {
                    Ok(Some(true)) => {}
                    Ok(Some(false)) => continue,
                    Ok(None) => {
                        warn!(
                            rule = %rule.name, day = %day,
                            "schedule runner: when guard is not a boolean; skipping rule for this day"
                        );
                        continue;
                    }
                    Err(e) => {
                        warn!(
                            rule = %rule.name, day = %day, error = %e,
                            "schedule runner: when guard failed to evaluate; skipping rule for this day"
                        );
                        continue;
                    }
                }
            }
            let mut invocations = Vec::with_capacity(rule.do_steps.len());
            let mut arg_error = false;
            for ds in &rule.do_steps {
                let mut args = Vec::with_capacity(ds.args.len());
                for (k, expr) in &ds.args {
                    match super::expr::eval(expr, &ctx) {
                        Ok(v) => args.push((k.clone(), v)),
                        Err(e) => {
                            warn!(
                                rule = %rule.name, handler = %ds.handler, arg = %k,
                                error = %e,
                                "schedule runner: arg eval failed; skipping rule for this day"
                            );
                            arg_error = true;
                            break;
                        }
                    }
                }
                if arg_error {
                    break;
                }
                invocations.push(MatchedInvocation {
                    handler: ds.handler.clone(),
                    args,
                });
            }
            if arg_error {
                continue;
            }
            matched.push(MatchedRule {
                rule_name: rule.name.clone(),
                invocations,
            });
        }
        matched
    }

    /// Record the scheduled rules that fired on one dispatch (backlog
    /// 4b175523). The id is [`firing_id`] over the synthetic event id —
    /// `clock-day:<day>` or `clock-tick:<instant>` — so a day re-fired
    /// after a crash between the dispatch and the cursor persist
    /// computes the same id and the record says it fired once: the
    /// per-day cursor's at-most-one re-fire, made exactly-once in the
    /// record.
    ///
    /// BEST-EFFORT, the event runner's posture: it returns `()`, runs
    /// after the dispatch, and its failure is logged and dropped — a
    /// record that could not be written never changes what fired or
    /// when the cursor moves.
    async fn record_firings(
        &self,
        topic: &str,
        event_id: &str,
        results: &[handler::InvocationResult],
    ) {
        let Some(sink) = &self.firings else { return };
        let fired = fired_rules(results);
        if fired.is_empty() {
            return;
        }
        // A RECORD STAMP, not the sim day: the same wall instant the
        // event runner stamps, so the rules list ages both alike.
        let fired_at = boss_clock_client::wall_now();
        let rows: Vec<Firing> = fired
            .iter()
            .map(|rule| Firing {
                firing_id: firing_id(rule, event_id),
                rule: rule.clone(),
                fired_on: topic.to_string(),
                fired_at,
                detail: json!({ "event_id": event_id }),
                outcome: Outcome::Fired,
            })
            .collect();
        if let Err(e) = sink.record(&rows).await {
            warn!(
                error = %e,
                topic = %topic,
                triggering_event = %event_id,
                rules = rows.len(),
                "dispatcher firings: a scheduled firing could not be recorded; \
                 the side effects still landed and the log line is the only trace"
            );
        }
    }

    /// Fire every day rule due on `day`. A handler failure is logged and
    /// the day still counts as processed (see `run`).
    async fn fire_day(
        &self,
        calendars: &HashMap<String, BusinessCalendar>,
        day: NaiveDate,
        live: &crate::liveness::DispatcherLiveness,
    ) {
        let (matched, payload) =
            Self::matched_for_day(&self.registry, calendars, day, self.helpers.as_ref());
        if matched.is_empty() {
            return;
        }
        // Synthesize a clock-day dispatch context: the topic is
        // `clock.day` and the "triggering event id" is the day itself,
        // so audit provenance chains the spawned work back to the
        // calendar day that produced it.
        let event_id = format!("clock-day:{}", day.format("%Y-%m-%d"));
        match handler::dispatch(&matched, &self.handlers, &event_id, "clock.day", &payload).await {
            Ok(results) => {
                let mut fired = 0u64;
                let mut failed = 0u64;
                for r in &results {
                    match &r.outcome {
                        Ok(()) => {
                            fired += 1;
                            debug!(
                                rule = %r.rule_name, handler = %r.handler,
                                day = %day, "schedule rule fired"
                            );
                        }
                        Err(e) => {
                            failed += 1;
                            warn!(
                                rule = %r.rule_name, handler = %r.handler,
                                day = %day, error = %e,
                                "schedule rule handler failed"
                            );
                        }
                    }
                }
                info!(day = %day, fired, failed, "schedule runner: fired day");
                self.record_firings("clock.day", &event_id, &results).await;
                live.record_schedule();
            }
            Err(e) => {
                // UnknownHandler is a registry/code drift — surface it
                // loudly, but DON'T wedge the loop or skip the cursor
                // advance (a bad rule must not freeze every other
                // schedule). The day still counts as processed; the
                // operator fixes the rule.
                warn!(day = %day, error = %e, "schedule runner: dispatch error");
            }
        }
    }

    /// Fire the sub-day rules whose bucket this tick crossed, advancing
    /// the in-memory bucket cursors. A handler failure is logged and
    /// the bucket still counts as fired: the next boundary fires again
    /// regardless, and a poll that failed once must not be re-run every
    /// second until it succeeds.
    async fn fire_tick(
        &self,
        calendars: &HashMap<String, BusinessCalendar>,
        now: DateTime<Utc>,
        buckets: &mut HashMap<String, i64>,
        live: &crate::liveness::DispatcherLiveness,
    ) {
        // Decide every rule's bucket first, so the cursor advances for
        // a rule the `when` guard then declines (its bucket has passed
        // either way).
        let mut fires: HashSet<String> = HashSet::new();
        for rule in self.registry.rules() {
            let Some(every) = rule.schedule().and_then(|s| s.cadence.sub_day_minutes()) else {
                continue;
            };
            let bucket = tick_bucket(now, every);
            let (fire, cursor) = advance_bucket(buckets.get(&rule.name).copied(), bucket);
            buckets.insert(rule.name.clone(), cursor);
            if fire {
                fires.insert(rule.name.clone());
            }
        }
        if fires.is_empty() {
            return;
        }
        let (matched, payload) = Self::matched_for_tick(
            &self.registry,
            calendars,
            now,
            &|name| fires.contains(name),
            self.helpers.as_ref(),
        );
        if matched.is_empty() {
            return;
        }
        // Provenance: the topic is `clock.tick` and the event id names
        // the instant, so a packet a poll opens chains back to the tick
        // that produced it, the way a day-spawn chains to its day.
        let event_id = format!("clock-tick:{}", now.to_rfc3339());
        match handler::dispatch(&matched, &self.handlers, &event_id, "clock.tick", &payload).await {
            Ok(results) => {
                for r in &results {
                    match &r.outcome {
                        Ok(()) => {
                            debug!(rule = %r.rule_name, handler = %r.handler, at = %now, "schedule rule fired (tick)")
                        }
                        Err(e) => {
                            warn!(rule = %r.rule_name, handler = %r.handler, at = %now, error = %e, "schedule rule handler failed (tick)")
                        }
                    }
                }
                self.record_firings("clock.tick", &event_id, &results).await;
                live.record_schedule();
            }
            Err(e) => warn!(at = %now, error = %e, "schedule runner: tick dispatch error"),
        }
    }

    /// Consume the clock stream and fire schedule rules on each sim-day
    /// boundary. Runs forever (the SSE stream auto-reconnects); drop the
    /// future to stop.
    pub async fn run(&self, live: Arc<crate::liveness::DispatcherLiveness>) -> Result<()> {
        if !has_schedule_rules(&self.registry) {
            info!("schedule runner: no schedule-triggered rules, not starting");
            return Ok(());
        }
        let calendars = self.load_calendars().await;
        let mut cursor = self.load_cursor().await.unwrap_or_else(|e| {
            warn!(error = %e, "schedule runner: cursor load failed; starting fresh");
            None
        });
        info!(
            schedule_rule_count = self.registry.rules().iter().filter(|r| r.schedule().is_some()).count(),
            calendars = ?calendars.keys().collect::<Vec<_>>(),
            ?cursor,
            cap = self.catchup_cap,
            "schedule runner: starting clock-driven loop"
        );
        live.mark_schedule_running();

        // The sub-day bucket cursors, per rule, in memory (see the
        // module doc: a restart baselines and fires at the next
        // boundary). A paused sim clock ticks the same instant and so
        // never crosses a bucket.
        let mut buckets: HashMap<String, i64> = HashMap::new();

        let mut ticks = Box::pin(boss_clock_client::subscribe_ticks(self.clock_url.clone()));
        while let Some(now) = ticks.next().await {
            if !now.paused && !now.restart_in_progress {
                self.fire_tick(&calendars, now.now, &mut buckets, &live)
                    .await;
            }
            let advance = advance_cursor(cursor, &now, self.catchup_cap);
            if advance.skipped > 0 {
                warn!(
                    skipped = advance.skipped,
                    cap = self.catchup_cap,
                    "schedule runner: catch-up gap exceeded cap; skipped oldest days"
                );
            }
            // Fire each due day, persisting the cursor PER DAY (not once
            // after the whole range). The spawn is not idempotent, so a
            // crash between firing and persisting must re-fire at most the
            // single in-flight day on restart — never the whole catch-up
            // range. The in-memory cursor advances only in lockstep with the
            // persisted one; a persist failure stops the advance so we never
            // run ahead of what's durably recorded (we retry next tick).
            let mut persist_failed = false;
            for day in &advance.days_to_fire {
                self.fire_day(&calendars, *day, &live).await;
                // Record this day done before moving to the next, so a crash
                // re-fires at most this one day. Stop advancing if the persist
                // fails — the in-memory cursor must never lead the durable one.
                if let Err(e) = self.save_cursor(*day).await {
                    warn!(
                        day = %day, error = %e,
                        "schedule runner: cursor persist failed; pausing advance until next tick"
                    );
                    persist_failed = true;
                    break;
                }
                cursor = Some(*day);
            }
            // Non-fire cursor moves (first-observation baseline, backward-jump
            // reset): `days_to_fire` is empty but the cursor still settles.
            if !persist_failed
                && advance.days_to_fire.is_empty()
                && advance.new_cursor != cursor
                && let Some(nc) = advance.new_cursor
            {
                if let Err(e) = self.save_cursor(nc).await {
                    warn!(error = %e, "schedule runner: failed to persist cursor");
                } else {
                    cursor = advance.new_cursor;
                }
            }
        }
        live.mark_schedule_stopped();
        info!("schedule runner: clock stream ended");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::NoHelpers;
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// Build a `ClockNow` for a given date with the given flags.
    fn clock(day: NaiveDate, paused: bool, restart: bool) -> ClockNow {
        ClockNow {
            now: day.and_hms_opt(12, 0, 0).unwrap().and_utc(),
            simulated: true,
            epoch_start: None,
            epoch_end: None,
            paused,
            restart_in_progress: restart,
            warp_factor: None,
        }
    }

    // ----- advance_cursor — the sim-day cursor decision -----

    #[test]
    fn first_observation_sets_baseline_fires_nothing() {
        let a = advance_cursor(None, &clock(d(2026, 4, 27), false, false), 90);
        assert_eq!(a.days_to_fire, Vec::<NaiveDate>::new());
        assert_eq!(a.new_cursor, Some(d(2026, 4, 27)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn same_day_is_noop() {
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 4, 27), false, false),
            90,
        );
        assert!(a.days_to_fire.is_empty());
        assert_eq!(a.new_cursor, Some(d(2026, 4, 27)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn advance_one_day_fires_that_day() {
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 4, 28), false, false),
            90,
        );
        assert_eq!(a.days_to_fire, vec![d(2026, 4, 28)]);
        assert_eq!(a.new_cursor, Some(d(2026, 4, 28)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn multi_day_catch_up_fires_each_day_in_order() {
        // Cursor at 04-27, clock jumps to 04-30 → fire 28, 29, 30.
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 4, 30), false, false),
            90,
        );
        assert_eq!(
            a.days_to_fire,
            vec![d(2026, 4, 28), d(2026, 4, 29), d(2026, 4, 30)]
        );
        assert_eq!(a.new_cursor, Some(d(2026, 4, 30)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn paused_does_not_advance_or_fire() {
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 5, 10), true, false),
            90,
        );
        assert!(a.days_to_fire.is_empty());
        assert_eq!(
            a.new_cursor,
            Some(d(2026, 4, 27)),
            "cursor unchanged while paused"
        );
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn restart_in_progress_does_not_advance_or_fire() {
        let a = advance_cursor(
            Some(d(2026, 4, 27)),
            &clock(d(2026, 5, 10), false, true),
            90,
        );
        assert!(a.days_to_fire.is_empty());
        assert_eq!(a.new_cursor, Some(d(2026, 4, 27)));
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn paused_from_no_cursor_stays_none() {
        // A paused clock observed before any baseline keeps the cursor
        // None (don't adopt a baseline off a frozen observation).
        let a = advance_cursor(None, &clock(d(2026, 4, 27), true, false), 90);
        assert!(a.days_to_fire.is_empty());
        assert_eq!(a.new_cursor, None);
    }

    #[test]
    fn backward_jump_resets_cursor_fires_nothing() {
        // Epoch restart: clock jumps from 2026-12 back to 2026-01.
        let a = advance_cursor(
            Some(d(2026, 12, 31)),
            &clock(d(2026, 1, 1), false, false),
            90,
        );
        assert!(a.days_to_fire.is_empty(), "no backfill on a backward jump");
        assert_eq!(
            a.new_cursor,
            Some(d(2026, 1, 1)),
            "cursor reset to the new (earlier) day"
        );
        assert_eq!(a.skipped, 0);
    }

    #[test]
    fn catch_up_cap_fires_only_most_recent_and_reports_skip() {
        // Cursor at day 0, clock jumps 100 days forward, cap = 10.
        let cursor = d(2026, 1, 1);
        let target = cursor + chrono::Duration::days(100);
        let a = advance_cursor(Some(cursor), &clock(target, false, false), 10);
        assert_eq!(a.days_to_fire.len(), 10, "exactly cap days fire");
        // The fired days are the most recent 10: (target-10, target].
        assert_eq!(
            *a.days_to_fire.first().unwrap(),
            target - chrono::Duration::days(9)
        );
        assert_eq!(*a.days_to_fire.last().unwrap(), target);
        assert_eq!(a.skipped, 90, "the older 90 days are skipped");
        assert_eq!(a.new_cursor, Some(target));
    }

    #[test]
    fn catch_up_exactly_at_cap_fires_all_no_skip() {
        let cursor = d(2026, 1, 1);
        let target = cursor + chrono::Duration::days(10);
        let a = advance_cursor(Some(cursor), &clock(target, false, false), 10);
        assert_eq!(a.days_to_fire.len(), 10);
        assert_eq!(a.skipped, 0);
        assert_eq!(
            *a.days_to_fire.first().unwrap(),
            cursor + chrono::Duration::days(1)
        );
        assert_eq!(*a.days_to_fire.last().unwrap(), target);
    }

    // ----- matched_for_day — which schedule rules fire on a day -----

    fn sched_registry() -> Registry {
        // Two schedule rules: a daily one (no calendar) and a monthly
        // one (no calendar), plus an event rule that must NOT fire.
        let toml = r#"
[[rule]]
name = "daily-sweep"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"bank-sweep\"", subject_kind = "\"account\"", subject = "_day" }

[[rule]]
name = "monthly-close"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-15"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"month-close\"", subject_kind = "\"account\"", subject = "\"acct-gl\"" }

[[rule]]
name = "event-only"
on_event = "step.done.x"
[[rule.do]]
handler = "h"
"#;
        Registry::from_toml(toml).unwrap()
    }

    #[test]
    fn matched_for_day_fires_only_due_schedule_rules() {
        let reg = sched_registry();
        let cals = HashMap::new();
        // 2026-01-15: both daily AND monthly fire; event rule never.
        let (matched, payload) =
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 1, 15), &NoHelpers);
        let names: Vec<&str> = matched.iter().map(|m| m.rule_name.as_str()).collect();
        assert!(names.contains(&"daily-sweep"));
        assert!(names.contains(&"monthly-close"));
        assert!(
            !names.contains(&"event-only"),
            "event rules never fire on a clock day"
        );
        assert_eq!(payload["_day"], "2026-01-15");

        // 2026-01-16: only daily fires.
        let (matched, _) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 1, 16), &NoHelpers);
        let names: Vec<&str> = matched.iter().map(|m| m.rule_name.as_str()).collect();
        assert_eq!(names, vec!["daily-sweep"]);
    }

    // ----- sub-day cadences — the tick path (backlog 2d33e111) -----

    fn sub_day_registry() -> Registry {
        let toml = r#"
[[rule]]
name = "sensors-poll"
[rule.schedule]
cadence = "every-5-minutes"
anchor_date = "2026-09-17"
[[rule.do]]
handler = "sensor.poll"

[[rule]]
name = "daily-sweep"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"bank-sweep\"", subject_kind = "\"account\"", subject = "_day" }
"#;
        Registry::from_toml(toml).unwrap()
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// A sub-day rule is NOT a day rule: the midnight crossing fires
    /// the daily rules only, and the tick path fires the sub-day rule
    /// only.
    #[test]
    fn a_sub_day_rule_rides_the_tick_path_not_the_day_path() {
        let reg = sub_day_registry();
        let cals = HashMap::new();
        let (day, _) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 9, 18), &NoHelpers);
        let names: Vec<&str> = day.iter().map(|m| m.rule_name.as_str()).collect();
        assert_eq!(names, vec!["daily-sweep"]);

        let (tick, payload) = ScheduleRunner::matched_for_tick(
            &reg,
            &cals,
            at("2026-09-18T10:05:00Z"),
            &|_| true,
            &NoHelpers,
        );
        let names: Vec<&str> = tick.iter().map(|m| m.rule_name.as_str()).collect();
        assert_eq!(names, vec!["sensors-poll"]);
        assert_eq!(payload["_day"], "2026-09-18");
        assert_eq!(payload["_at"], "2026-09-18T10:05:00+00:00");
    }

    /// The bucket cursor: first observation baselines and fires
    /// nothing; the same bucket is silent; a later bucket fires ONCE
    /// however many buckets were missed (no backfill — a poll reads
    /// the world as it is now).
    #[test]
    fn a_bucket_fires_once_when_it_changes_and_never_backfills() {
        let every = 5;
        let first = tick_bucket(at("2026-09-18T10:03:00Z"), every);
        assert_eq!(advance_bucket(None, first), (false, first), "baseline");
        let same = tick_bucket(at("2026-09-18T10:04:59Z"), every);
        assert_eq!(advance_bucket(Some(first), same), (false, first));
        let next = tick_bucket(at("2026-09-18T10:05:00Z"), every);
        assert_eq!(advance_bucket(Some(first), next), (true, next));
        let much_later = tick_bucket(at("2026-09-18T12:00:00Z"), every);
        assert_eq!(advance_bucket(Some(next), much_later), (true, much_later));
        // A backward jump re-baselines quietly.
        assert_eq!(advance_bucket(Some(much_later), first), (false, first));
    }

    /// Before the anchor the rule is silent, and the `due` predicate
    /// (the runner's bucket cursor) is what gates a firing.
    #[test]
    fn the_tick_path_honours_the_anchor_and_the_due_predicate() {
        let reg = sub_day_registry();
        let cals = HashMap::new();
        let (tick, _) = ScheduleRunner::matched_for_tick(
            &reg,
            &cals,
            at("2026-09-16T10:05:00Z"),
            &|_| true,
            &NoHelpers,
        );
        assert!(tick.is_empty(), "before the anchor");
        let (tick, _) = ScheduleRunner::matched_for_tick(
            &reg,
            &cals,
            at("2026-09-18T10:05:00Z"),
            &|_| false,
            &NoHelpers,
        );
        assert!(tick.is_empty(), "not due");
    }

    #[test]
    fn matched_for_day_injects_day_into_args() {
        // The `subject = "_day"` arg on daily-sweep must resolve to the
        // firing day from the synthetic payload.
        let reg = sched_registry();
        let cals = HashMap::new();
        let (matched, _) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 3, 9), &NoHelpers);
        let daily = matched
            .iter()
            .find(|m| m.rule_name == "daily-sweep")
            .unwrap();
        let subj = daily.invocations[0]
            .args
            .iter()
            .find(|(k, _)| k == "subject")
            .map(|(_, v)| v)
            .unwrap();
        assert_eq!(
            *subj,
            super::super::expr::Value::String("2026-03-09".into())
        );
    }

    #[test]
    fn matched_for_day_respects_calendar_postponement() {
        // Monthly rule WITH a weekday-only calendar: the 15th-anchored
        // monthly fire on a month where the 15th is a Saturday postpones
        // to Monday the 17th. (2026-08-15 is a Saturday.)
        let toml = r#"
[[rule]]
name = "monthly-on-cal"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-15"
business_calendar = "weekdays-only"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let mut cals = HashMap::new();
        cals.insert(
            "weekdays-only".to_string(),
            BusinessCalendar::new("weekdays-only", "Weekdays Only"),
        );
        // 15th (Sat): does NOT fire.
        assert!(
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 8, 15), &NoHelpers)
                .0
                .is_empty()
        );
        // 16th (Sun): does NOT fire.
        assert!(
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 8, 16), &NoHelpers)
                .0
                .is_empty()
        );
        // 17th (Mon): postponed fire.
        assert_eq!(
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 8, 17), &NoHelpers)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn matched_for_day_missing_calendar_fires_on_nominal_day() {
        // The rule names a calendar that wasn't loaded (absent/errored).
        // Degrade: fire on the nominal cadence day, no postponement.
        let toml = r#"
[[rule]]
name = "monthly-missing-cal"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-15"
business_calendar = "does-not-exist"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let cals = HashMap::new(); // calendar NOT loaded
        // Fires on the nominal 15th even though it's a weekend, because
        // a missing calendar means "every day is a business day".
        assert_eq!(
            ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 8, 15), &NoHelpers)
                .0
                .len(),
            1,
            "missing calendar must fail OPEN — fire on nominal day"
        );
    }

    // ----- referenced_calendar_codes / has_schedule_rules (pure) -----

    #[test]
    fn calendar_codes_collects_distinct_referenced_calendars() {
        let toml = r#"
[[rule]]
name = "a"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
business_calendar = "us-banking"
[[rule.do]]
handler = "h"

[[rule]]
name = "b"
[rule.schedule]
cadence = "weekly"
anchor_date = "2026-01-01"
business_calendar = "us-banking"
[[rule.do]]
handler = "h"

[[rule]]
name = "c"
[rule.schedule]
cadence = "monthly"
anchor_date = "2026-01-01"
business_calendar = "us-tax"
[[rule.do]]
handler = "h"

[[rule]]
name = "d-no-cal"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let codes = referenced_calendar_codes(&reg);
        assert_eq!(codes.len(), 2, "deduped + skips the no-calendar rule");
        assert!(codes.contains("us-banking"));
        assert!(codes.contains("us-tax"));
    }

    #[test]
    fn has_schedule_rules_true_only_with_a_schedule() {
        let with = Registry::from_toml(
            r#"
[[rule]]
name = "s"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h"
"#,
        )
        .unwrap();
        assert!(has_schedule_rules(&with));

        let without = Registry::from_toml(
            r#"
[[rule]]
name = "e"
on_event = "step.done.x"
[[rule.do]]
handler = "h"
"#,
        )
        .unwrap();
        assert!(!has_schedule_rules(&without));
    }

    // ----- load_calendars fail-open (uses FakeCalendarClient) -----

    #[tokio::test]
    async fn load_calendars_skips_absent_calendar() {
        // FakeCalendarClient::get_business_calendar always returns None,
        // so every referenced calendar is "absent" → the map is empty and
        // no panic. (Fail-open behavior is exercised in matched_for_day.)
        // The pool is built lazily inside a Tokio context here and never
        // queried, so it never opens a socket.
        let reg = Registry::from_toml(
            r#"
[[rule]]
name = "s"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
business_calendar = "us-banking"
[[rule.do]]
handler = "h"
"#,
        )
        .unwrap();
        let r = ScheduleRunner {
            registry: reg,
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
            clock_url: "http://127.0.0.1:7060".into(),
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://boss:boss@127.0.0.1/boss")
                .expect("lazy pool"),
            calendar: Arc::new(boss_calendar_client::FakeCalendarClient::new()),
            catchup_cap: DEFAULT_CATCHUP_CAP,
            firings: None,
        };
        let cals = r.load_calendars().await;
        assert!(cals.is_empty(), "absent calendar is skipped, not faked");
    }

    // ----- a scheduled firing is recorded (backlog 4b175523) ---------
    //
    // The rules list read every scheduled rule as "not recorded",
    // because only the event runner wrote dispatcher_firings — so a
    // stalled cadence and an idle one looked the same.

    const RECORDED_RULES: &str = r#"
[[rule]]
name = "daily-ok"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h.ok"

[[rule]]
name = "daily-broken"
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h.fail"

[[rule]]
name = "sensors-poll"
[rule.schedule]
cadence = "every-5-minutes"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "h.ok"
"#;

    fn recording_runner(firings: Option<Arc<dyn FiringSink>>) -> ScheduleRunner {
        let mut handlers = HandlerRegistry::new();
        handlers.register(handler::RecordingHandler::new("h.ok"));
        handlers.register(handler::FailingHandler::new("h.fail", "503"));
        ScheduleRunner {
            registry: Registry::from_toml(RECORDED_RULES).unwrap(),
            handlers,
            helpers: Arc::new(NoHelpers),
            clock_url: "http://127.0.0.1:7060".into(),
            pool: sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://boss:boss@127.0.0.1/boss")
                .expect("lazy pool"),
            calendar: Arc::new(boss_calendar_client::FakeCalendarClient::new()),
            catchup_cap: DEFAULT_CATCHUP_CAP,
            firings,
        }
    }

    #[tokio::test]
    async fn a_scheduled_day_is_recorded_once_per_rule_and_sim_day() {
        use crate::rules::firings::testing::RecordingFirings;
        let sink = RecordingFirings::new();
        let runner = recording_runner(Some(sink.clone()));
        let live = crate::liveness::DispatcherLiveness::default();
        let cals = HashMap::new();
        // The same sim-day twice: a crash between the dispatch and the
        // cursor persist re-fires it. The id must not change, so the
        // table's primary key holds ONE firing for the day.
        runner.fire_day(&cals, d(2026, 3, 9), &live).await;
        runner.fire_day(&cals, d(2026, 3, 9), &live).await;
        let rows = sink.recorded.lock().await.clone();
        assert_eq!(
            rows.iter().map(|f| f.rule.as_str()).collect::<Vec<_>>(),
            ["daily-ok", "daily-ok"],
            "the rule whose handler failed did not fire, and the sub-day rule \
             rides the tick path: {rows:?}"
        );
        assert_eq!(
            rows[0].firing_id,
            "dispatcher:daily-ok:clock-day:2026-03-09"
        );
        assert_eq!(rows[0].firing_id, rows[1].firing_id, "one day, one id");
        assert_eq!(rows[0].fired_on, "clock.day");
        assert_eq!(rows[0].outcome, Outcome::Fired);
        assert_eq!(rows[0].detail["event_id"], "clock-day:2026-03-09");

        runner.fire_day(&cals, d(2026, 3, 10), &live).await;
        let rows = sink.recorded.lock().await.clone();
        assert_eq!(
            rows[2].firing_id, "dispatcher:daily-ok:clock-day:2026-03-10",
            "the next day is the next firing"
        );
    }

    #[tokio::test]
    async fn a_sub_day_firing_is_recorded_under_its_tick() {
        use crate::rules::firings::testing::RecordingFirings;
        let sink = RecordingFirings::new();
        let runner = recording_runner(Some(sink.clone()));
        let live = crate::liveness::DispatcherLiveness::default();
        let cals = HashMap::new();
        let mut buckets = HashMap::new();
        // The first tick baselines; the next bucket fires.
        runner
            .fire_tick(&cals, at("2026-09-18T10:03:00Z"), &mut buckets, &live)
            .await;
        runner
            .fire_tick(&cals, at("2026-09-18T10:05:00Z"), &mut buckets, &live)
            .await;
        let rows = sink.recorded.lock().await.clone();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].rule, "sensors-poll");
        assert_eq!(rows[0].fired_on, "clock.tick");
        assert_eq!(
            rows[0].firing_id,
            "dispatcher:sensors-poll:clock-tick:2026-09-18T10:05:00+00:00"
        );
    }

    #[tokio::test]
    async fn a_scheduled_firing_record_that_cannot_be_written_changes_nothing() {
        use crate::rules::firings::testing::FailingFirings;
        let runner = recording_runner(Some(Arc::new(FailingFirings)));
        let live = crate::liveness::DispatcherLiveness::default();
        runner.fire_day(&HashMap::new(), d(2026, 3, 9), &live).await;
        assert!(
            live.snapshot()["schedule_events"].as_u64().unwrap_or(0) > 0,
            "the day still counts as fired: {}",
            live.snapshot()
        );
    }

    // ----- when guards on schedule rules -----------------------------
    //
    // Until 2026-08-18 a `when` on a schedule-triggered rule was parsed,
    // stored, and never consulted — matched_for_day built its context
    // over NoHelpers and skipped straight to args. These pin the guard
    // actually gating the firing, and the skip-with-warning posture for
    // a guard that cannot evaluate.

    /// Answers one helper with a fixed bool and RECORDS what it was
    /// asked, so a test can assert the dedup key, not just the verdict.
    struct StubOpen {
        answer: bool,
        asked: std::sync::Mutex<Vec<String>>,
    }
    impl StubOpen {
        fn new(answer: bool) -> Self {
            Self {
                answer,
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    impl super::super::expr::HelperResolver for StubOpen {
        fn call(
            &self,
            name: &str,
            args: &[super::super::expr::Value],
        ) -> std::result::Result<super::super::expr::Value, super::super::expr::EvalError> {
            match name {
                "open_publish_exists" => {
                    if let Some(super::super::expr::Value::String(s)) = args.first() {
                        self.asked.lock().expect("stub lock").push(s.clone());
                    }
                    Ok(super::super::expr::Value::Bool(self.answer))
                }
                other => Err(super::super::expr::EvalError::UnknownHelper(
                    other.to_string(),
                )),
            }
        }
    }

    const GUARDED: &str = r#"
[[rule]]
name = "guarded-daily"
when = 'NOT open_publish_exists("github-mirror")'
[rule.schedule]
cadence = "daily"
anchor_date = "2026-01-01"
[[rule.do]]
handler = "jobs.spawn"
"#;

    #[test]
    fn a_guarded_schedule_rule_fires_when_the_guard_clears() {
        let reg = Registry::from_toml(GUARDED).unwrap();
        let cals = HashMap::new();
        let stub = StubOpen::new(false);
        let (matched, _) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 1, 15), &stub);
        assert_eq!(matched.len(), 1, "no open packet → the day fires");
        assert_eq!(
            stub.asked.lock().unwrap().as_slice(),
            ["github-mirror"],
            "the guard asks about the SUBJECT, the only stable identity"
        );
    }

    #[test]
    fn a_guarded_schedule_rule_skips_while_a_packet_is_open() {
        let reg = Registry::from_toml(GUARDED).unwrap();
        let cals = HashMap::new();
        let stub = StubOpen::new(true);
        let (matched, _) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 1, 15), &stub);
        assert!(
            matched.is_empty(),
            "an open packet suppresses the day's spawn: {matched:?}"
        );
    }

    #[test]
    fn a_guard_that_cannot_evaluate_skips_only_its_own_rule() {
        // GUARDED plus an unguarded sibling: the broken guard must not
        // take the sibling's firing down with it.
        let toml = format!(
            "{GUARDED}
[[rule]]
name = \"unguarded-daily\"
[rule.schedule]
cadence = \"daily\"
anchor_date = \"2026-01-01\"
[[rule.do]]
handler = \"jobs.spawn\"
"
        );
        let reg = Registry::from_toml(&toml).unwrap();
        let cals = HashMap::new();
        // NoHelpers cannot resolve open_publish_exists → eval error.
        let (matched, _) = ScheduleRunner::matched_for_day(&reg, &cals, d(2026, 1, 15), &NoHelpers);
        let names: Vec<_> = matched.iter().map(|m| m.rule_name.as_str()).collect();
        assert_eq!(
            names,
            ["unguarded-daily"],
            "the erroring guard skips its rule for the day, nothing else"
        );
    }
}
