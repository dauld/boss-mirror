//! `estate.alarm` — the raiser the comparison series was recorded for.
//!
//! Post-mortem #2's blind spot, twice: on 2026-08-27 the cp-2/cp-3
//! kubelets flapped, boss served 500s for ~25 minutes, six CronJob
//! runs died — and it was found HOURS later, by hand, because every
//! noticing mechanism BOSS has keys off packets and a kubelet is not
//! a packet (a5adfb99). The estate loop already measures: observers
//! post, `estate.compare` records findings as a series on
//! `jobs.estate.compared` — "the series the eventual raiser will be
//! calibrated on (59ef456a, report first, raise later)". This is that
//! raiser: when a HARD finding persists across consecutive
//! comparisons, it becomes an urgent packet the overdue/watchlist
//! machinery can finally see. The algedonic channel BOSS is named
//! for, wired to the cluster itself.
//!
//! THE ALARM HEARS THE HOSTS (a7a19a1a, c6c0f3b1): the 2026-09-02
//! outage's chain 5 was components screaming for hours with nobody
//! interrupted — the host-scope series existed and the raiser ignored
//! them. Three additions closed that:
//! - `units_unhealthy` findings key (`host-units` scope): the quiet
//!   conductor — a unit active-but-functionless or dead where the
//!   observer derived unhealthy — persists like a NotReady node does.
//! - persistence is per SERIES, `(scope, host)`, not per scope: the
//!   self-scoped host series interleave (boss-gcp and the forge both
//!   post `host-units` every five minutes), and keyed by scope alone a
//!   clean row from one host erased the other's evidence, so a host
//!   finding could NEVER survive N consecutive rows. `estate.compare`
//!   stamps `host` on self-scoped comparisons for exactly this filter.
//! - the SILENCE SWEEP: an expected series that stops arriving IS a
//!   finding. A host observation older than [`STALE_MULTIPLIER`]x its
//!   own measured cadence means the observer died or the host did —
//!   either way the alarm's source went dark, and an alarm that only
//!   hears what its sources say dies with its patient. The sweep runs
//!   on EVERY firing (any surviving series triggers it), so one dead
//!   observer is noticed by its neighbors' heartbeats.
//!
//! CALIBRATION, so the alarm is worth trusting:
//! - HARD findings only — `not_ready` (a declared node that is sick),
//!   `declared_not_observed` (a declared node that is GONE),
//!   `disk_tight` (a host below the floor a gate needs),
//!   `units_unhealthy` (a watched unit the observer derived sick),
//!   `dead_letters_unrecorded` (a dispatcher dead-letter with no
//!   durable record anywhere else, 8834804a), and `door_dark` (a door
//!   half dark past its declared band, e6406701 — band-judged, so it
//!   raises on sight rather than after PERSIST_N; see
//!   [`banded_findings`]).
//!   `observed_not_declared` is a paperwork gap and `drift` is config
//!   — real, but not 03:00-urgent, and an alarm that cries over
//!   paperwork trains operators to ignore it.
//! - PERSISTENCE over [`PERSIST_N`] consecutive same-series
//!   comparisons, read back from the recorded series (the SoR is the
//!   state; the handler stays stateless). One flapped reading is
//!   weather; N in a row is a condition. Staleness needs no separate
//!   persistence — [`STALE_MULTIPLIER`] missed cadences IS the
//!   persistence, already integrated over time.
//! - DEDUP against open packets carrying the same `estate_finding`
//!   key: a persisting condition is ONE packet, not one per firing.
//!
//! THE PACKET'S `(scope, host)` IS THE SERIES KEY THE ROWS CARRY, not
//! the series' name. `estate.recover` closes an alarm by finding N
//! clean rows of the SAME series after the raise, matching on the
//! packet's `(scope, host)` exactly as persistence filters rows — so a
//! raise that stamps a key the rows do not carry files an alarm nothing
//! can ever close. The silence sweep did exactly that for the cluster
//! observer (3908d555): `unobserved:kubernetes-nodes` was stamped
//! `host = "kubernetes-nodes"` because the series' NAME is its scope,
//! while every kubernetes-nodes comparison row is host-less — and the
//! alarm class that recovers most often (an observer restart) was the
//! one still closed by hand. A stale entry therefore carries `series`
//! (what it is called — the host for a self-scoped series, the scope
//! for the cluster's) for the key and the title, and `host` only when
//! the rows are stamped with one. The packet describes the series
//! truthfully: the cluster has no host, so its alarm has none.
//!
//! The raise is an URGENT packet on the operator's queue naming host +
//! condition + the latest evidence excerpt. Delivery beyond the queue
//! (push, phone) is deliberately NOT built here — that is channel
//! work for the harness, and the packet is the system-of-record raise
//! it would deliver.
//!
//! A no-op, not an error, when findings are absent, not yet
//! persistent, or already raised.

use boss_jobs::channels::InputChannel;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{api_client, get_json, owner_for_filing, post_json, rows_or_refuse};
use super::estate_compare::{DOOR_SCOPE, HOST_SCOPE, KNOWN_SCOPE, UNITS_SCOPE};

/// Consecutive same-series comparisons a hard finding must survive to
/// raise. Three: at the tightened 15-minute observer cadence that is
/// ~30-45 minutes of a node being gone or NotReady — slower than a
/// pager, far faster than "David asked hours later", and immune to a
/// single flapped reading. (The five-minute unit series raises in ~15
/// minutes; the daily host-disk series in three readings, which is
/// what its cadence affords — tightening that is a timer-file change,
/// not an alarm change.)
pub(super) const PERSIST_N: usize = 3;

/// A series is stale when its newest observation is older than this
/// many of its own measured cadences. Three, like PERSIST_N and for
/// the same reason: one missed firing is weather (a reboot, a timer's
/// AccuracySec), three in a row is a dead observer or a dead host.
const STALE_MULTIPLIER: i64 = 3;

/// Observations a series must have in the read window before silence
/// is judged: with fewer than two gaps there is no measured cadence,
/// only a guess, and a guessed alarm is the crying-wolf class.
const STALE_MIN_OBSERVATIONS: usize = 3;

/// Floor on the measured cadence, so two manual back-to-back posts
/// (seconds apart) cannot make a series look "quiet" minutes later.
const STALE_MIN_CADENCE_S: i64 = 60;

/// The observation series the silence sweep watches: every source the
/// estate loop expects to keep arriving. `true` = self-scoped (one
/// series per host, identity in `nodes[0].id`); `false` = one series
/// for the whole scope. The scope names are `estate_compare`'s own
/// consts — one definition, not a copy.
///
/// The door series (backlog e6406701) is one series for the scope, like
/// the cluster's: an observer that stops probing the door is
/// `unobserved:door`, or the door watch would die as quietly as the
/// door did.
const WATCHED_SERIES: [(&str, bool); 4] = [
    (KNOWN_SCOPE, false),
    (HOST_SCOPE, true),
    (UNITS_SCOPE, true),
    (DOOR_SCOPE, false),
];

