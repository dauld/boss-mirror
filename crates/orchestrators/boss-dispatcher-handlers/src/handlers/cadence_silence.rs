//! `cadence.silence.sweep` — a DECLARED cadence with no packet files
//! an alarm (backlog ecca2f43).
//!
//! THE SILENCE THE SYSTEM OF RECORD DID NOT SPEAK ABOUT. Two of them,
//! both measured by hand on 2026-09-08:
//!
//! - `maintenance-ml-inference-batch` had been dead for 23 nights
//!   (e109f57e). Its unit died in ExecStartPre every night, so the run
//!   never started and the packet never opened; the ML predictions went
//!   three weeks stale. ZERO packets of that kind exist.
//! - `maintenance-estate-observe-units` had been quiet for four days
//!   (408c81f6) — a five-minute observer whose newest packet was opened
//!   2026-09-04 and never closed.
//!
//! Both are DECLARED cadences. Both stopped arriving. Nothing said so.
//! The estate alarm's silence sweep (`estate_alarm::stale_series`) is
//! the right idea one layer over: it watches the estate OBSERVATION
//! series and notices a host that stopped being observed. It cannot
//! see a chore that stopped running, because a chore's evidence is a
//! PACKET, not an observation.
//!
//! CLAUDE.md §Diagnosis states the class twice — "a check nobody reads
//! is a check that is not running" and "an alarm that reports through
//! its subject dies with it". A cadence that stops is the first;
//! this sweep is the reader.
//!
//! WHAT IT DOES, once a sim-day:
//! 1. Read the DECLARED cadences off its own rule row's args (see
//!    "where the declaration lives" below).
//! 2. For each declared kind, read the newest packet of that kind from
//!    the system of record.
//! 3. Age past [`SILENCE_MULTIPLIER`]x the declared interval — or no
//!    packet at all, ever — is a finding.
//! 4. File ONE urgent packet per silent kind, naming the kind, its
//!    expected interval, and the age of the last packet.
//! 5. Idempotent: an open alarm for that kind is UPDATED with the
//!    fresh measurement, never twinned. When packets of the kind start
//!    arriving again the alarm CLOSES ITSELF (`stale` — the claim no
//!    longer holds), stamped so the settled-suppression below can tell
//!    a machine clear from a human's answer.
//!
//! WHERE THE DECLARATION LIVES, and why it is not the workflow row.
//! CLAUDE.md §9 prefers registry data on the Workflow, and that was
//! the first choice. It does not work today: `boss-platform-workflow-
//! seed` is INSERT-IF-MISSING by design (protocols-as-data Q1, David
//! 2026-08-15 — "the seed binary inserts what is missing and touches
//! nothing that exists"), so adding `expected_interval_minutes` to an
//! already-seeded kind's `metadata` in `infra/platform/workflows.toml`
//! never reaches the registry, and minting a new workflow VERSION for
//! eighteen kinds to carry one integer each is not proportionate to
//! the fact being declared.
//!
//! The DISPATCHER RULE ROW is registry data with the same properties —
//! append-only, versioned, editable through `/api/dispatcher/rules`
//! with no deploy — and it is already one of this change's three
//! homes. So each declared cadence is one arg on the rule:
//!
//! ```toml
//! args = { "interval_minutes.maintenance-ml-inference-batch" = "1440" }
//! ```
//!
//! Same idiom as `credential.rotate.forgejo`, whose args ARE one
//! credential's registry declaration. Changing a cadence is a rule-row
//! edit, not a code change — which is the property §9 is actually
//! about.
//!
//! THE FACT LIVES TWICE, SO IT IS PINNED (CLAUDE.md §9a). The interval
//! is also stated by the systemd timer that executes the chore.
//! `infra/lint/timers-leave-a-packet.sh` check 8 compares the two and
//! names the offending kind when they drift, so a timer retuned from
//! 5min to hourly cannot leave this rule alarming every day.
//!
//! FAIL-CLOSED. A kind declared with something that is not a positive
//! integer of minutes is reported as `undetermined`, never silently
//! skipped: "I cannot tell whether this is silent" is itself the
//! finding. The other half of fail-closed lives in the lint — a
//! rostered timer whose kind this rule does not declare AT ALL is
//! invisible to a runtime sweep, and CI is where that gap is caught.
//!
//! WHAT IT DOES NOT COVER. This sweep reports THROUGH the jobs API it
//! reads. If the system of record itself is down, it files nothing and
//! says nothing — the same architectural gap CLAUDE.md §Diagnosis names
//! ("an alarm that reports through its subject dies with it"), tracked
//! as backlog 6bf34846 and deliberately NOT addressed here. What this
//! does buy is that a chore dying while the SoR is healthy — which is
//! both measured instances — is spoken about within a day.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{Map, Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{api_client, get_json, post_json, write_json};

/// Arg-key prefix for one declared cadence. `interval_minutes.<kind>`
/// = "a packet of `<kind>` is expected every N minutes".
const ARG_PREFIX: &str = "interval_minutes.";

