//! Station queue evaluation — the pure half of the station registry
//! (stations.md, Q1/Q2 ratified): membership is DERIVED (a station is
//! a predicate over packet state, recomputed at read time — no
//! mutable current-station field), and ordering is a data-declared
//! discipline (`priority, then age` default).
//!
//! Everything in this module is a pure function of `(StationSpec
//! fields, packets)` — no I/O, no clock — so the queue an operator
//! sees is reproducible from the projection alone. The HTTP handler
//! (`http/stations.rs`) supplies the open-Job universe and renders
//! the [`StationQueue`] envelope.

use std::collections::BTreeMap;

use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Predicate
// ---------------------------------------------------------------------------

/// The **self placeholder**: what a station row writes where the
/// *requesting actor's* id belongs.
///
/// Station predicates are static registry data, but the taxonomy needs
/// per-actor queues — "every executor has an actor station"
/// (stations.md), and a filer's watchlist is one. Without a
/// placeholder that is one row per person, generated whenever a person
/// is hired and stale whenever they change. With it, `my-watchlist` is
/// **one row every actor can query**, and the evaluator binds `@me` to
/// whoever is asking.
///
/// Two rules keep it safe, both enforced by
/// [`StationPredicate::bind_self`]:
/// - Binding happens ONCE, at the read edge, before any packet is
///   compared. `matches` never sees the placeholder.
/// - An unbindable placeholder (no identified actor — a guest) yields
///   NO predicate, and the caller must render an empty queue. Failing
///   to bind can never widen a queue.
pub const SELF: &str = "@me";

/// The queue-membership predicate over packets: a CONJUNCTION of
/// optional clauses (an omitted clause always matches). Deliberately
/// a small documented JSON shape rather than a general expression
/// language: `boss-expr` / `ready_when` predicates bind flat
/// identifiers from a single event payload or a Job's own step set —
/// neither evaluates "does some step of this Job look like X" over a
/// job+steps aggregate, which is the whole vocabulary a station
/// needs (kind / status / step-state / tags / metadata presence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StationPredicate {
    /// Exact match on the Job's Workflow kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Match on the Job's status (kebab-case on the wire). The
    /// evaluation universe is open Jobs, so this only narrows
    /// further; it exists so a predicate can say what it means
    /// instead of relying on the universe's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<JobStatus>,
    /// At least one of these tags is present on the Job.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags_any: Vec<String>,
    /// Metadata keys that must exist and be non-null.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_present: Vec<String>,
    /// Metadata keys that must be missing or null.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_absent: Vec<String>,
    /// Metadata keys whose value must equal exactly this string. Only
    /// string values compare — the clause exists to match ids, and an
    /// id is text; a number or object under the key is simply not a
    /// match.
    ///
    /// A value of [`SELF`] is the self placeholder, bound to the
    /// requesting actor by [`StationPredicate::bind_self`]. `BTreeMap`
    /// (not `HashMap`) so the row round-trips through JSON in a stable
    /// key order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata_equals: BTreeMap<String, String>,
    /// Some step of the Job matches every given field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepMatch>,
}

/// The step clause of a [`StationPredicate`]: a Job matches when ANY
/// of its steps satisfies every field given here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StepMatch {
    /// The step's `spec_slug` (the Workflow StepSpec's stable slug).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// The step's kind (StepType registry vocabulary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The step's status is one of these (kebab-case on the wire).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_in: Vec<StepStatus>,
    /// The step is assigned to exactly this executor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    /// Step metadata keys whose value must equal exactly this string,
    /// with the same string-only rule as the Job-level clause.
    ///
    /// Some facts only exist on the step. A red train is the worked
    /// case: the conductor records its verdict as `result` on the `ci`
    /// step and nowhere else, so a station that wants to hold red
    /// trains has to read it there. The alternative — stamping a
    /// second copy of the verdict on the Job so a Job-level clause
    /// could see it — is the duplication CLAUDE.md 9a exists to stop.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata_equals: BTreeMap<String, String>,
}

impl StepMatch {
    fn matches(&self, step: &Step) -> bool {
        if let Some(slug) = &self.slug
            && step.spec_slug.as_deref() != Some(slug.as_str())
        {
            return false;
        }
        if let Some(kind) = &self.kind
            && &step.kind != kind
        {
            return false;
        }
        if !self.status_in.is_empty() && !self.status_in.contains(&step.status) {
            return false;
        }
        if let Some(assignee) = &self.assignee_id
            && step.assignee_id.as_deref() != Some(assignee.as_str())
        {
            return false;
        }
        for (key, want) in &self.metadata_equals {
            if step.metadata.get(key).and_then(Value::as_str) != Some(want.as_str()) {
                return false;
            }
        }
        true
    }
}

impl StationPredicate {
    /// Whether `job` (with its `steps`) is a member of this station's
    /// queue. Pure; conjunction of every declared clause.
    ///
    /// An UNBOUND self predicate has no members. Binding is the read
    /// edge's job ([`Self::bind_self`]); if it was ever skipped, the
    /// placeholder must not be compared against packet data — a packet
    /// whose metadata literally holds `"@me"` would otherwise land in
    /// everyone's queue at once. Failing closed here means a missed
    /// bind shows an empty queue, which is visible, instead of another
    /// actor's packets, which is not.
    pub fn matches(&self, job: &Job, steps: &[Step]) -> bool {
        if self.binds_self() {
            return false;
        }
        if let Some(kind) = &self.kind
            && &job.kind != kind
        {
            return false;
        }
        if let Some(status) = &self.status
            && &job.status != status
        {
            return false;
        }
        if !self.tags_any.is_empty() && !self.tags_any.iter().any(|t| job.tags.contains(t)) {
            return false;
        }
        for key in &self.metadata_present {
            if job.metadata.get(key).is_none_or(|v| v.is_null()) {
                return false;
            }
        }
        for key in &self.metadata_absent {
            if job.metadata.get(key).is_some_and(|v| !v.is_null()) {
                return false;
            }
        }
        for (key, want) in &self.metadata_equals {
            if job.metadata.get(key).and_then(|v| v.as_str()) != Some(want.as_str()) {
                return false;
            }
        }
        if let Some(step_match) = &self.step
            && !steps.iter().any(|s| step_match.matches(s))
        {
            return false;
        }
        true
    }

