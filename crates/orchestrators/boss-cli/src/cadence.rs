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
use chrono::{DateTime, Duration, NaiveTime, Timelike, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use tokio::task::JoinHandle;

use crate::train;

/// The `boss train` verbs a cadence rule may fire — the same set the
/// CLI exposes. Pinned here so a hand-edited registry row cannot make
/// the loop spawn arbitrary arguments.
const VERBS: &[&str] = &["preflight", "reconcile", "board", "run"];

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
}

impl Basis {
    fn as_str(&self) -> &'static str {
        match self {
            Basis::Wall { .. } => "wall",
            Basis::Clock { .. } => "clock",
            Basis::QueueDepth { .. } => "queue-depth",
        }
    }
}

/// The most recent recorded firing of a rule — what evaluation
/// compares the candidate window against.
#[derive(Debug, Clone)]
pub(crate) struct LastFiring {
    pub firing_id: String,
    pub fired_at: DateTime<Utc>,
}

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
// Registry + measurement I/O — thin adapters over cadence_rules /
// cadence_firings. Timestamps are always bound from boss-clock time,
// never SQL NOW().
// ---------------------------------------------------------------------------

fn rule_from_row(row: &PgRow) -> Result<CadenceRule> {
    let name: String = row.try_get("name")?;
    let verb: String = row.try_get("verb")?;
    // Validate at LOAD, not at fire. A malformed row is skipped loudly
    // every tick (see `load_rules`); discovering it only when the rule
    // is due would hide a typo until the moment it matters.
    parse_action(&verb)?;
    let basis: String = row.try_get("basis")?;
    let positive = |field: &str| -> Result<u32> {
        let v: Option<i32> = row.try_get(field)?;
        v.ok_or_else(|| anyhow!("{field} is required for basis {basis:?}"))?
            .try_into()
            .with_context(|| format!("{field} must be positive"))
    };
    let basis = match basis.as_str() {
        "wall" => Basis::Wall {
            every_minutes: positive("every_minutes")?,
        },
        "clock" => {
            let at: Option<Value> = row.try_get("at_times")?;
            let at = at.ok_or_else(|| anyhow!("at_times is required for basis \"clock\""))?;
            Basis::Clock {
                at: parse_at_times(&at)?,
            }
        }
        "queue-depth" => Basis::QueueDepth {
            min_depth: positive("min_dock_depth")?,
            cooldown_minutes: positive("cooldown_minutes")?,
        },
        other => bail!("unknown basis {other:?}"),
    };
    Ok(CadenceRule { name, verb, basis })
}