/// How many of its own declared intervals a kind may go without a
/// packet before it is silent. Two, from the packet that asked for
/// this (ecca2f43): one missed firing is weather — a reboot, a timer's
/// AccuracySec, a deploy roll — and two in a row is the chore not
/// running. Deliberately looser than the estate alarm's
/// `STALE_MULTIPLIER` of 3, because this sweep runs DAILY rather than
/// every few minutes: three intervals of a daily chore is most of a
/// week, and the ML batch went 23 nights before a human noticed.
const SILENCE_MULTIPLIER: i64 = 2;

/// FLOOR on the silence window, in minutes. Six hours.
///
/// The threshold is [`SILENCE_MULTIPLIER`]x the declared interval, and
/// for a five-minute chore that is ten minutes — a window this sweep
/// cannot honestly judge, because it fires once a sim-DAY (the
/// dispatcher's schedule runner has day granularity and nothing finer).
/// A deploy roll or a slow tick during the one pass would read as a
/// dead observer, and an alarm that cries wolf trains operators to
/// ignore it (the estate alarm's calibration, and the reason it demands
/// persistence).
///
/// So no cadence is judged silent inside six hours. For a five-minute
/// observer that is seventy-two consecutive missed firings; for
/// anything daily the 2x threshold is already larger and this floor
/// never binds. Both silences that motivated this sweep — four days and
/// twenty-three nights — clear it by a wide margin. The honest cost is
/// stated rather than hidden: a fast chore that dies is spoken about
/// within a day, not within ten minutes.
const MIN_SILENCE_WINDOW_MIN: i64 = 360;

/// How long a finding a HUMAN settled stays settled — the estate
/// alarm's constant and its reasoning: an alarm the operator has just
/// answered is noise until the answer can change. A close this sweep
/// made ITSELF does not suppress (see [`settled_recently`]): the
/// machine closes an alarm because the kind came back, and a kind that
/// goes quiet again is a new fact.
const SETTLED_DAYS: i64 = 7;

/// The dedup read's page size — the jobs API's own `MAX_LIMIT`. A
/// `total` past it trips the truncation HOLD rather than raising blind.
const DEDUP_PAGE: usize = 1000;

/// Stamped on a triage completion this sweep made, so
/// [`settled_recently`] can tell a machine clear from a human's answer.
const CLEARED_BY: &str = "cadence.silence.sweep";

/// The step this sweep completes to close its own alarm. `backlog-item`
/// routes on `triage.disposition`, and `stale` is the terminal whose
/// title is literally "Closed — the claim no longer holds".
const TRIAGE_SLUG: &str = "triage";

pub struct CadenceSilenceSweep {
    client: reqwest::Client,
    jobs_base: String,
    /// The `now` every age is measured against. The dispatcher is not
    /// on the no-wallclock allowlist, so this comes from the clock
    /// service like every other stamp.
    clock: Arc<dyn boss_clock_client::ClockClient>,
}

