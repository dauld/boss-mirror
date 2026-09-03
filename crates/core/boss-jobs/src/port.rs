//! Hexagonal port: `JobsRepository` defines what the domain needs from
//! persistence. Adapters (in-memory for tests, Postgres for prod)
//! implement this trait.

use async_trait::async_trait;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus};
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
    #[error("step {id} is {status} — a terminal step's metadata is frozen")]
    TerminalStep { id: StepId, status: String },
}

/// Optional filters for listing jobs.
#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub kind: Option<String>,
    /// Prefix match on `kind`. Used by the UI for nav buckets that
    /// span related registry kinds (e.g. `refurb-used` + `refurb-oem-new`
    /// both match `kind_prefix = "refurb"`).
    pub kind_prefix: Option<String>,
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
    /// Row-level policy scope — translated from `boss_policy_client::Predicate`
    /// by the HTTP handler before calling the adapter. Pushing it down
    /// into SQL here means scoped roles get accurate `total` counts
    /// and pages that only contain jobs they can see (no wasted page
    /// space on rows the post-fetch filter would discard).
    pub scope: JobScope,
    /// Keep only real packets (`Some(false)`) or only simulated ones
    /// (`Some(true)`). `None` — the default — is every packet, which
    /// is what every existing caller already gets.
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
    /// `simulated` is set at admission and immutable afterwards
    /// (`update_job` restores it from the existing row), so this is a
    /// stable partition rather than a mutable label. Measured on the
    /// same population: of 39 kinds, **zero are mixed** — a kind is
    /// either entirely simulated or entirely real — so filtering here
    /// never splits a kind's packets across two answers.
    pub simulated: Option<bool>,
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

/// One row in the launch-calendar projection. Flat shape the frontend
/// renders directly — the caller doesn't need to fetch the full Job.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LaunchCalendarRow {
    pub job_id: JobId,
    pub title: String,
    pub owner_id: Option<String>,
    pub subject_id: Option<String>,
    pub status: JobStatus,
    /// Min sort_order of any non-done step = current tier. Null means
    /// every step is terminal but the Job isn't closed yet.
    pub current_tier: Option<i32>,
    /// `launch_date` from the tier-4 `marketing-launch` step's metadata.
    /// Null when the step exists but the date hasn't been set yet.
    pub launch_date: Option<chrono::NaiveDate>,
    /// Channel label from the launch step ("email" / "webinar" / etc.).
    pub launch_channel: Option<String>,
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
    pub workflow: String,
    /// The protocol version this packet was admitted under. Rides on
    /// the row so an executor can resolve the step's spec (its
    /// spec-authored `duration_hours`, for one) against the exact
    /// Workflow row the Job is pinned to, without a second fetch.
    pub workflow_version: i32,
    pub subject_kind: String,
    pub subject_id: String,
    pub priority: Priority,
    /// The Job's admission-fixed sim-vs-real flag, and its tags. A
    /// projection, not the Job — but a queue lens renders a packet
    /// card from the row alone, and a simulated packet has to look
    /// simulated in a personal queue exactly as it does in the yard.
    /// `tags` rides along for the same reason: the shared card
    /// predicate falls back to a `sim` / `simulated` / `synthetic` tag
    /// for packets that predate the column (there was no backfill), so
    /// without it the two lenses would disagree on the same packet.
    pub simulated: bool,
    pub tags: Vec<String>,
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
    pub simulated: bool,
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
    pub role: String,
    pub cpu: Option<i32>,
    pub memory_gb: Option<i32>,
    pub disk_gb: Option<i32>,
    pub notes: Option<String>,
    /// Retired machines stay readable so history resolves, exactly as
    /// retired subject kinds do.
    pub retired: bool,
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
/// (`create_job_at`, `add_step_at`) are ON CONFLICT DO NOTHING
/// replay-tolerant — their events record ONLY when the insert
/// actually inserted, so a replayed create records nothing (before,
/// every replay published duplicate created events). The convenience
/// overloads pass no events (test-path ergonomics).
#[async_trait]
pub trait JobsRepository: Send + Sync {
    // ----- Jobs -----

    async fn create_job(&self, job: &Job) -> Result<(), JobsError> {
        self.create_job_at(job, Utc::now(), &[]).await
    }