async fn load_rules(pool: &PgPool) -> Result<Vec<CadenceRule>> {
    let rows = sqlx::query(
        "SELECT name, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes \
         FROM cadence_rules WHERE status = 'active' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .context("loading cadence_rules")?;
    let mut out = Vec::new();
    for row in &rows {
        let name: String = row.try_get("name")?;
        match rule_from_row(row) {
            Ok(rule) => out.push(rule),
            // A malformed row is skipped LOUDLY every tick, not
            // dropped once at startup: the registry is editable data.
            Err(e) => log(format!("skipping unreadable rule {name}: {e:#}")),
        }
    }
    Ok(out)
}

async fn last_firing(pool: &PgPool, rule: &str) -> Result<Option<LastFiring>> {
    let row = sqlx::query(
        "SELECT firing_id, fired_at FROM cadence_firings WHERE rule_name = $1 \
         ORDER BY fired_at DESC LIMIT 1",
    )
    .bind(rule)
    .fetch_optional(pool)
    .await
    .context("reading the last cadence firing")?;
    match row {
        None => Ok(None),
        Some(r) => Ok(Some(LastFiring {
            firing_id: r.try_get("firing_id")?,
            fired_at: r.try_get("fired_at")?,
        })),
    }
}

/// Claim a firing id. `false` = the window was already claimed (a
/// concurrent instance, or a re-run after a crash mid-verb) — the
/// caller must not run the verb.
async fn claim_firing(
    pool: &PgPool,
    id: &str,
    rule: &CadenceRule,
    now: DateTime<Utc>,
    dock_depth: Option<u32>,
) -> Result<bool> {
    let detail = match (&rule.basis, dock_depth) {
        (Basis::QueueDepth { .. }, Some(d)) => json!({"dock_depth": d}),
        _ => json!({}),
    };
    let res = sqlx::query(
        "INSERT INTO cadence_firings (firing_id, rule_name, verb, basis, fired_at, detail) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (firing_id) DO NOTHING",
    )
    .bind(id)
    .bind(&rule.name)
    .bind(&rule.verb)
    .bind(rule.basis.as_str())
    .bind(now) // boss-clock time, bound — never the DB's wallclock
    .bind(detail)
    .execute(pool)
    .await
    .context("claiming the cadence firing")?;
    Ok(res.rows_affected() == 1)
}

/// Merge the verb's outcome into the firing row — the runtime and
/// exit code are what make "what did the cadence cost" a query.
async fn record_outcome(pool: &PgPool, id: &str, rc: i32, runtime_secs: u64) -> Result<()> {
    sqlx::query("UPDATE cadence_firings SET detail = detail || $2 WHERE firing_id = $1")
        .bind(id)
        .bind(json!({"rc": rc, "runtime_secs": runtime_secs}))
        .execute(pool)
        .await
        .context("recording the cadence outcome")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The dock probe — parked ready cars, counted from the jobs API with
// the same predicate boarding itself collects by (train::parked_ready).
// ---------------------------------------------------------------------------

/// The probe reads the same system of record the conductor does, and
/// the same pod roll hits it: on 2026-08-13 a `Connection refused`
/// here held the queue-depth rules for a tick. Same blip guard, same
/// classifier — journalled in this loop's idiom (`cadence: `).
async fn get_json(http: &reqwest::Client, base: &str, path: &str) -> Result<Option<Value>> {
    train::retrying(
        &train::JOBS_API_RETRY,
        &reqwest::Method::GET,
        // The cadence loop is not a train and resolves no delivery
        // policy — it decides only WHEN to spawn a verb. Its journal
        // keeps the compiled cause budget, which is the same number the
        // registry seeds; if the loop ever needs to read policy, this is
        // the line that changes.
        crate::delivery_policy::COMPILED_BLIP_CAUSE_BUDGET,
        &|m| log(m),
        || get_json_once(http, base, path),
    )
    .await
}

async fn get_json_once(
    http: &reqwest::Client,
    base: &str,
    path: &str,
) -> std::result::Result<Option<Value>, train::ApiFailure> {
    let resp = http
        .get(format!("{base}{path}"))
        .header("content-type", "application/json")
        .header("x-boss-user", train::boss_user())
        .send()
        .await
        .map_err(|e| train::ApiFailure::transport(e, format!("GET {path}")))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| train::ApiFailure::transport(e, format!("reading GET {path} response")))?;
    if !status.is_success() {
        return Err(train::ApiFailure {
            kind: train::Failure::Http(status.as_u16()),
            cause: anyhow!("GET {path}: HTTP {status}: {}", body.trim()),
        });
    }
    if body.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&body)
        .map(Some)
        .map_err(|e| train::ApiFailure {
            kind: train::Failure::Malformed,
            cause: anyhow::Error::new(e).context(format!("parsing GET {path} response")),
        })
}