impl CadenceSilenceSweep {
    pub fn new(jobs_base: impl Into<String>, clock_url: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            clock: Arc::new(boss_clock_client::ReqwestClockClient::new(clock_url)),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

// ---------------------------------------------------------------------------
// The declaration
// ---------------------------------------------------------------------------

/// What one rule arg declares about one kind's cadence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declared {
    /// A packet is expected every N minutes, N > 0.
    Minutes(i64),
    /// The kind is declared as cadenced, but its interval cannot be
    /// read. Reported as a finding — never skipped.
    Undetermined(String),
}

/// The declared cadences carried by this rule's args, kind-sorted so a
/// pass is deterministic. Args not prefixed [`ARG_PREFIX`] are ignored
/// (room for future scalar parameters); a prefixed arg is ALWAYS a
/// declaration, so a mistyped one becomes [`Declared::Undetermined`]
/// rather than vanishing.
pub fn declarations(
    args: &[(String, boss_dispatcher::rules::expr::Value)],
) -> Vec<(String, Declared)> {
    use boss_dispatcher::rules::expr::Value as ExprValue;
    let mut out: BTreeMap<String, Declared> = BTreeMap::new();
    for (key, value) in args {
        let Some(kind) = key.strip_prefix(ARG_PREFIX) else {
            continue;
        };
        if kind.is_empty() {
            continue;
        }
        let declared = match value {
            ExprValue::Int(n) if *n > 0 => Declared::Minutes(*n),
            ExprValue::Int(n) => Declared::Undetermined(format!(
                "declared interval {n} is not a positive number of minutes"
            )),
            other => Declared::Undetermined(format!(
                "declared interval is a {}, not a positive number of minutes",
                other.kind()
            )),
        };
        out.insert(kind.to_string(), declared);
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------
// The measurement
// ---------------------------------------------------------------------------

/// When the newest packet in a jobs listing came into being.
///
/// Preference order, most precise first:
/// 1. `metadata.opened_at` — the RFC3339 stamp a maintenance packet
///    carries. Exact.
/// 2. `opened_on` — a DATE, read as the END of that day (23:59:59Z),
///    which is the LATEST instant the packet could have opened and so
///    the SMALLEST age consistent with the data. An alarm that cries
///    wolf trains operators to ignore it, so date-only granularity
///    costs at most one interval of latency and never a false alarm.
///
/// Returns the maximum over the rows, so the caller may pass a page
/// rather than trusting the listing's order.
pub fn newest_packet_at(rows: &[Value]) -> Option<DateTime<Utc>> {
    rows.iter().filter_map(packet_at).max()
}

fn packet_at(row: &Value) -> Option<DateTime<Utc>> {
    if let Some(exact) = row
        .get("metadata")
        .and_then(|m| m.get("opened_at"))
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
    {
        return Some(exact.with_timezone(&Utc));
    }
    let day = row
        .get("opened_on")
        .and_then(Value::as_str)
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())?;
    // End of that day: the most generous reading, so a date-only
    // packet can never manufacture a false alarm.
    day.and_hms_opt(23, 59, 59)
        .and_then(|dt| Utc.from_local_datetime(&dt).single())
}

/// What one declared kind's newest packet says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A packet arrived inside the window. Nothing to say.
    Fresh,
    /// The newest packet is older than [`SILENCE_MULTIPLIER`]x the
    /// declared interval.
    Silent {
        interval_min: i64,
        age_min: i64,
        last: String,
    },
    /// No packet of this kind has EVER been filed — the ML batch's
    /// shape, and the loudest one, because there is not even a
    /// history to be stale.
    NeverFiled { interval_min: i64 },
    /// The declaration could not be read. Fail-closed: reported, not
    /// skipped.
    Undetermined { reason: String },
}

impl Verdict {
    /// Is this verdict worth a packet?
    pub fn is_finding(&self) -> bool {
        !matches!(self, Verdict::Fresh)
    }
}

/// How long a kind may stay quiet before it is silent:
/// [`SILENCE_MULTIPLIER`]x its declared interval, never less than
/// [`MIN_SILENCE_WINDOW_MIN`].
pub fn silence_window_min(interval_min: i64) -> i64 {
    (SILENCE_MULTIPLIER * interval_min).max(MIN_SILENCE_WINDOW_MIN)
}

/// THE decision, pure: given one declaration, the newest packet's
/// instant (if any) and `now`, is this cadence silent?
pub fn verdict(declared: &Declared, newest: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Verdict {
    let interval_min = match declared {
        Declared::Undetermined(reason) => {
            return Verdict::Undetermined {
                reason: reason.clone(),
            };
        }
        Declared::Minutes(m) => *m,
    };
    let Some(last) = newest else {
        return Verdict::NeverFiled { interval_min };
    };
    let age_min = (now - last).num_minutes();
    if age_min > silence_window_min(interval_min) {
        Verdict::Silent {
            interval_min,
            age_min,
            last: last.to_rfc3339(),
        }
    } else {
        Verdict::Fresh
    }
}

// ---------------------------------------------------------------------------
// Dedup — the same idioms the estate alarm raises with
// ---------------------------------------------------------------------------

/// The dedup identity of one silent kind. One condition per kind, so a
/// kind that stays silent for a week is ONE packet updated daily, not
/// seven.
pub fn silence_key(kind: &str) -> String {
    format!("cadence-silence:{kind}")
}

/// Open alarm packets by their `cadence_silence` key.
pub fn open_alarms(open_jobs: &[Value]) -> BTreeMap<String, Value> {
    open_jobs
        .iter()
        .filter_map(|j| {
            let key = j
                .get("metadata")?
                .get("cadence_silence")?
                .as_str()?
                .to_string();
            Some((key, j.clone()))
        })
        .collect()
}

/// Keys a HUMAN settled within [`SETTLED_DAYS`] — suppressed, because
/// an alarm the operator has just answered is noise until the answer
/// can change.
///
/// A close THIS SWEEP made is excluded: it stamps `cleared_by` on the
/// triage step, and its own clear must not suppress the next genuine
/// silence. A five-minute observer that flaps back and quiet again
/// inside a week is two conditions, not one.
pub fn settled_recently(closed_jobs: &[Value], now: DateTime<Utc>) -> BTreeSet<String> {
    closed_jobs
        .iter()
        .filter_map(|j| {
            let key = j
                .get("metadata")?
                .get("cadence_silence")?
                .as_str()?
                .to_string();
            let closed_on = j.get("closed_on")?.as_str()?;
            let closed = NaiveDate::parse_from_str(closed_on, "%Y-%m-%d").ok()?;
            if (now.date_naive() - closed).num_days() > SETTLED_DAYS {
                return None;
            }
            let machine_cleared = j
                .get("steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| s.get("metadata")?.get("cleared_by")?.as_str())
                .any(|c| c == CLEARED_BY);
            (!machine_cleared).then_some(key)
        })
        .collect()
}

/// Did the dedup read return every matching row? A page shorter than
/// the list's own `total` was TRUNCATED and cannot prove a finding
/// unraised; the caller HOLDs rather than raising blind.
pub fn dedup_page_complete(rows: usize, total: usize) -> bool {
    rows >= total
}

// ---------------------------------------------------------------------------
// The packets
// ---------------------------------------------------------------------------

fn human_minutes(min: i64) -> String {
    if min < 120 {
        format!("{min}m")
    } else if min < 2880 {
        format!("{:.1}h", min as f64 / 60.0)
    } else {
        format!("{:.1}d", min as f64 / 1440.0)
    }
}

/// The one-line condition a finding states — title and detail share it.
fn headline(kind: &str, v: &Verdict) -> String {
    match v {
        Verdict::Fresh => format!("{kind} is arriving on cadence"),
        Verdict::Silent {
            interval_min,
            age_min,
            ..
        } => format!(
            "{kind} has filed no packet for {} — it is declared every {}",
            human_minutes(*age_min),
            human_minutes(*interval_min)
        ),
        Verdict::NeverFiled { interval_min } => format!(
            "{kind} is declared every {} and NO packet of that kind has ever been filed",
            human_minutes(*interval_min)
        ),
        Verdict::Undetermined { .. } => {
            format!("{kind} is declared as cadenced but its interval cannot be read")
        }
    }
}

/// The measured facts a finding carries — written on the raise AND, as
/// a metadata merge, on every refresh of a standing alarm, so the
/// packet always shows TODAY's measurement rather than the day it was
/// first raised.
pub fn measurement(kind: &str, v: &Verdict, now: DateTime<Utc>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("cadence_kind".into(), json!(kind));
    m.insert("last_measured_at".into(), json!(now.to_rfc3339()));
    m.insert("condition".into(), json!(headline(kind, v)));
    match v {
        Verdict::Fresh => {}
        Verdict::Silent {
            interval_min,
            age_min,
            last,
        } => {
            m.insert("expected_interval_minutes".into(), json!(interval_min));
            m.insert("silent_for_minutes".into(), json!(age_min));
            m.insert("last_packet_at".into(), json!(last));
        }
        Verdict::NeverFiled { interval_min } => {
            m.insert("expected_interval_minutes".into(), json!(interval_min));
            m.insert("silent_for_minutes".into(), Value::Null);
            m.insert("last_packet_at".into(), Value::Null);
        }
        Verdict::Undetermined { reason } => {
            m.insert("expected_interval_minutes".into(), json!("undetermined"));
            m.insert("undetermined_reason".into(), json!(reason));
        }
    }
    m
}

/// The urgent packet one silent kind becomes.
pub fn alarm_body(kind: &str, v: &Verdict, evidence: &str, now: DateTime<Utc>) -> Value {
    let mut metadata = measurement(kind, v, now);
    metadata.insert("area".into(), json!("estate-observation"));
    metadata.insert("cadence_silence".into(), json!(silence_key(kind)));
    metadata.insert("reporter".into(), json!("cadence.silence.sweep"));
    metadata.insert(
        "detail".into(),
        json!(format!(
            "Raised by cadence.silence.sweep (backlog ecca2f43): {}. The interval is \
             declared on this sweep's own dispatcher rule row \
             (`{ARG_PREFIX}{kind}`); the actual is the newest packet of that kind in \
             the system of record. Threshold is {SILENCE_MULTIPLIER}x the declared \
             interval and never under {MIN_SILENCE_WINDOW_MIN} minutes, so one missed \
             firing is weather and a daily sweep cannot cry wolf about a \
             five-minute chore. This alarm UPDATES itself on each daily pass and CLOSES itself \
             (`stale`) the moment a packet of `{kind}` arrives again — so if it is \
             still open, the cadence is still silent. Precedent for why this exists: \
             maintenance-ml-inference-batch died in ExecStartPre for 23 nights \
             (e109f57e) and the five-minute maintenance-estate-observe-units observer \
             was quiet for four days (408c81f6); both were found by hand. Evidence: \
             {evidence}.",
            headline(kind, v)
        )),
    );
    json!({
        "kind": "backlog-item",
        "title": alarm_title(kind, v),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": "emp-david",
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": Value::Object(metadata),
    })
}

fn alarm_title(kind: &str, v: &Verdict) -> String {
    match v {
        Verdict::Fresh => format!("CADENCE OK: {kind}"),
        Verdict::Silent { age_min, .. } => format!(
            "CADENCE SILENT: {kind} — no packet for {}",
            human_minutes(*age_min)
        ),
        Verdict::NeverFiled { .. } => {
            format!("CADENCE SILENT: {kind} — no packet has ever arrived")
        }
        Verdict::Undetermined { .. } => {
            format!("CADENCE UNDECLARED: {kind} — expected interval cannot be read")
        }
    }
}

/// The metadata merge that refreshes a STANDING alarm instead of
/// filing a twin. `PATCH /api/jobs/{id}/metadata` merges top-level
/// keys, so this is exactly the fields that change between passes.
pub fn refresh_patch(kind: &str, v: &Verdict, now: DateTime<Utc>) -> Value {
    Value::Object(measurement(kind, v, now))
}

/// The triage completion that CLOSES a standing alarm when the kind
/// starts arriving again. `disposition = "stale"` is the backlog-item
/// terminal titled "Closed — the claim no longer holds", which is
/// precisely true: the cadence is no longer silent.
///
/// PUT on a step REPLACES top-level metadata, so the step's existing
/// keys (`authority_role`) are carried through by the caller.
pub fn clear_step_body(existing: &Map<String, Value>, kind: &str, v: &Verdict) -> Value {
    let mut metadata = existing.clone();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "cadence.silence.sweep re-measured `{kind}` and it is arriving again: {}. \
             The claim this alarm carried no longer holds; closed by machine, not by \
             judgement.",
            match v {
                Verdict::Fresh => "its newest packet is inside the declared window".to_string(),
                other => headline(kind, other),
            }
        )),
    );
    metadata.insert("cleared_by".into(), json!(CLEARED_BY));
    json!({"status": "completed", "metadata": metadata})
}