pub struct EstateAlarm {
    client: reqwest::Client,
    jobs_base: String,
    /// The `now` the silence sweep measures observation ages against.
    /// The dispatcher is not on the no-wallclock allowlist, so this
    /// comes from the clock service like every other stamp.
    clock: Arc<dyn boss_clock_client::ClockClient>,
    /// Who the packets this handler files are owned by — the platform
    /// owner through the port (backlog 3c23662d), resolved once per
    /// invocation by `common::owner_for_filing`; never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl EstateAlarm {
    pub fn new(
        jobs_base: impl Into<String>,
        clock_url: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            clock: Arc::new(boss_clock_client::ReqwestClockClient::new(clock_url)),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// The entries of one findings array, tolerant of the field being
/// absent (a units comparison has no `not_ready`, and vice versa).
fn entries<'a>(comparison: &'a Value, field: &str) -> impl Iterator<Item = &'a Value> {
    comparison
        .get("findings")
        .and_then(|f| f.get(field))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The HARD findings of one comparison payload as (key, entry) pairs:
/// `not_ready:<id>` / `gone:<id>` / `disk_tight:<id>` /
/// `unit_unhealthy:<host>/<unit>`. The key is the dedup identity; the
/// entry is the evidence excerpt the packet will carry. Ids arrive
/// both bare (`not_ready` pushes strings) and wrapped (`{"id": ...}`),
/// so both are read; anything else is ignored rather than guessed at.
fn hard_findings(comparison: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for (field, prefix) in [
        ("not_ready", "not_ready"),
        ("declared_not_observed", "gone"),
        // The host scope's disk floor (49a8d842): a machine below the
        // headroom a full gate needs is as hard as a sick node.
        ("disk_tight", "disk_tight"),
        // A dead-letter that left NO durable record (8834804a): no
        // packet to annotate, or the annotation write was itself the
        // failure. The dispatcher counted it on its own surface, the
        // cluster observer carried the count here, and this is the
        // reader that owes nothing to the jobs API — the path CLAUDE.md
        // §Diagnosis asks of an arm. Keyed on the dispatcher's id.
        ("dead_letters_unrecorded", "dead_letters_unrecorded"),
        // A door half dark past its declared band (backlog e6406701):
        // keyed `<door>/<half>`, so WHICH half is down is the finding.
        // Band-judged — see [`banded_findings`] — so it raises on sight.
        ("door_dark", "door_dark"),
    ] {
        for v in entries(comparison, field) {
            let id = v
                .as_str()
                .or_else(|| v.get("id").and_then(Value::as_str))
                .unwrap_or("");
            if !id.is_empty() {
                out.push((format!("{prefix}:{id}"), v.clone()));
            }
        }
    }
    // The quiet-conductor class (729329c6): a watched unit the
    // observer derived unhealthy — dead, failed, crash-looping, or
    // active-but-functionless enough that its own health derivation
    // said no. Keyed host + unit: the same unit sick on two hosts is
    // two conditions.
    for v in entries(comparison, "units_unhealthy") {
        let host = v.get("host").and_then(Value::as_str).unwrap_or("");
        let unit = v.get("unit").and_then(Value::as_str).unwrap_or("");
        if !host.is_empty() && !unit.is_empty() {
            out.push((format!("unit_unhealthy:{host}/{unit}"), v.clone()));
        }
    }
    out
}

/// The hard findings whose persistence the INSTRUMENT already
/// integrated over time: a door half is `door_dark` only once it has
/// been dark for its declared band (`infra/estate/doors.toml`, judged in
/// `estate_compare::compare_door`). They raise on the first comparison
/// that carries them — the silence sweep's reasoning ("the staleness
/// window IS the persistence"): counting [`PERSIST_N`] more comparisons
/// on top would make the declared band a lie by ten minutes.
fn banded_findings(comparison: &Value) -> Vec<(String, Value)> {
    hard_findings(comparison)
        .into_iter()
        .filter(|(k, _)| k.starts_with("door_dark:"))
        .collect()
}

/// The keys that say a condition has NOT recovered: every hard key,
/// plus a door half dark again inside its band (`door_dimming`), keyed
/// as the `door_dark` it would become. A door that answered once and
/// went dark again is not answering, and closing its alarm on three
/// dimming readings would re-raise it a quarter-hour later — the flap
/// the band exists to absorb. `estate.recover` judges by this set.
pub(super) fn unrecovered_keys(comparison: &Value) -> BTreeSet<String> {
    let mut keys = hard_finding_keys(comparison);
    keys.extend(
        entries(comparison, "door_dimming")
            .filter_map(|v| v.get("id").and_then(Value::as_str))
            .map(|id| format!("door_dark:{id}")),
    );
    keys
}

/// Just the keys of [`hard_findings`] — the set the persistence
/// intersection runs over.
pub(super) fn hard_finding_keys(comparison: &Value) -> BTreeSet<String> {
    hard_findings(comparison)
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

/// The keys present in EVERY one of the `n` most recent same-SERIES
/// comparisons — the persistence test, pure. A series is `(scope,
/// host)`: the self-scoped host series interleave in the recorded
/// stream, and matching on scope alone lets host B's clean row erase
/// host A's evidence (`host: None` matches the cluster scope's
/// host-less rows). `comparisons` arrives newest-first (the API's
/// order); fewer than `n` same-series rows means not enough evidence,
/// so nothing persists.
fn persistent_keys(
    comparisons: &[Value],
    scope: &str,
    host: Option<&str>,
    n: usize,
) -> BTreeSet<String> {
    let same_series: Vec<&Value> = comparisons
        .iter()
        .filter(|c| {
            c.get("scope").and_then(Value::as_str) == Some(scope)
                && c.get("host").and_then(Value::as_str) == host
        })
        .take(n)
        .collect();
    if same_series.len() < n {
        return BTreeSet::new();
    }
    let mut iter = same_series.iter();
    let mut keys = iter
        .next()
        .map(|c| hard_finding_keys(c))
        .unwrap_or_default();
    for c in iter {
        let these = hard_finding_keys(c);
        keys = keys.intersection(&these).cloned().collect();
    }
    keys
}

/// The series in one scope's observation rows whose newest reading is
/// older than [`STALE_MULTIPLIER`]x the series' own measured cadence —
/// the silence test, pure. Rows are event envelopes (`payload` +
/// envelope `timestamp`); the cadence is the median gap between
/// consecutive observations, so the test needs no copy of any timer
/// file's schedule and survives the schedule changing. Returns one
/// entry per stale series: series, scope, last_observed_at, cadence_s,
/// age_s — the evidence the raise will carry — plus `host` on a
/// per-host series only. `series` is what the series is CALLED (the
/// host for a self-scoped series, the scope for the cluster's); `host`
/// is what its comparison rows are STAMPED with, and the cluster's
/// rows carry none (3908d555).
fn stale_series(rows: &[Value], per_host: bool, scope: &str, now: DateTime<Utc>) -> Vec<Value> {
    let mut series: BTreeMap<String, Vec<DateTime<Utc>>> = BTreeMap::new();
    for row in rows {
        let payload = row.get("payload").unwrap_or(row);
        let id = if per_host {
            payload
                .get("nodes")
                .and_then(|n| n.get(0))
                .and_then(|n| n.get("id"))
                .and_then(Value::as_str)
        } else {
            Some(scope)
        };
        let Some(id) = id else { continue };
        let ts = payload
            .get("observed_at")
            .and_then(Value::as_str)
            .or_else(|| row.get("timestamp").and_then(Value::as_str))
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
        let Some(ts) = ts else { continue };
        series
            .entry(id.to_string())
            .or_default()
            .push(ts.with_timezone(&Utc));
    }
    let mut out = Vec::new();
    for (id, mut times) in series {
        times.sort_unstable_by(|a, b| b.cmp(a)); // newest first
        if times.len() < STALE_MIN_OBSERVATIONS {
            continue;
        }
        let mut gaps: Vec<i64> = times
            .windows(2)
            .map(|w| (w[0] - w[1]).num_seconds())
            .collect();
        gaps.sort_unstable();
        let cadence = gaps[gaps.len() / 2].max(STALE_MIN_CADENCE_S);
        let age = (now - times[0]).num_seconds();
        if age > STALE_MULTIPLIER * cadence {
            let mut entry = json!({
                "series": id,
                "scope": scope,
                "last_observed_at": times[0].to_rfc3339(),
                "cadence_s": cadence,
                "age_s": age,
            });
            if per_host {
                entry["host"] = json!(id);
            }
            out.push(entry);
        }
    }
    out
}

/// How long a finding a HUMAN settled as `stale` or `duplicate` stays
/// settled. The bastion's retired boss-train.service raised the same
/// alarm at 05:07 and again at 16:0x on 2026-09-05, both closed as
/// "retired by design" — a condition the estate registry already
/// states, re-raised every three comparisons because nothing converges
/// the bastion yet. An alarm the operator has just answered is noise
/// until the answer can change; a week is that horizon here. A packet
/// closed as BUILT (the condition was fixed) does not suppress: a
/// recurrence after a fix is a new fact.
const SETTLED_DAYS: i64 = 7;

/// The dedup read's page size. One bounded read (`closed_within`, so
/// open packets plus only the last [`SETTLED_DAYS`] of closed ones)
/// stays well under this in steady state; a `total` past it trips the
/// truncation HOLD in [`dedup_page_complete`] rather than raising blind.
/// This is the jobs API's own `MAX_LIMIT`, the largest page it serves.
pub(super) const DEDUP_PAGE: usize = 1000;

/// `estate_finding` keys whose packet a HUMAN closed as `stale` or
/// `duplicate` within [`SETTLED_DAYS`] — pure over the closed listing.
///
/// A close `estate.recover` made is excluded (ef421cd3): it closes a
/// recovered alarm as `stale` too, stamped `cleared_by`, and the
/// machine's answer must not suppress the next genuine raise of the
/// same finding. A unit that recovers and dies again inside the day
/// is two conditions, not one settled question.
fn settled_recently(closed_jobs: &[Value], now: DateTime<Utc>) -> BTreeSet<String> {
    closed_jobs
        .iter()
        .filter_map(|j| {
            let key = j
                .get("metadata")?
                .get("estate_finding")?
                .as_str()?
                .to_string();
            let closed_on = j.get("closed_on")?.as_str()?;
            let closed = chrono::NaiveDate::parse_from_str(closed_on, "%Y-%m-%d").ok()?;
            if (now.date_naive() - closed).num_days() > SETTLED_DAYS {
                return None;
            }
            let steps = j.get("steps").and_then(Value::as_array);
            let settled = steps
                .into_iter()
                .flatten()
                .filter_map(|s| s.get("metadata")?.get("disposition")?.as_str())
                .any(|d| d == "stale" || d == "duplicate");
            let machine_cleared = steps
                .into_iter()
                .flatten()
                .filter_map(|s| s.get("metadata")?.get("cleared_by")?.as_str())
                .any(|c| c == super::estate_recover::CLEARED_BY);
            (settled && !machine_cleared).then_some(key)
        })
        .collect()
}

/// `estate_finding` keys already carried by an open packet — the dedup
/// set, pure over the jobs listing.
fn already_raised(open_jobs: &[Value]) -> BTreeSet<String> {
    open_jobs
        .iter()
        .filter_map(|j| {
            j.get("metadata")?
                .get("estate_finding")?
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

/// Did the dedup read return every matching row? A page that came back
/// shorter than the list's own `total` (`rows < total`) was TRUNCATED
/// and cannot prove a finding unraised — treating it as complete
/// re-files an alarm sitting beyond the page every ~10-min pass (the
/// notification flood this guard exists to stop). The caller HOLDs on
/// `false`, the same posture it already takes on a failed fetch.
fn dedup_page_complete(rows: usize, total: usize) -> bool {
    rows >= total
}

/// A compact, bounded rendering of one finding entry — the "latest
/// evidence excerpt" the packet carries so the operator reads the
/// reading, not just its name.
fn excerpt(entry: &Value) -> String {
    let s = entry.to_string();
    if s.chars().count() > 400 {
        let mut t: String = s.chars().take(400).collect();
        t.push('…');
        t
    } else {
        s
    }
}

/// The urgent packet one persistent finding becomes. The key names
/// host + condition (`disk_tight:forge-host`,
/// `unit_unhealthy:boss-gcp/boss-train.service`), so the title does
/// too; the excerpt is the finding's entry from the TRIGGERING
/// comparison — the latest reading, not a stale one.
fn alarm_body(
    key: &str,
    scope: &str,
    host: Option<&str>,
    evidence: &str,
    excerpt: &str,
    owner: &str,
) -> Value {
    let mut metadata = json!({
        "area": "estate",
        "estate_finding": key,
        "scope": scope,
        "detail": format!(
            "Raised by estate.alarm (a5adfb99, the raiser 59ef456a's series was \
             recorded for): the finding `{key}` appeared in {PERSIST_N} consecutive \
             `{scope}` comparisons — a condition, not a flap. The 2026-08-27 class \
             (kubelet flap, 25min of 500s, found hours later by hand) now files \
             itself while it is happening. Latest reading: {excerpt}. Evidence: \
             {evidence}. The observation and comparison series at \
             /api/estate/observations and /api/estate/comparisons carry the full \
             readings."
        ),
    });
    if let (Some(h), Some(obj)) = (host, metadata.as_object_mut()) {
        obj.insert("host".into(), json!(h));
    }
    json!({
        "kind": "backlog-item",
        "title": format!("ESTATE ALARM: {key} persisted {PERSIST_N} consecutive comparisons"),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": super::common::with_lane(metadata, InputChannel::Telemetry),
    })
}

/// The urgent packet one door half dark past its band becomes (backlog
/// e6406701). The title names the half, what it points at and the band
/// — WHICH door is down is the first question, and a person reading the
/// queue should not have to open the packet to learn it. No persistence
/// count: the band is the persistence. No `host`: the door series' rows
/// carry none, and `estate.recover` matches the packet to them by
/// `(scope, host)` (3908d555).
fn door_body(key: &str, entry: &Value, evidence: &str, owner: &str) -> Value {
    let text = |k: &str| entry.get(k).and_then(Value::as_str).unwrap_or("?");
    let half = text("half");
    let target = text("target");
    let band = entry
        .get("band_s")
        .and_then(Value::as_i64)
        .map(|s| format!("{}-minute", s / 60))
        .unwrap_or_else(|| "undeclared".to_string());
    let metadata = json!({
        "area": "estate",
        "estate_finding": key,
        "scope": DOOR_SCOPE,
        "detail": format!(
            "Raised by estate.alarm from the door series (backlog e6406701; \
             incident 55d001b0, where both of the dev pod's ssh doors were dark \
             ~36h and a person found it). The {half} half of door `{door}` \
             ({target}) has been dark since {since}, past its {band} band \
             declared in infra/estate/doors.toml. Why the probe failed: \
             {reason}. The prober is infra/estate/observe-door.sh on the forge, \
             outside the pod, so this alarm does not depend on the pod it is \
             about. It closes itself once the half answers again \
             (estate.recover). Latest reading: {latest}. Evidence: {evidence}. \
             The series rides /api/estate/comparisons?scope=door.",
            door = text("door"),
            since = text("dark_since"),
            reason = text("reason"),
            latest = excerpt(entry),
        ),
    });
    json!({
        "kind": "backlog-item",
        "title": format!(
            "ESTATE ALARM: {key} — the {half} half ({target}) is dark past its {band} band"
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": super::common::with_lane(metadata, InputChannel::Telemetry),
    })
}

/// The dedup key of one stale series: `unobserved:<series>` — one
/// condition per quiet host, even when both of its series go dark;
/// `unobserved:kubernetes-nodes` for the cluster observer.
fn unobserved_key(stale: &Value) -> String {
    format!(
        "unobserved:{}",
        stale
            .get("series")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    )
}

/// The urgent packet one quiet series becomes. No persistence count in
/// the title — the staleness window IS the persistence.
///
/// `host` is stamped only when the series has one. The packet's
/// `(scope, host)` is the key `estate.recover` matches it to its
/// comparison rows by, so it must be the key the ROWS carry: the
/// cluster scope's rows are host-less, and a cluster alarm stamped with
/// the series name as its host matched no series and never auto-closed
/// (3908d555). The title and detail still name the series.
fn staleness_body(stale: &Value, evidence: &str, owner: &str) -> Value {
    let series = stale
        .get("series")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let scope = stale.get("scope").and_then(Value::as_str).unwrap_or("?");
    let mut metadata = json!({
        "area": "estate",
        "estate_finding": unobserved_key(stale),
        "scope": scope,
        "detail": format!(
            "Raised by estate.alarm's silence sweep (a7a19a1a: an alarm that only \
             hears what its sources say dies with its patient — an expected series \
             that stops arriving IS a finding). The `{scope}` observation series \
             for `{series}` went quiet: {evidence_json}. Either the observer died \
             (the quiet-observer class) or the host itself is down; either way \
             nothing downstream of this series can see that host any more. \
             Evidence: {evidence}. The series rides \
             /api/estate/observations?scope={scope}.",
            evidence_json = excerpt(stale),
        ),
    });
    if let (Some(h), Some(obj)) = (
        stale.get("host").and_then(Value::as_str),
        metadata.as_object_mut(),
    ) {
        obj.insert("host".into(), json!(h));
    }
    json!({
        "kind": "backlog-item",
        "title": format!(
            "ESTATE ALARM: {series} unobserved — {scope} series quiet past \
             {STALE_MULTIPLIER}x cadence"
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": super::common::with_lane(metadata, InputChannel::Telemetry),
    })
}

#[async_trait]
impl Handler for EstateAlarm {
    fn name(&self) -> &'static str {
        "estate.alarm"
    }

    async fn invoke(
        &self,
        _args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let comparison = &ctx.event_payload;
        let scope = comparison
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // An unscoped payload is not an estate comparison at all.
        if scope.is_empty() {
            return Ok(());
        }
        let host = comparison.get("host").and_then(Value::as_str);
        let evidence = format!(
            "triggering event {} on topic {}",
            ctx.triggering_event_id, ctx.triggering_topic
        );
        let owner = owner_for_filing(self.owner.as_ref(), &ctx.rule_name).await;

        // First-key-wins map: two stale series on one host collapse to
        // one raise before the dedup fetch ever runs.
        let mut to_raise: BTreeMap<String, Value> = BTreeMap::new();
        // Best-effort accumulator (2026-09-06). A single scope's fetch or
        // a single finding's POST must never block the others: the alarm
        // system silently blocking its own alarms is the worst failure
        // mode for the thing meant to catch a dead conductor. Sub-ops fail
        // soft into here; every one that failed is surfaced as ONE error
        // at the end, so a transient still NAKs for retry (dedup makes the
        // retry idempotent) while every alarm that COULD raise, did.
        let mut errors: Vec<String> = Vec::new();

        // --- The persistence half: does the TRIGGERING comparison's
        // finding survive the last PERSIST_N of its own series? Only
        // worth a fetch when it found something hard at all.
        //
        // A band-judged finding (a door half dark past its declared band,
        // backlog e6406701) needs no series read: the band already
        // integrated it over time. It goes straight to the dedup below.
        let banded = banded_findings(comparison);
        for (key, entry) in &banded {
            to_raise
                .entry(key.clone())
                .or_insert_with(|| door_body(key, entry, &evidence, &owner));
        }
        let hard: Vec<(String, Value)> = hard_findings(comparison)
            .into_iter()
            .filter(|(k, _)| !banded.iter().any(|(b, _)| b == k))
            .collect();
        if !hard.is_empty() {
            // The recorded series IS the state (the handler keeps
            // none). Scope travels down in the query — a page across
            // all scopes is spent by whichever series ticks fastest.
            //
            // A series with no `data` array is NO ANSWER, and it fails
            // soft exactly as a failed fetch does. Read as an empty series
            // nothing persisted and the pass ACKed clean — the alarm
            // silently not alarming (backlog 37fc5837).
            let recent = get_json(
                &self.client,
                &format!(
                    "{}/api/estate/comparisons?scope={scope}&limit=20",
                    self.base()
                ),
                &ctx.rule_name,
            )
            .await
            .and_then(|recent| {
                rows_or_refuse::<Value>(&recent, &format!("the comparisons read (scope {scope})"))
                    .map_err(HandlerError::Downstream)
            });
            match recent {
                Ok(rows) => {
                    // Rows are event envelopes; the comparison rides in
                    // `payload` (recorded verbatim by the dumb door). Fall
                    // back to the row itself so a flattened future shape
                    // keeps working.
                    let payloads: Vec<Value> = rows
                        .iter()
                        .map(|r| r.get("payload").cloned().unwrap_or_else(|| r.clone()))
                        .collect();
                    for key in persistent_keys(&payloads, scope, host, PERSIST_N) {
                        let latest = hard
                            .iter()
                            .find(|(k, _)| *k == key)
                            .map(|(_, e)| excerpt(e))
                            .unwrap_or_default();
                        let body = alarm_body(&key, scope, host, &evidence, &latest, &owner);
                        to_raise.entry(key).or_insert(body);
                    }
                }
                Err(e) => errors.push(format!(
                    "persistence-half comparisons fetch (scope {scope}) failed; the silence half still ran: {e}"
                )),
            }
        }

        // --- The silence half, on EVERY firing: any surviving series'
        // heartbeat is the clock that notices a dead neighbor. Gating
        // this on the triggering comparison having findings would make
        // the sweep run least when the estate looks healthiest — which
        // is exactly when a dead observer is lying loudest.
        let now = boss_clock_client::now_from(&self.clock).await;
        for (watched_scope, per_host) in WATCHED_SERIES {
            // The same refusal, failing soft the same way: an error-shaped
            // series read as empty is a dead observer nobody hears about
            // (backlog 37fc5837).
            let obs = get_json(
                &self.client,
                &format!(
                    "{}/api/estate/observations?scope={watched_scope}&limit=50",
                    self.base()
                ),
                &ctx.rule_name,
            )
            .await
            .and_then(|obs| {
                rows_or_refuse::<Value>(
                    &obs,
                    &format!("the observations read (scope {watched_scope})"),
                )
                .map_err(HandlerError::Downstream)
            });
            let rows = match obs {
                Ok(rows) => rows,
                Err(e) => {
                    errors.push(format!(
                        "observation fetch for scope {watched_scope} failed; other scopes still checked: {e}"
                    ));
                    continue;
                }
            };
            for stale in stale_series(&rows, per_host, watched_scope, now) {
                let body = staleness_body(&stale, &evidence, &owner);
                to_raise.entry(unobserved_key(&stale)).or_insert(body);
            }
        }

        // Dedup + raise, only when there is something to raise. The dedup
        // read is a safety prerequisite — without it a re-raise storms
        // duplicates — so if it FAILS, or comes back TRUNCATED, we HOLD
        // this pass's findings for retry rather than raise blind.
        //
        // ONE bounded read answers both dedup questions: `closed_within`
        // returns every open backlog-item PLUS anything closed in the
        // last SETTLED_DAYS — exactly the window `settled_recently` asks
        // about. It replaces two `limit=200` reads that treated a
        // truncated page as the whole set. That was doubly blind: the
        // closed set grows without bound, so `status=closed&limit=200`
        // silently stopped seeing anything settled beyond row 200, and
        // once open backlog-items passed 200 the open dedup missed its
        // OWN alarm and re-filed it every ~10-min pass (the notification
        // flood). Bounding the read to the recency window keeps it a
        // page or two; a genuine overflow past DEDUP_PAGE trips the
        // truncation HOLD below rather than re-raising blind.
        if !to_raise.is_empty() {
            let listing = get_json(
                &self.client,
                &format!(
                    "{}/api/jobs?kind=backlog-item&closed_within={SETTLED_DAYS}&limit={DEDUP_PAGE}",
                    self.base()
                ),
                &ctx.rule_name,
            )
            .await
            // No `data` array is no answer, and HOLDS like a failed read.
            // It was held before only by accident of the truncation
            // check (a count with no rows looks short); an error body
            // carrying `total: 0` would have passed that and raised
            // blind (d4698bc2).
            .and_then(|body| {
                rows_or_refuse::<Value>(&body, "the dedup read (GET /api/jobs)")
                    .map(|rows| (rows, body))
                    .map_err(HandlerError::Downstream)
            });
            match listing {
                Ok((rows, body)) => {
                    // The list's own `total` is authoritative over the
                    // page length; a missing `total` is treated as
                    // truncated (fail-safe), never as zero.
                    let complete = body
                        .get("total")
                        .and_then(Value::as_u64)
                        .map(|t| usize::try_from(t).unwrap_or(usize::MAX))
                        .is_some_and(|total| dedup_page_complete(rows.len(), total));
                    if !complete {
                        let total = body.get("total").and_then(Value::as_u64);
                        errors.push(format!(
                            "dedup read truncated ({} rows, total {total:?}); {} finding(s) held for retry to avoid duplicate alarms",
                            rows.len(),
                            to_raise.len()
                        ));
                    } else {
                        // Partition by status: open packets carry the
                        // live `estate_finding` dedup keys; the
                        // recently-closed ones carry the settled-answer
                        // keys. `status=open` matches the old open read
                        // exactly (a cancelled packet never deduped).
                        let (open_rows, closed_rows): (Vec<Value>, Vec<Value>) = rows
                            .into_iter()
                            .partition(|j| j.get("status").and_then(Value::as_str) == Some("open"));
                        let mut raised = already_raised(&open_rows);
                        raised.extend(settled_recently(&closed_rows, now));

                        for (key, body) in to_raise {
                            if raised.contains(&key) {
                                tracing::info!(finding = %key, "estate.alarm: already raised or recently settled — not re-raising");
                                continue;
                            }
                            if let Err(e) = post_json(
                                &self.client,
                                &format!("{}/api/jobs", self.base()),
                                &body,
                                &ctx.rule_name,
                            )
                            .await
                            {
                                errors.push(format!(
                                    "raise for {key} failed; other findings still raised: {e}"
                                ));
                                continue;
                            }
                            tracing::info!(finding = %key, scope, "estate.alarm raised a packet");
                        }
                    }
                }
                Err(e) => {
                    errors.push(format!(
                        "dedup fetch failed; {} finding(s) held for retry to avoid duplicate alarms ({e})",
                        to_raise.len()
                    ));
                }
            }
        }

        // One aggregated exit. Every alarm that could raise did; any
        // sub-op that failed NAKs the firing for redelivery, where dedup
        // makes the retry idempotent. Silence is never the result of one
        // bad scope or one bad POST.
        if errors.is_empty() {
            Ok(())
        } else {
            Err(HandlerError::Downstream(format!(
                "estate.alarm: {} sub-operation(s) failed this pass (every alarm that could raise did; the rest retry): {}",
                errors.len(),
                errors.join(" | ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn comparison(scope: &str, not_ready: &[&str], gone: &[&str]) -> Value {
        json!({
            "scope": scope,
            "findings": {
                "not_ready": not_ready,
                "declared_not_observed": gone.iter().map(|g| json!({"id": g})).collect::<Vec<_>>(),
                "observed_not_declared": [{"id": "paperwork-only"}],
                "drift": [{"id": "w-1", "fields": {}}],
            }
        })
    }

    /// A self-scoped units comparison as `estate.compare` records it:
    /// scope + host stamp + units_unhealthy findings.
    fn units_comparison(host: &str, unhealthy: &[&str]) -> Value {
        json!({
            "scope": "host-units",
            "host": host,
            "findings": {
                "units_unhealthy": unhealthy.iter().map(|u| json!({
                    "host": host, "unit": u,
                    "load_state": "loaded", "active_state": "inactive",
                    "sub_state": "dead", "result": "success",
                })).collect::<Vec<_>>(),
            }
        })
    }

    fn host_comparison(host: &str, disk_tight: bool) -> Value {
        let tight: Vec<Value> = if disk_tight {
            vec![json!({"id": host, "free_gb": 8, "disk_gb": 228})]
        } else {
            vec![]
        };
        json!({
            "scope": "host",
            "host": host,
            "findings": { "disk_tight": tight, "not_ready": [] }
        })
    }

    #[test]
    fn only_hard_findings_key_and_both_id_shapes_are_read() {
        let keys = hard_finding_keys(&comparison("kubernetes-nodes", &["cp-2"], &["w-9"]));
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec!["gone:w-9".to_string(), "not_ready:cp-2".to_string()],
            "paperwork (observed_not_declared) and drift must not alarm"
        );
    }

    #[test]
    fn an_unrecorded_dead_letter_is_hard_and_an_unread_dispatcher_is_not() {
        // 8834804a: a dead-letter with no packet, or whose annotation
        // write failed, has the dispatcher's own counter as its only
        // record that outlives the log line. estate.compare carries it
        // into the cluster series as `dead_letters_unrecorded`; it has
        // to be HARD here or the path ends one hop short of a reader.
        // `dispatcher_unread` is the best-effort read going dark —
        // informational, like disk_unmeasured, so it does not wake anyone.
        let mut c = comparison("kubernetes-nodes", &[], &[]);
        c["findings"]["dead_letters_unrecorded"] = json!([{
            "id": "boss-dispatcher", "dead_letters": 5,
            "dead_letters_unrecorded": 2, "age_s": 600 }]);
        c["findings"]["dispatcher_unread"] = json!(null);
        assert_eq!(
            hard_finding_keys(&c).into_iter().collect::<Vec<_>>(),
            vec!["dead_letters_unrecorded:boss-dispatcher".to_string()],
        );
        let mut c = comparison("kubernetes-nodes", &[], &[]);
        c["findings"]["dead_letters_unrecorded"] = json!([]);
        c["findings"]["dispatcher_unread"] = json!("curl: (7) Failed to connect");
        assert!(hard_finding_keys(&c).is_empty(), "unread is informational");
    }

    #[test]
    fn an_unhealthy_unit_keys_by_host_and_unit() {
        // The quiet conductor: dead boss-train.service on boss-gcp must
        // be a different condition than the same unit dead elsewhere.
        let keys = hard_finding_keys(&units_comparison("boss-gcp", &["boss-train.service"]));
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec!["unit_unhealthy:boss-gcp/boss-train.service".to_string()],
        );
    }

    #[test]
    fn a_finding_must_survive_every_one_of_the_last_n() {
        let c = |nr: &[&str]| comparison("kubernetes-nodes", nr, &[]);
        // Newest-first: present, present, present — persists.
        let steady = [c(&["cp-2"]), c(&["cp-2"]), c(&["cp-2"])];
        assert!(persistent_keys(&steady, "kubernetes-nodes", None, 3).contains("not_ready:cp-2"));
        // A flap (missing in the middle reading) does not.
        let flap = [c(&["cp-2"]), c(&[]), c(&["cp-2"])];
        assert!(persistent_keys(&flap, "kubernetes-nodes", None, 3).is_empty());
    }

    #[test]
    fn too_few_same_scope_rows_is_not_enough_evidence() {
        let c = comparison("kubernetes-nodes", &["cp-2"], &[]);
        let other = comparison("forge-host", &["cp-2"], &[]);
        // Two matching + one other scope: only 2 of scope — no alarm.
        let rows = [c.clone(), other, c.clone()];
        assert!(persistent_keys(&rows, "kubernetes-nodes", None, 3).is_empty());
    }

    #[test]
    fn persistence_is_per_host_so_interleaved_series_cannot_erase_each_other() {
        // The defect this car fixes: boss-gcp and the forge both post
        // host-units every five minutes. Keyed by scope alone, the
        // forge's CLEAN rows sit between boss-gcp's sick ones and the
        // intersection goes empty — a dead conductor unit NEVER raises.
        let rows = [
            units_comparison("boss-gcp", &["boss-train.service"]),
            units_comparison("forge-host", &[]),
            units_comparison("boss-gcp", &["boss-train.service"]),
            units_comparison("forge-host", &[]),
            units_comparison("boss-gcp", &["boss-train.service"]),
        ];
        let keys = persistent_keys(&rows, "host-units", Some("boss-gcp"), 3);
        assert!(keys.contains("unit_unhealthy:boss-gcp/boss-train.service"));
        // And the forge's own clean series raises nothing.
        assert!(persistent_keys(&rows, "host-units", Some("forge-host"), 3).is_empty());
    }

    #[test]
    fn disk_tight_persists_per_host_too() {
        // The forge's 8GB moment, interleaved with a healthy conductor.
        let rows = [
            host_comparison("forge-host", true),
            host_comparison("boss-gcp", false),
            host_comparison("forge-host", true),
            host_comparison("boss-gcp", false),
            host_comparison("forge-host", true),
        ];
        let keys = persistent_keys(&rows, "host", Some("forge-host"), 3);
        assert!(keys.contains("disk_tight:forge-host"));
    }

    #[test]
    fn a_units_flap_below_n_does_not_raise() {
        let rows = [
            units_comparison("boss-gcp", &["boss-train.service"]),
            units_comparison("boss-gcp", &[]),
            units_comparison("boss-gcp", &["boss-train.service"]),
        ];
        assert!(persistent_keys(&rows, "host-units", Some("boss-gcp"), 3).is_empty());
    }

    #[test]
    fn rows_without_a_host_stamp_are_not_evidence_for_a_hosts_series() {
        // The migration window: comparisons recorded before the host
        // stamp landed are anonymous. They must not count toward (nor
        // against) any host's series — the alarm waits for N stamped
        // rows instead of guessing.
        let mut old = units_comparison("boss-gcp", &["boss-train.service"]);
        old.as_object_mut().unwrap().remove("host");
        let rows = [
            units_comparison("boss-gcp", &["boss-train.service"]),
            units_comparison("boss-gcp", &["boss-train.service"]),
            old,
        ];
        assert!(persistent_keys(&rows, "host-units", Some("boss-gcp"), 3).is_empty());
    }

    #[test]
    fn an_open_packet_with_the_key_suppresses_a_second() {
        let open = [json!({"metadata": {"estate_finding": "not_ready:cp-2"}})];
        assert!(already_raised(&open).contains("not_ready:cp-2"));
    }

    #[test]
    fn dedup_page_complete_only_trusts_a_whole_page() {
        assert!(
            dedup_page_complete(0, 0),
            "nothing matched is a complete answer"
        );
        assert!(dedup_page_complete(50, 50));
        assert!(dedup_page_complete(250, 250));
        assert!(
            dedup_page_complete(260, 250),
            "an over-count still covers the whole set"
        );
        assert!(
            !dedup_page_complete(200, 250),
            "a 200-of-250 page is truncated — it cannot prove a finding unraised"
        );
    }

    #[test]
    fn a_truncated_dedup_page_holds_rather_than_reraising() {
        // The flood shape: 250 open alarms, and the one already carrying
        // our finding sits at row 220 — beyond a 200-row page.
        let key = "disk_tight:forge-host";
        let mut open: Vec<Value> = (0..250)
            .map(
                |i| json!({"status": "open", "metadata": {"estate_finding": format!("other:{i}")}}),
            )
            .collect();
        open[220] = json!({"status": "open", "metadata": {"estate_finding": key}});

        // A COMPLETE read (all 250) finds the existing alarm and dedups.
        assert!(dedup_page_complete(open.len(), 250));
        assert!(
            already_raised(&open).contains(key),
            "a complete read sees the alarm on page three"
        );

        // A TRUNCATED read (first 200 rows) MISSES it — this is the
        // defect: `already_raised` returns "not raised" and the alarm
        // re-files every pass...
        let truncated: Vec<Value> = open.iter().take(200).cloned().collect();
        assert!(
            !already_raised(&truncated).contains(key),
            "the truncated page cannot see the alarm beyond row 200"
        );
        // ...so the completeness guard refuses to trust the page and the
        // handler HOLDs instead of re-filing.
        assert!(
            !dedup_page_complete(truncated.len(), 250),
            "a 200-of-250 read is truncated; the handler must HOLD, not re-file"
        );
    }

    #[test]
    fn the_alarm_packet_is_urgent_and_carries_the_key() {
        let b = alarm_body(
            "not_ready:cp-2",
            "kubernetes-nodes",
            None,
            "evt",
            "",
            "emp-owner",
        );
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["owner_id"], "emp-owner", "the owner is the one handed in");
        assert_eq!(b["metadata"]["estate_finding"], "not_ready:cp-2");
        assert!(b["title"].as_str().unwrap().contains("not_ready:cp-2"));
    }

    #[test]
    fn the_alarm_packet_names_host_and_carries_the_latest_reading() {
        let entry = json!({"host": "boss-gcp", "unit": "boss-train.service",
                           "active_state": "inactive", "sub_state": "dead"});
        let b = alarm_body(
            "unit_unhealthy:boss-gcp/boss-train.service",
            "host-units",
            Some("boss-gcp"),
            "evt",
            &excerpt(&entry),
            "emp-owner",
        );
        let title = b["title"].as_str().unwrap();
        assert!(title.contains("boss-gcp/boss-train.service"));
        assert_eq!(b["metadata"]["host"], "boss-gcp");
        // The latest evidence excerpt rides the detail: the operator
        // reads the reading, not just its name.
        assert!(
            b["metadata"]["detail"]
                .as_str()
                .unwrap()
                .contains("\"sub_state\":\"dead\"")
        );
    }

    // ----- the door scope (backlog e6406701) -----

    fn door_entry(half: &str, target: &str) -> Value {
        json!({"id": format!("dev-ssh/{half}"), "door": "dev-ssh", "half": half,
               "target": target, "reason": "connection refused",
               "dark_since": "2026-09-24T11:40:00Z", "dark_for_s": 1200, "band_s": 900})
    }

    fn door_comparison(dark: &[Value], dimming: &[Value]) -> Value {
        json!({
            "scope": "door",
            "findings": { "door_dark": dark, "door_dimming": dimming },
        })
    }

    #[test]
    fn a_door_dark_past_its_band_is_hard_and_a_dimming_one_is_not() {
        let c = door_comparison(
            &[door_entry("lan", "10.20.0.35:22")],
            &[door_entry("public", "dev.algedonic.dev")],
        );
        // One key per HALF: which half is down is the finding.
        assert_eq!(
            hard_finding_keys(&c),
            BTreeSet::from(["door_dark:dev-ssh/lan".to_string()])
        );
    }

    #[test]
    fn a_door_finding_is_judged_by_its_band_not_by_counting_comparisons() {
        // The band already integrated the darkness over time, the way
        // STALE_MULTIPLIER does for silence: waiting PERSIST_N more
        // comparisons would make the declared band a lie by ten minutes.
        let c = door_comparison(&[door_entry("lan", "10.20.0.35:22")], &[]);
        let banded: Vec<String> = banded_findings(&c).into_iter().map(|(k, _)| k).collect();
        assert_eq!(banded, vec!["door_dark:dev-ssh/lan".to_string()]);
        // Every other hard finding still waits for its persistence.
        assert!(banded_findings(&comparison("kubernetes-nodes", &["cp-2"], &[])).is_empty());
    }

    #[test]
    fn the_door_alarm_names_the_half_its_target_and_its_band() {
        let entry = door_entry("lan", "10.20.0.35:22");
        let b = door_body("door_dark:dev-ssh/lan", &entry, "evt", "emp-owner");
        let title = b["title"].as_str().unwrap();
        assert!(title.contains("door_dark:dev-ssh/lan"), "{title}");
        assert!(title.contains("lan half"), "{title}");
        assert!(title.contains("10.20.0.35:22"), "{title}");
        assert!(title.contains("15-minute band"), "{title}");
        assert!(
            !title.contains("consecutive"),
            "the band is the persistence: {title}"
        );
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["owner_id"], "emp-owner");
        assert_eq!(b["metadata"]["estate_finding"], "door_dark:dev-ssh/lan");
        assert_eq!(b["metadata"]["scope"], "door");
        // No host: the door series' rows carry none, and the packet's
        // (scope, host) is what estate.recover closes it by (3908d555).
        assert!(b["metadata"].get("host").is_none(), "{b}");
        let detail = b["metadata"]["detail"].as_str().unwrap();
        assert!(detail.contains("connection refused"), "{detail}");
        assert!(detail.contains("2026-09-24T11:40:00Z"), "{detail}");
        assert!(detail.contains("infra/estate/doors.toml"), "{detail}");
    }

    #[test]
    fn the_door_series_is_watched_for_silence_as_one_series() {
        // An observer that stops posting is its own alarm
        // (`unobserved:door`), or the door watch dies quietly the way
        // the door did.
        assert!(WATCHED_SERIES.contains(&(DOOR_SCOPE, false)));
    }

    // ----- the silence sweep (a7a19a1a) -----

    fn obs_row(scope: &str, host: &str, at: &str) -> Value {
        json!({
            "event_id": "e", "timestamp": at, "source": "jobs",
            "kind": "jobs.estate.observed",
            "payload": {
                "scope": scope, "observed_at": at, "observer": "t",
                "nodes": [{"id": host}]
            }
        })
    }

    fn at(now: DateTime<Utc>, minutes_ago: i64) -> String {
        (now - chrono::Duration::minutes(minutes_ago)).to_rfc3339()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 3, 12, 0, 0).unwrap()
    }

    #[test]
    fn a_series_quiet_past_three_cadences_is_stale() {
        // A five-minute series whose newest reading is 20 minutes old:
        // 20m > 3 x 5m — the observer died or the host did.
        let n = now();
        let rows = [
            obs_row("host-units", "boss-gcp", &at(n, 20)),
            obs_row("host-units", "boss-gcp", &at(n, 25)),
            obs_row("host-units", "boss-gcp", &at(n, 30)),
            obs_row("host-units", "boss-gcp", &at(n, 35)),
        ];
        let stale = stale_series(&rows, true, "host-units", n);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0]["host"], "boss-gcp");
        assert_eq!(stale[0]["scope"], "host-units");
        assert_eq!(stale[0]["cadence_s"], 300);
        assert_eq!(stale[0]["age_s"], 1200);
    }

    #[test]
    fn a_series_inside_three_cadences_is_not_stale() {
        // Newest reading 10 minutes old on a 5-minute cadence: one
        // missed firing is weather, not a condition.
        let n = now();
        let rows = [
            obs_row("host-units", "boss-gcp", &at(n, 10)),
            obs_row("host-units", "boss-gcp", &at(n, 15)),
            obs_row("host-units", "boss-gcp", &at(n, 20)),
        ];
        assert!(stale_series(&rows, true, "host-units", n).is_empty());
    }

    #[test]
    fn only_the_quiet_host_is_stale_not_its_healthy_neighbor() {
        let n = now();
        let rows = [
            obs_row("host-units", "forge-host", &at(n, 2)),
            obs_row("host-units", "boss-gcp", &at(n, 40)),
            obs_row("host-units", "forge-host", &at(n, 7)),
            obs_row("host-units", "boss-gcp", &at(n, 45)),
            obs_row("host-units", "forge-host", &at(n, 12)),
            obs_row("host-units", "boss-gcp", &at(n, 50)),
        ];
        let stale = stale_series(&rows, true, "host-units", n);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0]["host"], "boss-gcp");
    }

    #[test]
    fn too_few_observations_is_no_measured_cadence_so_no_staleness() {
        // Two rows is one gap — a guess, not a cadence.
        let n = now();
        let rows = [
            obs_row("host", "boss-gcp", &at(n, 600)),
            obs_row("host", "boss-gcp", &at(n, 605)),
        ];
        assert!(stale_series(&rows, true, "host", n).is_empty());
    }

    #[test]
    fn back_to_back_manual_posts_cannot_fake_a_fast_cadence() {
        // Three posts seconds apart, then 3 minutes of quiet: without
        // the cadence floor the "measured" cadence would be seconds and
        // this would already alarm.
        let n = now();
        let s = |secs_ago: i64| (n - chrono::Duration::seconds(secs_ago)).to_rfc3339();
        let rows = [
            obs_row("host", "boss-gcp", &s(170)),
            obs_row("host", "boss-gcp", &s(175)),
            obs_row("host", "boss-gcp", &s(180)),
        ];
        assert!(stale_series(&rows, true, "host", n).is_empty());
    }

    #[test]
    fn the_cluster_series_is_one_series_keyed_by_its_scope() {
        // kubernetes-nodes observations carry many nodes; the series
        // identity is the scope itself, and its silence means the
        // CLUSTER observer died — the a5adfb99 instrument going dark.
        let n = now();
        let rows = [
            obs_row("kubernetes-nodes", "cp-1", &at(n, 60)),
            obs_row("kubernetes-nodes", "cp-1", &at(n, 75)),
            obs_row("kubernetes-nodes", "cp-1", &at(n, 90)),
        ];
        let stale = stale_series(&rows, false, "kubernetes-nodes", n);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0]["series"], "kubernetes-nodes");
        assert!(
            stale[0].get("host").is_none(),
            "the cluster series has no host, so its stale entry carries none (3908d555)"
        );
    }

