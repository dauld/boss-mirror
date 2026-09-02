//! `boss packet census` — measure packet conservation. Report only.
//!
//! docs/design/packet-loss.md proposes the invariant this measures:
//!
//! > Every admitted packet reaches a terminal, and every non-terminal
//! > packet is visible at ≥1 station.
//!
//! The first half is conservation over TIME (open vs terminal counts,
//! ages, and packets with no step motion). The second is conservation
//! over SPACE, and it is newly checkable because stations are registry
//! data: a packet matching zero station predicates is **orphaned by
//! definition** — no lens will present it, so no actor will work it.
//!
//! ## Reporting, not raising
//!
//! Q2 of the design doc defers raising until the base rate is known: a
//! noisy raiser trains people to ignore it. So this verb files nothing,
//! messages nobody, mutates nothing, and **exits 0 whatever it finds**.
//! Every read is a GET.
//!
//! ## Why it reuses the station evaluator instead of reimplementing it
//!
//! An orphan verdict that disagrees with `GET /api/stations/{name}/queue`
//! is worse than no verdict — it would send an operator hunting for a
//! packet the queue is in fact showing, or (worse) certify as visible a
//! packet nobody can see. So membership here is
//! [`StationPredicate::matches`], the same pure function the queue
//! handler calls, over the same universe the queue handler uses
//! (status=open Jobs). The two agreements that matter:
//!
//! - **Universe.** `http/stations.rs` filters `status = Open`. The
//!   census evaluates exactly the open set. Packets that are live but
//!   NOT at status=open (draft / blocked / pending-sign-off) are
//!   outside every station queue's universe by construction; they are
//!   counted and named in the time section rather than silently folded
//!   into the orphan number, because their invisibility has a different
//!   cause and a different fix.
//! - **Steps.** The handler fetches steps only when
//!   `predicate.needs_steps()` and passes an empty slice otherwise;
//!   `matches` ignores `steps` unless its `step` clause is set, so
//!   always passing the real steps (which `/api/jobs` already embeds)
//!   is observationally identical.
//!
//! One honest difference: the queue handler scopes packets to the
//! CALLER's policy predicate. The census reads as an operator, so it
//! sees the whole instance — an individual's queue may be a subset.
//! That is the point of a census.
//!
//! ## Edge integrity resolves the way the write path resolves
//!
//! Same argument, second surface. `job_edges` declarations are checked
//! by a Postgres trigger whose `job_edge_resolves()` is **prefix-aware**:
//! an exact id match, else an unambiguous prefix of length ≥ 8. Migration
//! 104 recorded why — `backlog_item` / `boarded_jobs` values are mostly
//! 8-char id prefixes, and 14 of ~15 dangle under exact match. A census
//! that checked exact ids only would report a dozen phantom danglings on
//! its first run. [`resolve`] mirrors the SQL, including its
//! "empty value = no claim" rule.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;

use boss_core::job::{Job, Step};
use boss_jobs::job_edges::{InMemoryJobEdges, JobEdgeSpec, JobEdgesRegistry};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::stations::StationSpec;

/// Reads are policy-gated. The census claims operator scope because
/// its subject IS the whole instance — a scoped read would report
/// "orphan" for every packet the reader merely cannot see. Read-only:
/// this module issues GETs and nothing else.
const CENSUS_USER: &str = r#"{"id":"automation:packet-census","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

/// Job statuses that mean "still in the network". Terminal is the
/// complement: closed + cancelled.
const LIVE_STATUSES: [&str; 4] = ["draft", "open", "blocked", "pending-sign-off"];
const TERMINAL_STATUSES: [&str; 2] = ["closed", "cancelled"];

/// Page size for packet fetches. `MAX_LIMIT` in the jobs API is 1000;
/// staying under it keeps one page one round trip.
const PAGE: usize = 500;

// ---------------------------------------------------------------------------
// Pure classification
// ---------------------------------------------------------------------------

/// A packet as the census evaluates it: the Job plus its steps —
/// exactly the pair [`StationPredicate::matches`] consumes.
///
/// [`StationPredicate::matches`]: boss_jobs::StationPredicate::matches
#[derive(Debug, Clone)]
pub(crate) struct Packet {
    pub job: Job,
    pub steps: Vec<Step>,
}

/// Names of the active stations whose predicate this packet satisfies.
/// Empty = orphaned: no queue will ever present it.
pub(crate) fn matching_stations<'a>(packet: &Packet, stations: &'a [StationSpec]) -> Vec<&'a str> {
    stations
        .iter()
        .filter(|s| s.status == WorkflowStatus::Active)
        .filter(|s| s.predicate.matches(&packet.job, &packet.steps))
        .map(|s| s.name.as_str())
        .collect()
}

/// Which fact dated a packet's last motion. Ordered by precision:
/// a conductor's RFC3339 stamp beats a day-granular completion date,
/// which beats "nothing has happened since it opened".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MotionBasis {
    CompletedAt,
    CompletedOn,
    OpenedOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct Motion {
    pub at: DateTime<Utc>,
    pub basis: MotionBasis,
}

fn midnight(d: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&d.and_time(NaiveTime::MIN))
}

/// When this packet last provably moved: the newest step completion,
/// falling back to `opened_on`. Never a guess — the basis rides along
/// so a stale verdict can say what dated it.
///
/// `completed_at` lives in step metadata (the conductor stamps it);
/// `completed_on` is the step column and is day-granular.
pub(crate) fn last_motion(job: &Job, steps: &[Step]) -> Motion {
    let mut best = Motion {
        at: midnight(job.opened_on),
        basis: MotionBasis::OpenedOn,
    };
    for s in steps {
        if let Some(d) = s.completed_on {
            let at = midnight(d);
            if at >= best.at {
                best = Motion {
                    at,
                    basis: MotionBasis::CompletedOn,
                };
            }
        }
        if let Some(at) = s
            .metadata
            .get("completed_at")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
            && at >= best.at
        {
            best = Motion {
                at,
                basis: MotionBasis::CompletedAt,
            };
        }
    }
    best
}

/// Whole days since the packet last moved. Clamped at 0 — a stamp in
/// the future is a clock problem, not negative staleness.
pub(crate) fn days_since_motion(motion: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (now - motion).num_days().max(0)
}

/// Stale = no step motion for at least `threshold_days`.
pub(crate) fn is_stale(motion: DateTime<Utc>, now: DateTime<Utc>, threshold_days: i64) -> bool {
    days_since_motion(motion, now) >= threshold_days
}

/// Whole days a packet has been open.
pub(crate) fn age_days(opened_on: NaiveDate, now: DateTime<Utc>) -> i64 {
    (now.date_naive() - opened_on).num_days().max(0)
}

/// Inclusive upper bounds, in days, of every age bucket but the last.
pub(crate) const AGE_BUCKETS: [(&str, i64); 4] =
    [("0-1d", 1), ("2-7d", 7), ("8-30d", 30), ("31-90d", 90)];
pub(crate) const AGE_OVERFLOW: &str = "90d+";

pub(crate) fn age_bucket(days: i64) -> &'static str {
    for (label, upper) in AGE_BUCKETS {
        if days <= upper {
            return label;
        }
    }
    AGE_OVERFLOW
}

/// One declared `job_edges` reference found on a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeRef {
    pub field: String,
    pub value: String,
}

/// Postgres `->>` semantics: a JSON string yields its text, anything
/// else its JSON encoding, null yields nothing (the guard treats a
/// NULL candidate as no claim to check).
fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Every Job reference this packet's metadata declares, per the
/// `job_edges` registry. Mirrors `check_job_edges()`: `'*'` matches
/// every kind; a `job_id_list` whose value is not an array is skipped
/// the way the trigger skips it; an absent, null, or empty value is
/// not a claim and yields no ref.
pub(crate) fn edge_refs(job: &Job, edges: &[JobEdgeSpec]) -> Vec<EdgeRef> {
    let mut out = Vec::new();
    let mut push = |field: &str, v: &Value| {
        if let Some(s) = text_of(v).filter(|s| !s.is_empty()) {
            out.push(EdgeRef {
                field: field.to_string(),
                value: s,
            });
        }
    };
    for e in edges
        .iter()
        .filter(|e| e.source_kind == "*" || e.source_kind == job.kind)
    {
        let Some(raw) = job.metadata.get(&e.field_path) else {
            continue;
        };
        if e.field_kind == "job_id_list" {
            let Some(arr) = raw.as_array() else { continue };
            for v in arr {
                push(&e.field_path, v);
            }
        } else {
            push(&e.field_path, raw);
        }
    }
    out
}

/// What the census can say about one edge reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Resolution {
    /// A Job answers to this value.
    Present,
    /// Nothing answers to it. The write path would now refuse this
    /// metadata — the destroyed/misrouted fingerprint.
    Dangling,
    /// The prefix answers to more than one Job: unresolvable for the
    /// same reason as dangling, different cause.
    Ambiguous,
    /// Not decidable from what the census fetched. Never reported as a
    /// defect — an incomplete scan is a cost problem, not a finding.
    Unknown,
}

/// The Job ids the census knows about, and how completely.
#[derive(Debug, Default)]
pub(crate) struct Universe {
    /// Ids observed, from the open set and any id scan.
    pub ids: BTreeSet<String>,
    /// True when `ids` is every Job in the instance. Prefix
    /// resolution is only sound against a complete universe: a prefix
    /// that looks unambiguous in a subset may not be in the whole.
    pub complete: bool,
    /// Verdicts from targeted `GET /api/jobs/{id}` probes. Sound for
    /// full ids only — nothing can be a strict prefix of a full uuid
    /// but itself, so a 404 there is proof of absence.
    pub probed: BTreeMap<String, bool>,
}

