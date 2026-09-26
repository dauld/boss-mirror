//! Hexagonal port: `JobsRepository` defines what the domain needs from
//! persistence. Adapters (in-memory for tests, Postgres for prod)
//! implement this trait.

use async_trait::async_trait;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus};
use boss_core::partition::Partition;
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum JobsError {
    #[error("job not found: {0}")]
    NotFound(JobId),
    #[error("step not found: {0}")]
    StepNotFound(StepId),
    #[error("storage failure: {0}")]
    Storage(String),
    /// A claim lost its race (or targeted an unclaimable step). The
    /// loser learns the current holder and status so the queue lens
    /// can say "taken by X" instead of failing blankly.
    #[error("step not claimable: held by {holder:?}, status {status}")]
    ClaimConflict {
        holder: Option<String>,
        status: String,
    },
    /// A metadata merge targeted a terminal (completed/skipped) step.
    /// `update_step_at` freezes those fields SILENTLY — its callers
    /// re-send whole rows and must stay idempotent — but a merge's
    /// entire purpose is changing metadata, so a freeze here would be
    /// the 204-that-wrote-nothing defect (job 903e6b90) reborn. The
    /// adapters refuse instead, atomically with the row check, and the
    /// handler turns this into the 409 the caller can act on.
    ///
    /// The judged whole-row write refuses with it too: a write over a
    /// terminal row it read afresh that would move a column the row
    /// freezes ([`terminal_write_moves_frozen`], backlog 6ec22d71).
    #[error("step {id} is {status} — what a terminal step recorded is frozen")]
    TerminalStep { id: StepId, status: String },
    /// A whole-row job write would move a finished (closed or
    /// cancelled) packet's status. The job PUT judges the row it READ;
    /// a close that commits between that read and this write is only
    /// visible here, so the adapters refuse atomically with the row
    /// check rather than write a stale open copy over the close
    /// (backlog 570e72bd, road 5).
    #[error("job {id} is {status} — a finished packet's status does not move")]
    TerminalJob { id: JobId, status: String },
    /// A whole-row step write was computed from a read of a version the
    /// row no longer is: another writer (the merge door, a claim, a
    /// completion) wrote it between that read and this write. Written,
    /// the stale copy would erase the other write while this one
    /// answered success — measured on run 6b6fe011, 2026-09-25 (backlog
    /// e381689d), and judged on every column since backlog 6ec22d71.
    /// Refused atomically with the row check instead; the caller
    /// re-reads and re-sends.
    #[error(
        "step {id} changed since this write read it — the row is no longer the version the write was computed from"
    )]
    StepChanged { id: StepId },
    /// A sign-off stamp attests a shape the row no longer has: the
    /// sign-off door built it over the step it READ, and a write moved
    /// the row before the append took the lock (backlog 4174c4a9, the
    /// review of car e1a62aa5). Written, it would sit alive on the
    /// wrong shape — no edit ever voided it, because it landed after
    /// the edit — and a later write that put the row back on the signed
    /// shape (a claim dropping the run edge) would make it count.
    /// Refused atomically with the row lock instead; the approver reads
    /// the step again and signs what is there.
    #[error(
        "step {id} moved since the stamp was built — it signs shape {signed}, the step is {current}"
    )]
    StampOffShape {
        id: StepId,
        signed: String,
        current: String,
    },
}

/// The version a step row was at when it was read — the judgement a
/// read-modify-write step writer's write is held to
/// ([`JobsRepository::update_step_if_unchanged_at`], backlog 6ec22d71).
///
/// EVERY write to the row moves it, by any writer and whatever column
/// it touched, and a read never does. That is the whole contract, and
/// it is why this is a version and not a value: car 88123ae0 judged
/// the write on the metadata it read, so a claim — status and holder,
/// no metadata — was erased by an assignment PUT computed a moment
/// before it, and a metadata number beyond f64 precision, written by
/// hand, never compared equal to the reader's parsed copy and wedged
/// the step.
///
/// The Pg adapter's is the row's `xmin`, the id of the transaction that
/// last wrote it: no column to maintain, no writer that can forget to
/// bump it, and nothing a rebuild has to reproduce — it is a
/// concurrency token, never state, so it is in no event and no
/// projection, and a read taken before a rebuild is simply refused
/// after it. `updated_at` was the alternative and is not one: it is the
/// write's own `now`, which the simulation clock can hold still, so two
/// writes can share it. The in-memory adapter's is a counter bumped by
/// every write under its lock. Opaque to callers — compare, never
/// compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepVersion(i64);

impl StepVersion {
    pub(crate) fn new(raw: i64) -> Self {
        Self(raw)
    }
}

/// PURE: whether writing `write` over the TERMINAL row `row` would move
/// a column the row freezes — the columns the whole-row write keeps
/// under its terminal CASE (Pg) or copies back (in-memory): status,
/// completion date, who and when, metadata, the authored fields, and
/// what was completed (title, holder, notes; backlog 42e7c6b9).
///
/// A write that would is refused rather than written (backlog
/// 6ec22d71): the row would keep its values, but the write's
/// `jobs.step.updated` carries the write's, and `rebuild.rs`
/// replays that event verbatim — so the rebuilt row would differ from
/// the live one. One definition for both adapters (CLAUDE.md §9a).
/// Compared on the parsed `Step` both sides hold, so a value the
/// reader's copy cannot represent exactly (a number beyond f64) does
/// not read as a change: the caller's copy of a terminal row came from
/// the same parse.
pub fn terminal_write_moves_frozen(row: &Step, write: &Step) -> bool {
    row.status != write.status
        || row.completed_on != write.completed_on
        || row.completed_by != write.completed_by
        || row.completed_at != write.completed_at
        || row.metadata != write.metadata
        || row.fields != write.fields
        || row.title != write.title
        || row.assignee_id != write.assignee_id
        || row.notes != write.notes
}

/// Optional filters for listing jobs.
#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub kind: Option<String>,
    /// Prefix match on `kind`. Used by the UI for nav buckets that
    /// span related registry kinds (e.g. `refurb-used` + `refurb-oem-new`
    /// both match `kind_prefix = "refurb"`).
    pub kind_prefix: Option<String>,
    /// Keep only packets whose `kind` is IN this set — and `Some(vec![])`
    /// keeps NOTHING. Born as the department listing's filter (backlog
    /// cc76f755, 2026-09-18), which has its own field now
    /// (`department`); the regions read's inbound kind set still asks
    /// for exactly a set of kinds. An empty set answering the
    /// unfiltered count would be the trap cc76f755 was filed on —
    /// measured on prod, `?department=sales` answered 1944, the
    /// unfiltered total, because nothing read the parameter at all.
    pub kinds: Option<Vec<String>>,
    /// Keep only the packets IN one department — the `?department=`
    /// listing's filter. See [`DepartmentFilter`] for which packets
    /// that is; `None` is no filter.
    pub department: Option<DepartmentFilter>,
    pub status: Option<JobStatus>,
    /// A retention window on TERMINAL packets: keep everything still
    /// live, plus anything closed on or after this date. Drop
    /// everything closed before it.
    ///
    /// A board renders a card in the column of its current step, so
    /// terminal packets have to be fetched to appear in terminal
    /// columns — and the feedback board was fetching all 173
    /// user-feedback packets to show 14 live ones, 92% of it finished
    /// work, 27 packets away from silently truncating at its
    /// `limit=200`. Filtering after the fetch does not fix that; the
    /// window has to be in the query.
    ///
    /// Same idea as `stations.md`'s `terminal_window_days`, which
    /// `my-watchlist` sets to 14 so a filer can still see an outcome.
    /// Combines with `status` as OR, not AND: `status = open` plus a
    /// window means "live OR recently closed", which is the useful
    /// question and the only one a board asks.
    pub closed_since: Option<chrono::NaiveDate>,
    pub priority: Option<Priority>,
    pub owner_id: Option<String>,
    /// Filter by subject reference (e.g., device serial, account id).
    pub subject_id: Option<String>,
    /// Only jobs waiting on this Job (its full id): matches a
    /// `metadata.waiting_on` holding the full id or a >= 8-char
    /// prefix of it — the same resolution contract as
    /// `job_edge_resolves`. The clear-on-close handler's query.
    pub waiting_on: Option<String>,
    /// Jobs whose `metadata` CONTAINS this document — the JSONB
    /// containment shape (`metadata @> $1`), so a station predicate's
    /// `metadata_equals` clause narrows in SQL instead of after the
    /// page is drawn. A per-actor station is the case that needs it: a
    /// watchlist filtered only in memory would page through the whole
    /// company's newest packets to find one person's.
    ///
    /// Flat string-valued objects only — that is the whole of what
    /// `metadata_equals` expresses.
    pub metadata_contains: Option<serde_json::Value>,
    /// Jobs whose `metadata` carries this top-level key, whatever its
    /// value — the JSONB existence shape (`metadata ? $n`). The
    /// question a probe asks most: "the alerts carrying
    /// `estate_finding`", "the cars with a `proof_probe`". Before this
    /// existed (4d9aa761, 2026-09-14) every such reader paged 60-200
    /// rows and filtered in jq, exact only while the page was bigger
    /// than the world; one measured 356 closed rows in 14 days against
    /// a 200-row page.
    ///
    /// Top-level keys only: `?` does not walk paths, and the HTTP layer
    /// refuses anything that is not a plain identifier so a dotted key
    /// cannot silently match nothing.
    pub metadata_has: Option<String>,
    /// Row-level policy scope — translated from `boss_policy_client::Predicate`
    /// by the HTTP handler before calling the adapter. Pushing it down
    /// into SQL here means scoped roles get accurate `total` counts
    /// and pages that only contain jobs they can see (no wasted page
    /// space on rows the post-fetch filter would discard).
    pub scope: JobScope,
    /// Keep only the packets of ONE partition — `Some(Real)`,
    /// `Some(Simulated)` or `Some(Shadow)`. `None` — the default — is
    /// every packet, which is what every existing caller already gets.
    /// There is deliberately no "not real" filter: a real-lane surface
    /// asks for `Real` and so excludes shadow packets without knowing
    /// the word, and the sim asks for `Simulated` and never sees them
    /// (packet 508cc38c, Q5).
    ///
    /// WHY IT IS A QUERY FILTER AND NOT A CLIENT-SIDE `.filter()`, for
    /// exactly the reason `closed_since` above is: measured
    /// 2026-08-17, **5,201 of 5,964 packets (87%) are simulated**, and
    /// a page of 200 drawn from that population holds roughly 26 real
    /// ones. A surface that fetches a page and then discards the
    /// simulated rows shows a nearly empty list, a wrong `total`, and
    /// silently truncates — the same failure the retention window was
    /// added to fix, one order of magnitude worse.
    ///
    /// The partition is set at admission and immutable afterwards
    /// (`update_job` restores it from the existing row), so this is a
    /// stable partition rather than a mutable label. Measured on the
    /// same population: of 39 kinds, **zero are mixed** — a kind is
    /// either entirely simulated or entirely real — so filtering here
    /// never splits a kind's packets across two answers.
    pub partition: Option<Partition>,
}