    /// Whether evaluating this predicate needs the Job's steps at
    /// all — lets the handler skip the per-job steps fetch for
    /// step-less predicates.
    pub fn needs_steps(&self) -> bool {
        self.step.is_some()
    }

    /// Whether any clause names the [`SELF`] placeholder — i.e. this
    /// is a per-actor station and the queue it serves depends on who
    /// is asking.
    pub fn binds_self(&self) -> bool {
        self.metadata_equals.values().any(|v| v == SELF)
            || self.step.as_ref().is_some_and(|s| {
                s.assignee_id.as_deref() == Some(SELF)
                    || s.metadata_equals.values().any(|v| v == SELF)
            })
    }

    /// Bind [`SELF`] to `actor`, yielding the concrete predicate to
    /// evaluate. Pure.
    ///
    /// - A predicate with no placeholder binds to itself, actor or not.
    /// - `None` means **this station has no queue for this caller**:
    ///   the placeholder is present and there is no identified actor to
    ///   bind it to. Callers render an empty queue; they must never
    ///   fall back to the unbound predicate, which would compare the
    ///   literal `"@me"` against packet data.
    ///
    /// Substitution is confined to the two positions where an actor id
    /// can sensibly appear — a `metadata_equals` value and the step
    /// clause's `assignee_id`. Everywhere else `@me` is just a string,
    /// because everywhere else an actor id would be a category error.
    pub fn bind_self(&self, actor: Option<&str>) -> Option<StationPredicate> {
        if !self.binds_self() {
            return Some(self.clone());
        }
        let actor = actor.filter(|a| !a.is_empty())?;
        let mut bound = self.clone();
        for value in bound.metadata_equals.values_mut() {
            if value == SELF {
                *value = actor.to_string();
            }
        }
        if let Some(step) = &mut bound.step {
            if step.assignee_id.as_deref() == Some(SELF) {
                step.assignee_id = Some(actor.to_string());
            }
            for value in step.metadata_equals.values_mut() {
                if value == SELF {
                    *value = actor.to_string();
                }
            }
        }
        Some(bound)
    }
}

// ---------------------------------------------------------------------------
// Discipline
// ---------------------------------------------------------------------------

/// One key of a station's ordering discipline. A discipline is an
/// array of these, applied lexicographically; ties beyond the
/// declared keys break on the job id so the order is deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisciplineKey {
    /// Packet priority, emergency first.
    Priority,
    /// Oldest `opened_on` first.
    Age,
    /// Earliest `due_on` first; undated packets last.
    Due,
    /// Newest activity first — the reverse of `Age`, and the only key
    /// that reads a packet's *end* as well as its start: activity is
    /// `closed_on` when the packet closed, `opened_on` while it is in
    /// flight.
    ///
    /// Why a queue would want this: `priority, then age` answers "what
    /// should I work on next", which is the question a worker asks of
    /// a queue they pull from. A watchlist is not pulled from — it is
    /// *read*, by someone asking "what became of my packets", and the
    /// answer they came for is whatever moved most recently. Priority
    /// ordering would bury a packet that just closed under an
    /// emergency that has sat open for a week.
    Recency,
}

/// The ratified default: `priority, then age`.
pub fn default_discipline() -> Vec<DisciplineKey> {
    vec![DisciplineKey::Priority, DisciplineKey::Age]
}

/// Priority's queue rank — emergency drains first. Kept here (not an
/// `Ord` on the enum) so the ordering stays a station-discipline
/// concern, not an accidental global.
fn priority_rank(p: Priority) -> u8 {
    match p {
        Priority::Emergency => 0,
        Priority::Urgent => 1,
        Priority::Standard => 2,
        Priority::Scheduled => 3,
    }
}

/// The date a packet last moved: its close date once it closed, its
/// open date while it is in flight.
fn last_activity(job: &Job) -> NaiveDate {
    job.closed_on.unwrap_or(job.opened_on)
}

fn compare_by_key(key: DisciplineKey, a: &Job, b: &Job) -> std::cmp::Ordering {
    match key {
        DisciplineKey::Priority => priority_rank(a.priority).cmp(&priority_rank(b.priority)),
        DisciplineKey::Age => a.opened_on.cmp(&b.opened_on),
        // Descending: newest first.
        DisciplineKey::Recency => last_activity(b).cmp(&last_activity(a)),
        DisciplineKey::Due => match (a.due_on, b.due_on) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        },
    }
}