/// The `triage` step of one alarm packet, as (id, existing metadata).
pub fn triage_step(job: &Value) -> Option<(String, Map<String, Value>)> {
    job.get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(TRIAGE_SLUG))
        .and_then(|s| {
            let id = s.get("id").and_then(Value::as_str)?.to_string();
            let meta = s
                .get("metadata")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            Some((id, meta))
        })
}

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

#[async_trait]
impl Handler for CadenceSilenceSweep {
    fn name(&self) -> &'static str {
        "cadence.silence.sweep"
    }

    async fn invoke(
        &self,
        args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let declared = declarations(args);
        if declared.is_empty() {
            // A sweep declaring nothing watches nothing, and would
            // report "all clear" forever. That is the silent-check
            // class this handler exists to end, so it is an error.
            return Err(HandlerError::Permanent(
                "cadence.silence.sweep: the rule row declares no cadences (no \
                 `interval_minutes.<kind>` args) — a sweep with an empty roster is a \
                 check that is not running"
                    .into(),
            ));
        }
        let now = boss_clock_client::now_from(&self.clock).await;
        let evidence = format!(
            "cadence.silence.sweep firing on rule {} (event {} / topic {})",
            ctx.rule_name, ctx.triggering_event_id, ctx.triggering_topic
        );

        // Best-effort accumulator, the estate alarm's posture: one
        // kind's failed read must never block the other seventeen.
        // Every sub-op that failed surfaces as ONE error at the end, so
        // a transient still NAKs for retry (dedup makes the retry
        // idempotent) while every alarm that COULD raise, did.
        let mut errors: Vec<String> = Vec::new();
        let mut findings: BTreeMap<String, Verdict> = BTreeMap::new();
        let mut clear: BTreeMap<String, Verdict> = BTreeMap::new();

        for (kind, decl) in &declared {
            // An undetermined declaration needs no read: the finding is
            // that we cannot measure it.
            if let Declared::Undetermined(_) = decl {
                findings.insert(kind.clone(), verdict(decl, None, now));
                continue;
            }
            let listing = match get_json(
                &self.client,
                &format!("{}/api/jobs?kind={kind}&limit=1", self.base()),
                &ctx.rule_name,
            )
            .await
            {
                Ok(l) => l,
                Err(e) => {
                    errors.push(format!(
                        "newest-packet read for {kind} failed; other kinds still swept: {e}"
                    ));
                    continue;
                }
            };
            let rows: Vec<Value> = listing
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let v = verdict(decl, newest_packet_at(&rows), now);
            if v.is_finding() {
                findings.insert(kind.clone(), v);
            } else {
                clear.insert(kind.clone(), v);
            }
        }

        // ONE bounded dedup read answers three questions: which alarms
        // are open (update, don't twin), which were settled by a human
        // inside the window (stay quiet), and which standing alarms
        // belong to a kind that came back (close them).
        let listing = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind=backlog-item&closed_within={SETTLED_DAYS}&limit={DEDUP_PAGE}",
                self.base()
            ),
            &ctx.rule_name,
        )
        .await;
        let body = match listing {
            Ok(b) => b,
            Err(e) => {
                errors.push(format!(
                    "dedup fetch failed; {} finding(s) held for retry to avoid duplicate alarms ({e})",
                    findings.len()
                ));
                return finish(errors);
            }
        };
        let rows: Vec<Value> = body
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // The list's own `total` is authoritative over the page length;
        // a missing `total` is treated as truncated (fail-safe).
        let complete = body
            .get("total")
            .and_then(Value::as_u64)
            .map(|t| usize::try_from(t).unwrap_or(usize::MAX))
            .is_some_and(|total| dedup_page_complete(rows.len(), total));
        if !complete {
            errors.push(format!(
                "dedup read truncated ({} rows, total {:?}); {} finding(s) held for retry to \
                 avoid duplicate alarms",
                rows.len(),
                body.get("total").and_then(Value::as_u64),
                findings.len()
            ));
            return finish(errors);
        }

        let (open_rows, closed_rows): (Vec<Value>, Vec<Value>) = rows
            .into_iter()
            .partition(|j| j.get("status").and_then(Value::as_str) == Some("open"));
        let open = open_alarms(&open_rows);
        let settled = settled_recently(&closed_rows, now);

        // Raise or refresh, one packet per silent kind.
        for (kind, v) in &findings {
            let key = silence_key(kind);
            if let Some(existing) = open.get(&key) {
                let Some(id) = existing.get("id").and_then(Value::as_str) else {
                    errors.push(format!("open alarm for {kind} has no id; not refreshed"));
                    continue;
                };
                if let Err(e) = write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{id}/metadata", self.base()),
                    &refresh_patch(kind, v, now),
                    &ctx.rule_name,
                )
                .await
                {
                    errors.push(format!("refresh of the open alarm for {kind} failed: {e}"));
                    continue;
                }
                tracing::info!(cadence = %kind, "cadence.silence.sweep refreshed a standing alarm");
                continue;
            }
            if settled.contains(&key) {
                tracing::info!(cadence = %kind, "cadence.silence.sweep: settled by a human inside the window — not re-raising");
                continue;
            }
            if let Err(e) = post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &alarm_body(kind, v, &evidence, now),
                &ctx.rule_name,
            )
            .await
            {
                errors.push(format!(
                    "raise for {kind} failed; other findings still raised: {e}"
                ));
                continue;
            }
            tracing::info!(cadence = %kind, "cadence.silence.sweep raised a packet");
        }

        // A kind that came back closes its own alarm.
        for (kind, v) in &clear {
            let Some(existing) = open.get(&silence_key(kind)) else {
                continue;
            };
            let Some(id) = existing.get("id").and_then(Value::as_str) else {
                errors.push(format!("open alarm for {kind} has no id; not cleared"));
                continue;
            };
            let Some((step_id, step_meta)) = triage_step(existing) else {
                errors.push(format!(
                    "open alarm for {kind} has no `{TRIAGE_SLUG}` step; cannot close itself"
                ));
                continue;
            };
            if let Err(e) = write_json(
                &self.client,
                reqwest::Method::PUT,
                &format!("{}/api/jobs/{id}/steps/{step_id}", self.base()),
                &clear_step_body(&step_meta, kind, v),
                &ctx.rule_name,
            )
            .await
            {
                errors.push(format!("auto-close of the alarm for {kind} failed: {e}"));
                continue;
            }
            tracing::info!(cadence = %kind, "cadence.silence.sweep closed its own alarm — the kind is arriving again");
        }

        finish(errors)
    }
}