/// Which packets are IN a department — `GET /api/jobs?department=`.
///
/// A department is declared as data in two places, and a packet is in
/// the one its OWN `metadata.department` names, or — when it names
/// none — the one its kind's active workflow row declares
/// (`crate::department::carried`, the same rule for both). One packet,
/// one department: the packet's word is the more specific, so it wins.
///
/// Why the packet's word counts at all (backlog 481d7939, measured
/// 2026-09-23): the kinds every department has — `department-retro`,
/// `page-audit`, the `backlog-item`s a page audit files — are platform
/// rows that declare no department, because they serve all of them;
/// each packet carries the department it is about, and its schema
/// requires it. Joined over kinds alone, the warehouse's retro and two
/// page-audits answered `?department=warehouse` with total 0, and the
/// finance retro was absent from finance's view. Membership stays data
/// — the handler resolves `declaring_kinds` from the registry and the
/// packet carries its own word — never a list of kinds in code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepartmentFilter {
    /// The department code asked for.
    pub code: String,
    /// The kinds whose ACTIVE workflow row declares `code`
    /// (`crate::department::kinds_declaring`). Empty is a real answer —
    /// no kind declares it — and then only packets naming it match.
    pub declaring_kinds: Vec<String>,
}

impl DepartmentFilter {
    /// Whether a packet of `kind` carrying `metadata` is in this
    /// department. The Postgres adapter spells the same rule as a
    /// `CASE` over the same two sources; each is pinned by a test.
    pub fn keeps(&self, kind: &str, metadata: &serde_json::Value) -> bool {
        match crate::department::carried(metadata) {
            Some(own) => own == self.code,
            None => self.declaring_kinds.iter().any(|k| k == kind),
        }
    }
}

/// The policy-scope slice applied to a listing. Mirrors the shapes
/// of `boss_policy_client::Predicate` that translate cleanly to SQL;
/// `DepartmentIs` is absent because Jobs don't carry a department
/// column (the HTTP handler still handles that case as an all-or-nothing
/// pre-check since the answer only depends on the caller's department).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum JobScope {
    /// No additional constraint. Adapter applies only the usual filter
    /// fields.
    #[default]
    All,
    /// Short-circuit to an empty result set (policy says the caller
    /// can see nothing).
    None,
    /// Only jobs where `owner_id = user_id`.
    OwnerIs(String),
    /// Only jobs where `owner_id IN (ids)` (user + direct reports).
    OwnerIn(Vec<String>),
    /// Only jobs whose subject references a account_id in the
    /// provided list. Matches both `Subject::Account { id }` and
    /// `Subject::Employee { id }` — the policy convention treats
    /// an employee's account_id bucket the same as a account row.
    AccountIn(Vec<String>),
}

/// One cohort's block in the per-kind terminal report — Tier 1 of
/// the experiments program (docs/design/network-experiments.md):
/// measure what version pinning already records. The version
/// dimension is the packet's PINNED `workflow_version`, so the report
/// compares protocol variants by what actually ran each packet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VersionTerminalReport {
    pub version: i32,
    /// The arm dimension (Tier 2, packet 6ea5a12a): the
    /// `experiment_arm` stamp split admission writes on every packet
    /// it admits (`control` / `candidate`), `None` for packets that
    /// ran outside any experiment window. Grouped alongside the
    /// version so a cohort never blends with same-version bystanders.
    pub arm: Option<String>,
    /// Every packet pinned to this version (any status).
    pub total: i64,
    /// Packet count per status — the six job statuses, zero-count
    /// statuses omitted.
    pub by_status: std::collections::BTreeMap<String, i64>,
    /// Outcome distribution over CLOSED packets: `metadata.outcome`
    /// value → count. Cancelled packets are terminal but not closed,
    /// so they stay out of the measurement.
    pub outcomes: std::collections::BTreeMap<String, i64>,
    /// Closed packets that declared no outcome (the catch-all close).
    /// Counted separately rather than under a sentinel key so a
    /// machine reading `outcomes` only ever sees real outcome values.
    pub closed_without_outcome: i64,
    /// Open→close cycle time over closed packets, in days. Fractional
    /// when the packet carries the precise `opened_at` / `closed_at`
    /// metadata stamps; otherwise whole days from the row's dates
    /// (`opened_on` / `closed_on` — both reproduced by the rebuilder).
    pub cycle_time_days: CycleTimeDays,
}

/// Median + p90 with the sample count they were computed over. A
/// closed packet without a `closed_on` date (or a pair of precise
/// metadata stamps) is not a sample, which is why `samples` can
/// undercut `by_status["closed"]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CycleTimeDays {
    pub samples: i64,
    pub median: Option<f64>,
    pub p90: Option<f64>,
}

/// `percentile_cont` over an already-sorted slice — the same
/// continuous-percentile arithmetic Postgres runs, mirrored here so
/// the default (in-memory) report and the SQL override agree to the
/// bit-level formula, not just approximately.
fn percentile_cont(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rn = p * (sorted.len() - 1) as f64;
    let frn = rn.floor();
    let crn = rn.ceil();
    let lo = sorted[frn as usize];
    if frn == crn {
        return Some(lo);
    }
    let hi = sorted[crn as usize];
    Some((crn - rn) * lo + (rn - frn) * hi)
}

/// The status's public wire string — derived from the one serde
/// definition on [`JobStatus`] rather than a third hand-written
/// match (postgres.rs and http/jobs.rs already carry two).
fn job_status_key(status: JobStatus) -> String {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(s)) => s,
        _ => format!("{status:?}"),
    }
}