/// Resolve one candidate the way `job_edge_resolves()` does: exact id,
/// else an unambiguous prefix of length ≥ 8. Pure.
pub(crate) fn resolve(candidate: &str, u: &Universe) -> Resolution {
    if candidate.is_empty() {
        return Resolution::Present;
    }
    if u.ids.contains(candidate) {
        return Resolution::Present;
    }
    if let Some(&exists) = u.probed.get(candidate) {
        return if exists {
            Resolution::Present
        } else {
            Resolution::Dangling
        };
    }
    // `length()` in the SQL counts characters, so this does too.
    if candidate.chars().count() < 8 {
        // Too short to resolve as a prefix under any universe — sound
        // to call dangling even from partial evidence.
        return Resolution::Dangling;
    }
    if !u.complete {
        return Resolution::Unknown;
    }
    match u.ids.iter().filter(|id| id.starts_with(candidate)).count() {
        0 => Resolution::Dangling,
        1 => Resolution::Present,
        _ => Resolution::Ambiguous,
    }
}

/// The one line an operator reads if they read nothing else.
pub(crate) fn summary_line(
    open: usize,
    stale: usize,
    stale_days: i64,
    orphans: usize,
    dangling: usize,
) -> String {
    format!(
        "census: {open} open, {stale} stale (>{stale_days}d), \
         {orphans} orphaned, {dangling} dangling edges"
    )
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct KindRow {
    pub kind: String,
    pub open: i64,
    pub other_live: i64,
    pub terminal: i64,
    pub total: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct BucketRow {
    pub bucket: &'static str,
    pub packets: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct StaleRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub age_days: i64,
    pub days_since_motion: i64,
    pub last_motion: DateTime<Utc>,
    pub motion_basis: MotionBasis,
}

#[derive(Debug, Serialize)]
pub(crate) struct TimeSection {
    pub by_kind: Vec<KindRow>,
    pub by_status: BTreeMap<String, i64>,
    pub open: i64,
    pub other_live: i64,
    pub terminal: i64,
    pub total: i64,
    pub age_histogram: Vec<BucketRow>,
    pub stale_after_days: i64,
    pub stale: usize,
    pub stale_packets: Vec<StaleRow>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MatchRow {
    pub stations: usize,
    pub packets: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct StationDepth {
    pub station: String,
    pub packets: usize,
    /// A station whose predicate binds `@me` has no global depth. Its
    /// membership is per-actor by construction, and `matches()`
    /// deliberately fails closed when the placeholder is unbound — so
    /// the census, which evaluates without an actor, sees zero and
    /// would otherwise report "0" where the truth is "not applicable".
    ///
    /// This mattered: the first census run called `my-watchlist` empty
    /// and counted every packet whose only home was someone's
    /// watchlist as orphaned, overstating the orphan share by 46
    /// packets (158 reported vs 112 genuinely unreachable).
    pub personal: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct OrphanRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub age_days: i64,
    pub opened_on: NaiveDate,
    pub owner_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SpaceSection {
    pub open_evaluated: usize,
    pub active_stations: usize,
    pub matches_histogram: Vec<MatchRow>,
    pub station_depth: Vec<StationDepth>,
    pub orphans: Vec<OrphanRow>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EdgeRow {
    pub job_id: String,
    pub job_kind: String,
    pub field: String,
    pub value: String,
    pub resolution: Resolution,
}

#[derive(Debug, Serialize)]
pub(crate) struct EdgeSection {
    pub declarations_from: String,
    pub declared: usize,
    pub scope: &'static str,
    pub refs_checked: usize,
    pub dangling: Vec<EdgeRow>,
    pub unknown: Vec<EdgeRow>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Cost {
    pub api_calls: usize,
    pub open_packets_fetched: usize,
    pub open_packets_reported: i64,
    pub open_truncated: bool,
    pub ids_scanned: usize,
    pub id_scan_complete: bool,
    pub id_probes: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct Census {
    pub generated_at: DateTime<Utc>,
    pub jobs_url: String,
    pub conservation_over_time: TimeSection,
    pub conservation_over_space: SpaceSection,
    pub edge_integrity: EdgeSection,
    pub cost: Cost,
    pub notes: Vec<String>,
    pub summary: String,
}

// ---------------------------------------------------------------------------
// I/O
// ---------------------------------------------------------------------------

pub struct Options {
    pub stale_days: i64,
    pub json: bool,
    pub max_open: usize,
    pub max_scan: usize,
    pub jobs_url: Option<String>,
}

/// GET-only client over jobs-api that counts what it cost.
///
/// jobs-api directly, not the gateway: the gateway is the BROWSER
/// edge and strips inbound `x-boss-*`, so operator tooling has no way
/// to present itself there (same path `boss queue` takes).
struct Api {
    client: reqwest::Client,
    base: String,
    calls: usize,
}

impl Api {
    fn new(base: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base,
            calls: 0,
        }
    }

    async fn get_raw(&mut self, path: &str) -> Result<(reqwest::StatusCode, Value)> {
        let url = format!("{}{path}", self.base);
        self.calls += 1;
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", CENSUS_USER)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .with_context(|| format!("read body of {url}"))?;
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok((status, body))
    }

    async fn get(&mut self, path: &str) -> Result<Value> {
        let (status, body) = self.get_raw(path).await?;
        if !status.is_success() {
            bail!("GET {}{path} -> HTTP {status}", self.base);
        }
        Ok(body)
    }
}

fn rows(body: &Value) -> Vec<Value> {
    if let Some(a) = body.as_array() {
        return a.clone();
    }
    body.get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Split one `/api/jobs` row into the Job and its embedded steps.
/// `/api/jobs` enriches every row with `steps`, so the whole packet —
/// the pair the station predicate needs — arrives in one round trip.
fn packet_from_row(row: &Value) -> Option<Packet> {
    let job: Job = serde_json::from_value(row.clone()).ok()?;
    let steps: Vec<Step> = row
        .get("steps")
        .cloned()
        .map(serde_json::from_value)
        .and_then(Result::ok)
        .unwrap_or_default();
    Some(Packet { job, steps })
}

/// Take the census and print it. Exits 0 whatever it found — the only
/// failure is not being able to read the network at all.
pub async fn run(opts: Options, now: DateTime<Utc>) -> Result<()> {
    let json = opts.json;
    let census = collect(opts, now).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&census)?);
    } else {
        println!("{}", render(&census));
    }
    // Report, not gate (packet-loss.md Q2 — raising waits on the base
    // rate this measures).
    Ok(())
}

/// Every read the census makes, folded into the report. Separated from
/// printing so a test can drive the whole collection against a stubbed
/// jobs-api and assert on the findings.
async fn collect(opts: Options, now: DateTime<Utc>) -> Result<Census> {
    let base = opts
        .jobs_url
        .unwrap_or_else(|| crate::train::env_or("BOSS_JOBS_URL", "http://127.0.0.1:7900"));
    let base = base.trim_end_matches('/').to_string();
    let mut api = Api::new(base.clone());
    let mut notes: Vec<String> = Vec::new();

    // --- stations -------------------------------------------------
    let stations_body = api.get("/api/stations").await?;
    let stations: Vec<StationSpec> = rows(&stations_body)
        .into_iter()
        .filter_map(|r| serde_json::from_value(r).ok())
        .filter(|s: &StationSpec| s.status == WorkflowStatus::Active)
        .collect();
    if stations.is_empty() {
        notes.push(
            "no active stations — every open packet reads as orphaned, which says more \
             about the registry than about the packets"
                .into(),
        );
    }

    // --- conservation over time: counts by kind × status ----------
    // `/api/jobs/summary` is O(kinds), not O(jobs): one cheap call
    // per status beats paging every closed Job in history.
    let mut by_status: BTreeMap<String, i64> = BTreeMap::new();
    let mut per_kind: BTreeMap<String, KindRow> = BTreeMap::new();
    for status in LIVE_STATUSES.iter().chain(TERMINAL_STATUSES.iter()) {
        let body = api
            .get(&format!("/api/jobs/summary?status={status}"))
            .await?;
        let counts = body
            .get("counts")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut total = 0i64;
        for (kind, n) in counts {
            let n = n.as_i64().unwrap_or(0);
            total += n;
            let row = per_kind.entry(kind.clone()).or_insert_with(|| KindRow {
                kind,
                open: 0,
                other_live: 0,
                terminal: 0,
                total: 0,
            });
            row.total += n;
            match *status {
                "open" => row.open += n,
                s if TERMINAL_STATUSES.contains(&s) => row.terminal += n,
                _ => row.other_live += n,
            }
        }
        by_status.insert((*status).to_string(), total);
    }
    let by_kind: Vec<KindRow> = per_kind.into_values().collect();
    let open_reported: i64 = by_status.get("open").copied().unwrap_or(0);
    let other_live: i64 = LIVE_STATUSES
        .iter()
        .filter(|s| **s != "open")
        .filter_map(|s| by_status.get(*s))
        .sum();
    let terminal: i64 = TERMINAL_STATUSES
        .iter()
        .filter_map(|s| by_status.get(*s))
        .sum();

    // --- the open packet universe ---------------------------------
    let mut packets: Vec<Packet> = Vec::new();
    let mut offset = 0usize;
    loop {
        let want = opts.max_open.saturating_sub(packets.len()).min(PAGE);
        if want == 0 {
            break;
        }
        let body = api
            .get(&format!(
                "/api/jobs?status=open&limit={want}&offset={offset}"
            ))
            .await?;
        let page = rows(&body);
        let got = page.len();
        packets.extend(page.iter().filter_map(packet_from_row));
        offset += got;
        if got < want {
            break;
        }
    }
    let open_truncated = (packets.len() as i64) < open_reported;
    if open_truncated {
        notes.push(format!(
            "TRUNCATED: fetched {} of {open_reported} open packets (--max-open {}); \
             orphan, staleness and edge findings below cover the fetched slice only",
            packets.len(),
            opts.max_open
        ));
    }

    // --- conservation over space ----------------------------------
    let mut matches: BTreeMap<usize, usize> = BTreeMap::new();
    let mut depth: BTreeMap<&str, usize> =
        stations.iter().map(|s| (s.name.as_str(), 0usize)).collect();
    let mut orphans: Vec<OrphanRow> = Vec::new();
    for p in &packets {
        let hits = matching_stations(p, &stations);
        *matches.entry(hits.len()).or_insert(0) += 1;
        for name in &hits {
            *depth.entry(name).or_insert(0) += 1;
        }
        if hits.is_empty() {
            orphans.push(OrphanRow {
                id: p.job.id.to_string(),
                kind: p.job.kind.clone(),
                title: p.job.title.clone(),
                age_days: age_days(p.job.opened_on, now),
                opened_on: p.job.opened_on,
                owner_id: p.job.owner_id.clone(),
            });
        }
    }
    orphans.sort_by(|a, b| b.age_days.cmp(&a.age_days).then(a.id.cmp(&b.id)));

    // --- ages + staleness -----------------------------------------
    let mut buckets: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut stale_packets: Vec<StaleRow> = Vec::new();
    for p in &packets {
        let age = age_days(p.job.opened_on, now);
        *buckets.entry(age_bucket(age)).or_insert(0) += 1;
        let motion = last_motion(&p.job, &p.steps);
        if is_stale(motion.at, now, opts.stale_days) {
            stale_packets.push(StaleRow {
                id: p.job.id.to_string(),
                kind: p.job.kind.clone(),
                title: p.job.title.clone(),
                age_days: age,
                days_since_motion: days_since_motion(motion.at, now),
                last_motion: motion.at,
                motion_basis: motion.basis,
            });
        }
    }
    stale_packets.sort_by(|a, b| {
        b.days_since_motion
            .cmp(&a.days_since_motion)
            .then(a.id.cmp(&b.id))
    });
    let age_histogram: Vec<BucketRow> = AGE_BUCKETS
        .iter()
        .map(|(label, _)| *label)
        .chain(std::iter::once(AGE_OVERFLOW))
        .map(|bucket| BucketRow {
            bucket,
            packets: buckets.get(bucket).copied().unwrap_or(0),
        })
        .collect();

    // --- edge integrity -------------------------------------------
    let (edges, declarations_from) = match api.get("/api/jobs/job-edges").await {
        Ok(body) => (
            rows(&body)
                .into_iter()
                .filter_map(|r| serde_json::from_value::<JobEdgeSpec>(r).ok())
                .collect::<Vec<_>>(),
            format!("{base}/api/jobs/job-edges"),
        ),
        Err(e) => {
            notes.push(format!(
                "job-edges endpoint unavailable ({e}); using the built-in seeded defaults"
            ));
            (
                InMemoryJobEdges.list().await.unwrap_or_default(),
                "built-in defaults (boss_jobs::job_edges::InMemoryJobEdges)".to_string(),
            )
        }
    };

    let mut refs: Vec<(String, String, EdgeRef)> = Vec::new();
    for p in &packets {
        for r in edge_refs(&p.job, &edges) {
            refs.push((p.job.id.to_string(), p.job.kind.clone(), r));
        }
    }

    let mut universe = Universe {
        ids: packets.iter().map(|p| p.job.id.to_string()).collect(),
        complete: false,
        probed: BTreeMap::new(),
    };
    let (ids_scanned, id_probes) =
        resolve_universe(&mut api, &mut universe, &refs, opts.max_scan, &mut notes).await?;

    let mut dangling: Vec<EdgeRow> = Vec::new();
    let mut unknown: Vec<EdgeRow> = Vec::new();
    for (job_id, job_kind, r) in &refs {
        let resolution = resolve(&r.value, &universe);
        let row = EdgeRow {
            job_id: job_id.clone(),
            job_kind: job_kind.clone(),
            field: r.field.clone(),
            value: r.value.clone(),
            resolution,
        };
        match resolution {
            Resolution::Dangling | Resolution::Ambiguous => dangling.push(row),
            Resolution::Unknown => unknown.push(row),
            Resolution::Present => {}
        }
    }

    // --- stranded gate-runs: green verdicts no car ever claimed -------
    // A change that gated green but was never parked never reaches the
    // dock, so it cannot board — the "why did a green gate never load"
    // question, as a number. Reads closed gate-runs and cars, which the
    // open-packet slice above never sees. BEST-EFFORT: it is one hygiene
    // note, not a hard dependency, so a failed read skips it rather than
    // failing the whole census.
    let gate_run_read = api.get("/api/jobs?kind=gate-run&limit=60").await;
    let car_read = api.get("/api/jobs?kind=ship-a-change&limit=800").await;
    if let (Ok(gate_run_body), Ok(car_body)) = (gate_run_read, car_read) {
        let car_branches: BTreeSet<String> = rows(&car_body)
            .iter()
            .filter_map(|c| {
                c.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let stranded = stranded_gate_runs(&rows(&gate_run_body), &car_branches);
        if !stranded.is_empty() {
            notes.push(format!(
                "{} stranded gate-run(s) — gated green, never parked, so never on the dock: {}",
                stranded.len(),
                stranded.join(", ")
            ));
        }
    }

    let summary = summary_line(
        packets.len(),
        stale_packets.len(),
        opts.stale_days,
        orphans.len(),
        dangling.len(),
    );

    let census = Census {
        generated_at: now,
        jobs_url: base,
        conservation_over_time: TimeSection {
            by_kind,
            by_status,
            open: open_reported,
            other_live,
            terminal,
            total: open_reported + other_live + terminal,
            age_histogram,
            stale_after_days: opts.stale_days,
            stale: stale_packets.len(),
            stale_packets,
        },
        conservation_over_space: SpaceSection {
            open_evaluated: packets.len(),
            active_stations: stations.len(),
            matches_histogram: matches
                .into_iter()
                .map(|(stations, packets)| MatchRow { stations, packets })
                .collect(),
            station_depth: depth
                .into_iter()
                .map(|(station, packets)| StationDepth {
                    personal: stations
                        .iter()
                        .find(|s| s.name == station)
                        .map(|s| s.predicate.binds_self())
                        .unwrap_or(false),
                    station: station.to_string(),
                    packets,
                })
                .collect(),
            orphans,
        },
        edge_integrity: EdgeSection {
            declarations_from,
            declared: edges.len(),
            scope: "open packets",
            refs_checked: refs.len(),
            dangling,
            unknown,
        },
        cost: Cost {
            api_calls: api.calls,
            open_packets_fetched: packets.len(),
            open_packets_reported: open_reported,
            open_truncated,
            ids_scanned,
            id_scan_complete: universe.complete,
            id_probes,
        },
        notes,
        summary,
    };
    Ok(census)
}

/// Give the edge resolver enough of the id universe to be sound, and
/// report what that cost. Returns `(ids_scanned, id_probes)`.
///
/// Two strategies, cheapest first:
///
/// - A candidate that is a full 36-char uuid can be settled by one
///   `GET /api/jobs/{id}`: nothing but itself can be a prefix of it,
///   so a 404 is proof of absence.
/// - A candidate that is a PREFIX cannot be settled that way — the SQL
///   asks whether exactly one Job id starts with it, which needs every
///   id. That triggers a full id scan, bounded by `--max-scan`. If the
///   bound truncates, prefixes come back `unknown` rather than being
///   guessed at from a partial universe.
async fn resolve_universe(
    api: &mut Api,
    universe: &mut Universe,
    refs: &[(String, String, EdgeRef)],
    max_scan: usize,
    notes: &mut Vec<String>,
) -> Result<(usize, usize)> {
    let unresolved: BTreeSet<&str> = refs
        .iter()
        .map(|(_, _, r)| r.value.as_str())
        .filter(|v| !v.is_empty() && !universe.ids.contains(*v))
        .collect();
    let (full_ids, prefixes): (Vec<&str>, Vec<&str>) = unresolved
        .into_iter()
        .filter(|v| v.chars().count() >= 8)
        .partition(|v| uuid::Uuid::parse_str(v).is_ok());

    if prefixes.is_empty() {
        // Cheap path: probe the handful of full ids that are not in
        // the open set (closed trains, mostly).
        let mut probes = 0usize;
        for id in full_ids {
            let (status, _) = api.get_raw(&format!("/api/jobs/{id}")).await?;
            probes += 1;
            if status == reqwest::StatusCode::NOT_FOUND {
                universe.probed.insert(id.to_string(), false);
            } else if status.is_success() {
                universe.probed.insert(id.to_string(), true);
            } else {
                notes.push(format!(
                    "edge referent {id}: GET /api/jobs/{id} -> HTTP {status}; reported unknown"
                ));
            }
        }
        return Ok((0, probes));
    }

    if max_scan == 0 {
        notes.push(format!(
            "{} edge reference(s) are id PREFIXES, which resolve only against the full id \
             universe; --max-scan 0 disabled the scan, so they are reported unknown",
            prefixes.len()
        ));
        return Ok((0, 0));
    }

    // Full id scan. Costed and reported, never silent.
    let mut scanned = 0usize;
    let mut offset = 0usize;
    let mut exhausted = false;
    while scanned < max_scan {
        let want = (max_scan - scanned).min(PAGE);
        let body = api
            .get(&format!("/api/jobs?limit={want}&offset={offset}"))
            .await?;
        let page = rows(&body);
        let got = page.len();
        for r in &page {
            if let Some(id) = r.get("id").and_then(Value::as_str) {
                universe.ids.insert(id.to_string());
            }
        }
        scanned += got;
        offset += got;
        if got < want {
            exhausted = true;
            break;
        }
    }
    universe.complete = exhausted;
    if exhausted {
        notes.push(format!(
            "{} edge reference(s) are id prefixes (migration 104's measured folklore), so the \
             census scanned all {scanned} Jobs to resolve them the way the write-path guard does",
            prefixes.len()
        ));
    } else {
        notes.push(format!(
            "TRUNCATED: id scan stopped at --max-scan {max_scan}; {} prefix reference(s) \
             reported unknown rather than guessed",
            prefixes.len()
        ));
    }
    Ok((scanned, 0))
}

// ---------------------------------------------------------------------------
// Human rendering
// ---------------------------------------------------------------------------

fn trunc(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        let head: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// How many stale packets the human view names before it stops and
/// points at `--json`. The count is the finding; the list is a
/// starting point for acting on it.
const STALE_SHOWN: usize = 10;

/// The human report, as text. Returned rather than printed so the
/// shape an operator reads is assertable — the summary line and the
/// orphan roster are the product here, not a debugging by-product.
fn render(c: &Census) -> String {
    let t = &c.conservation_over_time;
    let s = &c.conservation_over_space;
    let e = &c.edge_integrity;
    let mut o: Vec<String> = Vec::new();

    o.push(format!(
        "packet census · {} · {}",
        c.jobs_url,
        c.generated_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    o.push(String::new());

    o.push("CONSERVATION OVER TIME — does every admitted packet reach a terminal?".into());
    o.push(format!(
        "  {:<32}{:>8}{:>12}{:>10}{:>9}",
        "KIND", "OPEN", "OTHER-LIVE", "TERMINAL", "TOTAL"
    ));
    for row in &t.by_kind {
        o.push(format!(
            "  {:<32}{:>8}{:>12}{:>10}{:>9}",
            trunc(&row.kind, 31),
            row.open,
            row.other_live,
            row.terminal,
            row.total
        ));
    }
    o.push(format!(
        "  {:<32}{:>8}{:>12}{:>10}{:>9}",
        "TOTAL", t.open, t.other_live, t.terminal, t.total
    ));
    if t.other_live > 0 {
        let breakdown: Vec<String> = LIVE_STATUSES
            .iter()
            .filter(|k| **k != "open")
            .map(|k| format!("{k} {}", t.by_status.get(*k).copied().unwrap_or(0)))
            .collect();
        o.push(String::new());
        o.push(format!(
            "  {} live packet(s) are not at status=open ({}).",
            t.other_live,
            breakdown.join(", ")
        ));
        o.push(
            "  No station queue can present these: the queue lens evaluates status=open only."
                .into(),
        );
    }

    o.push(String::new());
    o.push(format!(
        "  age of the {} open packets fetched",
        s.open_evaluated
    ));
    for b in &t.age_histogram {
        o.push(format!("    {:<8}{:>6}", b.bucket, b.packets));
    }
    o.push(String::new());
    o.push(format!(
        "  no step motion in {}d: {}",
        t.stale_after_days, t.stale
    ));
    for row in t.stale_packets.iter().take(STALE_SHOWN) {
        o.push(format!(
            "    {}  {:<20}{:>5}d since motion ({})  {}",
            short(&row.id),
            trunc(&row.kind, 19),
            row.days_since_motion,
            match row.motion_basis {
                MotionBasis::CompletedAt => "completed_at",
                MotionBasis::CompletedOn => "completed_on",
                MotionBasis::OpenedOn => "never moved",
            },
            trunc(&row.title, 44)
        ));
    }
    if t.stale_packets.len() > STALE_SHOWN {
        o.push(format!(
            "    … and {} more (--json for the full list)",
            t.stale_packets.len() - STALE_SHOWN
        ));
    }

    o.push(String::new());
    o.push("CONSERVATION OVER SPACE — is every open packet visible at ≥1 station?".into());
    o.push(format!(
        "  {} open packet(s) evaluated against {} active station(s)",
        s.open_evaluated, s.active_stations
    ));
    let hist: Vec<String> = s
        .matches_histogram
        .iter()
        .map(|m| format!("{}→{}", m.stations, m.packets))
        .collect();
    o.push(format!(
        "  stations matched per packet:  {}",
        hist.join("   ")
    ));
    o.push("  per-station depth (compare GET /api/stations/<name>/queue .total)".into());
    for d in &s.station_depth {
        if d.personal {
            // Not a number, because there isn't one. A per-actor
            // station has as many depths as there are actors, and the
            // census has no actor to bind.
            o.push(format!(
                "    {:<32}{:>6}  per-actor — ask GET /api/stations/{}/queue as someone",
                trunc(&d.station, 31),
                "n/a",
                d.station
            ));
        } else {
            o.push(format!("    {:<32}{:>6}", trunc(&d.station, 31), d.packets));
        }
    }
    if s.station_depth.iter().any(|d| d.personal) {
        o.push(String::new());
        o.push(
            "  NOTE: orphan counts below are 'matched no SHARED station'. A packet
  listed there may still be visible to its owner through a per-actor
  station — the census cannot see those, so it must not claim they are
  invisible to everyone."
                .into(),
        );
    }
    o.push(String::new());
    if s.orphans.is_empty() {
        o.push("  ORPHANS (0 stations matched): none".into());
    } else {
        // Listed individually, always: a count alone tells an operator
        // that something is unworkable without telling them what.
        o.push(format!(
            "  ORPHANS (0 stations matched) — {}",
            s.orphans.len()
        ));
        o.push(format!("    {:<10}{:<24}{:>6}  TITLE", "ID", "KIND", "AGE"));
        for row in &s.orphans {
            o.push(format!(
                "    {:<10}{:<24}{:>5}d  {}",
                short(&row.id),
                trunc(&row.kind, 23),
                row.age_days,
                trunc(&row.title, 50)
            ));
        }
    }

    o.push(String::new());
    o.push("EDGE INTEGRITY — do declared job_edges point at Jobs that exist?".into());
    o.push(format!(
        "  {} declaration(s) from {} · {} reference(s) checked on {}",
        e.declared, e.declarations_from, e.refs_checked, e.scope
    ));
    if e.dangling.is_empty() {
        o.push("  dangling: none".into());
    } else {
        o.push(format!("  dangling — {}", e.dangling.len()));
        for row in &e.dangling {
            o.push(format!(
                "    {}  {:<20}{} → {}  ({})",
                short(&row.job_id),
                trunc(&row.job_kind, 19),
                row.field,
                row.value,
                match row.resolution {
                    Resolution::Ambiguous => "prefix matches more than one Job",
                    _ => "no such Job",
                }
            ));
        }
    }
    if !e.unknown.is_empty() {
        o.push(format!(
            "  unknown (not decidable from what was fetched) — {}",
            e.unknown.len()
        ));
    }

    o.push(String::new());
    o.push("COST".into());
    o.push(format!(
        "  {} API call(s) · {} open packet(s) fetched of {} reported{}",
        c.cost.api_calls,
        c.cost.open_packets_fetched,
        c.cost.open_packets_reported,
        if c.cost.open_truncated {
            " (TRUNCATED)"
        } else {
            ""
        }
    ));
    if c.cost.ids_scanned > 0 || c.cost.id_probes > 0 {
        o.push(format!(
            "  {} Job id(s) scanned for edge resolution ({}) · {} targeted probe(s)",
            c.cost.ids_scanned,
            if c.cost.id_scan_complete {
                "complete"
            } else {
                "incomplete"
            },
            c.cost.id_probes
        ));
    }

    if !c.notes.is_empty() {
        o.push(String::new());
        o.push("NOTES".into());
        for n in &c.notes {
            o.push(format!("  - {n}"));
        }
    }

    o.push(String::new());
    o.push(c.summary.clone());
    o.join("\n")
}

/// Green gate-runs whose branch has no ship-a-change car: a change that
/// gated but was never parked, so it never reached the dock and could
/// not board. The hygiene number behind "a green gate that never
/// loaded" — nothing turned the verdict into a car, which is the gap
/// gate-green auto-park closes. Read-only: it names the branches so an
/// operator can park the good ones or drop the obsolete.
pub(crate) fn stranded_gate_runs(
    gate_runs: &[Value],
    car_branches: &BTreeSet<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for g in gate_runs {
        // A gate-run an operator has marked SUPERSEDED is not stranded —
        // its green is dead, not waiting: the branch was deleted, or the
        // same change landed via another branch (facts only git can see,
        // so they arrive as an annotation on the packet, not a derivation
        // here). Two such corpses sat in this list for a day reading as
        // rescuable work (754b01b5); rescue guidance pointing at a
        // deleted branch is worse than none.
        if g.get("metadata")
            .and_then(|m| m.get("superseded"))
            .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
        {
            continue;
        }
        let green = g
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|s| {
                s.get("metadata")
                    .and_then(|m| m.get("verdict"))
                    .and_then(Value::as_str)
                    == Some("green")
            });
        if !green {
            continue;
        }
        let branch = g
            .get("metadata")
            .and_then(|m| m.get("branch"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if branch.is_empty() || car_branches.contains(branch) {
            continue;
        }
        if !out.iter().any(|b| b == branch) {
            out.push(branch.to_string());
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{JobId, JobStatus, Priority, StepStatus, Subject};
    use boss_jobs::station_queue::{StationPredicate, StepMatch};
    use boss_jobs::stations::StationKind;
    use serde_json::json;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, d).unwrap()
    }

    fn at(d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, d, h, 0, 0).unwrap()
    }

    fn job(kind: &str, opened: u32) -> Job {
        let mut j = Job::new(
            kind,
            Subject::new("custom", "x"),
            "a packet",
            "emp-1",
            Priority::Standard,
            day(opened),
        );
        j.status = JobStatus::Open;
        j
    }

    fn step(slug: &str, status: StepStatus) -> Step {
        let mut s = Step::new(JobId::new(), "task", slug, 0);
        s.spec_slug = Some(slug.to_string());
        s.status = status;
        s
    }

    fn station(name: &str, predicate: StationPredicate) -> StationSpec {
        // `now` is threaded in explicitly since the no-wallclock lint
        // moved the clock out of the constructor; a test fixture is the
        // one place a literal Utc::now() is honest.
        let mut s = StationSpec::draft(
            name,
            name,
            StationKind::Batch,
            predicate,
            chrono::Utc::now(),
        );
        s.status = WorkflowStatus::Active;
        s
    }

    fn packet(job: Job, steps: Vec<Step>) -> Packet {
        Packet { job, steps }
    }

    // -----------------------------------------------------------
    // Stranded gate-runs — green verdicts no car claimed
    // -----------------------------------------------------------

    #[test]
    fn a_green_gate_run_with_no_car_is_stranded() {
        let gate_runs = vec![
            json!({"metadata": {"branch": "fix/parked"},   "steps": [{"metadata": {"verdict": "green"}}]}),
            json!({"metadata": {"branch": "fix/stranded"}, "steps": [{"metadata": {"verdict": "green"}}]}),
            json!({"metadata": {"branch": "fix/redded"},   "steps": [{"metadata": {"verdict": "failed"}}]}),
        ];
        let mut cars = BTreeSet::new();
        cars.insert("fix/parked".to_string());
        // Only the green gate-run whose branch never became a car is
        // stranded: the parked one has a car, the red one never vouched.
        assert_eq!(
            stranded_gate_runs(&gate_runs, &cars),
            vec!["fix/stranded".to_string()]
        );
    }

    #[test]
    fn nothing_is_stranded_when_every_green_branch_has_a_car() {
        let gate_runs = vec![
            json!({"metadata": {"branch": "fix/a"}, "steps": [{"metadata": {"verdict": "green"}}]}),
        ];
        let mut cars = BTreeSet::new();
        cars.insert("fix/a".to_string());
        assert!(stranded_gate_runs(&gate_runs, &cars).is_empty());
    }

    /// A green marked `superseded` is dead, not waiting (754b01b5): the
    /// branch was deleted, or the change landed via another branch —
    /// facts only git can see, recorded as an annotation. Listing it as
    /// stranded hands the operator rescue guidance for a corpse.
    #[test]
    fn a_superseded_green_is_not_stranded() {
        let gate_runs = vec![
            json!({"metadata": {"branch": "fix/corpse", "superseded": true},
                   "steps": [{"metadata": {"verdict": "green"}}]}),
            json!({"metadata": {"branch": "fix/live"},
                   "steps": [{"metadata": {"verdict": "green"}}]}),
            // Explicit false = not superseded; still stranded.
            json!({"metadata": {"branch": "fix/kept", "superseded": false},
                   "steps": [{"metadata": {"verdict": "green"}}]}),
        ];
        assert_eq!(
            stranded_gate_runs(&gate_runs, &BTreeSet::new()),
            vec!["fix/kept".to_string(), "fix/live".to_string()]
        );
    }

    // Orphan detection — a packet against a station set
    // -----------------------------------------------------------

    #[test]
    fn orphan_is_a_packet_no_active_station_predicate_matches() {
        let stations = vec![
            station(
                "dock",
                StationPredicate {
                    kind: Some("ship-a-change".into()),
                    ..Default::default()
                },
            ),
            station(
                "yard",
                StationPredicate {
                    kind: Some("pr-train".into()),
                    ..Default::default()
                },
            ),
        ];
        let seen = packet(job("ship-a-change", 10), vec![]);
        assert_eq!(matching_stations(&seen, &stations), vec!["dock"]);

        let orphan = packet(job("user-feedback", 10), vec![]);
        assert!(
            matching_stations(&orphan, &stations).is_empty(),
            "no station claims it — nobody's lens will ever show it"
        );
    }

    #[test]
    fn a_packet_can_sit_at_several_stations() {
        let stations = vec![
            station("all", StationPredicate::default()),
            station(
                "urgent-lane",
                StationPredicate {
                    tags_any: vec!["hotfix".into()],
                    ..Default::default()
                },
            ),
        ];
        let p = packet(job("k", 1).with_tags(vec!["hotfix".into()]), vec![]);
        assert_eq!(matching_stations(&p, &stations), vec!["all", "urgent-lane"]);
    }

    #[test]
    fn retired_and_draft_stations_do_not_rescue_an_orphan() {
        // Only ACTIVE rows serve a queue, so only active rows can
        // make a packet visible.
        let mut retired = station(
            "old-dock",
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
        );
        retired.status = WorkflowStatus::Retired;
        let mut draft = station("new-dock", StationPredicate::default());
        draft.status = WorkflowStatus::Draft;
        let p = packet(job("ship-a-change", 1), vec![]);
        assert!(matching_stations(&p, &[retired, draft]).is_empty());
    }

    #[test]
    fn orphan_verdict_uses_the_steps_a_step_predicate_reads() {
        // The station evaluator is reused whole: a step-clause station
        // matches only when the packet's steps say so. Getting this
        // wrong is exactly the "census disagrees with the queue" bug.
        let stations = vec![station(
            "review",
            StationPredicate {
                step: Some(StepMatch {
                    slug: Some("review".into()),
                    status_in: vec![StepStatus::Ready],
                    ..Default::default()
                }),
                ..Default::default()
            },
        )];
        let visible = packet(job("k", 1), vec![step("review", StepStatus::Ready)]);
        assert_eq!(matching_stations(&visible, &stations), vec!["review"]);

        let stepless = packet(job("k", 1), vec![]);
        assert!(matching_stations(&stepless, &stations).is_empty());

        let done = packet(job("k", 1), vec![step("review", StepStatus::Completed)]);
        assert!(matching_stations(&done, &stations).is_empty());
    }

    // -----------------------------------------------------------
    // Staleness — step timestamps against a threshold
    // -----------------------------------------------------------

    #[test]
    fn motion_falls_back_to_opened_on_when_nothing_completed() {
        let m = last_motion(&job("k", 1), &[]);
        assert_eq!(m.basis, MotionBasis::OpenedOn);
        assert_eq!(m.at, midnight(day(1)));
    }

    #[test]
    fn motion_prefers_the_newest_completion() {
        let mut old = step("a", StepStatus::Completed);
        old.completed_on = Some(day(3));
        let mut new = step("b", StepStatus::Completed);
        new.completed_on = Some(day(9));
        let m = last_motion(&job("k", 1), &[old, new]);
        assert_eq!(m.basis, MotionBasis::CompletedOn);
        assert_eq!(m.at, midnight(day(9)));
    }

    #[test]
    fn a_completed_at_stamp_beats_the_day_granular_column() {
        // The conductor stamps RFC3339 into step metadata; it is the
        // more precise fact, so it wins the tie on the same day.
        let mut s = step("a", StepStatus::Completed);
        s.completed_on = Some(day(9));
        s.metadata = json!({"completed_at": "2026-08-09T14:30:00Z"});
        let m = last_motion(&job("k", 1), &[s]);
        assert_eq!(m.basis, MotionBasis::CompletedAt);
        assert_eq!(m.at, Utc.with_ymd_and_hms(2026, 8, 9, 14, 30, 0).unwrap());
    }

    #[test]
    fn an_unparseable_stamp_is_ignored_not_guessed_at() {
        let mut s = step("a", StepStatus::Completed);
        s.metadata = json!({"completed_at": "yesterday-ish"});
        let m = last_motion(&job("k", 4), &[s]);
        assert_eq!(m.basis, MotionBasis::OpenedOn);
        assert_eq!(m.at, midnight(day(4)));
    }

    #[test]
    fn staleness_is_whole_days_at_or_past_the_threshold() {
        let motion = at(1, 0);
        assert!(!is_stale(motion, at(7, 23), 7), "6d23h is not yet 7 days");
        assert!(is_stale(motion, at(8, 0), 7), "exactly 7 days is stale");
        assert!(is_stale(motion, at(30, 0), 7));
        assert_eq!(days_since_motion(motion, at(8, 0)), 7);
    }

    #[test]
    fn a_future_stamp_reads_as_zero_days_not_negative() {
        assert_eq!(days_since_motion(at(20, 0), at(10, 0)), 0);
        assert!(!is_stale(at(20, 0), at(10, 0), 1));
    }

    #[test]
    fn a_packet_that_moved_today_is_never_stale() {
        let mut s = step("a", StepStatus::Completed);
        s.metadata = json!({"completed_at": "2026-08-13T09:00:00Z"});
        let m = last_motion(&job("k", 1), &[s]);
        assert!(!is_stale(m.at, at(13, 18), 7));
    }

    // -----------------------------------------------------------
    // Ages
    // -----------------------------------------------------------

    #[test]
    fn age_is_whole_days_since_opened_on() {
        assert_eq!(age_days(day(1), at(8, 6)), 7);
        assert_eq!(age_days(day(8), at(8, 6)), 0);
        assert_eq!(age_days(day(20), at(8, 6)), 0, "future-dated clamps to 0");
    }

    #[test]
    fn age_buckets_partition_every_age() {
        assert_eq!(age_bucket(0), "0-1d");
        assert_eq!(age_bucket(1), "0-1d");
        assert_eq!(age_bucket(2), "2-7d");
        assert_eq!(age_bucket(7), "2-7d");
        assert_eq!(age_bucket(8), "8-30d");
        assert_eq!(age_bucket(30), "8-30d");
        assert_eq!(age_bucket(31), "31-90d");
        assert_eq!(age_bucket(90), "31-90d");
        assert_eq!(age_bucket(91), AGE_OVERFLOW);
        assert_eq!(age_bucket(4000), AGE_OVERFLOW);
    }

    // -----------------------------------------------------------
    // Edge integrity — refs found, referents present or absent
    // -----------------------------------------------------------

    fn edges() -> Vec<JobEdgeSpec> {
        vec![
            JobEdgeSpec {
                source_kind: "*".into(),
                field_path: "waiting_on".into(),
                field_kind: "job_id".into(),
                on_missing: "abort".into(),
                description: String::new(),
            },
            JobEdgeSpec {
                source_kind: "ship-a-change".into(),
                field_path: "train".into(),
                field_kind: "job_id".into(),
                on_missing: "abort".into(),
                description: String::new(),
            },
            JobEdgeSpec {
                source_kind: "ship-a-change".into(),
                field_path: "backlog_item".into(),
                field_kind: "job_id".into(),
                on_missing: "abort".into(),
                description: String::new(),
            },
            JobEdgeSpec {
                source_kind: "pr-train".into(),
                field_path: "boarded_jobs".into(),
                field_kind: "job_id_list".into(),
                on_missing: "abort".into(),
                description: String::new(),
            },
        ]
    }

    #[test]
    fn edge_refs_follow_the_declarations_for_this_kind_plus_the_wildcard() {
        let j = job("ship-a-change", 1).with_metadata(json!({
            "train": "aaaaaaaa-1111-2222-3333-444444444444",
            "waiting_on": "bbbbbbbb-1111-2222-3333-444444444444",
            "boarded_jobs": ["nope"],
            "branch": "feat/x",
        }));
        let refs = edge_refs(&j, &edges());
        assert_eq!(
            refs,
            vec![
                EdgeRef {
                    field: "waiting_on".into(),
                    value: "bbbbbbbb-1111-2222-3333-444444444444".into()
                },
                EdgeRef {
                    field: "train".into(),
                    value: "aaaaaaaa-1111-2222-3333-444444444444".into()
                },
            ],
            "boarded_jobs is declared for pr-train, not ship-a-change; \
             branch is not an edge at all"
        );
    }

    #[test]
    fn a_job_id_list_yields_one_ref_per_element() {
        let j = job("pr-train", 1).with_metadata(json!({"boarded_jobs": ["1a2b3c4d", "5e6f7a8b"]}));
        let refs = edge_refs(&j, &edges());
        assert_eq!(
            refs.iter().map(|r| r.value.as_str()).collect::<Vec<_>>(),
            vec!["1a2b3c4d", "5e6f7a8b"]
        );
    }

    #[test]
    fn absent_null_and_cleared_values_are_not_claims() {
        // The guard's own rule: a missing key continues, and
        // job_edge_resolves('') returns TRUE — a cleared waiting_on is
        // how the dispatcher wakes a waiter, and must not read as loss.
        assert!(edge_refs(&job("ship-a-change", 1), &edges()).is_empty());
        let nulled = job("ship-a-change", 1).with_metadata(json!({"train": null}));
        assert!(edge_refs(&nulled, &edges()).is_empty());
        let cleared = job("ship-a-change", 1).with_metadata(json!({"waiting_on": ""}));
        assert!(edge_refs(&cleared, &edges()).is_empty());
        assert_eq!(resolve("", &Universe::default()), Resolution::Present);
    }

    #[test]
    fn a_non_array_job_id_list_is_skipped_the_way_the_trigger_skips_it() {
        let j = job("pr-train", 1).with_metadata(json!({"boarded_jobs": "not-an-array"}));
        assert!(edge_refs(&j, &edges()).is_empty());
    }

    fn universe(ids: &[&str], complete: bool) -> Universe {
        Universe {
            ids: ids.iter().map(|s| (*s).to_string()).collect(),
            complete,
            probed: BTreeMap::new(),
        }
    }

    #[test]
    fn an_exact_id_that_exists_resolves() {
        let u = universe(&["aaaaaaaa-1111-2222-3333-444444444444"], true);
        assert_eq!(
            resolve("aaaaaaaa-1111-2222-3333-444444444444", &u),
            Resolution::Present
        );
    }

    #[test]
    fn a_referent_that_no_longer_exists_is_dangling() {
        let u = universe(&["aaaaaaaa-1111-2222-3333-444444444444"], true);
        assert_eq!(
            resolve("cccccccc-1111-2222-3333-444444444444", &u),
            Resolution::Dangling
        );
    }

    #[test]
    fn an_unambiguous_prefix_resolves_the_way_the_write_path_resolves_it() {
        // Migration 104's measured folklore: backlog_item / boarded_jobs
        // carry 8-char PREFIXES. Exact-match-only would call all of
        // these dangling and the census would open with a dozen lies.
        let u = universe(
            &[
                "1a2b3c4d-1111-2222-3333-444444444444",
                "9999aaaa-1111-2222-3333-444444444444",
            ],
            true,
        );
        assert_eq!(resolve("1a2b3c4d", &u), Resolution::Present);
    }

    #[test]
    fn an_ambiguous_prefix_is_unresolvable_just_like_a_missing_one() {
        let u = universe(
            &[
                "1a2b3c4d-1111-2222-3333-444444444444",
                "1a2b3c4d-9999-2222-3333-444444444444",
            ],
            true,
        );
        assert_eq!(resolve("1a2b3c4d", &u), Resolution::Ambiguous);
    }

    #[test]
    fn a_candidate_shorter_than_eight_can_never_resolve() {
        // job_edge_resolves() returns FALSE below length 8 whatever
        // the universe holds — sound to call dangling from partial
        // evidence.
        assert_eq!(resolve("1a2b", &universe(&[], false)), Resolution::Dangling);
    }

    #[test]
    fn a_prefix_is_unknown_not_dangling_when_the_scan_was_truncated() {
        // Never claim loss from evidence that cannot support it.
        let partial = universe(&["9999aaaa-1111-2222-3333-444444444444"], false);
        assert_eq!(resolve("1a2b3c4d", &partial), Resolution::Unknown);
        let complete = universe(&["9999aaaa-1111-2222-3333-444444444444"], true);
        assert_eq!(resolve("1a2b3c4d", &complete), Resolution::Dangling);
    }

    #[test]
    fn a_probed_full_id_settles_without_the_full_universe() {
        let mut u = universe(&[], false);
        u.probed
            .insert("aaaaaaaa-1111-2222-3333-444444444444".into(), false);
        u.probed
            .insert("bbbbbbbb-1111-2222-3333-444444444444".into(), true);
        assert_eq!(
            resolve("aaaaaaaa-1111-2222-3333-444444444444", &u),
            Resolution::Dangling
        );
        assert_eq!(
            resolve("bbbbbbbb-1111-2222-3333-444444444444", &u),
            Resolution::Present
        );
    }

    // -----------------------------------------------------------
    // The summary line's arithmetic
    // -----------------------------------------------------------

    #[test]
    fn summary_line_reads_the_way_the_design_doc_asks() {
        assert_eq!(
            summary_line(178, 12, 7, 3, 0),
            "census: 178 open, 12 stale (>7d), 3 orphaned, 0 dangling edges"
        );
    }

    #[test]
    fn summary_line_carries_the_flag_settable_threshold() {
        assert_eq!(
            summary_line(4, 4, 30, 0, 2),
            "census: 4 open, 4 stale (>30d), 0 orphaned, 2 dangling edges"
        );
    }

    #[test]
    fn a_clean_instance_says_so_without_a_single_finding() {
        assert_eq!(
            summary_line(0, 0, 7, 0, 0),
            "census: 0 open, 0 stale (>7d), 0 orphaned, 0 dangling edges"
        );
    }

    /// The counts in the summary are the counts in the sections — the
    /// arithmetic that makes the headline trustworthy, driven end to
    /// end over a packet set with one of each defect.
    #[test]
    fn the_headline_counts_agree_with_the_packets_that_produced_them() {
        let now = at(13, 12);
        let stations = vec![station(
            "dock",
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
        )];

        let mut moved = step("build", StepStatus::Completed);
        moved.metadata = json!({"completed_at": "2026-08-13T09:00:00Z"});
        let packets = [
            // visible + fresh
            packet(job("ship-a-change", 12), vec![moved]),
            // visible + stale (opened long ago, never moved)
            packet(job("ship-a-change", 1), vec![]),
            // orphaned + stale
            packet(job("user-feedback", 1), vec![]),
        ];

        let orphans = packets
            .iter()
            .filter(|p| matching_stations(p, &stations).is_empty())
            .count();
        let stale = packets
            .iter()
            .filter(|p| is_stale(last_motion(&p.job, &p.steps).at, now, 7))
            .count();
        assert_eq!(orphans, 1);
        assert_eq!(stale, 2);
        assert_eq!(
            summary_line(packets.len(), stale, 7, orphans, 0),
            "census: 3 open, 2 stale (>7d), 1 orphaned, 0 dangling edges"
        );
    }

    // -----------------------------------------------------------
    // Wire shapes the census parses
    // -----------------------------------------------------------

    #[test]
    fn a_packet_parses_out_of_one_api_jobs_row_with_its_steps() {
        // `/api/jobs` builds each row as `to_value(job)` with `steps`
        // injected, so one round trip carries the whole (job, steps)
        // pair the station predicate needs. Built here the same way
        // the handler builds it — a row assembled by hand would pin a
        // shape the API does not actually emit.
        let source = job("ship-a-change", 1).with_metadata(json!({"branch": "feat/x"}));
        let mut row = serde_json::to_value(&source).expect("serialises");
        row["steps"] = serde_json::to_value(vec![step("build", StepStatus::Ready)]).expect("steps");

        let p = packet_from_row(&row).expect("parses");
        assert_eq!(p.job, source, "the whole Job survives the round trip");
        assert_eq!(p.job.status, JobStatus::Open);
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].spec_slug.as_deref(), Some("build"));
    }

    #[test]
    fn a_row_without_steps_is_still_a_packet() {
        // Nothing downstream should explode on a stepless packet —
        // and a step-clause station must simply not match it, rather
        // than the census skipping the packet and under-reporting.
        let row = serde_json::to_value(job("k", 1)).expect("serialises");
        let p = packet_from_row(&row).expect("parses");
        assert!(p.steps.is_empty());
    }

    #[test]
    fn rows_reads_both_the_envelope_and_a_bare_array() {
        assert_eq!(rows(&json!({"data": [1, 2], "total": 9})).len(), 2);
        assert_eq!(rows(&json!([1, 2, 3])).len(), 3);
        assert!(rows(&json!({})).is_empty());
    }

    #[test]
    fn a_station_row_round_trips_through_the_api_shape() {
        let spec = station(
            "loading-dock",
            StationPredicate {
                kind: Some("ship-a-change".into()),
                metadata_absent: vec!["train".into()],
                ..Default::default()
            },
        );
        let wire = serde_json::to_value(&spec).expect("serialises");
        let back: StationSpec = serde_json::from_value(wire).expect("parses back");
        assert_eq!(back, spec, "the census must read what /api/stations writes");
    }

    // -----------------------------------------------------------
    // The report an operator actually reads
    // -----------------------------------------------------------

    #[test]
    fn a_personal_station_is_reported_as_per_actor_not_as_zero() {
        // Regression for the first live census run, which printed
        // `my-watchlist  0` and folded every packet whose only home was
        // someone's watchlist into the orphan list — overstating the
        // orphan share by 46 packets (158 reported, 112 genuinely
        // unreachable). `matches()` fails closed on an unbound `@me` by
        // design, so the census sees zero; the report must say "not
        // applicable" rather than "empty".
        let mut c = sample_census();
        c.conservation_over_space.station_depth = vec![
            StationDepth {
                station: "loading-dock".into(),
                packets: 3,
                personal: false,
            },
            StationDepth {
                station: "my-watchlist".into(),
                packets: 0,
                personal: true,
            },
        ];
        let out = render(&c);

        assert!(
            out.contains("my-watchlist") && out.contains("per-actor"),
            "a personal station must be labelled per-actor:\n{out}"
        );
        assert!(
            !out.contains("my-watchlist                        0"),
            "a personal station must not be rendered as a depth of 0:\n{out}"
        );
        assert!(
            out.contains("matched no SHARED station"),
            "the orphan caveat must appear when a personal station exists:\n{out}"
        );
        // A shared station still reports a real number.
        assert!(
            out.contains("loading-dock") && out.contains("3"),
            "shared station depth regressed:\n{out}"
        );
    }

    fn sample_census() -> Census {
        Census {
            generated_at: at(13, 18),
            jobs_url: "http://127.0.0.1:7900".into(),
            conservation_over_time: TimeSection {
                by_kind: vec![KindRow {
                    kind: "ship-a-change".into(),
                    open: 2,
                    other_live: 1,
                    terminal: 40,
                    total: 43,
                }],
                by_status: [
                    ("draft".to_string(), 0),
                    ("open".to_string(), 2),
                    ("blocked".to_string(), 1),
                    ("pending-sign-off".to_string(), 0),
                    ("closed".to_string(), 40),
                    ("cancelled".to_string(), 0),
                ]
                .into_iter()
                .collect(),
                open: 2,
                other_live: 1,
                terminal: 40,
                total: 43,
                age_histogram: vec![BucketRow {
                    bucket: "2-7d",
                    packets: 2,
                }],
                stale_after_days: 7,
                stale: 1,
                stale_packets: vec![StaleRow {
                    id: "9f3a1c2e-1111-2222-3333-444444444444".into(),
                    kind: "ship-a-change".into(),
                    title: "a packet nobody moved".into(),
                    age_days: 31,
                    days_since_motion: 31,
                    last_motion: at(1, 0),
                    motion_basis: MotionBasis::OpenedOn,
                }],
            },
            conservation_over_space: SpaceSection {
                open_evaluated: 2,
                active_stations: 1,
                matches_histogram: vec![
                    MatchRow {
                        stations: 0,
                        packets: 1,
                    },
                    MatchRow {
                        stations: 1,
                        packets: 1,
                    },
                ],
                station_depth: vec![StationDepth {
                    station: "loading-dock".into(),
                    packets: 1,
                    personal: false,
                }],
                orphans: vec![OrphanRow {
                    id: "9f3a1c2e-1111-2222-3333-444444444444".into(),
                    kind: "user-feedback".into(),
                    title: "nobody's lens shows this".into(),
                    age_days: 31,
                    opened_on: day(1),
                    owner_id: "emp-1".into(),
                }],
            },
            edge_integrity: EdgeSection {
                declarations_from: "http://127.0.0.1:7900/api/jobs/job-edges".into(),
                declared: 4,
                scope: "open packets",
                refs_checked: 3,
                dangling: vec![EdgeRow {
                    job_id: "3b2c9d1a-1111-2222-3333-444444444444".into(),
                    job_kind: "ship-a-change".into(),
                    field: "backlog_item".into(),
                    value: "1a2b3c4d".into(),
                    resolution: Resolution::Dangling,
                }],
                unknown: vec![],
            },
            cost: Cost {
                api_calls: 9,
                open_packets_fetched: 2,
                open_packets_reported: 2,
                open_truncated: false,
                ids_scanned: 43,
                id_scan_complete: true,
                id_probes: 0,
            },
            notes: vec!["a note worth reading".into()],
            summary: summary_line(2, 1, 7, 1, 1),
        }
    }

    #[test]
    fn the_report_ends_on_the_summary_line() {
        // Whatever else scrolls past, the last line is the verdict.
        let out = render(&sample_census());
        assert_eq!(
            out.lines().next_back().unwrap(),
            "census: 2 open, 1 stale (>7d), 1 orphaned, 1 dangling edges"
        );
    }

    #[test]
    fn the_report_names_every_orphan_not_just_the_count() {
        let out = render(&sample_census());
        assert!(out.contains("ORPHANS (0 stations matched) — 1"));
        assert!(out.contains("9f3a1c2e"), "the id, so it can be looked up");
        assert!(out.contains("user-feedback"), "the kind");
        assert!(out.contains("nobody's lens shows this"), "the title");
        assert!(out.contains("31d"), "the age");
    }

    #[test]
    fn the_report_says_what_it_cost_rather_than_sampling_quietly() {
        let out = render(&sample_census());
        assert!(out.contains("9 API call(s)"));
        assert!(out.contains("43 Job id(s) scanned for edge resolution (complete)"));
    }

    #[test]
    fn live_packets_outside_the_queue_universe_are_called_out_separately() {
        // A blocked packet is invisible to every station queue for a
        // different reason than an orphan, and folding it into the
        // orphan count would misdirect whoever acts on the number.
        let out = render(&sample_census());
        assert!(out.contains("1 live packet(s) are not at status=open"));
        assert!(out.contains("blocked 1"));
        assert!(!out.contains("ORPHANS (0 stations matched) — 2"));
    }

    #[test]
    fn a_clean_report_still_states_each_verdict() {
        let mut c = sample_census();
        c.conservation_over_space.orphans.clear();
        c.edge_integrity.dangling.clear();
        c.conservation_over_time.other_live = 0;
        c.summary = summary_line(2, 0, 7, 0, 0);
        let out = render(&c);
        assert!(out.contains("ORPHANS (0 stations matched): none"));
        assert!(out.contains("dangling: none"));
        assert!(!out.contains("are not at status=open"));
        assert!(out.ends_with("census: 2 open, 0 stale (>7d), 0 orphaned, 0 dangling edges"));
    }

    // -----------------------------------------------------------
    // End to end against a stubbed jobs-api
    //
    // Not a live server: a socket on 127.0.0.1:0 answering the
    // census's own GETs from canned bodies, so the collection path —
    // URLs, paging, envelope parsing, edge resolution, the cost
    // accounting — is exercised without a BOSS instance anywhere.
    // -----------------------------------------------------------

    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    type Handler = Arc<dyn Fn(&str) -> (u16, Value) + Send + Sync>;

    async fn spawn_stub(handler: Handler) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&hits);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                seen.lock().unwrap().push(target.clone());
                let (code, body) = handler(&target);
                let body = body.to_string();
                let reason = if code == 200 { "OK" } else { "Not Found" };
                let resp = format!(
                    "HTTP/1.1 {code} {reason}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), hits)
    }

    fn row(job: &Job, steps: Vec<Step>) -> Value {
        let mut v = serde_json::to_value(job).expect("job serialises");
        v["steps"] = serde_json::to_value(steps).expect("steps serialise");
        v
    }

    fn envelope(data: Vec<Value>) -> Value {
        let total = data.len();
        json!({"data": data, "total": total})
    }

    fn opts(base: &str) -> Options {
        Options {
            stale_days: 7,
            json: false,
            max_open: 2000,
            max_scan: 20000,
            jobs_url: Some(base.to_string()),
        }
    }

    #[tokio::test]
    async fn a_full_census_finds_the_orphan_the_stall_and_the_dangling_edge() {
        let dock = station(
            "loading-dock",
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
        );

        // A: visible at the dock, moved this morning, edge resolves.
        let mut moved = step("build", StepStatus::Completed);
        moved.metadata = json!({"completed_at": "2026-08-13T09:00:00Z"});
        let carried = "cccccccc-1111-2222-3333-444444444444";
        let a = job("ship-a-change", 12).with_metadata(json!({"train": carried}));
        // B: visible, but nothing has moved since it opened, and its
        // backlog_item prefix answers to nothing.
        let b = job("ship-a-change", 1).with_metadata(json!({"backlog_item": "deadbeef"}));
        // C: no station claims it. Orphaned AND stalled.
        let c = job("user-feedback", 1);

        let open = envelope(vec![row(&a, vec![moved]), row(&b, vec![]), row(&c, vec![])]);
        // The id scan sees the open three plus the closed train A
        // points at — so A's edge resolves and B's prefix does not.
        let mut train = job("pr-train", 5);
        train.id = JobId::from_uuid(carried.parse().expect("uuid"));
        let all = envelope(vec![
            row(&a, vec![]),
            row(&b, vec![]),
            row(&c, vec![]),
            row(&train, vec![]),
        ]);
        let stations_body = envelope(vec![serde_json::to_value(&dock).unwrap()]);
        let edge_defs = serde_json::to_value(edges()).unwrap();

        let handler: Handler = Arc::new(move |target: &str| {
            let body = if target == "/api/stations" {
                stations_body.clone()
            } else if target.starts_with("/api/jobs/summary") {
                if target.contains("status=open") {
                    json!({"counts": {"ship-a-change": 2, "user-feedback": 1}, "total": 3})
                } else if target.contains("status=blocked") {
                    json!({"counts": {"ship-a-change": 1}, "total": 1})
                } else if target.contains("status=closed") {
                    json!({"counts": {"pr-train": 1}, "total": 1})
                } else {
                    json!({"counts": {}, "total": 0})
                }
            } else if target == "/api/jobs/job-edges" {
                edge_defs.clone()
            } else if target.contains("status=open") {
                open.clone()
            } else if target.starts_with("/api/jobs?") {
                all.clone()
            } else {
                return (404, json!({"error": "not found"}));
            };
            (200, body)
        });

        let (base, hits) = spawn_stub(handler).await;
        let census = collect(opts(&base), at(13, 18)).await.expect("collects");

        // Conservation over space: exactly one packet no station claims.
        let orphans = &census.conservation_over_space.orphans;
        assert_eq!(orphans.len(), 1, "one orphan");
        assert_eq!(orphans[0].kind, "user-feedback");
        assert_eq!(orphans[0].id, c.id.to_string());
        assert_eq!(census.conservation_over_space.open_evaluated, 3);
        assert_eq!(census.conservation_over_space.active_stations, 1);
        assert_eq!(
            census.conservation_over_space.station_depth[0].packets, 2,
            "the two ship-a-change packets sit at the dock"
        );

        // Conservation over time: A moved today, B and C never did.
        let t = &census.conservation_over_time;
        assert_eq!(t.stale, 2);
        assert_eq!(t.open, 3);
        assert_eq!(t.other_live, 1, "the blocked packet from the summary");
        assert_eq!(t.terminal, 1);
        assert_eq!(t.age_histogram.iter().map(|b| b.packets).sum::<usize>(), 3);

        // Edge integrity: A's full-id train resolves off the scan,
        // B's prefix answers to nothing.
        let e = &census.edge_integrity;
        assert_eq!(e.refs_checked, 2);
        assert_eq!(e.dangling.len(), 1);
        assert_eq!(e.dangling[0].field, "backlog_item");
        assert_eq!(e.dangling[0].value, "deadbeef");
        assert_eq!(e.dangling[0].resolution, Resolution::Dangling);
        assert!(e.unknown.is_empty());

        // Cost, and the summary arithmetic over the whole run.
        assert!(census.cost.id_scan_complete);
        assert_eq!(census.cost.ids_scanned, 4);
        assert_eq!(census.cost.id_probes, 0);
        assert!(!census.cost.open_truncated);
        assert_eq!(
            census.cost.api_calls,
            hits.lock().unwrap().len(),
            "the reported cost is the calls actually made"
        );
        assert_eq!(
            census.summary,
            "census: 3 open, 2 stale (>7d), 1 orphaned, 1 dangling edges"
        );
        // The text an operator would actually be handed, end to end.
        let out = render(&census);
        assert!(out.contains("ORPHANS (0 stations matched) — 1"));
        assert!(out.ends_with(&census.summary));
    }

    #[tokio::test]
    async fn full_id_referents_are_probed_one_by_one_instead_of_scanning() {
        // No prefix references, so the expensive id scan is not worth
        // paying for: a 404 on the id itself is proof of absence.
        let gone = "eeeeeeee-1111-2222-3333-444444444444";
        let a = job("ship-a-change", 12).with_metadata(json!({"train": gone}));
        let open = envelope(vec![row(&a, vec![])]);
        let edge_defs = serde_json::to_value(edges()).unwrap();

        let handler: Handler = Arc::new(move |target: &str| {
            if target == "/api/stations" {
                (200, envelope(vec![]))
            } else if target.starts_with("/api/jobs/summary") {
                (200, json!({"counts": {}, "total": 0}))
            } else if target == "/api/jobs/job-edges" {
                (200, edge_defs.clone())
            } else if target.contains("status=open") {
                (200, open.clone())
            } else {
                // The probe: nothing answers to that id.
                (404, json!({"error": "no such job"}))
            }
        });

        let (base, hits) = spawn_stub(handler).await;
        let census = collect(opts(&base), at(13, 18)).await.expect("collects");

        assert_eq!(census.cost.id_probes, 1);
        assert_eq!(census.cost.ids_scanned, 0, "no scan was needed");
        assert_eq!(census.edge_integrity.dangling.len(), 1);
        assert_eq!(census.edge_integrity.dangling[0].value, gone);
        assert!(
            hits.lock()
                .unwrap()
                .iter()
                .any(|h| h == &format!("/api/jobs/{gone}")),
            "the probe went to the id itself"
        );
        // No active stations: every open packet reads as orphaned, and
        // the report says why rather than leaving it as a finding.
        assert_eq!(census.conservation_over_space.orphans.len(), 1);
        assert!(
            census
                .notes
                .iter()
                .any(|n| n.contains("no active stations"))
        );
    }

    #[tokio::test]
    async fn a_prefix_reference_is_unknown_not_dangling_when_the_scan_is_capped() {
        let b = job("ship-a-change", 1).with_metadata(json!({"backlog_item": "deadbeef"}));
        let open = envelope(vec![row(&b, vec![])]);
        let edge_defs = serde_json::to_value(edges()).unwrap();

        let handler: Handler = Arc::new(move |target: &str| {
            if target == "/api/stations" {
                (200, envelope(vec![]))
            } else if target.starts_with("/api/jobs/summary") {
                (200, json!({"counts": {}, "total": 0}))
            } else if target == "/api/jobs/job-edges" {
                (200, edge_defs.clone())
            } else if target.contains("status=open") {
                (200, open.clone())
            } else {
                (200, envelope(vec![]))
            }
        });

        let (base, _) = spawn_stub(handler).await;
        let mut o = opts(&base);
        o.max_scan = 0;
        let census = collect(o, at(13, 18)).await.expect("collects");

        assert!(census.edge_integrity.dangling.is_empty(), "no false claim");
        assert_eq!(census.edge_integrity.unknown.len(), 1);
        assert_eq!(census.cost.ids_scanned, 0);
        assert!(
            census
                .notes
                .iter()
                .any(|n| n.contains("--max-scan 0 disabled the scan")),
            "a bound that changes the answer has to say so: {:?}",
            census.notes
        );
    }

    #[test]
    fn the_json_view_carries_the_same_findings_as_the_text_view() {
        let c = sample_census();
        let v = serde_json::to_value(&c).expect("serialises");
        assert_eq!(v["summary"], c.summary);
        assert_eq!(
            v["conservation_over_space"]["orphans"][0]["kind"],
            "user-feedback"
        );
        assert_eq!(v["edge_integrity"]["dangling"][0]["resolution"], "dangling");
        assert_eq!(
            v["conservation_over_time"]["stale_packets"][0]["motion_basis"],
            "opened-on"
        );
        assert_eq!(v["cost"]["api_calls"], 9);
    }
}