/// One aggregated exit: every alarm that could raise did; any sub-op
/// that failed NAKs the firing for redelivery, where dedup makes the
/// retry idempotent.
fn finish(errors: Vec<String>) -> Result<(), HandlerError> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(HandlerError::Downstream(format!(
            "cadence.silence.sweep: {} sub-operation(s) failed this pass (every alarm that \
             could raise did; the rest retry): {}",
            errors.len(),
            errors.join(" | ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_dispatcher::rules::expr::Value as ExprValue;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("test timestamp parses")
            .with_timezone(&Utc)
    }

    fn packet(opened_at: &str) -> Value {
        json!({
            "id": "pkt-1",
            "opened_on": &opened_at[..10],
            "metadata": {"chore": "x", "opened_at": opened_at},
        })
    }

    // -- the declaration -------------------------------------------------

    #[test]
    fn declarations_read_prefixed_int_args_and_ignore_the_rest() {
        let args = vec![
            (
                "interval_minutes.maintenance-ml-inference-batch".to_string(),
                ExprValue::Int(1440),
            ),
            (
                "interval_minutes.maintenance-estate-observe-units".to_string(),
                ExprValue::Int(5),
            ),
            ("some_other_knob".to_string(), ExprValue::Int(7)),
        ];
        assert_eq!(
            declarations(&args),
            vec![
                (
                    "maintenance-estate-observe-units".to_string(),
                    Declared::Minutes(5)
                ),
                (
                    "maintenance-ml-inference-batch".to_string(),
                    Declared::Minutes(1440)
                ),
            ]
        );
    }

    /// FAIL-CLOSED. A declared kind whose interval cannot be read is a
    /// finding, never a silent skip — "I cannot tell whether this is
    /// silent" is exactly the state this sweep exists to end.
    #[test]
    fn an_unreadable_interval_is_reported_as_undetermined_not_skipped() {
        let args = vec![
            (
                "interval_minutes.maintenance-mystery".to_string(),
                ExprValue::String("soonish".into()),
            ),
            (
                "interval_minutes.maintenance-zero".to_string(),
                ExprValue::Int(0),
            ),
        ];
        let decls = declarations(&args);
        assert_eq!(decls.len(), 2, "neither declaration may vanish");
        for (kind, d) in &decls {
            assert!(
                matches!(d, Declared::Undetermined(_)),
                "{kind} must be undetermined, got {d:?}"
            );
            assert!(
                verdict(d, None, at("2026-09-08T12:00:00Z")).is_finding(),
                "{kind} must produce a finding"
            );
        }
    }

    // -- the measurement -------------------------------------------------

    #[test]
    fn the_exact_opened_at_stamp_wins_over_the_date() {
        let rows = vec![packet("2026-09-08T03:30:01+00:00")];
        assert_eq!(
            newest_packet_at(&rows),
            Some(at("2026-09-08T03:30:01Z")),
            "metadata.opened_at is the precise stamp"
        );
    }

    /// A date-only packet is read as the END of its day — the latest
    /// instant it could have opened, so the age is the SMALLEST
    /// consistent with the data and a date-only row can never
    /// manufacture a false alarm.
    #[test]
    fn a_date_only_packet_is_read_generously() {
        let rows = vec![json!({"id": "p", "opened_on": "2026-09-06"})];
        assert_eq!(newest_packet_at(&rows), Some(at("2026-09-06T23:59:59Z")));
    }

    #[test]
    fn no_rows_means_no_newest() {
        assert_eq!(newest_packet_at(&[]), None);
    }

    // -- the decision ----------------------------------------------------

    /// THE headline case, in the numbers that were actually measured:
    /// the five-minute unit observer whose newest packet was opened
    /// 2026-09-04 and never followed (backlog 408c81f6).
    #[test]
    fn a_kind_silent_past_twice_its_interval_is_a_finding() {
        let now = at("2026-09-08T12:00:00Z");
        let v = verdict(
            &Declared::Minutes(5),
            newest_packet_at(&[packet("2026-09-04T09:15:00+00:00")]),
            now,
        );
        match v {
            Verdict::Silent {
                interval_min,
                age_min,
                ..
            } => {
                assert_eq!(interval_min, 5);
                assert_eq!(
                    age_min, 5925,
                    "four days two and three-quarter hours, in minutes"
                );
            }
            other => panic!("expected Silent, got {other:?}"),
        }
    }

    /// The ML batch's shape: the kind is declared and NO packet of it
    /// has ever been filed (23 nights of ExecStartPre failures,
    /// backlog e109f57e). A cadence with no history at all must alarm
    /// — "nothing to compare against" is the loudest finding, not an
    /// excuse to stay quiet.
    #[test]
    fn a_declared_kind_with_no_packet_at_all_is_a_finding() {
        let v = verdict(&Declared::Minutes(1440), None, at("2026-09-08T12:00:00Z"));
        assert_eq!(v, Verdict::NeverFiled { interval_min: 1440 });
        assert!(v.is_finding());
    }

    #[test]
    fn a_kind_with_fresh_packets_is_not_a_finding() {
        let now = at("2026-09-08T12:00:00Z");
        // A daily chore that ran this morning.
        let v = verdict(
            &Declared::Minutes(1440),
            newest_packet_at(&[packet("2026-09-08T03:30:01+00:00")]),
            now,
        );
        assert_eq!(v, Verdict::Fresh);
        assert!(!v.is_finding());
    }

    /// ONE missed firing is weather. The threshold is 2x, so a daily
    /// chore that skipped exactly one night stays quiet.
    #[test]
    fn exactly_one_missed_interval_is_still_fresh() {
        let now = at("2026-09-08T12:00:00Z");
        let v = verdict(
            &Declared::Minutes(1440),
            Some(at("2026-09-06T13:00:00Z")), // 2820 minutes, under 2880
            now,
        );
        assert_eq!(v, Verdict::Fresh);
    }

    /// A five-minute chore is NOT judged on a ten-minute window: this
    /// sweep fires once a sim-day, and a deploy roll during that one
    /// pass would read as a dead observer. The floor is six hours —
    /// seventy-two missed firings — and it binds only for the fast
    /// cadences, never for a daily one.
    #[test]
    fn a_fast_cadence_gets_a_floor_so_the_daily_sweep_cannot_cry_wolf() {
        assert_eq!(silence_window_min(5), MIN_SILENCE_WINDOW_MIN);
        assert_eq!(silence_window_min(10), MIN_SILENCE_WINDOW_MIN);
        assert_eq!(
            silence_window_min(1440),
            2880,
            "the floor never binds daily"
        );

        let now = at("2026-09-08T12:00:00Z");
        // Twenty minutes quiet — four missed firings of a five-minute
        // observer, and nothing worth waking anyone for.
        assert_eq!(
            verdict(&Declared::Minutes(5), Some(at("2026-09-08T11:40:00Z")), now),
            Verdict::Fresh
        );
        // Seven hours quiet is past the floor, and the same observer's
        // real four-day silence is far past it.
        assert!(verdict(&Declared::Minutes(5), Some(at("2026-09-08T05:00:00Z")), now).is_finding());
    }

    // -- idempotence -----------------------------------------------------

    /// A second pass over the same silence must UPDATE the standing
    /// alarm, not file a twin. The mechanism is the dedup key: the
    /// open packet already carries it, so the raise path never runs.
    #[test]
    fn a_second_pass_finds_the_open_alarm_by_its_key() {
        let now = at("2026-09-09T12:00:00Z");
        let kind = "maintenance-ml-inference-batch";
        let v = Verdict::NeverFiled { interval_min: 1440 };
        let raised = alarm_body(kind, &v, "first pass", at("2026-09-08T12:00:00Z"));
        // The packet as the jobs API would list it back.
        let open = json!({
            "id": "alarm-1",
            "status": "open",
            "kind": "backlog-item",
            "metadata": raised["metadata"].clone(),
            "steps": [{"id": "s-1", "spec_slug": "triage", "metadata": {"authority_role": "platform-admin"}}],
        });
        let index = open_alarms(&[open]);
        assert!(
            index.contains_key(&silence_key(kind)),
            "the raised packet must be found by the same key the next pass computes"
        );

        // And the refresh carries the NEW measurement, so a standing
        // alarm shows today's age rather than the day it was raised.
        let patch = refresh_patch(kind, &v, now);
        assert_eq!(
            patch["last_measured_at"],
            json!(now.to_rfc3339()),
            "a refresh re-stamps the measurement"
        );
        assert_eq!(patch["cadence_kind"], json!(kind));
    }

    /// When the kind comes back, the alarm closes ITSELF: the
    /// `backlog-item` triage step completes with the `stale`
    /// disposition, whose terminal is titled "Closed — the claim no
    /// longer holds". The step's existing metadata survives, because
    /// a step PUT replaces top-level metadata wholesale.
    #[test]
    fn a_returning_kind_closes_its_own_alarm_without_losing_step_metadata() {
        let open = json!({
            "id": "alarm-1",
            "status": "open",
            "steps": [
                {"id": "s-0", "spec_slug": "filed", "metadata": {}},
                {"id": "s-1", "spec_slug": "triage", "metadata": {"authority_role": "platform-admin"}}
            ],
        });
        let (step_id, meta) = triage_step(&open).expect("the alarm has a triage step");
        assert_eq!(step_id, "s-1");
        let body = clear_step_body(&meta, "maintenance-views-catchup", &Verdict::Fresh);
        assert_eq!(body["status"], json!("completed"));
        assert_eq!(body["metadata"]["disposition"], json!("stale"));
        assert_eq!(
            body["metadata"]["authority_role"],
            json!("platform-admin"),
            "a PUT replaces step metadata wholesale, so existing keys must be carried"
        );
        assert_eq!(body["metadata"]["cleared_by"], json!(CLEARED_BY));
    }

    /// A close THIS SWEEP made must not suppress the next raise: a
    /// five-minute observer that comes back and goes quiet again
    /// inside the settle window is a NEW condition. A human's `stale`
    /// close does suppress, for SETTLED_DAYS.
    #[test]
    fn a_machine_clear_does_not_suppress_but_a_human_close_does() {
        let now = at("2026-09-09T12:00:00Z");
        let closed = |key: &str, cleared_by: Value| {
            json!({
                "id": "x", "status": "closed", "closed_on": "2026-09-08",
                "metadata": {"cadence_silence": key},
                "steps": [{"spec_slug": "triage", "metadata": {"disposition": "stale", "cleared_by": cleared_by}}],
            })
        };
        let rows = vec![
            closed("cadence-silence:by-machine", json!(CLEARED_BY)),
            closed("cadence-silence:by-human", Value::Null),
        ];
        let settled = settled_recently(&rows, now);
        assert!(
            !settled.contains("cadence-silence:by-machine"),
            "the sweep's own clear must not suppress the next genuine silence"
        );
        assert!(
            settled.contains("cadence-silence:by-human"),
            "an answer a human just gave stays settled"
        );
    }

    #[test]
    fn a_human_close_older_than_the_window_stops_suppressing() {
        let now = at("2026-09-30T12:00:00Z");
        let rows = vec![json!({
            "id": "x", "status": "closed", "closed_on": "2026-09-08",
            "metadata": {"cadence_silence": "cadence-silence:k"},
            "steps": [{"spec_slug": "triage", "metadata": {"disposition": "stale"}}],
        })];
        assert!(settled_recently(&rows, now).is_empty());
    }

    #[test]
    fn a_truncated_dedup_page_is_not_complete() {
        assert!(!dedup_page_complete(1000, 1204));
        assert!(dedup_page_complete(12, 12));
    }

    // -- the packet ------------------------------------------------------

    #[test]
    fn the_alarm_names_the_kind_its_interval_and_the_age() {
        let now = at("2026-09-08T12:00:00Z");
        let v = Verdict::Silent {
            interval_min: 5,
            age_min: 6165,
            last: "2026-09-04T09:15:00+00:00".into(),
        };
        let body = alarm_body("maintenance-estate-observe-units", &v, "evidence", now);
        let title = body["title"].as_str().expect("a title");
        assert!(
            title.contains("maintenance-estate-observe-units"),
            "{title}"
        );
        assert!(title.starts_with("CADENCE SILENT:"), "{title}");
        assert_eq!(body["priority"], json!("urgent"));
        assert_eq!(body["metadata"]["expected_interval_minutes"], json!(5));
        assert_eq!(body["metadata"]["silent_for_minutes"], json!(6165));
        assert_eq!(
            body["metadata"]["last_packet_at"],
            json!("2026-09-04T09:15:00+00:00")
        );
        assert_eq!(
            body["metadata"]["cadence_silence"],
            json!("cadence-silence:maintenance-estate-observe-units")
        );
    }

    #[test]
    fn an_undetermined_declaration_files_a_distinct_alarm() {
        let now = at("2026-09-08T12:00:00Z");
        let v = Verdict::Undetermined {
            reason: "declared interval is a string, not a positive number of minutes".into(),
        };
        let body = alarm_body("maintenance-mystery", &v, "evidence", now);
        assert!(
            body["title"]
                .as_str()
                .is_some_and(|t| t.starts_with("CADENCE UNDECLARED:")),
            "{:?}",
            body["title"]
        );
        assert_eq!(
            body["metadata"]["expected_interval_minutes"],
            json!("undetermined")
        );
    }
}