/// The outcome as `metadata->>'outcome'` would read it: absent or
/// JSON null is no outcome; a string is itself; any other JSON value
/// is its text.
fn outcome_key(metadata: &serde_json::Value) -> Option<String> {
    match metadata.get("outcome") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// The packet's cycle-time sample, in fractional days. The precise
/// `opened_at` / `closed_at` metadata stamps (RFC3339 instants,
/// written at admission and at the close hooks) win when both parse —
/// the row's dates have one-day resolution by construction, so a
/// same-day close measured 0 days no matter how long it really took.
/// Packets that predate the stamps, or carry only one, keep the
/// `closed_on - opened_on` date arithmetic. Mirrors the SQL override's
/// `EXTRACT(EPOCH ...) / 86400.0` COALESCEd to the date form
/// (postgres.rs), pinned by tests/terminal_report_pg.rs.
fn cycle_days_sample(job: &Job) -> Option<f64> {
    let stamp = |key: &str| -> Option<chrono::DateTime<chrono::FixedOffset>> {
        chrono::DateTime::parse_from_rfc3339(job.metadata.get(key)?.as_str()?).ok()
    };
    if let (Some(opened), Some(closed)) = (stamp("opened_at"), stamp("closed_at"))
        && let Some(us) = (closed - opened).num_microseconds()
    {
        return Some(us as f64 / 86_400_000_000.0);
    }
    job.closed_on
        .map(|closed| (closed - job.opened_on).num_days() as f64)
}

/// Pure selection behind [`JobsRepository::newest_closed_job`]: the
/// closed packet with the greatest `closed_on`, then the greatest
/// `opened_on`. `jobs` arrives newest-opened first from `list_jobs`,
/// and a stable max keeps that order as the last tie-break.
pub fn newest_closed_from_jobs(jobs: Vec<Job>) -> Option<Job> {
    jobs.into_iter()
        .filter(|j| j.status == JobStatus::Closed)
        .fold(None, |best: Option<Job>, j| match best {
            Some(b) if (b.closed_on, b.opened_on) >= (j.closed_on, j.opened_on) => Some(b),
            _ => Some(j),
        })
}

/// Pure aggregation behind [`JobsRepository::workflow_terminal_report`]
/// — a function of the packets, so any adapter's answer is checkable
/// against it. Versions sort newest first.
pub fn terminal_report_from_jobs(
    jobs: &[Job],
    since: Option<chrono::NaiveDate>,
) -> Vec<VersionTerminalReport> {
    use std::collections::BTreeMap;

    struct Acc {
        total: i64,
        by_status: BTreeMap<String, i64>,
        outcomes: BTreeMap<String, i64>,
        closed_without_outcome: i64,
        cycle_days: Vec<f64>,
    }

    // Cohort key: (pinned version, experiment_arm stamp). The arm is
    // the second axis so a BTreeMap's reverse iteration yields
    // version-desc, and within a version the stamped cohorts before
    // the unstamped bystanders (`None` sorts below `Some` and last
    // after `.rev()`) — matching the Postgres override's
    // `ORDER BY workflow_version DESC, arm DESC NULLS LAST`.
    let mut per_version: BTreeMap<(i32, Option<String>), Acc> = BTreeMap::new();
    for job in jobs {
        if let Some(since) = since
            && job.opened_on < since
        {
            continue;
        }
        let arm = job
            .metadata
            .get(crate::experiments::ARM_KEY)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let acc = per_version
            .entry((job.workflow_version, arm))
            .or_insert(Acc {
                total: 0,
                by_status: BTreeMap::new(),
                outcomes: BTreeMap::new(),
                closed_without_outcome: 0,
                cycle_days: Vec::new(),
            });
        acc.total += 1;
        *acc.by_status.entry(job_status_key(job.status)).or_insert(0) += 1;
        if job.status == JobStatus::Closed {
            match outcome_key(&job.metadata) {
                Some(outcome) => *acc.outcomes.entry(outcome).or_insert(0) += 1,
                None => acc.closed_without_outcome += 1,
            }
            if let Some(days) = cycle_days_sample(job) {
                acc.cycle_days.push(days);
            }
        }
    }

    per_version
        .into_iter()
        .rev()
        .map(|((version, arm), mut acc)| {
            acc.cycle_days
                .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            VersionTerminalReport {
                version,
                arm,
                total: acc.total,
                by_status: acc.by_status,
                outcomes: acc.outcomes,
                closed_without_outcome: acc.closed_without_outcome,
                cycle_time_days: CycleTimeDays {
                    samples: acc.cycle_days.len() as i64,
                    median: percentile_cont(&acc.cycle_days, 0.5),
                    p90: percentile_cont(&acc.cycle_days, 0.9),
                },
            }
        })
        .collect()
}

/// One open, workable step surfaced to an executor's "My Day" pull
/// query — the step plus the minimum Job context the caller needs to
/// act on it without a second fetch. Returned by
/// [`JobsRepository::list_assignments`]; consumed by the SPA My Day
/// surface and the sim's workforce loop.
#[derive(Debug, Clone, Serialize)]
pub struct AssignmentRow {
    pub job_id: JobId,
    /// The envelope's identity, so a queue lens can name the packet
    /// without a second fetch.
    pub job_title: String,
    pub due_on: Option<chrono::NaiveDate>,
    /// The day the packet was admitted, so a personal queue can say
    /// how long each step has waited without a second fetch — the
    /// same argument as `job_title`. `boss orient`'s MY WORK section
    /// is the first reader (65a89769: 25 ready steps sat on the
    /// agent's alias unseen, and a list with no age hides which of
    /// them has been waiting a week).
    pub opened_on: chrono::NaiveDate,
    pub workflow: String,
    /// The protocol version this packet was admitted under. Rides on
    /// the row so an executor can resolve the step's spec (its
    /// spec-authored `duration_hours`, for one) against the exact
    /// Workflow row the Job is pinned to, without a second fetch.
    pub workflow_version: i32,
    pub subject_kind: String,
    pub subject_id: String,
    pub priority: Priority,
    /// The Job's admission-fixed partition, and its tags. A
    /// projection, not the Job — but a queue lens renders a packet
    /// card from the row alone, and a simulated packet has to look
    /// simulated in a personal queue exactly as it does in the yard.
    /// On the wire this is `partition` plus the legacy `simulated`
    /// bool (derived as not-real — `boss_core::partition::wire`), so
    /// the sim workforce's fail-closed read of the row keeps working
    /// and a shadow packet reads as not-real to it.
    /// `tags` rides along for the same reason: the shared card
    /// predicate falls back to a `sim` / `simulated` / `synthetic` tag
    /// for packets that predate the column (there was no backfill), so
    /// without it the two lenses would disagree on the same packet.
    #[serde(flatten, with = "boss_core::partition::wire")]
    pub partition: Partition,
    pub tags: Vec<String>,
    /// How many red trains have released this car — the conductor's
    /// `red_trains` stamp on the job's metadata, absent = 0. The row
    /// carries the packet's identity and no metadata, so until
    /// d6e53a35 (2026-09-14) a builder's own struck car read clean on
    /// their My Day while the yard drew the same car struck — and the
    /// builder is the one who can act on a strike before the next red
    /// holds the car out. `default` so a row from an older server reads
    /// back as 0 rather than failing to parse, as [`crate::yard::DockCar`]
    /// does.
    #[serde(default)]
    pub red_trains: u32,
    pub step: Step,
}

/// One outstanding obligation — a `ready` or `active` step on an
/// `open` packet — and the instant it has been waiting since. The row
/// type of the queue-age lens (`GET /api/jobs/queue-age`, packet
/// 2a0b034e).
///
/// A LENS, NOT A FIELD. The wait instant lives one layer below the
/// domain types (`steps.became_ready_at` / `steps.updated_at` in
/// Postgres; the write-instant maps in the in-memory adapter), and
/// hoisting timestamps onto `Job` / `Step` was measured at a
/// 97-struct-literal-site mechanical change to Tier-1 core — so the
/// lens returns its own shape and the domain types stay untouched,
/// the same trade [`VersionTerminalReport`] made.
///
/// `since` semantics, stated rather than implied: when the projection
/// recorded the ready flip (`became_ready_at`, written once, never
/// moved by later writes) `exact` is `true` and `since` IS the moment
/// the step became an obligation. For rows that predate the stamp,
/// `since` falls back to `updated_at` and `exact` is `false`: any
/// write bumps `updated_at` — annotating a packet is enough
/// (2a77e5fc) — so `now - since` is then a LOWER BOUND on the wait.
/// A lower bound still sorts a queue by staleness; it just may
/// under-report, never over-report, a labelled direction.
#[derive(Debug, Clone, Serialize)]
pub struct QueueAgeRow {
    pub job_id: JobId,
    /// Protocol + title, so a reader can name the packet without a
    /// second fetch — a bare UUID is not an answer.
    pub job_kind: String,
    pub job_title: String,
    pub step_id: StepId,
    pub spec_slug: Option<String>,
    pub step_title: String,
    pub status: StepStatus,
    pub assignee_id: Option<String>,
    /// Rides along for the same reason it rides on [`AssignmentRow`]:
    /// a simulated packet has to look simulated in every lens.
    #[serde(flatten, with = "boss_core::partition::wire")]
    pub partition: Partition,
    /// The instant this obligation has been waiting since.
    pub since: DateTime<Utc>,
    /// `true` when `since` is the recorded ready-flip instant;
    /// `false` when it is the `updated_at` lower bound.
    pub exact: bool,
}

/// A machine BOSS runs on, as the estate registry declares it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EstateNode {
    pub id: String,
    pub label: String,
    pub address: String,
    /// The PRIMARY role — what the estate page keys on (`bastion` is
    /// the jump host). One value, from `nodes.role`.
    pub role: String,
    /// Every role the node DECLARES, sorted — Classes of `node` joined
    /// through `node_roles` (202609120300). The set a managed host
    /// derives its unit roster from; empty when the node declares
    /// nothing, which the converge reads as "install every row".
    #[serde(default)]
    pub roles: Vec<String>,
    pub cpu: Option<i32>,
    pub memory_gb: Option<i32>,
    pub disk_gb: Option<i32>,
    pub notes: Option<String>,
    /// Retired machines stay readable so history resolves, exactly as
    /// retired subject kinds do.
    pub retired: bool,
}

/// A machine as the tree DECLARES it (`[[node]]` in
/// infra/estate/estate.toml) and as `POST /api/estate/nodes/batch`
/// takes it — the columns 144-estate-subjects.sql gave `nodes`, plus
/// the roles 202609120300 gave `node_roles`. Declared capacity only:
/// free space now is an observation and rides the log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EstateNodeInput {
    pub id: String,
    pub label: String,
    pub address: String,
    /// The PRIMARY role (`talos-control-plane`, `talos-worker`,
    /// `forge`, `bastion`) — what the estate page keys on and the
    /// comparison selects on (`talos-*` participates in the cluster
    /// compare).
    pub role: String,
    /// Every role the node declares — Classes of `node`, so an
    /// undeclared code is refused by the schema's FK, never invented.
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub cpu: Option<i32>,
    #[serde(default)]
    pub memory_gb: Option<i32>,
    #[serde(default)]
    pub disk_gb: Option<i32>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Why a node declaration is refused, named so the refusal says which