    async fn create_job_at(
        &self,
        job: &Job,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError>;

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

    /// Every machine the estate declares.
    ///
    /// READ ONLY, AND DELIBERATELY SO. `nodes` is seeded by schema
    /// migration — declaring a machine is a change to the tree that
    /// converges, not an API write — so there is no create/update here
    /// and there should not be. What is missing today is any way to
    /// READ it: the tables have existed since 144-estate-subjects.sql
    /// and no service has ever served them, so "what hardware is
    /// running" was unanswerable from inside BOSS and had to be
    /// re-derived by shelling into machines (59ef456a).
    ///
    /// DECLARED capacity, not observed. Free space now is a
    /// measurement with a timestamp and belongs on the log, which is
    /// what the `node` subject kind's own description says.
    async fn list_estate_nodes(&self) -> Result<Vec<EstateNode>, JobsError>;

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
    async fn recent_events_by_kind(
        &self,
        kind: &str,
        scope: Option<&str>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, JobsError>;

    /// Re-pin a packet to a different protocol version.
    ///
    /// A DELIBERATELY SEPARATE VERB, not a field on `update_job`.
    /// `workflow_version` is excluded from that UPDATE's SET list
    /// alongside `simulated`, because the storage enforces pinning
    /// rather than trusting every caller to respect it — and that
    /// immutability is what makes "in-flight packets stay on the
    /// version they were admitted under" true rather than aspirational.
    ///
    /// So conversion gets its own door, and the door is narrow: it
    /// changes exactly one column, and the caller is expected to have
    /// asked [`crate::protocol_conversion::convertibility_for_packet`]
    /// first. Widening `update_job` instead would have let any PUT
    /// re-pin a packet by accident, which is the failure this shape
    /// exists to prevent (bfc74b3a).
    async fn repin_workflow_version_at(
        &self,
        id: &JobId,
        to_version: i32,
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

    async fn add_step_at(
        &self,
        step: &Step,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError>;

    async fn update_step(&self, step: &Step) -> Result<(), JobsError> {
        self.update_step_at(step, Utc::now(), &[]).await
    }

    async fn update_step_at(
        &self,
        step: &Step,
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
    async fn claim_step_at(
        &self,
        step_id: &StepId,
        actor: &str,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<Step, JobsError>;

    /// Append one sign-off stamp atomically. Stamps are
    /// append-only and owned by this path — the generic step UPDATE
    /// never writes `sign_offs`, so a concurrent read-modify-write
    /// (dispatcher auto-assign, predicate re-eval) cannot clobber a
    /// stamp that landed between its read and its write.
    async fn append_sign_off(
        &self,
        step_id: &StepId,
        stamp: &boss_core::job::SignOffStamp,
        now: DateTime<Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError>;

    /// Record events on the transactional outbox with NO accompanying
    /// row write — the reliable-delivery path for standalone marker
    /// events (the post-materialization `step.ready.<kind>` pass).
    /// Same delivery guarantees as the in-write recording; its own
    /// small transaction.
    async fn record_events(&self, events: &[boss_core::event::Event]) -> Result<(), JobsError>;

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
                        workflow: job.kind.clone(),
                        workflow_version: job.workflow_version,
                        subject_kind: boss_core::primitives::Subject::kind(&job.subject)
                            .to_string(),
                        subject_id: boss_core::primitives::Subject::id(&job.subject).to_string(),
                        priority: job.priority,
                        simulated: job.simulated,
                        tags: job.tags.clone(),
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
                    workflow: job.kind.clone(),
                    workflow_version: job.workflow_version,
                    subject_kind: boss_core::primitives::Subject::kind(&job.subject).to_string(),
                    subject_id: boss_core::primitives::Subject::id(&job.subject).to_string(),
                    priority: job.priority,
                    simulated: job.simulated,
                    tags: job.tags.clone(),
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
    /// `since` keeps packets opened on/after that date; `simulated`
    /// partitions like [`JobFilter::simulated`] (`None` is every
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
        simulated: Option<bool>,
    ) -> Result<Vec<VersionTerminalReport>, JobsError> {
        let filter = JobFilter {
            kind: Some(kind.to_string()),
            simulated,
            ..Default::default()
        };
        let (jobs, _total) = self.list_jobs(&filter, i64::MAX, 0).await?;
        Ok(terminal_report_from_jobs(&jobs, since))
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

    /// For each open Job, compute its "current tier" — the min
    /// sort_order of any non-terminal step on the Job. Group by
    /// `(kind, current_tier)` and return the counts. The tier number
    /// is the step index the Job is currently working on; -1 means
    /// every step is terminal (completed/skipped) but the Job itself
    /// hasn't been closed yet.
    ///
    /// Drives the live histogram on the operating-model view so a
    /// Workflow bar can show "how many refurbs are in Acquire vs.
    /// Refurbish vs. Certify right now." Caller maps tier → phase
    /// via its own Workflow-specific mapping.
    async fn jobs_tier_distribution(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i32, i64)>, JobsError>;

    /// Projection backing the launch-calendar surface and the exec
    /// next-30-days panel per examples/used-device-shop/design/marketing-needs.md E2. Returns every
    /// open/in-flight `marketing-motion` Job joined to its tier-4
    /// `marketing-launch` step so the caller can render a forward
    /// calendar. `from` / `to` bound the launch_date window; Jobs
    /// whose launch step has no date yet are returned with `launch_date
    /// = None` so the UI can bucket them under "unscheduled".
    async fn list_launch_calendar(
        &self,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    ) -> Result<Vec<LaunchCalendarRow>, JobsError>;

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