    #[test]
    fn a_per_host_stale_entry_names_its_host_as_the_series() {
        let n = now();
        let rows = [
            obs_row("host-units", "boss-gcp", &at(n, 60)),
            obs_row("host-units", "boss-gcp", &at(n, 65)),
            obs_row("host-units", "boss-gcp", &at(n, 70)),
        ];
        let stale = stale_series(&rows, true, "host-units", n);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0]["series"], "boss-gcp");
        assert_eq!(stale[0]["host"], "boss-gcp");
    }

    #[test]
    fn the_cluster_silence_alarm_carries_no_host_because_its_series_has_none() {
        // 3908d555: the raise stamped host = "kubernetes-nodes" (the
        // series name) while every kubernetes-nodes comparison row is
        // host-less, so estate.recover — which matches an alarm to its
        // series on (scope, host) exactly as persistence does — matched
        // it to nothing and the one alarm that recovers most often (an
        // observer restart) was the one still closed by hand. The
        // packet describes the series truthfully: no host. The key and
        // the title still name the series.
        let stale = json!({
            "series": "kubernetes-nodes", "scope": "kubernetes-nodes",
            "last_observed_at": "2026-09-03T11:20:00+00:00",
            "cadence_s": 900, "age_s": 3600,
        });
        let b = staleness_body(&stale, "evt", "emp-owner");
        assert_eq!(
            b["metadata"]["estate_finding"],
            "unobserved:kubernetes-nodes"
        );
        assert_eq!(b["metadata"]["scope"], "kubernetes-nodes");
        assert!(
            b["metadata"].get("host").is_none(),
            "a host-less series raises a host-less alarm, the shape recovery matches"
        );
        let title = b["title"].as_str().unwrap();
        assert!(title.contains("kubernetes-nodes") && title.contains("unobserved"));
    }

    #[test]
    fn the_staleness_packet_is_urgent_and_names_host_and_condition() {
        let stale = json!({
            "series": "boss-gcp", "host": "boss-gcp", "scope": "host-units",
            "last_observed_at": "2026-09-03T11:20:00+00:00",
            "cadence_s": 300, "age_s": 2400,
        });
        let b = staleness_body(&stale, "evt", "emp-owner");
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["metadata"]["estate_finding"], "unobserved:boss-gcp");
        assert_eq!(b["metadata"]["host"], "boss-gcp");
        let title = b["title"].as_str().unwrap();
        assert!(title.contains("boss-gcp") && title.contains("unobserved"));
        // The evidence excerpt — when it went quiet, at what cadence.
        let detail = b["metadata"]["detail"].as_str().unwrap();
        assert!(detail.contains("2026-09-03T11:20:00") && detail.contains("300"));
    }

    #[test]
    fn two_quiet_series_on_one_host_share_one_dedup_key() {
        // Both the host and host-units series going dark is ONE sick
        // host, not two packets.
        let a = json!({"series": "boss-gcp", "host": "boss-gcp", "scope": "host"});
        let b = json!({"series": "boss-gcp", "host": "boss-gcp", "scope": "host-units"});
        assert_eq!(unobserved_key(&a), unobserved_key(&b));
        assert_eq!(unobserved_key(&a), "unobserved:boss-gcp");
    }

    fn closed_alarm(key: &str, closed_on: &str, disposition: &str) -> Value {
        json!({
            "id": "x", "kind": "backlog-item", "status": "closed", "closed_on": closed_on,
            "metadata": {"estate_finding": key},
            "steps": [{"title": "Measure the claim, choose a route", "status": "completed", "metadata": {"disposition": disposition}}]
        })
    }

    #[test]
    fn a_finding_a_human_settled_this_week_is_not_re_raised() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 17, 0, 0).unwrap();
        let closed = vec![
            closed_alarm(
                "unit_unhealthy:boss-gcp/boss-train.service",
                "2026-09-05",
                "stale",
            ),
            closed_alarm(
                "unit_unhealthy:boss-gcp/other.service",
                "2026-09-04",
                "duplicate",
            ),
            closed_alarm("unobserved:forge", "2026-08-20", "stale"),
            closed_alarm("not_ready:cp-2", "2026-09-05", "build"),
        ];
        let settled = settled_recently(&closed, now);
        assert!(
            settled.contains("unit_unhealthy:boss-gcp/boss-train.service"),
            "closed stale today"
        );
        assert!(
            settled.contains("unit_unhealthy:boss-gcp/other.service"),
            "closed duplicate yesterday"
        );
        assert!(
            !settled.contains("unobserved:forge"),
            "settled sixteen days ago — the answer may have changed"
        );
        assert!(
            !settled.contains("not_ready:cp-2"),
            "closed as BUILT — a recurrence after a fix is a new fact"
        );
    }

    #[test]
    fn a_machine_clear_does_not_suppress_but_a_human_stale_does() {
        // ef421cd3: `estate.recover` closes a recovered alarm as
        // `stale`, the same disposition a human uses. Read as a human
        // answer it would silence the next genuine raise of that
        // finding for a week — a unit that recovers and then dies
        // again inside the day would never re-raise. The machine's
        // close is stamped `cleared_by`, and that stamp is what tells
        // the two apart.
        let now = Utc.with_ymd_and_hms(2026, 9, 14, 20, 0, 0).unwrap();
        let mut machine = closed_alarm(
            "unit_unhealthy:boss-gcp/boss-codebase-metrics.service",
            "2026-09-14",
            "stale",
        );
        machine["steps"][0]["metadata"]["cleared_by"] =
            json!(crate::handlers::estate_recover::CLEARED_BY);
        let human = closed_alarm(
            "unit_unhealthy:boss-gcp/other.service",
            "2026-09-14",
            "stale",
        );
        let settled = settled_recently(&[machine, human], now);
        assert!(
            !settled.contains("unit_unhealthy:boss-gcp/boss-codebase-metrics.service"),
            "a recovery the machine recorded must not suppress the next raise"
        );
        assert!(
            settled.contains("unit_unhealthy:boss-gcp/other.service"),
            "a human's stale still holds for the week"
        );
    }
}