/// Order `jobs` by the discipline, in place. An empty discipline
/// falls back to the ratified default. Deterministic: after every
/// declared key ties break on the job id's string form.
pub fn order_by_discipline(discipline: &[DisciplineKey], jobs: &mut [Job]) {
    let effective: Vec<DisciplineKey> = if discipline.is_empty() {
        default_discipline()
    } else {
        discipline.to_vec()
    };
    jobs.sort_by(|a, b| {
        for key in &effective {
            let ord = compare_by_key(*key, a, b);
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        a.id.to_string().cmp(&b.id.to_string())
    });
}

// ---------------------------------------------------------------------------
// The terminal window
// ---------------------------------------------------------------------------

/// Whether a packet is inside the station's evaluation window, before
/// the predicate gets a say. Pure — the clock arrives as `today`.
///
/// Stations hold in-flight traffic, so the default universe is packets
/// that have not reached a terminal status. But a queue read by the
/// person who *filed* the packet needs the opposite of vanishing at
/// closure: the terminal state IS the information they came for. A
/// station declaring `terminal_window_days` keeps departed packets
/// visible for that many days after they closed, then lets them age
/// out.
///
/// Three states, not two:
/// - **Terminal** (`Closed` / `Cancelled`) — a member only inside a
///   declared window, measured from `closed_on`. A terminal packet
///   with no close date cannot be placed in the window, so it is out.
/// - **Draft** — neither in flight nor terminal. An unadmitted packet
///   has not entered the network, so it is never a queue member.
/// - Everything else is in flight, and the predicate decides.
pub fn in_window(job: &Job, today: NaiveDate, terminal_window_days: Option<u32>) -> bool {
    match job.status {
        JobStatus::Draft => false,
        JobStatus::Closed | JobStatus::Cancelled => {
            let (Some(days), Some(closed_on)) = (terminal_window_days, job.closed_on) else {
                return false;
            };
            (today - closed_on).num_days() <= i64::from(days)
        }
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// The evaluated queue of one station: ordered packets plus the
/// discipline that ordered them (an operator should never wonder why
/// the queue is in this order) and the advisory bandwidth verdict.
#[derive(Debug, Clone, Serialize)]
pub struct StationQueue {
    pub station: String,
    pub kind: crate::stations::StationKind,
    pub discipline: Vec<DisciplineKey>,
    pub wip_limit: Option<i32>,
    /// Advisory (Q3): true when the queue holds more packets than
    /// `wip_limit`. Never enforced here — lenses warn, telemetry
    /// reads it.
    pub over_limit: bool,
    /// Echoed for the same reason `discipline` is: a reader who finds
    /// a closed packet in a queue should be able to see, from the
    /// envelope, that the station declared it would hold departed
    /// packets this long.
    pub terminal_window_days: Option<u32>,
    /// The station's declared upstream, echoed for the same reason
    /// `discipline` is: a lens that already holds the queue can render
    /// the walk-upstream affordance without a second call to the
    /// registry. `None` = the row declares none, and no button renders.
    pub upstream: Option<crate::stations::StationUpstream>,
    /// The page context for a surface that renders this station whole,
    /// echoed for the same reason `upstream` is: the call that fetched
    /// the packets already answered "what page is this", so the page
    /// draws itself from one response instead of a queue read plus a
    /// registry read. `None` = no surface claims this station.
    pub lens: Option<crate::stations::StationLens>,
    pub total: usize,
    pub data: Vec<Job>,
    /// Members' steps, keyed by job id — present only when the
    /// station's lens declares `with_steps`, empty otherwise.
    ///
    /// A separate map rather than steps nested on each Job because
    /// `Job` is the envelope the whole system passes around and it
    /// does not carry its steps; widening it here would put a second
    /// shape of Job on the wire. A lens that needs them looks them up;
    /// every other lens serialises an empty object and is unaffected.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<String, Vec<Step>>,
}

/// Evaluate a station over a packet universe: keep what is inside the
/// window, filter by the predicate, order by the discipline, wrap in
/// the envelope. Pure — the clock arrives as `today`.
///
/// `packets` is `(job, steps)`; pass an empty step slice when the
/// predicate doesn't need steps ([`StationPredicate::needs_steps`]).
///
/// `spec.predicate` must already be BOUND
/// ([`StationPredicate::bind_self`]): this function has no notion of
/// who is asking, and a station whose placeholder could not bind has
/// an empty universe, not an unbound predicate.
pub fn evaluate_station(
    spec: &crate::stations::StationSpec,
    packets: Vec<(Job, Vec<Step>)>,
    today: NaiveDate,
) -> StationQueue {
    let with_steps = spec.lens.as_ref().is_some_and(|l| l.with_steps);
    let kept: Vec<(Job, Vec<Step>)> = packets
        .into_iter()
        .filter(|(job, steps)| {
            in_window(job, today, spec.terminal_window_days) && spec.predicate.matches(job, steps)
        })
        .collect();
    // Only MEMBERS' steps, and only when asked. A queue that leaked the
    // steps of packets it rejected would be answering a question nobody
    // asked with data the caller's scope was never checked against.
    let steps: BTreeMap<String, Vec<Step>> = if with_steps {
        kept.iter()
            .map(|(job, steps)| (job.id.to_string(), steps.clone()))
            .collect()
    } else {
        BTreeMap::new()
    };
    let mut members: Vec<Job> = kept.into_iter().map(|(job, _)| job).collect();
    order_by_discipline(&spec.discipline, &mut members);
    let discipline = if spec.discipline.is_empty() {
        default_discipline()
    } else {
        spec.discipline.clone()
    };
    StationQueue {
        station: spec.name.clone(),
        kind: spec.kind,
        discipline,
        wip_limit: spec.wip_limit,
        over_limit: spec
            .wip_limit
            .is_some_and(|limit| members.len() as i64 > i64::from(limit)),
        terminal_window_days: spec.terminal_window_days,
        upstream: spec.upstream.clone(),
        lens: spec.lens.clone(),
        total: members.len(),
        steps,
        data: members,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stations::{StationKind, StationSpec};
    use boss_core::job::{JobId, Subject};
    use chrono::{NaiveDate, Utc};

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, d).unwrap()
    }

    fn job(kind: &str, priority: Priority, opened: u32) -> Job {
        let mut j = Job::new(
            kind,
            Subject::new("custom", "x"),
            "t",
            "emp-1",
            priority,
            day(opened),
        );
        j.status = JobStatus::Open;
        j
    }

    /// A packet that reached a terminal step: closed on `day(on)` with
    /// `metadata.outcome` stamped, exactly as `close_job_on_terminal`
    /// leaves it.
    fn closed(mut j: Job, on: u32, outcome: &str) -> Job {
        j.status = JobStatus::Closed;
        j.closed_on = Some(day(on));
        if let serde_json::Value::Object(map) = &mut j.metadata {
            map.insert("outcome".into(), serde_json::json!(outcome));
        } else {
            j.metadata = serde_json::json!({ "outcome": outcome });
        }
        j
    }

    fn step(slug: &str, status: StepStatus) -> Step {
        let mut s = Step::new(JobId::new(), "task", slug, 0);
        s.spec_slug = Some(slug.to_string());
        s.status = status;
        s
    }

    // ------------------------------------------------------------
    // Predicate matching
    // ------------------------------------------------------------

    #[test]
    fn empty_predicate_matches_everything() {
        let p = StationPredicate::default();
        assert!(p.matches(&job("anything", Priority::Standard, 1), &[]));
        assert!(!p.needs_steps());
    }

    #[test]
    fn kind_and_status_clauses_conjoin() {
        let p = StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            ..Default::default()
        };
        assert!(p.matches(&job("ship-a-change", Priority::Standard, 1), &[]));
        assert!(!p.matches(&job("pr-train", Priority::Standard, 1), &[]));
        let mut closed = job("ship-a-change", Priority::Standard, 1);
        closed.status = JobStatus::Closed;
        assert!(!p.matches(&closed, &[]));
    }

    #[test]
    fn tags_any_matches_any_declared_tag() {
        let p = StationPredicate {
            tags_any: vec!["hotfix".into(), "urgent-lane".into()],
            ..Default::default()
        };
        let tagged = job("k", Priority::Standard, 1).with_tags(vec!["urgent-lane".into()]);
        assert!(p.matches(&tagged, &[]));
        let other = job("k", Priority::Standard, 1).with_tags(vec!["routine".into()]);
        assert!(!p.matches(&other, &[]));
        let untagged = job("k", Priority::Standard, 1);
        assert!(!p.matches(&untagged, &[]));
    }

    #[test]
    fn metadata_presence_and_absence() {
        // The loading-dock shape: branch present, train absent.
        let p = StationPredicate {
            metadata_present: vec!["branch".into()],
            metadata_absent: vec!["train".into()],
            ..Default::default()
        };
        let parked =
            job("k", Priority::Standard, 1).with_metadata(serde_json::json!({"branch": "feat/a"}));
        assert!(p.matches(&parked, &[]));
        let boarded = job("k", Priority::Standard, 1)
            .with_metadata(serde_json::json!({"branch": "feat/b", "train": "t1"}));
        assert!(!p.matches(&boarded, &[]));
        let branchless = job("k", Priority::Standard, 1);
        assert!(!p.matches(&branchless, &[]));
        // A null value counts as absent for `present` and as absent
        // for `absent` — null is not a value.
        let null_branch =
            job("k", Priority::Standard, 1).with_metadata(serde_json::json!({"branch": null}));
        assert!(!p.matches(&null_branch, &[]));
        let null_train = job("k", Priority::Standard, 1)
            .with_metadata(serde_json::json!({"branch": "feat/c", "train": null}));
        assert!(p.matches(&null_train, &[]));
    }

    #[test]
    fn step_clause_needs_some_step_matching_every_field() {
        let p = StationPredicate {
            step: Some(StepMatch {
                slug: Some("review".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(p.needs_steps());
        let j = job("k", Priority::Standard, 1);
        assert!(p.matches(&j, &[step("review", StepStatus::Ready)]));
        assert!(p.matches(
            &j,
            &[
                step("pr", StepStatus::Ready),
                step("review", StepStatus::Active)
            ]
        ));
        // Wrong status, wrong slug, or no steps at all: not a member.
        assert!(!p.matches(&j, &[step("review", StepStatus::Completed)]));
        assert!(!p.matches(&j, &[step("pr", StepStatus::Ready)]));
        assert!(!p.matches(&j, &[]));
    }

    #[test]
    fn step_clause_matches_assignee() {
        let p = StationPredicate {
            step: Some(StepMatch {
                assignee_id: Some("emp-7".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut mine = step("work", StepStatus::Active);
        mine.assignee_id = Some("emp-7".into());
        assert!(p.matches(&job("k", Priority::Standard, 1), &[mine]));
        let mut theirs = step("work", StepStatus::Active);
        theirs.assignee_id = Some("emp-9".into());
        assert!(!p.matches(&job("k", Priority::Standard, 1), &[theirs]));
    }

    /// A station has to be able to hold packets on a fact the STEP
    /// carries, not just the Job. The worked case is David's repair
    /// queue (bb86d687, "maybe we need a repair station queue for
    /// trains with errors"): a red train is a `pr-train` whose `ci`
    /// step recorded `result = failing`, and redness lives there and
    /// nowhere else. Without this clause the only way to express it
    /// would be to stamp a second copy of the verdict on the Job,
    /// which is the duplication CLAUDE.md 9a exists to stop.
    #[test]
    fn step_clause_matches_step_metadata() {
        let p = StationPredicate {
            step: Some(StepMatch {
                slug: Some("ci".into()),
                metadata_equals: BTreeMap::from([("result".to_string(), "failing".to_string())]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut red = step("ci", StepStatus::Active);
        red.spec_slug = Some("ci".into());
        red.metadata = serde_json::json!({"result": "failing"});
        assert!(p.matches(&job("pr-train", Priority::Standard, 1), &[red]));

        let mut green = step("ci", StepStatus::Active);
        green.spec_slug = Some("ci".into());
        green.metadata = serde_json::json!({"result": "green"});
        assert!(!p.matches(&job("pr-train", Priority::Standard, 1), &[green]));

        // A step that never recorded a verdict is not red — a pending
        // train must not sit in the repair queue.
        let mut pending = step("ci", StepStatus::Active);
        pending.spec_slug = Some("ci".into());
        assert!(!p.matches(&job("pr-train", Priority::Standard, 1), &[pending]));
    }

    /// The self placeholder must be honoured wherever it can be
    /// written, not only where it was first supported. Adding a new
    /// string-valued clause without extending `binds_self`/`bind_self`
    /// would let `@me` past both: unbound, it would compare literally,
    /// and the module's own warning says what that costs — a packet
    /// whose metadata holds "@me" lands in EVERY actor's queue.
    #[test]
    fn self_placeholder_in_a_step_clause_binds_and_fails_closed() {
        let p = StationPredicate {
            step: Some(StepMatch {
                metadata_equals: BTreeMap::from([("filed_by".to_string(), SELF.to_string())]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(p.binds_self(), "an @me in a step clause must be detected");
        // No actor: no queue. Never a literal comparison.
        assert!(p.bind_self(None).is_none());
        let bound = p.bind_self(Some("emp-7")).expect("binds with an actor");
        assert_eq!(
            bound.step.as_ref().unwrap().metadata_equals.get("filed_by"),
            Some(&"emp-7".to_string()),
        );
        // And the unbound predicate matches nothing, even against a
        // packet that literally carries the placeholder.
        let mut spoof = step("work", StepStatus::Active);
        spoof.metadata = serde_json::json!({"filed_by": SELF});
        assert!(!p.matches(&job("k", Priority::Standard, 1), &[spoof]));
    }

    /// Same rule the Job-level clause follows: only strings compare.
    #[test]
    fn step_metadata_clause_compares_strings_only() {
        let p = StationPredicate {
            step: Some(StepMatch {
                metadata_equals: BTreeMap::from([("result".to_string(), "3".to_string())]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut numeric = step("ci", StepStatus::Active);
        numeric.metadata = serde_json::json!({"result": 3});
        assert!(!p.matches(&job("k", Priority::Standard, 1), &[numeric]));
    }

    #[test]
    fn unknown_predicate_fields_are_rejected_not_ignored() {
        // deny_unknown_fields: a typo'd clause must fail loudly at
        // parse time, not silently match everything.
        let bad = serde_json::json!({"kinds": "ship-a-change"});
        assert!(serde_json::from_value::<StationPredicate>(bad).is_err());
    }

    // ------------------------------------------------------------
    // Discipline ordering
    // ------------------------------------------------------------

    #[test]
    fn default_discipline_is_priority_then_age() {
        let mut jobs = vec![
            job("k", Priority::Standard, 1), // old but standard
            job("k", Priority::Urgent, 5),   // newer urgent
            job("k", Priority::Urgent, 2),   // older urgent
            job("k", Priority::Emergency, 9),
        ];
        order_by_discipline(&default_discipline(), &mut jobs);
        let got: Vec<(Priority, NaiveDate)> =
            jobs.iter().map(|j| (j.priority, j.opened_on)).collect();
        assert_eq!(
            got,
            vec![
                (Priority::Emergency, day(9)),
                (Priority::Urgent, day(2)),
                (Priority::Urgent, day(5)),
                (Priority::Standard, day(1)),
            ]
        );
    }

    #[test]
    fn empty_discipline_falls_back_to_the_default() {
        let mut jobs = vec![
            job("k", Priority::Standard, 1),
            job("k", Priority::Emergency, 2),
        ];
        order_by_discipline(&[], &mut jobs);
        assert_eq!(jobs[0].priority, Priority::Emergency);
    }

    #[test]
    fn due_discipline_puts_undated_last() {
        let mut a = job("k", Priority::Standard, 1);
        a.due_on = Some(day(20));
        let mut b = job("k", Priority::Standard, 1);
        b.due_on = Some(day(12));
        let c = job("k", Priority::Standard, 1); // undated
        let mut jobs = vec![c.clone(), a.clone(), b.clone()];
        order_by_discipline(&[DisciplineKey::Due], &mut jobs);
        assert_eq!(
            jobs.iter().map(|j| j.due_on).collect::<Vec<_>>(),
            vec![Some(day(12)), Some(day(20)), None]
        );
    }

    #[test]
    fn full_ties_break_on_job_id_for_determinism() {
        let a = job("k", Priority::Standard, 1);
        let b = job("k", Priority::Standard, 1);
        let mut forward = vec![a.clone(), b.clone()];
        let mut backward = vec![b, a];
        order_by_discipline(&default_discipline(), &mut forward);
        order_by_discipline(&default_discipline(), &mut backward);
        let f: Vec<String> = forward.iter().map(|j| j.id.to_string()).collect();
        let r: Vec<String> = backward.iter().map(|j| j.id.to_string()).collect();
        assert_eq!(f, r, "same set, same order, whatever the input order");
    }

    #[test]
    fn discipline_keys_are_kebab_on_the_wire() {
        let keys: Vec<DisciplineKey> =
            serde_json::from_value(serde_json::json!(["priority", "age", "due"])).unwrap();
        assert_eq!(
            keys,
            vec![
                DisciplineKey::Priority,
                DisciplineKey::Age,
                DisciplineKey::Due
            ]
        );
    }

    // ------------------------------------------------------------
    // Envelope
    // ------------------------------------------------------------

    fn dock_spec(wip_limit: Option<i32>) -> StationSpec {
        let mut s = StationSpec::draft(
            "loading-dock",
            "Loading dock",
            StationKind::Batch,
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
            Utc::now(),
        );
        s.wip_limit = wip_limit;
        s
    }

    #[test]
    fn evaluate_filters_orders_and_reports_the_discipline() {
        let spec = dock_spec(None);
        let packets = vec![
            (job("ship-a-change", Priority::Standard, 3), vec![]),
            (job("pr-train", Priority::Emergency, 1), vec![]),
            (job("ship-a-change", Priority::Urgent, 5), vec![]),
        ];
        let q = evaluate_station(&spec, packets, day(20));
        assert_eq!(q.station, "loading-dock");
        assert_eq!(q.total, 2, "the pr-train packet is not a member");
        assert_eq!(q.data[0].priority, Priority::Urgent);
        assert_eq!(q.discipline, default_discipline());
        assert!(!q.over_limit, "no wip_limit declared -> never over");
    }

    #[test]
    fn wip_limit_is_advisory_over_limit_flag() {
        let spec = dock_spec(Some(1));
        let packets = vec![
            (job("ship-a-change", Priority::Standard, 1), vec![]),
            (job("ship-a-change", Priority::Standard, 2), vec![]),
        ];
        let q = evaluate_station(&spec, packets, day(20));
        assert_eq!(q.total, 2, "advisory: nothing is dropped");
        assert!(q.over_limit);

        let under = evaluate_station(
            &dock_spec(Some(5)),
            vec![(job("ship-a-change", Priority::Standard, 1), vec![])],
            day(20),
        );
        assert!(!under.over_limit);
    }

    /// The envelope carries the station's upstream pointer for the
    /// same reason it carries `discipline` and `wip_limit`: any lens
    /// that already reads the queue can render the walk-upstream
    /// affordance without a second call to the registry.
    #[test]
    fn the_envelope_carries_the_declared_upstream() {
        let mut spec = dock_spec(None);
        spec.upstream = Some(crate::stations::StationUpstream {
            label: "FEEDBACK".into(),
            href: "/system/feedback".into(),
        });
        let q = evaluate_station(&spec, vec![], day(20));
        let up = q.upstream.expect("declared, so present");
        assert_eq!(up.label, "FEEDBACK");
        assert_eq!(up.href, "/system/feedback");
    }

    #[test]
    fn a_station_with_no_upstream_says_so_rather_than_guessing() {
        let q = evaluate_station(&dock_spec(None), vec![], day(20));
        assert_eq!(q.upstream, None, "no pointer declared, no button");
    }

    /// Same contract as `upstream`, one level up: a surface that
    /// renders a whole page for a station gets its page context from
    /// the same envelope that carried the packets, so the page is one
    /// call and its identity is registry data.
    #[test]
    fn the_envelope_carries_the_declared_lens() {
        let mut spec = dock_spec(None);
        spec.lens = Some(crate::stations::StationLens {
            eyebrow: Some("System Model · Design review".into()),
            title: "Design review".into(),
            subtitle: Some("Open questions, pending decisions, ADRs".into()),
            panels: vec!["rejections".into(), "corpus".into()],
            with_steps: false,
        });
        let q = evaluate_station(&spec, vec![], day(20));
        let lens = q.lens.expect("declared, so present");
        assert_eq!(lens.title, "Design review");
        assert_eq!(lens.panels, vec!["rejections", "corpus"]);
    }

    /// A lens that places packets at the stop they reached needs their
    /// steps, and the queue is the one read that already has them —
    /// the handler fetched them to evaluate the predicate. Echoing
    /// them turns two calls into one.
    #[test]
    fn a_lens_that_asks_for_steps_gets_them_for_members_only() {
        let mut spec = dock_spec(None);
        spec.lens = Some(crate::stations::StationLens {
            eyebrow: None,
            title: "My watchlist".into(),
            subtitle: None,
            panels: vec![],
            with_steps: true,
        });
        let member = job("ship-a-change", Priority::Standard, 1);
        let member_id = member.id.to_string();
        let outsider = job("wholesale-keg-order", Priority::Standard, 1);
        let outsider_id = outsider.id.to_string();
        let q = evaluate_station(
            &spec,
            vec![
                (member, vec![step("review", StepStatus::Ready)]),
                (outsider, vec![step("deliver", StepStatus::Active)]),
            ],
            day(20),
        );
        assert_eq!(q.total, 1, "only the ship-a-change is a dock member");
        assert_eq!(
            q.steps.get(&member_id).map(Vec::len),
            Some(1),
            "the member's steps ride along"
        );
        assert!(
            !q.steps.contains_key(&outsider_id),
            "a packet the predicate rejected must not leak its steps"
        );
    }

    #[test]
    fn a_lens_that_does_not_ask_gets_no_steps() {
        // The default, and the reason it is a default: one list_steps
        // per member is real cost, and most lenses render from the Job.
        let mut spec = dock_spec(None);
        spec.lens = Some(crate::stations::StationLens {
            eyebrow: None,
            title: "Loading dock".into(),
            subtitle: None,
            panels: vec![],
            with_steps: false,
        });
        let q = evaluate_station(
            &spec,
            vec![(
                job("ship-a-change", Priority::Standard, 1),
                vec![step("review", StepStatus::Ready)],
            )],
            day(20),
        );
        assert_eq!(q.total, 1);
        assert!(q.steps.is_empty(), "not asked, not carried");
    }

    #[test]
    fn a_station_with_no_lens_says_so_rather_than_guessing() {
        // A queue nobody renders a page for must not have a page
        // invented for it from its `title`.
        let q = evaluate_station(&dock_spec(None), vec![], day(20));
        assert_eq!(q.lens, None, "no lens declared, no page context");
    }

    // ------------------------------------------------------------
    // metadata_equals + the self placeholder
    // ------------------------------------------------------------

    fn filed_by(who: &str) -> Job {
        job("user-feedback", Priority::Standard, 1)
            .with_metadata(serde_json::json!({ "submitted_by": who }))
    }

    fn watchlist_predicate() -> StationPredicate {
        StationPredicate {
            metadata_equals: BTreeMap::from([("submitted_by".into(), SELF.to_string())]),
            ..Default::default()
        }
    }

    #[test]
    fn metadata_equals_compares_string_values() {
        let p = StationPredicate {
            metadata_equals: BTreeMap::from([("submitted_by".into(), "emp-7".to_string())]),
            ..Default::default()
        };
        assert!(p.matches(&filed_by("emp-7"), &[]));
        assert!(!p.matches(&filed_by("emp-9"), &[]));
        // Key missing entirely.
        assert!(!p.matches(&job("user-feedback", Priority::Standard, 1), &[]));
        // A non-string value never equals a declared string — the
        // clause compares ids, and an id is text.
        let numeric = job("user-feedback", Priority::Standard, 1)
            .with_metadata(serde_json::json!({ "submitted_by": 7 }));
        assert!(!p.matches(&numeric, &[]));
    }

    #[test]
    fn a_static_predicate_binds_to_itself() {
        let p = StationPredicate {
            kind: Some("ship-a-change".into()),
            ..Default::default()
        };
        assert!(!p.binds_self());
        // No placeholder: binding is the identity, with or without an
        // actor. A station that names nobody serves everybody.
        assert_eq!(p.bind_self(Some("emp-7")).as_ref(), Some(&p));
        assert_eq!(p.bind_self(None).as_ref(), Some(&p));
    }

    #[test]
    fn the_self_placeholder_binds_to_the_requesting_actor() {
        let p = watchlist_predicate();
        assert!(p.binds_self());
        let bound = p.bind_self(Some("emp-7")).expect("an actor binds");
        assert!(!bound.binds_self(), "the placeholder is gone once bound");
        assert!(bound.matches(&filed_by("emp-7"), &[]));
        assert!(!bound.matches(&filed_by("emp-9"), &[]));
    }

    #[test]
    fn an_unbound_self_predicate_matches_nothing() {
        // The guest case. Failing to bind must never widen the queue:
        // an unbindable self predicate has NO members, and the literal
        // "@me" must never be compared against packet data.
        assert_eq!(watchlist_predicate().bind_self(None), None);
        assert!(
            !watchlist_predicate().matches(
                &job("user-feedback", Priority::Standard, 1)
                    .with_metadata(serde_json::json!({ "submitted_by": SELF })),
                &[]
            ),
            "an UNBOUND predicate is never evaluated by the handler; if it \
             ever is, it must not match a packet that literally wrote @me"
        );
        // An empty actor id is not an identity either.
        assert_eq!(watchlist_predicate().bind_self(Some("")), None);
    }

    #[test]
    fn the_self_placeholder_binds_the_step_assignee_position() {
        // The other place an actor id belongs: the actor station every
        // executor has (stations.md taxonomy) is one row, not one per
        // person.
        let p = StationPredicate {
            step: Some(StepMatch {
                assignee_id: Some(SELF.into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(p.binds_self());
        let bound = p.bind_self(Some("emp-7")).expect("an actor binds");
        let mut mine = step("work", StepStatus::Active);
        mine.assignee_id = Some("emp-7".into());
        assert!(bound.matches(&job("k", Priority::Standard, 1), &[mine]));
        let mut theirs = step("work", StepStatus::Active);
        theirs.assignee_id = Some("emp-9".into());
        assert!(!bound.matches(&job("k", Priority::Standard, 1), &[theirs]));
    }

    #[test]
    fn metadata_equals_round_trips_on_the_wire() {
        let json = serde_json::json!({"metadata_equals": {"submitted_by": "@me"}});
        let p: StationPredicate = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(p, watchlist_predicate());
        assert_eq!(serde_json::to_value(&p).unwrap(), json);
    }

    #[test]
    fn a_dismissed_packet_leaves_its_filers_watchlist() {
        // The production `my-watchlist` predicate as 130-watchlist-dismiss.sql
        // writes it: filed by me AND not dismissed. Dismissing writes the
        // `watchlist_dismissed` key onto the packet (through the metadata
        // merge), and this `metadata_absent` clause is what makes the row
        // vanish from the list — no per-actor table, no new route.
        let p = StationPredicate {
            metadata_equals: BTreeMap::from([("submitted_by".into(), "emp-7".to_string())]),
            metadata_absent: vec!["watchlist_dismissed".into()],
            ..Default::default()
        };

        // A packet I filed and have not dismissed is on my list.
        assert!(p.matches(&filed_by("emp-7"), &[]));

        // Once I dismiss it, the same packet drops out — and it is
        // idempotent: the flag is only ever "set", so a second dismiss
        // writes the same value and the packet stays gone.
        let dismissed = job("user-feedback", Priority::Standard, 1).with_metadata(
            serde_json::json!({ "submitted_by": "emp-7", "watchlist_dismissed": "true" }),
        );
        assert!(!p.matches(&dismissed, &[]));

        // A single shared flag is safe here precisely because membership
        // is already narrowed to the packet's own filer: no second actor
        // ever sees this packet through this station, so there is no
        // "dismissed by someone else, still shows for me" case to model —
        // someone else's packet is not mine to begin with, dismissed or
        // not. (An array of per-actor ids would be the answer on a station
        // several actors shared; here it would be dead weight.)
        assert!(!p.matches(&filed_by("emp-9"), &[]));
        let theirs_dismissed = job("user-feedback", Priority::Standard, 1).with_metadata(
            serde_json::json!({ "submitted_by": "emp-9", "watchlist_dismissed": "true" }),
        );
        assert!(!p.matches(&theirs_dismissed, &[]));
    }

    // ------------------------------------------------------------
    // Recency discipline
    // ------------------------------------------------------------

    #[test]
    fn recency_orders_newest_activity_first() {
        // Activity is the close date when the packet closed, the open
        // date while it is in flight — so a packet that just reached a
        // terminal state sorts above one opened yesterday.
        let old_open = job("k", Priority::Emergency, 2);
        let new_open = job("k", Priority::Scheduled, 9);
        let closed_yesterday = closed(job("k", Priority::Emergency, 1), 14, "completed");
        let mut jobs = vec![old_open.clone(), new_open.clone(), closed_yesterday.clone()];
        order_by_discipline(&[DisciplineKey::Recency], &mut jobs);
        assert_eq!(
            jobs.iter().map(|j| j.id).collect::<Vec<_>>(),
            vec![closed_yesterday.id, new_open.id, old_open.id],
            "priority is irrelevant here: newest activity leads"
        );
    }

    // ------------------------------------------------------------
    // The terminal window
    // ------------------------------------------------------------

    #[test]
    fn without_a_window_only_in_flight_packets_are_members() {
        let open = job("k", Priority::Standard, 1);
        assert!(in_window(&open, day(20), None));
        assert!(!in_window(
            &closed(open.clone(), 20, "completed"),
            day(20),
            None
        ));
        let mut cancelled = open.clone();
        cancelled.status = JobStatus::Cancelled;
        cancelled.closed_on = Some(day(20));
        assert!(!in_window(&cancelled, day(20), None));
    }

    #[test]
    fn a_window_admits_recent_terminals_and_ages_them_out() {
        let base = job("k", Priority::Standard, 1);
        let window = Some(14);
        // Closed today, and on the last day of the window: in.
        assert!(in_window(
            &closed(base.clone(), 20, "completed"),
            day(20),
            window
        ));
        assert!(in_window(
            &closed(base.clone(), 6, "completed"),
            day(20),
            window
        ));
        // One day past the window: out. The entry does not vanish at
        // closure, it ages out.
        assert!(!in_window(
            &closed(base.clone(), 5, "completed"),
            day(20),
            window
        ));
        // Cancelled is terminal too, and rides the same window.
        let mut cancelled = closed(base.clone(), 19, "completed");
        cancelled.status = JobStatus::Cancelled;
        assert!(in_window(&cancelled, day(20), window));
        // A terminal packet with no close date cannot be placed in the
        // window, so it is out — never in on a guess.
        let mut undated = closed(base.clone(), 19, "completed");
        undated.closed_on = None;
        assert!(!in_window(&undated, day(20), window));
    }

    #[test]
    fn a_draft_packet_is_never_a_member() {
        // Draft is neither in flight nor terminal: an unadmitted packet
        // has not entered the network.
        let mut draft = job("k", Priority::Standard, 1);
        draft.status = JobStatus::Draft;
        assert!(!in_window(&draft, day(20), None));
        assert!(!in_window(&draft, day(20), Some(365)));
    }

    #[test]
    fn a_watchlist_evaluates_open_and_recent_terminal_packets_together() {
        let mut spec = StationSpec::draft(
            "my-watchlist",
            "Packets I filed",
            StationKind::Actor,
            watchlist_predicate(),
            Utc::now(),
        );
        spec.discipline = vec![DisciplineKey::Recency];
        spec.terminal_window_days = Some(14);
        let spec = spec.bind_self(Some("emp-7")).expect("an actor binds");

        let open_mine = filed_by("emp-7");
        let closed_mine = closed(filed_by("emp-7"), 18, "duplicate");
        let stale_mine = closed(filed_by("emp-7"), 2, "declined");
        let closed_theirs = closed(filed_by("emp-9"), 18, "completed");
        let q = evaluate_station(
            &spec,
            vec![
                (open_mine.clone(), vec![]),
                (closed_mine.clone(), vec![]),
                (stale_mine, vec![]),
                (closed_theirs, vec![]),
            ],
            day(20),
        );

        assert_eq!(q.total, 2, "mine, minus the one that aged out");
        assert_eq!(
            q.data.iter().map(|j| j.id).collect::<Vec<_>>(),
            vec![closed_mine.id, open_mine.id],
            "newest activity first: the packet that just closed leads"
        );
        assert_eq!(
            q.terminal_window_days,
            Some(14),
            "the envelope names the window, like it names the discipline"
        );
        assert_eq!(q.data[0].metadata["outcome"], "duplicate");
    }

    #[test]
    fn an_actor_only_ever_sees_their_own_filings() {
        // The property that makes one station row safe for everybody:
        // two actors, one spec, disjoint queues.
        let mut spec = StationSpec::draft(
            "my-watchlist",
            "Packets I filed",
            StationKind::Actor,
            watchlist_predicate(),
            Utc::now(),
        );
        spec.terminal_window_days = Some(14);
        let packets = vec![
            (filed_by("emp-7"), vec![]),
            (filed_by("emp-9"), vec![]),
            (closed(filed_by("emp-9"), 19, "completed"), vec![]),
        ];
        let seven = evaluate_station(
            &spec.bind_self(Some("emp-7")).unwrap(),
            packets.clone(),
            day(20),
        );
        let nine = evaluate_station(&spec.bind_self(Some("emp-9")).unwrap(), packets, day(20));
        assert_eq!(seven.total, 1);
        assert_eq!(nine.total, 2);
    }
}