/// check failed; the same check runs at the file, the door and both
/// adapters.
pub fn validate_estate_node(n: &EstateNodeInput) -> Result<(), String> {
    let slug = |field: &str, v: &str| -> Result<(), String> {
        if v.is_empty() {
            return Err(format!("node {}: {field} is required", n.id));
        }
        if !v
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "node {}: {field} `{v}` is not kebab-case (lowercase, digits, hyphens)",
                n.id
            ));
        }
        Ok(())
    };
    if n.id.is_empty() {
        return Err("a node needs an id (e.g. cp-1)".into());
    }
    slug("id", &n.id)?;
    slug("role", &n.role)?;
    for r in &n.roles {
        slug("roles entry", r)?;
    }
    for (field, v) in [("label", &n.label), ("address", &n.address)] {
        if v.trim().is_empty() {
            return Err(format!("node {}: {field} is required", n.id));
        }
    }
    Ok(())
}

/// What `POST /api/estate/nodes/batch` takes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EstateNodeBatch {
    pub nodes: Vec<EstateNodeInput>,
}

/// What a declaration did: nodes received, nodes inserted, and roles
/// inserted (a role landed on a node already there counts here — the
/// forge gained `cluster-operator` after its row existed).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EstateBatchOutcome {
    pub received: usize,
    pub inserted: usize,
    pub roles_inserted: usize,
}

/// Which rows of one event kind [`JobsRepository::recent_events_by_kind`]
/// reads: an exact payload `scope`, and a half-open `[since, until)`
/// window on the event's timestamp — every filter applied where the
/// limit is. All absent reads the whole kind.
///
/// `latest_per` names a top-level payload key: set, the window answers
/// only the NEWEST row of each distinct value of that key (a row
/// without the key is one group of its own), grouped inside the window
/// and before the limit, and the page's `total` counts GROUPS. Backlog
/// 725532ab: `scope=host` still let forge's fifteen-minute rows spend
/// the whole page, so boss-gcp's daily comparison was unreadable about
/// half of every day; "the newest word from each host" is a question
/// about hosts, and only a read that groups by host answers it whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventWindow {
    pub scope: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub latest_per: Option<String>,
}

/// One page of an event series, newest first, as the raw rows
/// `{event_id, timestamp, source, kind, payload}`, and how many rows
/// its WINDOW holds — so `rows.len() < total` says there is more.
#[derive(Debug, Clone, PartialEq)]
pub struct EventPage {
    pub rows: Vec<serde_json::Value>,
    pub total: i64,
}

/// The fact one declaration leaves: `node.declared`, once per node the
/// batch changed (its row inserted, or a role landed on it), carrying
/// the declaration, what landed, and `declared_by` from the stamp.
pub const NODE_DECLARED: &str = "node.declared";

pub fn node_declared_event(
    stamp: &boss_core::publisher::EventStamp,
    node: &EstateNodeInput,
    inserted: bool,
    roles_inserted: &[String],
) -> Result<boss_core::event::Event, JobsError> {
    let mut payload = serde_json::to_value(node).map_err(|e| JobsError::Storage(e.to_string()))?;
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "declared_by".to_string(),
            serde_json::Value::String(stamp.actor().to_string()),
        );
        map.insert(
            "landed".to_string(),
            serde_json::json!({ "node": inserted, "roles": roles_inserted }),
        );
    }
    Ok(stamp.event(NODE_DECLARED, payload))
}

/// Persistence port for jobs and steps.
///
/// **Timestamp threading.** The four mutation methods come in two
/// flavors: a convenience overload (`create_job(&job)`) that stamps
/// `Utc::now()` server-side, and an `_at` variant
/// (`create_job_at(&job, now, events)`) that takes an explicit
/// timestamp. Handlers use the `_at` form so the projection write
/// and the audit_log event share one timestamp — required for the
/// audit_log → projection rebuild path to reproduce `created_at` /
/// `updated_at` exactly. See `docs/design/projection-rebuilders.md`.
///
/// **OUTBOX (phase 2) — events ride the write.** Every `_at` mutation
/// takes `events: &[boss_core::event::Event]` — the pre-built state
/// event(s) + marker event(s) describing the mutation — and records
/// them on the transactional outbox INSIDE the write transaction
/// (`boss_events::outbox::record_event_in_tx`); boss-event-relay
/// delivers to audit_log + NATS post-commit. The HANDLER keeps the
/// event-derivation logic (status-transition markers, `step.done` /
/// `step.ready` dispatcher signals, actor stamping); the adapter
/// guarantees fact + events commit or fail together. Creation paths
/// (`create_job_with_steps_at`, `add_step_at`) are ON CONFLICT DO
/// NOTHING replay-tolerant — their events record ONLY when the insert
/// actually inserted, so a replayed create records nothing (before,
/// every replay published duplicate created events). The convenience
/// overloads pass no events (test-path ergonomics).
#[async_trait]
pub trait JobsRepository: Send + Sync {
    // ----- Jobs -----

    async fn create_job(&self, job: &Job) -> Result<(), JobsError> {
        self.create_job_at(job, Utc::now(), &[]).await
    }

    /// A job with no steps of its own — every test that builds a bare
    /// job. (The HTTP `?materialize_steps=false` opt-out, whose sim
    /// caller posted its own steps afterwards, is refused since
    /// afbf4f73.) The one-transaction contract is
    /// [`JobsRepository::create_job_with_steps_at`]'s; this is that
    /// call with nothing to add.
    async fn create_job_at(
        &self,
        job: &Job,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        self.create_job_with_steps_at(job, &[], now, events, &[])
            .await
    }

    /// Admit a job WITH its materialized steps: the job row, every
    /// step row, the caller's job events and one STEP_CREATED per
    /// step commit in ONE transaction, or none of them do.
    ///
    /// This is the honest shape of admission (backlog f2ba226e). Until
    /// 2026-09-16 the handler committed the job through
    /// `create_job_at` and then wrote each step through `add_step_at`
    /// — ten transactions for a pr-train — warning on a failed step
    /// and answering 201 regardless. On a slow database (pr-train
    /// 06e5610f, 02:32Z) the client timed out, axum dropped the
    /// handler between step nine and step ten, and the terminal was
    /// never written: no error, a packet that could neither advance
    /// nor be cancelled, ninety minutes on the track. A dropped
    /// request now drops one open transaction, and the database
    /// rolls it back — the row a reader can see is always the whole
    /// graph.
    ///
    /// `step_events` is index-aligned with `steps` — one STEP_CREATED
    /// each, the event the rebuilder (`rebuild.rs`) reproduces the
    /// row from. A length mismatch is refused before any write: a
    /// short zip would record fewer events than rows and the replayed
    /// projection would hold fewer steps than the live one. The
    /// replay guard is per row, as before: a job or step whose id
    /// already exists inserts nothing and records nothing. Each step
    /// row is stamped with its plugin version by the rule
    /// [`JobsRepository::add_step_at`] states.
    ///
    /// A BIRTH-BY-JOB SUBJECT IS MINTED HERE, AND THAT IS
    /// ADAPTER-SCOPED (backlog 82448947). A subject kind whose
    /// SubjectKind row carries `metadata.birth = "job"` (`workflow`,
    /// `custom`) has no domain table: the job about it IS its birth
    /// record. The Postgres adapter mints its `subjects` identity row
    /// in the same transaction as the job insert, insert-if-absent;
    /// a domain kind, or a retired one, mints nothing. Pinned by
    /// `birth_by_workflows_pass_gate_and_create_mints_identity` in
    /// `tests/subject_existence_pg.rs`.
    ///
    /// The in-memory adapter does NOT implement this: it has no
    /// identity table to mint into, and this trait has no read of
    /// one, so the mint is invisible through the port and no port-level
    /// test can hold it — the Pg test is its only pin. What returns is
    /// the same either way. A new adapter that keeps subject
    /// identities must mint them here and say so.
    async fn create_job_with_steps_at(
        &self,
        job: &Job,
        steps: &[Step],
        now: DateTime<Utc>,
        job_events: &[boss_core::event::Event],
        step_events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError>;

    /// The packets `ids` name, in no particular order; an id that names
    /// no packet is simply absent from the answer, and a repeated id is
    /// one packet. The collection reads that cut their rows to a
    /// caller's scope judge every row's packet with it (backlog
    /// 046832d3). The default reads each one; the Postgres adapter
    /// overrides it with one `= ANY($1)` query.
    async fn get_jobs(&self, ids: &[JobId]) -> Result<Vec<Job>, JobsError> {
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            if let Some(job) = self.get_job(id).await? {
                out.push(job);
            }
        }
        Ok(out)
    }