async fn probe_dock_depth() -> Result<u32> {
    let jobs = train::env_or("BOSS_JOBS_URL", "http://127.0.0.1:7900");
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let listed = train::rows(
        get_json(
            &http,
            &jobs,
            "/api/jobs?kind=ship-a-change&status=open&limit=100",
        )
        .await?,
    )?;
    let mut depth = 0u32;
    for j in listed {
        let Some(id) = j.get("id").and_then(Value::as_str) else {
            continue;
        };
        let job = get_json(&http, &jobs, &format!("/api/jobs/{id}"))
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
        pool: &PgPool,
        rule: &CadenceRule,
        firing_id: String,
        now: DateTime<Utc>,
    ) {
        let pool = pool.clone();
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
            if let Err(e) = record_outcome(&pool, &firing_id, rc, secs).await {
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
    pool: &PgPool,
    clock: &dyn ClockClient,
    dry: bool,
    runs: &mut Runs,
) -> Result<TickSummary> {
    // Boss-clock time is the only "now" in this loop (clock-as-service;
    // the no-wallclock invariant). In the wall-mode production deploy
    // it IS wall time — served by the one authoritative clock.
    let now = clock.now().await.now;
    let rules = load_rules(pool).await?;
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
        match probe_dock_depth().await {
            Ok(d) => dock_depth = Some(d),
            Err(e) => log(format!("dock probe failed — queue-depth rules hold: {e:#}")),
        }
    }
    for rule in &rules {
        let last = last_firing(pool, &rule.name).await?;
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
        if !claim_firing(pool, &id, rule, now, dock_depth).await? {
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
        runs.spawn_verb(pool, rule, id, now);
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
    let pg_url = train::env_or("BOSS_POSTGRES_URL", "postgres://boss:boss@127.0.0.1/boss");
    let pool = PgPoolOptions::new()
        // The loop's own queries plus a connection for each spawned
        // verb's closing `record_outcome` — with verbs beside the
        // loop rather than inside it, those can now coincide.
        .max_connections(4)
        .connect(&pg_url)
        .await
        .context("connecting to Postgres for cadence_rules")?;
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
        "loop starting — rules from cadence_rules, clock at {clock_url}, tick {tick_secs}s{}",
        if dry { ", DRY" } else { "" }
    ));
    let mut runs = Runs::default();
    if once {
        // One evaluated tick — but an operator (or a test) asking for
        // it wants the verb's result, and a detached child would die
        // with this process. Wait it out; only the supervised loop
        // gets to move on while a verb runs.
        let outcome = tick(&pool, clock.as_ref(), dry, &mut runs).await;
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
        match tick(&pool, clock.as_ref(), dry, &mut runs).await {
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
    #[tokio::test(flavor = "multi_thread")]
    async fn seeded_rules_load_and_parse() {
        let db = boss_testing::TestDb::new().await;
        let rules = load_rules(&db.pool).await.unwrap();
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
        let depth = by_name("train-board-on-dock-depth");
        assert_eq!(depth.verb, "board");
        // 4 since 147-board-on-four.sql, back where 114 started. The
        // 8 and 12 raises were made when a train was expensive; the
        // forge runner now cycles build-image, locomotive, web and fast
        // in about three minutes, and a dock that never exceeds 3-5
        // made 12 unreachable — four consecutive trains opened and
        // cancelled "nothing to board" on 2026-08-17 while three
        // mergeable cars sat parked. `cooldown_minutes` is the setting
        // that protects the single-concurrency runner, not the depth.
        //
        // This assertion is why the number lives in exactly two places
        // and both must move together: the migration seeds the row, and
        // boss-gcp's LOCAL copy is what the boarding loop actually
        // reads (131 and 147 both say so). Note that `--auto` gates a
        // schema-only change with "fixture + lints only" and SKIPS the
        // tests, so editing the migration alone leaves this red and you
        // will not find out until a crate change drags boss-cli back
        // into scope.
        assert_eq!(
            depth.basis,
            Basis::QueueDepth {
                min_depth: 4,
                cooldown_minutes: 120,
            }
        );
    }

    /// One window, one firing: the second claim of the same id loses,
    /// and the recorded firing holds the window on re-evaluation —
    /// the restart / second-instance idempotence contract end to end.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_window_claims_exactly_once() {
        let db = boss_testing::TestDb::new().await;
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

        assert!(claim_firing(&db.pool, &id, &rule, now, None).await.unwrap());
        // A concurrent instance (or a restart mid-verb) computes the
        // same id and must lose the claim.
        assert!(!claim_firing(&db.pool, &id, &rule, now, None).await.unwrap());

        // The recorded firing is what evaluation sees next tick.
        let last = last_firing(&db.pool, &rule.name).await.unwrap().unwrap();
        assert_eq!(last.firing_id, id);
        assert_eq!(last.fired_at, now);
        assert_eq!(due_window(&rule, now, Some(&last), None), None);

        // The outcome merges into the claim's detail row.
        record_outcome(&db.pool, &id, 0, 42).await.unwrap();
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
}