/// AN ALARM READ WITH NO `data` ARRAY IS NO ANSWER (backlog 37fc5837).
/// Both halves read a series and, handed an error-shaped body, used to
/// read it as an empty series: the persistence half then found nothing
/// persistent and the silence half nothing stale, and the pass ACKed
/// clean — the alarm system silently not alarming. Each now lands in
/// the pass's error accumulator, so the firing NAKs and names the read.
#[cfg(test)]
mod no_data_array_tests {
    use super::*;
    use crate::handlers::listing_stub::{
        assert_refused_by_name, empty_listing, no_data_array, serve,
    };

    fn firing(comparison: Value) -> InvocationContext {
        InvocationContext {
            rule_name: "estate-alarm".into(),
            triggering_event_id: "evt-cmp-1".into(),
            triggering_topic: "estate.comparison.recorded".into(),
            event_payload: comparison,
        }
    }

    fn handler(base: String) -> Arc<EstateAlarm> {
        // The clock is unreachable on purpose: the silence half's `now`
        // falls back, and nothing here depends on its value.
        EstateAlarm::new(
            base,
            "http://127.0.0.1:1",
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    #[tokio::test]
    async fn a_comparisons_read_with_no_data_array_refuses_by_name() {
        let stub = serve(vec![
            ("/api/estate/comparisons", no_data_array()),
            ("/api/estate/observations", empty_listing()),
            ("/api/jobs", empty_listing()),
        ])
        .await;
        // A hard finding, so the persistence half fetches its series.
        let hard = json!({
            "scope": "kubernetes-nodes",
            "findings": { "not_ready": ["cp-2"] },
        });
        let res = handler(stub.base.clone()).invoke(&[], &firing(hard)).await;
        assert_refused_by_name(res, "the comparisons read");
    }

    #[tokio::test]
    async fn an_observations_read_with_no_data_array_refuses_by_name() {
        let stub = serve(vec![
            ("/api/estate/observations", no_data_array()),
            ("/api/jobs", empty_listing()),
        ])
        .await;
        // No findings: only the silence half runs, on every firing.
        let quiet = json!({ "scope": "kubernetes-nodes", "findings": {} });
        let res = handler(stub.base.clone()).invoke(&[], &firing(quiet)).await;
        assert_refused_by_name(res, "the observations read");
    }

    fn door_dark_comparison() -> Value {
        json!({
            "scope": "door",
            "findings": {
                "door_dark": [{"id": "dev-ssh/lan", "door": "dev-ssh", "half": "lan",
                               "target": "10.20.0.35:22", "reason": "connection refused",
                               "dark_since": "2026-09-24T11:40:00Z",
                               "dark_for_s": 1200, "band_s": 900}],
                "door_dimming": [],
            },
        })
    }

    #[tokio::test]
    async fn a_door_dark_past_its_band_files_one_packet_without_reading_the_series() {
        // No comparisons route: a read of the series would 404 and the
        // pass would fail. The band is the persistence, so none is made.
        let stub = serve(vec![
            ("/api/estate/observations", empty_listing()),
            ("/api/jobs", empty_listing()),
        ])
        .await;
        let res = handler(stub.base.clone())
            .invoke(&[], &firing(door_dark_comparison()))
            .await;
        assert!(res.is_ok(), "{res:?}");
        let posts: Vec<(String, Value)> = stub
            .sent()
            .into_iter()
            .filter(|(w, _)| w == "POST /api/jobs")
            .collect();
        assert_eq!(posts.len(), 1, "{posts:?}");
        assert_eq!(
            posts[0].1["metadata"]["estate_finding"],
            "door_dark:dev-ssh/lan"
        );
    }

    #[tokio::test]
    async fn a_door_already_raised_is_not_raised_again() {
        let open = json!({
            "data": [{"id": "a1", "status": "open",
                      "metadata": {"estate_finding": "door_dark:dev-ssh/lan", "scope": "door"}}],
            "total": 1,
        });
        let stub = serve(vec![
            ("/api/estate/observations", empty_listing()),
            ("/api/jobs", open),
        ])
        .await;
        let res = handler(stub.base.clone())
            .invoke(&[], &firing(door_dark_comparison()))
            .await;
        assert!(res.is_ok(), "{res:?}");
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }
}