    /// Resolve a lowercase hex id prefix to the ids it matches, capped
    /// at two — enough for the caller to tell none from one from many
    /// without scanning the whole table. The prefix is the canonical
    /// text form (`id::text`): lowercase, hyphenated, so a bare 8-char
    /// prefix and a hyphen-bearing longer one both match. The read
    /// handler owns the none→404 / one→200 / many→409 decision; the
    /// store only reports what matched.
    async fn resolve_job_id_prefix(&self, prefix: &str) -> Result<Vec<JobId>, JobsError>;

    async fn update_job(&self, job: &Job) -> Result<(), JobsError> {
        self.update_job_at(job, Utc::now(), &[]).await
    }

    /// Replace the Job's row with `job`, recording `events` with it.
    ///
    /// A compare-and-set on a FINISHED status: a row stored Closed or
    /// Cancelled keeps it, and a write whose `status` differs is refused
    /// as [`JobsError::TerminalJob`] with nothing written or recorded. A
    /// write that keeps the finished status (a retitle after the close)
    /// lands. The job PUT judged the row it read; only the store sees a
    /// close that committed after that read (backlog 570e72bd).
    async fn update_job_at(
        &self,
        job: &Job,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    /// Merge `patch`'s top-level keys into the Job's `metadata`,
    /// atomically, touching no envelope field. A `null` value REMOVES
    /// the key (the conductor's `overlay_metadata` convention); any
    /// other value replaces that key wholesale. Returns the post-merge
    /// Job.
    ///
    /// This is the server-side home of the read-modify-write every
    /// metadata-merging caller used to run client-side through the
    /// full-replacement job PUT — a race over the ENVELOPE: a packet
    /// closed (status + `metadata.outcome` stamped) between the GET
    /// and the PUT came back open with its outcome erased, on the
    /// system of record.
    ///
    /// Unlike the other `_at` mutations this takes the
    /// [`boss_core::publisher::EventStamp`] rather than pre-built
    /// events: the JOB_UPDATED payload is full
    /// row state (what the rebuild consumes), so it must be built from
    /// the POST-merge row, which only the adapter's transaction knows.
    /// Same precedent as the workflow registry's `publish_authored`
    /// recording WORKFLOW_PUBLISHED beside the row it describes. The
    /// stamp's `timestamp` is the write's timestamp.
    async fn merge_job_metadata_at(
        &self,
        id: &JobId,
        patch: &serde_json::Map<String, serde_json::Value>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError>;

    /// Close the Job, writing ONLY the fields a close owns: `status`
    /// becomes `closed`, `closed_on` is set, and `owned`'s top-level
    /// keys (`closed_at`, and `outcome` when the close names one) merge
    /// into `metadata` against the row as it stands. Every other key
    /// and every other envelope field is left exactly as the row holds
    /// it at write time.
    ///
    /// It is also a compare-and-set on the status: only an OPEN row
    /// closes. A row already Closed (another closer won), Cancelled or
    /// Draft is left untouched and the answer is `Ok(None)` — nothing
    /// written, nothing recorded. `Some` carries the post-close row.
    ///
    /// WHY (backlog 29a7ea09): the two closers of a step write — the
    /// declared-terminal close and the all-steps-terminal catch-all —
    /// were each GET → mutate → whole-row `update_job_at`. Measured on
    /// car 6b23d135 at 2026-09-24T22:18:10Z: the terminal close wrote
    /// `outcome=disproved`, then the catch-all, holding a copy read
    /// before that write committed, wrote its whole row back and the
    /// outcome was gone. Any key another writer merged between a
    /// closer's read and its write was lost the same way, silently — a
    /// conservation break in the system of record.
    ///
    /// Records, in the same transaction, JOB_UPDATED built from the
    /// POST-close row (full row state, what the rebuild consumes, as
    /// `merge_job_metadata_at`'s is) and then whatever `markers` builds
    /// from that same row (the status-changed and closed markers).
    async fn close_job_at(
        &self,
        id: &JobId,
        closed_on: chrono::NaiveDate,
        owned: &serde_json::Map<String, serde_json::Value>,
        stamp: &boss_core::publisher::EventStamp,
        markers: &(dyn for<'j> Fn(&'j Job) -> Vec<boss_core::event::Event> + Send + Sync),
    ) -> Result<Option<Job>, JobsError>;

    /// Append one entry to the Job's reserved `corrections` list
    /// (`crate::corrections`, design 4105b020), atomically against the
    /// row as it stands, and return the post-append Job with the index
    /// the entry landed at. A non-list under the key, or a non-object
    /// metadata, folds to an empty list first.
    ///
    /// Append, never read-modify-write: two corrections landing at once
    /// must both survive, which a caller-side GET → push → PATCH cannot
    /// promise. Records, in the same transaction, JOB_UPDATED (full row
    /// state, what the rebuild consumes — so it must be built from the
    /// post-append row, as `merge_job_metadata_at`'s is) and
    /// STEP_CORRECTED naming the step (the entry's `step`) and index.
    async fn append_step_correction_at(
        &self,
        id: &JobId,
        entry: &serde_json::Value,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(Job, usize), JobsError>;

    /// Every machine the estate declares.
    ///
    /// Declaring a machine is a change to the TREE that converges
    /// (infra/estate/estate.toml), never a hand write: until backlog
    /// ee368d0c (2026-09-18) the tree's copy was a schema migration, so
    /// every fresh database — every OSS install — carried this LAN's
    /// nodes; now the launcher publishes the tree's declaration through
    /// `declare_estate_nodes` on every start, insert-if-absent. What was
    /// missing before either existed was any way to READ it: the tables
    /// had existed since 144-estate-subjects.sql and no service served
    /// them, so "what hardware is running" was unanswerable from inside
    /// BOSS and had to be re-derived by shelling into machines
    /// (59ef456a).
    ///
    /// DECLARED capacity, not observed. Free space now is a
    /// measurement with a timestamp and belongs on the log, which is
    /// what the `node` subject kind's own description says.
    async fn list_estate_nodes(&self) -> Result<Vec<EstateNode>, JobsError>;

    /// Land the tree's declaration, insert-if-absent: a `nodes` row and
    /// its `subjects` identity for each id not already there (a row
    /// already there is KEPT — its notes and capacity as the last
    /// declaration or migration left them, never overwritten), and a
    /// `node_roles` row for each (node, role) pair not already there,
    /// on kept nodes too. One `node.declared` fact per node the batch
    /// changed, staged in the write's own transaction; none for a node
    /// it left alone. A role that is not a Class of `node` is refused
    /// by the schema's FK — the vocabulary stays registry data
    /// (202609120300).
    async fn declare_estate_nodes(
        &self,
        declared: &[EstateNodeInput],
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<EstateBatchOutcome, JobsError>;

    /// Recent recorded events of ONE exact kind, newest first, as the
    /// raw rows `{event_id, timestamp, source, kind, payload}`.
    ///
    /// The read half of `record_events`: the estate doors record
    /// observations and comparisons as events, and until this method
    /// existed those series were readable only through an in-pod
    /// port-forward to the events service — two proven arbiters were
    /// SATISFIED and unprobeable for exactly that reason (d471a8ce).
    /// Raw `Value` rows on purpose: the readers serve their instrument
    /// verbatim, and a port type per payload shape would be a second
    /// instrument.
    ///
    /// `scope`, when given, keeps only rows whose payload carries that
    /// exact top-level `scope` — and it is applied BEFORE `limit`, not
    /// after. One kind carries many series at different cadences: the
    /// estate observer records `kubernetes-nodes` every 15 minutes
    /// while `codebase` is recorded once a night. A limit taken across
    /// all of them is spent by whichever series ticks fastest, so the
    /// slow one is unreadable through the reader that is supposed to
    /// serve it — invisible by construction rather than by outage
    /// (measured 2026-09-02: the 50-row ceiling held 49
    /// `kubernetes-nodes` rows and 1 `host`, spanning half a day).
    /// This is the same rule `TailQuery::simulated` states in
    /// boss-events: a filter has to be where the LIMIT is applied, or
    /// it does not really filter.
    ///
    /// The window's `since` (inclusive) and `until` (exclusive) obey the
    /// same rule, in time rather than cadence (backlog bf362f25): a
    /// post-mortem thirty hours on could not reach the rows it needed
    /// through a reader that only ever served the newest page. The
    /// page's `total` counts the whole window, so a caller compares its
    /// rows against it instead of mistaking a full page for the answer.
    async fn recent_events_by_kind(
        &self,
        kind: &str,
        window: &EventWindow,
        limit: i64,
    ) -> Result<EventPage, JobsError>;

    /// The station flow cube over `[since, now]` — how many
    /// obligations of each `(job kind, step kind, spec slug, authority
    /// role)` shape became ready, and how many were completed, inside
    /// a WALL-CLOCK window.
    ///
    /// `since` is wall clock, deliberately NOT the sim clock: the
    /// question is how long real people and agents actually waited,
    /// and event time is sim-authoritative on a demo deployment
    /// (`boss-views/src/flow.rs` owns that doctrine).
    ///
    /// Deliberately NO default impl, for the same reason
    /// [`Self::queue_age`] has none: the window is a storage-level
    /// instant (`audit_log.created_at`, the in-memory adapter's
    /// recorded write instants) that `list_jobs` + `list_steps` cannot
    /// see, so a default would have to invent one. An adapter with no
    /// log answers with no cells, and the surface says "flow not
    /// countable" rather than printing a zero rate.
    async fn step_flow_cube(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<crate::station_flow::FlowCell>, JobsError>;

    /// Everything the log holds about ONE job, oldest first: every
    /// recorded event whose payload names the job (step events under
    /// `job_id`, the job's own lifecycle events under `id`). The
    /// per-packet audit read (c17871fe) — the provenance the log already
    /// holds, one read from the step. `limit` keeps the NEWEST rows:
    /// a long history answers with its most recent `limit` events in
    /// the order they happened.
    async fn events_for_job(
        &self,
        job_id: &JobId,
        limit: i64,
    ) -> Result<Vec<boss_core::event::Event>, JobsError>;

    /// Re-pin a packet to a different protocol version.
    ///
    /// A DELIBERATELY SEPARATE VERB, not a field on `update_job`.
    /// `workflow_version` is excluded from that UPDATE's SET list
    /// alongside `simulated`, because the storage enforces pinning
    /// rather than trusting every caller to respect it — and that
    /// immutability is what makes "in-flight packets stay on the
    /// version they were admitted under" true rather than aspirational.
    ///
    /// So conversion gets its own door. The caller is expected to have
    /// asked [`crate::protocol_conversion::convertibility_for_packet`]
    /// first, and hands over what the move writes
    /// ([`crate::repin::plan`]). Widening `update_job` instead would have
    /// let any PUT re-pin a packet by accident, which is the failure
    /// this shape exists to prevent (bfc74b3a).
    ///
    /// ONE TRANSACTION, because a move is true of the packet only whole
    /// (design 7cf202a9 Q2/Q3; backlog 1e973965 measured the door that
    /// moved the column alone): the pinned version, `record` appended
    /// to the reserved `repins` list, each re-projected step row, each
    /// inserted one — recorded as JOB_UPDATED, a STEP_UPDATED per
    /// rewritten row and a STEP_CREATED per inserted one (the state the
    /// rebuild replays), and the `jobs.job.repinned` marker carrying
    /// `record`. A re-projected row that finished between the caller's
    /// read and this write keeps everything but its `sort_order`: a
    /// completed step keeps the text it ran under, whoever raced.
    async fn repin_workflow_version_at(
        &self,
        id: &JobId,
        to_version: i32,
        plan: &crate::repin::RepinPlan,
        record: &serde_json::Value,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError>;

    async fn list_jobs(
        &self,
        filter: &JobFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Job>, i64), JobsError>;

    // ----- Steps -----

    async fn add_step(&self, step: &Step) -> Result<(), JobsError> {
        self.add_step_at(step, Utc::now(), &[]).await
    }

    /// Write one step row, recording `events` with it; a step whose id
    /// already exists inserts nothing and records nothing.
    ///
    /// THE ROW IS STAMPED WITH ITS PLUGIN VERSION (backlog 82448947).
    /// A step written with `step_plugin_version = 0` is stored at the
    /// version of the step plugin active for its `kind` at the write,
    /// and at 0 when no plugin serves the kind; a non-zero version the
    /// caller supplies is kept (a replay seeding its own). The stamp
    /// is a snapshot, so a plugin republished or retired later never
    /// moves which bundle an existing step renders against. Every
    /// adapter stamps from the step-plugin registry it reads: the
    /// Postgres adapter from the `step_plugins` table inside the
    /// insert's transaction, the in-memory adapter from the registry
    /// given to `InMemoryJobs::with_step_plugins` — none given is an
    /// empty registry, so it keeps the caller's value, as Postgres
    /// does over a table with no active row. The stamp changes what
    /// [`JobsRepository::get_step`] returns, and one body of
    /// assertions holds both adapters to it:
    /// `tests/the_adapters_agree_on_a_steps_plugin_version_pg.rs`.
    /// [`JobsRepository::create_job_with_steps_at`] writes each of its
    /// steps by the same rule.
    async fn add_step_at(
        &self,
        step: &Step,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError>;

    /// [`JobsRepository::get_step`] with the [`StepVersion`] the row was
    /// at, read in the same statement — the read a judged write
    /// ([`JobsRepository::update_step_if_unchanged_at`]) is computed
    /// from (backlog 6ec22d71).
    async fn get_step_versioned(
        &self,
        id: &StepId,
    ) -> Result<Option<(Step, StepVersion)>, JobsError>;

    /// [`JobsRepository::list_steps`] with each row's [`StepVersion`],
    /// for the writers that judge a list read: the readiness
    /// re-evaluator and the terminal close's skip (backlog 6ec22d71).
    async fn list_steps_versioned(
        &self,
        job_id: &JobId,
    ) -> Result<Vec<(Step, StepVersion)>, JobsError>;

    async fn update_step(&self, step: &Step) -> Result<(), JobsError> {
        self.update_step_at(step, Utc::now(), &[]).await
    }

    async fn update_step_at(
        &self,
        step: &Step,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    /// [`JobsRepository::update_step_at`] for a write computed from a
    /// READ: `read` is the [`StepVersion`] the caller's copy of the step
    /// was read at ([`JobsRepository::get_step_versioned`] /
    /// [`JobsRepository::list_steps_versioned`]), and the write lands
    /// only while the live row is still that version — no writer has
    /// touched ANY column since. Otherwise it is refused with
    /// [`JobsError::StepChanged`] and nothing — row or events — is
    /// written.
    ///
    /// WHY (backlog e381689d, measured 2026-09-25 on run 6b6fe011).
    /// `boss dispatch` merged `prompt_bytes` onto a step through the
    /// merge door (204, and its STEP_UPDATED carries the key) at
    /// 11:01:14.649Z; the dispatcher's assignment PUT, whose handler had
    /// read the step a moment BEFORE, wrote the whole row back at
    /// 11:01:14.651Z with the metadata it had read, and answered 204
    /// too. The key was gone and both writers had been told success.
    /// Every read-modify-write step writer has that shape, so every one
    /// in the service writes through this door; the check rides the
    /// write's own statement, so no window is left between them.
    ///
    /// ON EVERY COLUMN, AND OVER A TERMINAL ROW TOO (backlog 6ec22d71).
    /// The first version of this door compared the METADATA read, and
    /// exempted a terminal row because its metadata is frozen. Two
    /// reviews reproduced what that left on 2026-09-25: a claim moves
    /// status and holder and no metadata, so the assignment PUT computed
    /// a moment before it wrote `ready` and its own holder back over the
    /// claim; and a stale write over a row that went `completed`
    /// answered Ok, the row kept its values under the terminal CASE,
    /// and a `jobs.step.updated` carrying the stale ones was recorded
    /// anyway — which `rebuild.rs::upsert_step` replays verbatim, so
    /// replay demoted the step. Now any write since the read refuses,
    /// and a terminal row read at this version still refuses
    /// ([`JobsError::TerminalStep`]) a write that would move a column
    /// it freezes ([`terminal_write_moves_frozen`]) — so no event is
    /// ever recorded for a value the row refused. An idempotent re-send
    /// over a fresh read of a terminal row moves nothing and lands.
    async fn update_step_if_unchanged_at(
        &self,
        step: &Step,
        read: StepVersion,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    /// Merge `patch`'s top-level keys into the Step's `metadata`,
    /// atomically, touching no other field. Same contract as
    /// [`JobsRepository::merge_job_metadata_at`]: a `null` value
    /// REMOVES the key, any other value replaces that key wholesale,
    /// and the returned Step is the post-merge row. Takes the
    /// [`boss_core::publisher::EventStamp`] rather than pre-built
    /// events for the same reason the job merge does: the
    /// STEP_UPDATED payload is full row state, so it must be built
    /// from the POST-merge row, which only the adapter's transaction
    /// knows.
    ///
    /// A terminal (Completed/Skipped) step's metadata is frozen —
    /// `update_step_at`'s invariant — and the merge refuses with
    /// [`JobsError::TerminalStep`] rather than silently keeping the
    /// row: the check rides the same statement as the write, so a
    /// step completing between the caller's read and this write is
    /// still refused, never half-honored.
    async fn merge_step_metadata_at(
        &self,
        id: &StepId,
        patch: &serde_json::Map<String, serde_json::Value>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Step, JobsError>;

    /// Claim a ready step for an actor — the Ready→Active
    /// compare-and-set (queue-visibility Q2). Succeeds only while
    /// the step is `ready` and unassigned; a re-claim by the current
    /// holder (ready or active) is an idempotent success. Everything
    /// else is `ClaimConflict` naming the holder. Like
    /// `append_sign_off`, this write path owns its fields — the
    /// generic step UPDATE racing a claim cannot un-decide it.
    ///
    /// WHO COUNTS AS "THE CURRENT HOLDER" IS ADAPTER-SCOPED (backlog
    /// 28dcc735). The Postgres adapter reads the claimant's aliases
    /// from `actor_aliases` inside the claim transaction, admits a
    /// holder spelled by any of them, and rewrites `assignee_id` to
    /// the claimant's registered id (backlog d7fef617: steps nominated
    /// with an agent's login refused the agent's own claim). It is
    /// directional: an alias claiming a step the registered id holds
    /// is refused. Pinned by
    /// `tests/step_claim_admits_an_aliased_holder_pg.rs`.
    ///
    /// The in-memory adapter does NOT implement this: it has no alias
    /// source and compares spellings exactly, so a port-level test
    /// cannot catch a regression of the alias admission. Deliberately
    /// so — an alias store there would be a second identity registry
    /// to keep in step with the table. Pinned by
    /// `in_memory::tests::an_aliased_holder_is_refused_in_memory_because_it_has_no_alias_source`.
    /// A new adapter must decide which of the two it is and say so
    /// here.
    ///
    /// A CLAIM THAT MOVES THE SHAPE VOIDS WHAT IT MOVED (backlog
    /// 4174c4a9). Dropping the run edge changes the metadata, which is
    /// inside `step_shape_hash`, so the claim is an edit of the signed
    /// content: both adapters void every live stamp under the lock when
    /// it does, and record the `jobs.step.stamps_invalidated` event —
    /// built from `stamp`, after the caller's `events` — in the claim's
    /// own write. `stamp.timestamp` is the claim's instant.
    async fn claim_step_at(
        &self,
        step_id: &StepId,
        actor: &str,
        stamp: &boss_core::publisher::EventStamp,
        events: &[boss_core::event::Event],
    ) -> Result<Step, JobsError>;

    /// Append one sign-off stamp atomically. Stamps are
    /// append-only and owned by this path — the generic step UPDATE
    /// never writes `sign_offs`, so a concurrent read-modify-write
    /// (dispatcher auto-assign, predicate re-eval) cannot clobber a
    /// stamp that landed between its read and its write.
    ///
    /// The stamp lands only on the shape it signs: under the row's lock,
    /// a row whose `step_shape_hash` is not `stamp.shape_hash` refuses
    /// with [`JobsError::StampOffShape`] and nothing — stamp or events —
    /// is written (backlog 4174c4a9). The sign-off door builds the stamp
    /// from an earlier read, and this is the one place a write between
    /// the two can be seen.
    ///
    /// THE LOG REPRODUCES THE STAMP (backlog f146a13a). The append
    /// records a `jobs.step.updated` built from the row as its own
    /// UPDATE left it, from `event_stamp`, in the same write and BEFORE
    /// the caller's `events` — the rule `merge_step_metadata_at` and the
    /// re-pin already follow. The signed-off marker the door passes is
    /// the fact of the signing; the rebuild replays the state event, so
    /// a stamp no later edit carried is no longer lost by a replay.
    /// `event_stamp.timestamp` is the write's instant (`updated_at`).
    async fn append_sign_off(
        &self,
        step_id: &StepId,
        stamp: &boss_core::job::SignOffStamp,
        event_stamp: &boss_core::publisher::EventStamp,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    /// Record events on the transactional outbox with NO accompanying
    /// row write — the reliable-delivery path for standalone marker
    /// events (the post-materialization `step.ready.<kind>` pass).
    /// Same delivery guarantees as the in-write recording; its own
    /// small transaction.
    async fn record_events(&self, events: &[boss_core::event::Event]) -> Result<(), JobsError>;

    /// The plugin version a step of `kind` written now is stamped with:
    /// the version of the step plugin active for the kind in THIS
    /// adapter's registry, 0 when none serves it — the same lookup a
    /// step insert applies to a step written at 0.
    ///
    /// A writer stamps the step with this BEFORE it builds the step's
    /// STEP_CREATED, so the event carries what the row stores (backlog
    /// aba364fe, determinism). Both handlers built the event from the
    /// caller's 0 while the Pg insert stamped the row, and the
    /// rebuilder replays the payload: on 2026-09-25 every plugin-served
    /// STEP_CREATED in the live log said 0, and 144 steps with no later
    /// STEP_UPDATED would have rebuilt at 0 from rows at 1 or 3.
    /// Pinned by `tests/a_created_step_replays_at_its_plugin_version_pg.rs`.
    async fn active_step_plugin_version(&self, kind: &str) -> Result<i32, JobsError>;

    /// The one-time repair door for steps whose log says plugin version
    /// 0 while the row holds the stamped one (backlog 5a670a71; the
    /// contract is [`crate::plugin_version_repair`]). Scans every step
    /// whose log-derived version differs from its row, judges each
    /// under the row's lock, and — only when `write` — appends ONE
    /// STEP_UPDATED built from the row for each step whose divergence
    /// is exactly that defect, stamped by `stamp`. Everything else is
    /// listed as refused and left alone.
    ///
    /// Default: refused. An adapter that keeps no event log has no
    /// divergence to find, and an empty report would read as "the log
    /// agrees" — a confident answer to a question it cannot ask.
    async fn repair_step_plugin_versions(
        &self,
        write: bool,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<crate::plugin_version_repair::RepairReport, JobsError> {
        let _ = (write, stamp);
        Err(JobsError::Storage(
            "this adapter keeps no event log, so it cannot compare one with its rows".into(),
        ))
    }

    async fn list_steps(&self, job_id: &JobId) -> Result<Vec<Step>, JobsError>;

    /// Open, workable steps for an executor — the pull side of the
    /// "human-powered state machine" dispatcher. Returns steps whose
    /// status is `Ready | Active` AND that are either assigned to
    /// `assignee_id` OR unassigned with a `metadata.authority_role`
    /// in `roles` (claimable by role). Only steps on `Open` Jobs.
    /// `limit` caps the result.
    ///
    /// The default impl scans open Jobs + their steps in Rust — correct
    /// but O(open jobs); the Postgres adapter overrides it with a single
    /// indexed JOIN. Drives the SPA My Day surface and the sim's
    /// workforce loop (which queries as each simulated employee).
    async fn list_assignments(
        &self,
        assignee_id: Option<&str>,
        roles: &[String],
        limit: i64,
    ) -> Result<Vec<AssignmentRow>, JobsError> {
        // Unoptimized fallback: scan open Jobs + their steps and filter
        // in Rust. The Postgres adapter overrides this with one indexed
        // JOIN. Open Jobs only; ordered by (opened_on, sort_order).
        let filter = JobFilter {
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        let (mut jobs, _) = self.list_jobs(&filter, 10_000, 0).await?;
        jobs.sort_by_key(|j| j.opened_on);
        let mut out = Vec::new();
        for job in &jobs {
            for step in self.list_steps(&job.id).await? {
                if !matches!(step.status, StepStatus::Ready | StepStatus::Active) {
                    continue;
                }
                let assignee_match = match (step.assignee_id.as_deref(), assignee_id) {
                    (Some(a), Some(me)) => a == me,
                    _ => false,
                };
                // Role-match applies when the step is either unassigned
                // (claimable) OR already Active (in-progress, owned by the
                // role's workforce — so a worker can re-find and finish a
                // multi-day step it claimed earlier). A Ready step already
                // assigned to someone else is theirs, not poachable.
                let role_eligible =
                    step.assignee_id.is_none() || matches!(step.status, StepStatus::Active);
                let role_match = role_eligible
                    && step
                        .metadata
                        .get("authority_role")
                        .and_then(|v| v.as_str())
                        .is_some_and(|r| roles.iter().any(|x| x == r));
                if assignee_match || role_match {
                    out.push(AssignmentRow {
                        job_id: job.id,
                        job_title: job.title.clone(),
                        due_on: job.due_on,
                        opened_on: job.opened_on,
                        workflow: job.kind.clone(),
                        workflow_version: job.workflow_version,
                        subject_kind: boss_core::primitives::Subject::kind(&job.subject)
                            .to_string(),
                        subject_id: boss_core::primitives::Subject::id(&job.subject).to_string(),
                        priority: job.priority,
                        partition: job.partition,
                        tags: job.tags.clone(),
                        red_trains: crate::yard::red_trains_of(&job.metadata),
                        step,
                    });
                    if out.len() >= limit as usize {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The entire assigned-and-workable backlog in ONE query: every step
    /// of an open Job that is Ready or Active AND already carries an
    /// assignee. The sim workforce pulls this each pass and drives every
    /// assigned step regardless of who assigned it or to whom — which
    /// decouples the executor from assignment policy (the dispatcher, and
    /// later managers, own that) and replaces a per-employee query
    /// fan-out with a single round-trip.
    ///
    /// The default impl scans open Jobs in Rust; the Postgres adapter
    /// overrides it with one indexed JOIN. Ordered by (opened_on,
    /// sort_order) for a stable queue.
    async fn list_assigned_workable(&self, limit: i64) -> Result<Vec<AssignmentRow>, JobsError> {
        let filter = JobFilter {
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        let (mut jobs, _) = self.list_jobs(&filter, 10_000, 0).await?;
        jobs.sort_by_key(|j| j.opened_on);
        let mut out = Vec::new();
        for job in &jobs {
            for step in self.list_steps(&job.id).await? {
                if !matches!(step.status, StepStatus::Ready | StepStatus::Active) {
                    continue;
                }
                if step
                    .assignee_id
                    .as_deref()
                    .filter(|a| !a.is_empty())
                    .is_none()
                {
                    continue;
                }
                out.push(AssignmentRow {
                    job_id: job.id,
                    job_title: job.title.clone(),
                    due_on: job.due_on,
                    opened_on: job.opened_on,
                    workflow: job.kind.clone(),
                    workflow_version: job.workflow_version,
                    subject_kind: boss_core::primitives::Subject::kind(&job.subject).to_string(),
                    subject_id: boss_core::primitives::Subject::id(&job.subject).to_string(),
                    priority: job.priority,
                    partition: job.partition,
                    tags: job.tags.clone(),
                    red_trains: crate::yard::red_trains_of(&job.metadata),
                    step,
                });
                if out.len() >= limit as usize {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    /// Per-version terminal report for one workflow kind — Tier 1 of
    /// the experiments program (docs/design/network-experiments.md):
    /// the read surface that replaces the ad-hoc SQL the brewery
    /// protocol iterations were measured with. Groups every packet of
    /// `kind` by its PINNED `workflow_version` and reports counts,
    /// closed-outcome distribution, and open→close cycle-time stats.
    ///
    /// `since` keeps packets opened on/after that date; `partition`
    /// partitions like [`JobFilter::partition`] (`None` is every
    /// packet). A kind with no packets reports an empty Vec — absence
    /// is a fact, not an error.
    ///
    /// The default impl is the pure [`terminal_report_from_jobs`]
    /// over `list_jobs` — honest but O(packets of the kind) in Rust;
    /// the Postgres adapter overrides it with one SQL statement.
    async fn workflow_terminal_report(
        &self,
        kind: &str,
        since: Option<chrono::NaiveDate>,
        partition: Option<Partition>,
    ) -> Result<Vec<VersionTerminalReport>, JobsError> {
        let filter = JobFilter {
            kind: Some(kind.to_string()),
            partition,
            ..Default::default()
        };
        let (jobs, _total) = self.list_jobs(&filter, i64::MAX, 0).await?;
        Ok(terminal_report_from_jobs(&jobs, since))
    }

    /// The most recently CLOSED packet of `kind` — the newest terminal
    /// a department's readiness read reports per protocol (backlog
    /// 1dffde5d). Newest by `closed_on`, ties broken newest-opened
    /// first; `None` when no packet of the kind has closed. Cancelled
    /// packets are terminal but not closed, so they do not count — the
    /// same line `workflow_terminal_report` draws.
    ///
    /// The default impl is the pure [`newest_closed_from_jobs`] over
    /// every closed packet of the kind — honest but O(packets); the
    /// Postgres adapter answers with one ordered `LIMIT 1`.
    async fn newest_closed_job(&self, kind: &str) -> Result<Option<Job>, JobsError> {
        let filter = JobFilter {
            kind: Some(kind.to_string()),
            status: Some(JobStatus::Closed),
            ..Default::default()
        };
        let (jobs, _total) = self.list_jobs(&filter, i64::MAX, 0).await?;
        Ok(newest_closed_from_jobs(jobs))
    }

    /// Every outstanding obligation in `scope` — `ready` / `active`
    /// steps on `open` packets — longest-waiting first. The read
    /// surface behind the queue-age lens (`GET /api/jobs/queue-age`,
    /// packet 2a0b034e); [`QueueAgeRow`] documents what `since` /
    /// `exact` honestly mean.
    ///
    /// Deliberately NO default impl: the wait instant is adapter
    /// storage (`became_ready_at` / `updated_at` columns, the
    /// in-memory write-instant maps), invisible to `Job` / `Step` —
    /// so there is no honest way to derive it from `list_jobs` +
    /// `list_steps`, and a default would have to invent one.
    async fn queue_age(&self, scope: &JobScope) -> Result<Vec<QueueAgeRow>, JobsError>;

    /// Count steps whose kind matches `step_kind` and whose status is
    /// still non-terminal (pending, ready, active). Used by the Step
    /// UX plugin retire path to surface a blast-radius preview.
    async fn count_in_flight_steps_by_kind(&self, step_kind: &str) -> Result<i64, JobsError>;

    /// Count Jobs pinned to one Workflow ROW — `(kind, version)`,
    /// the pair a Job records at open — whose status is still
    /// non-terminal (anything but closed / cancelled).
    ///
    /// A Job stays pinned to the version it opened under, so this is
    /// the live-work blast radius of retiring that exact row. Boot's
    /// quarantine pass asks before it auto-retires an unviable
    /// Workflow: retiring a row with open Jobs on it would strand
    /// them.
    async fn count_open_jobs_for_workflow(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<i64, JobsError>;

    /// Group Jobs by kind and return `(kind, count)` pairs, optionally
    /// scoped to a specific status. Used by the operating-model view
    /// to drive the per-phase live counts without pulling the whole
    /// job list over the wire. Returns every kind present in the
    /// table (no zero-fills) — callers map the list into their own
    /// `{kind: count}` shape.
    async fn count_jobs_by_kind(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i64)>, JobsError>;

    // ----- Cross-job dependency resolution (D10) -----

    /// Given a set of step IDs (possibly spanning multiple jobs),
    /// return their current statuses. Used to check whether a blocked
    /// step's dependencies have been satisfied.
    async fn resolve_blockers(
        &self,
        ids: &[StepId],
    ) -> Result<Vec<(StepId, StepStatus)>, JobsError>;

    /// Read the brewery sim_clock state (if the repo's underlying
    /// store has the `sim_clock` table). Returns None for in-
    /// memory adapters or fresh DBs that haven't seeded a
    /// sim_clock row yet. Used by the public landing's
    /// "simulated time" indicator + /workflows live counter so
    /// operators see the sim epoch advancing in real time.
    async fn sim_clock_state(&self) -> Result<Option<SimClockState>, JobsError> {
        Ok(None)
    }

    /// Set the sim_clock's `paused` flag. The brewery-sim daemon
    /// reads this on every tick and stops advancing
    /// `current_sim_date` when paused. Used by the Debug menu's
    /// pause/resume actions; in-memory adapters no-op.
    async fn set_sim_clock_paused(&self, _paused: bool) -> Result<(), JobsError> {
        Ok(())
    }

    /// Restart the sim epoch with a **clean** baseline — drops the
    /// prior loop's audit_log + projection state, replays the
    /// canonical seed bundle, resets the sim_clock to
    /// `epoch_start_date`, unpauses. The daemon's next tick
    /// resumes from `epoch_start_date` against fresh data.
    ///
    /// This is the demo-loop reset: the brewery playground is a
    /// "flea circus" the audience watches, so accumulating state
    /// across loops would poison the visual. The truncate+replay
    /// path is faster than the operator shell script
    /// (`infra/postgres/reset-to-baseline.sh`) since it skips
    /// the DB drop + bootstrap + data-seed steps — typical
    /// runtime is 30-60 seconds vs. 5 minutes.
    ///
    /// In-memory adapters no-op.
    async fn restart_sim_clock_epoch(&self) -> Result<(), JobsError> {
        Ok(())
    }

    // ----- Refused writes -----
    //
    // The denominator for step reliability. A completed step is the
    // only thing the record holds today, and required-at-done
    // validation guarantees every completed step is conformant — so
    // conformance measures 100% and always will. What it cannot see is
    // the attempt that never became a completion. See
    // `crate::refusals` for the classifier and the two readings this
    // is for (unrecovered refusals; distinct actors per error class).

    /// Record a refused step write.
    ///
    /// Recording is a side-channel: it must never turn a refusal the
    /// caller can act on into a 500 it cannot. Callers log the error
    /// and continue.
    async fn record_step_write_refusal(
        &self,
        refusal: &crate::refusals::StepWriteRefusal,
    ) -> Result<(), JobsError> {
        self.record_step_write_refusal_at(refusal, Utc::now()).await
    }

    async fn record_step_write_refusal_at(
        &self,
        refusal: &crate::refusals::StepWriteRefusal,
        now: DateTime<Utc>,
    ) -> Result<(), JobsError>;

    /// Recent refusals, newest first. The read side — without it the
    /// table is a black hole and "let's try it and see how it goes"
    /// has nothing to look at.
    async fn step_write_refusals(
        &self,
        limit: i64,
    ) -> Result<Vec<crate::refusals::RecordedRefusal>, JobsError>;
}

/// Snapshot of the simulated clock for read-side surfaces. All
/// values are derived from clock-api's formula in real time.
#[derive(Debug, Clone, Serialize)]
pub struct SimClockState {
    /// Full sim-time instant. The SPA renders date + HH:MM from
    /// this so the within-day movement of the formula clock is
    /// visible.
    pub now: chrono::DateTime<chrono::Utc>,
    /// Convenience date-only projection for surfaces that only
    /// need the day (appToday()-style consumers).
    pub current_sim_date: chrono::NaiveDate,
    pub epoch_start_date: Option<chrono::NaiveDate>,
    pub epoch_end_date: Option<chrono::NaiveDate>,
    pub paused: bool,
    /// True while the clean-reset path is mid-flight (audit_log
    /// truncate + boss-rebuild-all replay + clock rewind). The
    /// SimClockBadge polls this to render a spinner instead of
    /// the "Restart epoch" button.
    #[serde(default)]
    pub restart_in_progress: bool,
}
