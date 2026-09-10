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
//! 1. Build the roster from BOTH its sources — the DECLARED cadences on
//!    its own rule row's args (see "where the declaration lives" below)
//!    and the cadences DERIVED from the clock rules that spawn packets
//!    (see "the roster has a second source").
//! 2. For each, read the newest packet of that identity from the system
//!    of record.
//! 3. Age past [`SILENCE_MULTIPLIER`]x the declared interval — or no
//!    packet at all, ever — is a finding.
//! 4. For a cadence with a dedup guard, ask the guard's own question
//!    before raising: a silence a still-open packet explains is reported
//!    as SUPPRESSED and names that packet, and stays quiet entirely
//!    while the block is younger than [`SUPPRESSION_INTERVALS`]
//!    intervals.
//! 5. File ONE urgent packet per quiet cadence, naming the identity, its
//!    expected interval, the age of the last packet, and — when it is
//!    blocked — the packet to drain.
//! 6. Idempotent: an open alarm for that cadence is UPDATED with the
//!    fresh measurement, never twinned. When packets start
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
//! THE ROSTER HAS A SECOND SOURCE, AND IT IS DERIVED, NOT DECLARED
//! (backlog cf0f5e2d, measured 2026-09-10). The args above cover the
//! kinds a systemd TIMER executes. The dispatcher also fires daily CLOCK
//! RULES that spawn a chore packet with no timer anywhere, and such a
//! cadence was outside this roster *by construction*: eight of them
//! existed, none were watched, and three families had been silent for
//! nine to nineteen days. The public mirror drifted 238 commits / 763
//! files behind and a human noticed by hand. So the second source is the
//! RULE REGISTRY itself — see [`super::cadence_roster`] for the
//! derivation and for why it is derived (the identity of a clock cadence
//! is the rule, not the kind: seven sweep rules share the kind
//! `maintenance-sweep` and differ only in subject).
//!
//! A SUPPRESSED FIRING IS NOT A SILENT ONE, and the judgement lives
//! here rather than in every rule. Each dead rule fires only
//! `NOT open_job_exists(<kind>, <target>)`: the guard's intent is right
//! (do not stack duplicate chores) but its effect is that ONE packet
//! nobody finished converts a cadence into never-again. The alternative
//! fix was to have each rule record its own suppression, which spreads
//! the logic across sixty-odd rule rows and still leaves the judgement
//! ("how long is too long?") nowhere. Instead this sweep reads the
//! guard, asks the guard's own question, and reports the silence as
//! SUPPRESSED — naming the packet that is holding the cadence, which is
//! also the remedy — once the block has outlasted
//! [`SUPPRESSION_INTERVALS`] of the declared interval. A block younger
//! than that is the guard doing its job, and stays quiet.
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
use boss_dispatcher::rules::registry::RawRule;

use super::cadence_roster::{ClockCadence, Guard, clock_cadences};
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

/// How many of its own declared intervals a cadence may stay SUPPRESSED
/// by its dedup guard before the suppression is itself the finding.
///
/// Three, one more than [`SILENCE_MULTIPLIER`], because an explained
/// silence is not the same event as an unexplained one: for the first
/// day or two the guard is doing exactly its job — yesterday's chore is
/// still open, so today has nothing new to say, and spawning a twin
/// would duplicate the obligation rather than discharge it (0517387b,
/// 9f0c566a). Past three intervals that reading stops being credible:
/// nobody is finishing the packet, and the cadence has been converted
/// into never-again. The two measured cases cleared this by a wide
/// margin — nine days for the seven sweeps, NINETEEN for the mirror
/// (cf0f5e2d) — and the number is deliberately a constant rather than a
/// rule arg, like [`SILENCE_MULTIPLIER`] beside it: it is a calibration
/// of this sweep's own judgement, not a per-cadence fact.
const SUPPRESSION_INTERVALS: i64 = 3;

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
    /// The rules the dispatcher is ENFORCING — the roster's second
    /// source (see [`super::cadence_roster`]).
    ///
    /// This is the registry snapshot the rules runner loaded at startup,
    /// handed in as an argument, not fetched. Two reasons, in order.
    /// First, it is what is actually FIRING: a published row the runner
    /// has not picked up yet (hot-reload is a planned follow-up) is a
    /// rule nothing is running, and a sweep that judged silence against
    /// rules nobody enforces would report a cadence as watched when it
    /// is not. Second, a handler that reports through the service it
    /// lives inside inherits that service's outages — CLAUDE.md
    /// §Diagnosis, "an alarm that reports through its subject dies with
    /// it" — and the rows are right here. The shape is exactly what
    /// `GET /api/dispatcher/rules` serves, so the derivation is
    /// unchanged if the reader ever becomes that surface.
    rules: Vec<RawRule>,
}

impl CadenceSilenceSweep {
    pub fn new(
        jobs_base: impl Into<String>,
        clock_url: impl Into<String>,
        rules: Vec<RawRule>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            clock: Arc::new(boss_clock_client::ReqwestClockClient::new(clock_url)),
            rules,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// Ask one cadence's dedup guard its OWN question — is there an open
    /// packet of `(kind, subject)`? — and answer with the one that has
    /// been open longest, which is the packet holding the cadence and the
    /// remedy for it.
    ///
    /// `Ok(None)` is "nothing is holding it", which leaves the silence
    /// unexplained and therefore still a finding.
    async fn blocking_packet(
        &self,
        kind: &str,
        subject: &str,
        guard: &str,
        rule_name: &str,
    ) -> Result<Option<Block>, HandlerError> {
        let url = format!(
            "{}/api/jobs?kind={kind}&status=open&subject_id={subject}&limit={DEDUP_PAGE}",
            self.base()
        );
        let listing = get_json(&self.client, &url, rule_name).await?;
        let rows: Vec<Value> = listing
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(oldest_open_block(&rows, guard))
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

/// Where one watched cadence's declaration came from. Carried into the
/// alarm so a reader knows which registry row to edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An `interval_minutes.<kind>` arg on this sweep's own rule row —
    /// the timer-executed chores.
    Args,
    /// Derived from the clock rule that spawns the packet.
    ClockRule(String),
}

impl Source {
    fn as_value(&self) -> Value {
        match self {
            Source::Args => json!("cadence-silence-sweep-daily:args"),
            Source::ClockRule(rule) => json!(format!("rule:{rule}")),
        }
    }
}

/// One cadence this sweep watches: what identifies its packets, how
/// often they are expected, where that was declared, and what may
/// legitimately suppress the firing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// The identity, in one string: `<kind>` for a timer-declared
    /// cadence, `<kind>/<subject>` for a clock-rule one. The dedup key
    /// and the name in every message.
    pub label: String,
    pub kind: String,
    /// Set only when the cadence is identified by subject as well as
    /// kind — seven sweep rules share one kind.
    pub subject: Option<String>,
    pub declared: Declared,
    pub source: Source,
    /// The guard that can legitimately hold this cadence, when it has
    /// one. Only a clock rule has a guard; a timer has no `when`.
    pub guard: Option<Guard>,
}

impl Watched {
    /// A cadence declared by one of this sweep's own args.
    pub fn from_arg(kind: impl Into<String>, declared: Declared) -> Self {
        let kind = kind.into();
        Self {
            label: kind.clone(),
            kind,
            subject: None,
            declared,
            source: Source::Args,
            guard: None,
        }
    }

    /// A cadence derived from a clock rule.
    pub fn from_clock_rule(c: &ClockCadence) -> Self {
        Self {
            label: c.label(),
            kind: c.kind.clone(),
            subject: Some(c.subject.clone()),
            declared: Declared::Minutes(c.interval_min),
            source: Source::ClockRule(c.rule.clone()),
            guard: c.guard.clone(),
        }
    }

    /// The listing that answers "when did a packet of this identity last
    /// arrive?" — `limit=1`, because only the newest matters.
    fn newest_packet_path(&self) -> String {
        match &self.subject {
            Some(s) => format!("/api/jobs?kind={}&subject_id={}&limit=1", self.kind, s),
            None => format!("/api/jobs?kind={}&limit=1", self.kind),
        }
    }
}

/// THE roster: both sources, merged, label-sorted so a pass is
/// deterministic.
///
/// A label claimed twice becomes [`Declared::Undetermined`] rather than
/// one entry silently winning — the same fail-closed posture an
/// unreadable interval gets. (The coarser collision — an arg declaring a
/// KIND that a clock rule declares per subject — cannot be seen from one
/// label and is pinned in CI instead, by
/// `no_clock_rule_kind_is_also_declared_on_the_sweeps_args`.)
pub fn watched_cadences(
    args: &[(String, boss_dispatcher::rules::expr::Value)],
    rules: &[RawRule],
) -> Vec<Watched> {
    let mut out: BTreeMap<String, Watched> = declarations(args)
        .into_iter()
        .map(|(kind, declared)| (kind.clone(), Watched::from_arg(kind, declared)))
        .collect();
    let (cadences, _skipped) = clock_cadences(rules);
    for c in &cadences {
        let w = Watched::from_clock_rule(c);
        match out.get_mut(&w.label) {
            Some(existing) => {
                let first = match &existing.source {
                    Source::Args => "this sweep's own args".to_string(),
                    Source::ClockRule(r) => format!("clock rule `{r}`"),
                };
                existing.declared = Declared::Undetermined(format!(
                    "`{}` is declared twice — by {first} and by clock rule `{}`; one fact in \
                     two places, so neither is trusted",
                    w.label, c.rule
                ));
            }
            None => {
                out.insert(w.label.clone(), w);
            }
        }
    }
    out.into_values().collect()
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
    /// Silent, and the cadence's OWN dedup guard is why: an open packet
    /// the guard asks about has been sitting there. Not a dead chore — a
    /// BLOCKED one, whose remedy is the named packet.
    ///
    /// `alarming` is the judgement [`SUPPRESSION_INTERVALS`] encodes: a
    /// young block is the guard working as designed and says nothing,
    /// while a block past the window has converted the cadence into
    /// never-again. It is also true when the silence STARTED before the
    /// block (`age_min` much larger than `blocked_min`), because a block
    /// cannot explain a quiet that predates it.
    Suppressed {
        interval_min: i64,
        age_min: i64,
        last: String,
        /// How long the blocking packet has been open.
        blocked_min: i64,
        since: String,
        by_job: String,
        by_title: String,
        /// The guard verbatim, so the alarm quotes what an operator will
        /// go and read.
        guard: String,
        alarming: bool,
    },
    /// The declaration could not be read. Fail-closed: reported, not
    /// skipped.
    Undetermined { reason: String },
}

impl Verdict {
    /// Is this verdict worth a packet?
    pub fn is_finding(&self) -> bool {
        match self {
            Verdict::Fresh => false,
            // A suppression inside its window is the guard doing its job.
            Verdict::Suppressed { alarming, .. } => *alarming,
            _ => true,
        }
    }

    /// Is this a silence the guard EXPLAINS but that is not yet worth
    /// waking anyone for? Neither a finding nor a clear: the cadence is
    /// quiet on purpose, so a standing alarm must not be closed as
    /// "arriving again" either.
    pub fn is_explained(&self) -> bool {
        matches!(
            self,
            Verdict::Suppressed {
                alarming: false,
                ..
            }
        )
    }
}

/// The open packet one cadence's guard is holding it behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub job_id: String,
    pub title: String,
    pub opened: DateTime<Utc>,
    /// The guard that asked about it, verbatim.
    pub guard: String,
}

/// The OLDEST open packet in a guard's listing — the one nobody drained,
/// which is both the longest-standing block and the actionable remedy.
/// A row whose open instant cannot be read is skipped here and cannot
/// suppress anything, which keeps an unreadable row loud rather than
/// quiet.
pub fn oldest_open_block(rows: &[Value], guard: &str) -> Option<Block> {
    rows.iter()
        .filter_map(|row| {
            Some(Block {
                job_id: row.get("id").and_then(Value::as_str)?.to_string(),
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)")
                    .to_string(),
                opened: packet_at(row)?,
                guard: guard.to_string(),
            })
        })
        .min_by_key(|b| b.opened)
}

/// How long a cadence may stay suppressed before the suppression is the
/// finding: [`SUPPRESSION_INTERVALS`]x its interval, never less than
/// [`MIN_SILENCE_WINDOW_MIN`].
pub fn suppression_window_min(interval_min: i64) -> i64 {
    (SUPPRESSION_INTERVALS * interval_min).max(MIN_SILENCE_WINDOW_MIN)
}

/// Fold the packet that explains a silence into the verdict.
///
/// Only a [`Verdict::Silent`] can be explained this way. `NeverFiled`
/// deliberately is not: for every shipped guard the blocking packet IS a
/// packet of the measured identity, so "no packet ever" plus "a packet is
/// open" is a contradiction the sweep reports rather than reconciles.
pub fn explained_by(base: Verdict, block: &Block, now: DateTime<Utc>) -> Verdict {
    let Verdict::Silent {
        interval_min,
        age_min,
        last,
    } = base
    else {
        return base;
    };
    let blocked_min = (now - block.opened).num_minutes();
    // Quiet that predates the block is quiet the block cannot account
    // for, so it still alarms — a one-hour block must not bury five days
    // of unexplained silence.
    let unexplained_min = age_min - blocked_min;
    let alarming = blocked_min > suppression_window_min(interval_min)
        || unexplained_min > silence_window_min(interval_min);
    Verdict::Suppressed {
        interval_min,
        age_min,
        last,
        blocked_min,
        since: block.opened.to_rfc3339(),
        by_job: block.job_id.clone(),
        by_title: block.title.clone(),
        guard: block.guard.clone(),
        alarming,
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
fn headline(label: &str, v: &Verdict) -> String {
    match v {
        Verdict::Fresh => format!("{label} is arriving on cadence"),
        Verdict::Silent {
            interval_min,
            age_min,
            ..
        } => format!(
            "{label} has filed no packet for {} — it is declared every {}",
            human_minutes(*age_min),
            human_minutes(*interval_min)
        ),
        Verdict::NeverFiled { interval_min } => format!(
            "{label} is declared every {} and NO packet of that kind has ever been filed",
            human_minutes(*interval_min)
        ),
        Verdict::Suppressed {
            interval_min,
            age_min,
            blocked_min,
            by_job,
            guard,
            ..
        } => format!(
            "{label} has filed no packet for {} — it is declared every {}, and its own guard \
             ({guard}) has been held open for {} by packet {by_job}",
            human_minutes(*age_min),
            human_minutes(*interval_min),
            human_minutes(*blocked_min)
        ),
        Verdict::Undetermined { .. } => {
            format!("{label} is declared as cadenced but its interval cannot be read")
        }
    }
}

/// What to DO about this finding — the sentence that separates a dead
/// chore from a blocked one. A verdict someone must go re-derive is not
/// a verdict (CLAUDE.md §Diagnosis).
fn remedy(label: &str, v: &Verdict) -> String {
    match v {
        Verdict::Suppressed {
            by_job,
            by_title,
            since,
            guard,
            blocked_min,
            interval_min,
            ..
        } => format!(
            "This cadence is not dead, it is BLOCKED, and the block is the remedy: the rule \
             fires only when `{guard}`, and packet {by_job} (\"{by_title}\") has been open \
             since {since} — {} against a declared interval of {}. Drive that packet to a \
             terminal and the next clock day spawns `{label}` again. ONE unfinished packet \
             converts a cadence into never-again: measured 2026-09-10 (cf0f5e2d), an open \
             publish packet from 2026-08-22 held the daily mirror check for NINETEEN days \
             while the public mirror drifted 238 commits / 763 files behind, and seven \
             undrained sweep packets from 2026-09-01 held their seven rules for nine days. \
             The threshold for saying so is {SUPPRESSION_INTERVALS} intervals, never under \
             {MIN_SILENCE_WINDOW_MIN} minutes.",
            human_minutes(*blocked_min),
            human_minutes(*interval_min)
        ),
        _ => format!(
            "The interval is declared on this sweep's own dispatcher rule row \
             (`{ARG_PREFIX}{label}`) or derived from the clock rule that spawns the packet \
             (see cadence_roster); the actual is the newest packet of that identity in the \
             system of record. Threshold is {SILENCE_MULTIPLIER}x the declared interval and \
             never under {MIN_SILENCE_WINDOW_MIN} minutes, so one missed firing is weather \
             and a daily sweep cannot cry wolf about a five-minute chore."
        ),
    }
}

/// The measured facts a finding carries — written on the raise AND, as
/// a metadata merge, on every refresh of a standing alarm, so the
/// packet always shows TODAY's measurement rather than the day it was
/// first raised.
pub fn measurement(w: &Watched, v: &Verdict, now: DateTime<Utc>) -> Map<String, Value> {
    let mut m = Map::new();
    // `cadence_kind` stays the packet KIND, as it has been since the
    // sweep's first alarm; `cadence` is the full identity, which for a
    // clock-rule cadence is kind + target.
    m.insert("cadence_kind".into(), json!(w.kind));
    m.insert("cadence".into(), json!(w.label));
    m.insert(
        "cadence_target".into(),
        w.subject.as_ref().map_or(Value::Null, |s| json!(s)),
    );
    m.insert("cadence_declared_by".into(), w.source.as_value());
    m.insert("last_measured_at".into(), json!(now.to_rfc3339()));
    m.insert("condition".into(), json!(headline(&w.label, v)));
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
        Verdict::Suppressed {
            interval_min,
            age_min,
            last,
            blocked_min,
            since,
            by_job,
            by_title,
            guard,
            alarming,
        } => {
            m.insert("expected_interval_minutes".into(), json!(interval_min));
            m.insert("silent_for_minutes".into(), json!(age_min));
            m.insert("last_packet_at".into(), json!(last));
            m.insert("suppressed".into(), json!(true));
            m.insert("suppressed_for_minutes".into(), json!(blocked_min));
            m.insert("suppressed_since".into(), json!(since));
            m.insert("suppressed_by_job".into(), json!(by_job));
            m.insert("suppressed_by_title".into(), json!(by_title));
            m.insert("suppression_guard".into(), json!(guard));
            m.insert(
                "suppression_threshold_minutes".into(),
                json!(suppression_window_min(*interval_min)),
            );
            m.insert("past_suppression_threshold".into(), json!(alarming));
        }
        Verdict::Undetermined { reason } => {
            m.insert("expected_interval_minutes".into(), json!("undetermined"));
            m.insert("undetermined_reason".into(), json!(reason));
        }
    }
    m
}

/// The urgent packet one silent kind becomes.
pub fn alarm_body(w: &Watched, v: &Verdict, evidence: &str, now: DateTime<Utc>) -> Value {
    let label = &w.label;
    let mut metadata = measurement(w, v, now);
    metadata.insert("area".into(), json!("estate-observation"));
    metadata.insert("cadence_silence".into(), json!(silence_key(label)));
    metadata.insert("reporter".into(), json!("cadence.silence.sweep"));
    metadata.insert(
        "detail".into(),
        json!(format!(
            "Raised by cadence.silence.sweep (backlog ecca2f43): {}. {} This alarm UPDATES \
             itself on each daily pass and CLOSES itself (`stale`) the moment a packet of \
             `{label}` arrives again — so if it is still open, the cadence is still quiet. \
             Precedent for why this exists: maintenance-ml-inference-batch died in \
             ExecStartPre for 23 nights (e109f57e) and the five-minute \
             maintenance-estate-observe-units observer was quiet for four days (408c81f6); \
             both were found by hand, as was the nineteen-day mirror drift the suppression \
             half of this sweep exists for (cf0f5e2d). Evidence: {evidence}.",
            headline(label, v),
            remedy(label, v)
        )),
    );
    json!({
        "kind": "backlog-item",
        "title": alarm_title(label, v),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": "emp-david",
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": Value::Object(metadata),
    })
}

fn alarm_title(label: &str, v: &Verdict) -> String {
    match v {
        Verdict::Fresh => format!("CADENCE OK: {label}"),
        Verdict::Silent { age_min, .. } => format!(
            "CADENCE SILENT: {label} — no packet for {}",
            human_minutes(*age_min)
        ),
        Verdict::NeverFiled { .. } => {
            format!("CADENCE SILENT: {label} — no packet has ever arrived")
        }
        // A blocked cadence reads differently from a dead one on
        // purpose: the title carries the remedy's shape, because the
        // operator's next move is a packet, not a host.
        Verdict::Suppressed { blocked_min, .. } => format!(
            "CADENCE SUPPRESSED: {label} — its own guard held {} by an open packet",
            human_minutes(*blocked_min)
        ),
        Verdict::Undetermined { .. } => {
            format!("CADENCE UNDECLARED: {label} — expected interval cannot be read")
        }
    }
}

/// The metadata merge that refreshes a STANDING alarm instead of
/// filing a twin. `PATCH /api/jobs/{id}/metadata` merges top-level
/// keys, so this is exactly the fields that change between passes.
pub fn refresh_patch(w: &Watched, v: &Verdict, now: DateTime<Utc>) -> Value {
    Value::Object(measurement(w, v, now))
}

/// The triage completion that CLOSES a standing alarm when the kind
/// starts arriving again. `disposition = "stale"` is the backlog-item
/// terminal titled "Closed — the claim no longer holds", which is
/// precisely true: the cadence is no longer silent.
///
/// PUT on a step REPLACES top-level metadata, so the step's existing
/// keys (`authority_role`) are carried through by the caller.
pub fn clear_step_body(existing: &Map<String, Value>, label: &str, v: &Verdict) -> Value {
    let mut metadata = existing.clone();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "cadence.silence.sweep re-measured `{label}` and it is arriving again: {}. \
             The claim this alarm carried no longer holds; closed by machine, not by \
             judgement.",
            match v {
                Verdict::Fresh => "its newest packet is inside the declared window".to_string(),
                other => headline(label, other),
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
        let watched = watched_cadences(args, &self.rules);
        if watched.is_empty() {
            // A sweep declaring nothing watches nothing, and would
            // report "all clear" forever. That is the silent-check
            // class this handler exists to end, so it is an error.
            return Err(HandlerError::Permanent(
                "cadence.silence.sweep: the roster is empty — the rule row declares no \
                 `interval_minutes.<kind>` args AND no enforced clock rule spawns a packet \
                 on a schedule. A sweep with an empty roster is a check that is not running"
                    .into(),
            ));
        }
        let now = boss_clock_client::now_from(&self.clock).await;
        let evidence = format!(
            "cadence.silence.sweep firing on rule {} (event {} / topic {})",
            ctx.rule_name, ctx.triggering_event_id, ctx.triggering_topic
        );

        // Best-effort accumulator, the estate alarm's posture: one
        // cadence's failed read must never block every other cadence.
        // Every sub-op that failed surfaces as ONE error at the end, so
        // a transient still NAKs for retry (dedup makes the retry
        // idempotent) while every alarm that COULD raise, did.
        let mut errors: Vec<String> = Vec::new();
        let mut findings: Vec<(&Watched, Verdict)> = Vec::new();
        let mut clear: Vec<(&Watched, Verdict)> = Vec::new();

        for w in &watched {
            // An undetermined declaration needs no read: the finding is
            // that we cannot measure it.
            if let Declared::Undetermined(_) = &w.declared {
                findings.push((w, verdict(&w.declared, None, now)));
                continue;
            }
            let listing = match get_json(
                &self.client,
                &format!("{}{}", self.base(), w.newest_packet_path()),
                &ctx.rule_name,
            )
            .await
            {
                Ok(l) => l,
                Err(e) => {
                    errors.push(format!(
                        "newest-packet read for {} failed; other cadences still swept: {e}",
                        w.label
                    ));
                    continue;
                }
            };
            let rows: Vec<Value> = listing
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut v = verdict(&w.declared, newest_packet_at(&rows), now);
            // A silence with a guard gets ONE more question: is the
            // cadence blocked rather than dead? Asked only when there is
            // already a finding — a cadence arriving on time needs no
            // explanation — so the common pass costs nothing.
            if v.is_finding()
                && let Some(Guard::OpenPacket {
                    kind,
                    subject,
                    source,
                }) = &w.guard
            {
                match self
                    .blocking_packet(kind, subject, source, &ctx.rule_name)
                    .await
                {
                    Ok(Some(block)) => v = explained_by(v, &block, now),
                    Ok(None) => {}
                    Err(e) => {
                        // The silence still raises, as an UNEXPLAINED
                        // one — losing the explanation is better than
                        // losing the alarm.
                        errors.push(format!(
                            "guard read for {} failed, so its silence is reported \
                             unexplained: {e}",
                            w.label
                        ));
                    }
                }
            }
            if v.is_finding() {
                findings.push((w, v));
            } else if v.is_explained() {
                // Quiet on purpose: the guard is holding it and has not
                // held it long enough to be the defect. Neither raise
                // nor clear — clearing would claim the cadence is
                // arriving again, which is not what was measured.
                tracing::info!(
                    cadence = %w.label,
                    "cadence.silence.sweep: suppressed by an open packet, inside the window"
                );
            } else {
                clear.push((w, v));
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

        // Raise or refresh, one packet per quiet cadence.
        for (w, v) in &findings {
            let label = &w.label;
            let key = silence_key(label);
            if let Some(existing) = open.get(&key) {
                let Some(id) = existing.get("id").and_then(Value::as_str) else {
                    errors.push(format!("open alarm for {label} has no id; not refreshed"));
                    continue;
                };
                if let Err(e) = write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{id}/metadata", self.base()),
                    &refresh_patch(w, v, now),
                    &ctx.rule_name,
                )
                .await
                {
                    errors.push(format!("refresh of the open alarm for {label} failed: {e}"));
                    continue;
                }
                tracing::info!(cadence = %label, "cadence.silence.sweep refreshed a standing alarm");
                continue;
            }
            if settled.contains(&key) {
                tracing::info!(cadence = %label, "cadence.silence.sweep: settled by a human inside the window — not re-raising");
                continue;
            }
            if let Err(e) = post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &alarm_body(w, v, &evidence, now),
                &ctx.rule_name,
            )
            .await
            {
                errors.push(format!(
                    "raise for {label} failed; other findings still raised: {e}"
                ));
                continue;
            }
            tracing::info!(cadence = %label, "cadence.silence.sweep raised a packet");
        }

        // A cadence that came back closes its own alarm.
        for (w, v) in &clear {
            let label = &w.label;
            let Some(existing) = open.get(&silence_key(label)) else {
                continue;
            };
            let Some(id) = existing.get("id").and_then(Value::as_str) else {
                errors.push(format!("open alarm for {label} has no id; not cleared"));
                continue;
            };
            let Some((step_id, step_meta)) = triage_step(existing) else {
                errors.push(format!(
                    "open alarm for {label} has no `{TRIAGE_SLUG}` step; cannot close itself"
                ));
                continue;
            };
            if let Err(e) = write_json(
                &self.client,
                reqwest::Method::PUT,
                &format!("{}/api/jobs/{id}/steps/{step_id}", self.base()),
                &clear_step_body(&step_meta, label, v),
                &ctx.rule_name,
            )
            .await
            {
                errors.push(format!("auto-close of the alarm for {label} failed: {e}"));
                continue;
            }
            tracing::info!(cadence = %label, "cadence.silence.sweep closed its own alarm — the cadence is arriving again");
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

    /// A timer-declared cadence, the shape every pre-existing alarm has.
    fn watched(kind: &str) -> Watched {
        Watched::from_arg(kind, Declared::Minutes(1440))
    }

    /// The image-freshness sweep as the registry holds it — the rule that
    /// went nine days dead behind its own guard (cf0f5e2d).
    fn image_freshness_rule() -> RawRule {
        RawRule {
            name: "maintenance-sweep-image-freshness-daily".into(),
            on_event: None,
            schedule: Some(boss_dispatcher::rules::registry::RawSchedule {
                cadence: boss_core::calendar::Cadence::Daily,
                anchor_date: "2026-08-14".parse().expect("a date"),
                business_calendar: None,
            }),
            when: Some(r#"NOT open_job_exists("maintenance-sweep", "image-freshness")"#.into()),
            do_steps: vec![boss_dispatcher::rules::registry::RawDoStep {
                handler: "jobs.spawn".into(),
                args: [
                    ("kind".to_string(), r#""maintenance-sweep""#.to_string()),
                    ("subject".to_string(), r#""image-freshness""#.to_string()),
                ]
                .into_iter()
                .collect(),
            }],
            delay: None,
            version: 2,
        }
    }

    /// The open packet that held it: spawned 2026-09-01, never drained.
    fn blocking_packet() -> Value {
        json!({
            "id": "c7a48d7f-0000-0000-0000-000000000000",
            "status": "open",
            "title": "CI image freshness sweep",
            "opened_on": "2026-09-01",
            "metadata": {"opened_at": "2026-09-01T03:05:00+00:00", "target": "image-freshness"},
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
        let raised = alarm_body(&watched(kind), &v, "first pass", at("2026-09-08T12:00:00Z"));
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
        let patch = refresh_patch(&watched(kind), &v, now);
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
        let body = alarm_body(
            &Watched::from_arg("maintenance-estate-observe-units", Declared::Minutes(5)),
            &v,
            "evidence",
            now,
        );
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

    // -- the roster's two sources ----------------------------------------

    /// The timer kinds and the clock-rule cadences land in ONE roster,
    /// and a clock cadence is identified by kind AND subject — the
    /// distinction seven sibling sweep rules need.
    #[test]
    fn the_roster_merges_the_args_with_the_clock_rules() {
        let args = vec![(
            "interval_minutes.maintenance-backup".to_string(),
            ExprValue::Int(1440),
        )];
        let roster = watched_cadences(&args, &[image_freshness_rule()]);
        let labels: Vec<&str> = roster.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["maintenance-backup", "maintenance-sweep/image-freshness"]
        );

        let timer = &roster[0];
        assert_eq!(timer.source, Source::Args);
        assert_eq!(timer.guard, None, "a timer has no `when` to suppress it");
        assert_eq!(
            timer.newest_packet_path(),
            "/api/jobs?kind=maintenance-backup&limit=1"
        );

        let clock = &roster[1];
        assert_eq!(
            clock.source,
            Source::ClockRule("maintenance-sweep-image-freshness-daily".into())
        );
        assert_eq!(clock.declared, Declared::Minutes(1440));
        assert_eq!(
            clock.newest_packet_path(),
            "/api/jobs?kind=maintenance-sweep&subject_id=image-freshness&limit=1",
            "a clock cadence is measured per TARGET; six silent siblings hid behind the kind"
        );
    }

    /// §9a at runtime: one identity declared by both sources is trusted
    /// from neither. (CI pins the coarser case — an arg for a KIND a
    /// clock rule declares per subject — since one label cannot see it.)
    #[test]
    fn an_identity_declared_by_both_sources_is_undetermined() {
        let args = vec![(
            "interval_minutes.maintenance-sweep/image-freshness".to_string(),
            ExprValue::Int(1440),
        )];
        let roster = watched_cadences(&args, &[image_freshness_rule()]);
        assert_eq!(roster.len(), 1, "one identity stays one entry");
        assert!(
            matches!(roster[0].declared, Declared::Undetermined(_)),
            "{:?}",
            roster[0].declared
        );
        assert!(
            verdict(&roster[0].declared, None, at("2026-09-10T12:00:00Z")).is_finding(),
            "a double declaration is reported, not silently resolved"
        );
    }

    // -- suppression: a blocked cadence is not a dead one ----------------

    /// THE measured case. `maintenance-sweep/image-freshness` was silent
    /// nine days because the packet its own guard asks about was opened
    /// 2026-09-01 and never drained. Before this, that was reported as a
    /// plain silence — or rather, not reported at all, since the cadence
    /// was off the roster entirely.
    #[test]
    fn a_block_past_three_intervals_is_reported_as_suppression_and_names_the_packet() {
        let now = at("2026-09-10T03:05:00Z");
        let base = verdict(
            &Declared::Minutes(1440),
            newest_packet_at(&[blocking_packet()]),
            now,
        );
        assert!(matches!(base, Verdict::Silent { .. }), "{base:?}");

        let block = oldest_open_block(
            &[blocking_packet()],
            r#"NOT open_job_exists("maintenance-sweep", "image-freshness")"#,
        )
        .expect("the open packet is readable");
        let v = explained_by(base, &block, now);

        let Verdict::Suppressed {
            blocked_min,
            by_job,
            by_title,
            alarming,
            ..
        } = &v
        else {
            panic!("expected Suppressed, got {v:?}");
        };
        assert!(*alarming, "nine days is past three daily intervals");
        assert_eq!(*blocked_min, 12_960, "nine days, in minutes");
        assert_eq!(by_job, "c7a48d7f-0000-0000-0000-000000000000");
        assert_eq!(by_title, "CI image freshness sweep");
        assert!(v.is_finding());

        let (mut cadences, _) = clock_cadences(&[image_freshness_rule()]);
        let w = Watched::from_clock_rule(&cadences.remove(0));
        let body = alarm_body(&w, &v, "evidence", now);
        let title = body["title"].as_str().expect("a title");
        assert!(title.starts_with("CADENCE SUPPRESSED:"), "{title}");
        assert!(
            title.contains("maintenance-sweep/image-freshness"),
            "{title}"
        );
        let meta = &body["metadata"];
        assert_eq!(meta["suppressed"], json!(true));
        assert_eq!(
            meta["suppressed_by_job"],
            json!("c7a48d7f-0000-0000-0000-000000000000"),
            "a verdict must name what failed — here, the packet to drain"
        );
        assert_eq!(meta["suppressed_since"], json!("2026-09-01T03:05:00+00:00"));
        assert_eq!(meta["suppression_threshold_minutes"], json!(4320));
        assert_eq!(meta["cadence_target"], json!("image-freshness"));
        assert_eq!(
            meta["cadence_declared_by"],
            json!("rule:maintenance-sweep-image-freshness-daily"),
            "the alarm says which registry row declares the cadence"
        );
        let detail = meta["detail"].as_str().expect("a detail");
        assert!(
            detail.contains("Drive that packet to a terminal"),
            "the remedy must be in the packet, not re-derived: {detail}"
        );
    }

    /// The guard doing its job says nothing. Yesterday's chore still
    /// open is exactly why the guard exists (0517387b) — an alarm there
    /// would fire on every healthy two-day chore and train operators to
    /// ignore it.
    #[test]
    fn a_block_inside_the_window_is_explained_and_stays_quiet() {
        let now = at("2026-09-10T12:00:00Z");
        let base = Verdict::Silent {
            interval_min: 1440,
            age_min: 4320, // three days
            last: "2026-09-07T12:00:00+00:00".into(),
        };
        let block = Block {
            job_id: "open-1".into(),
            title: "CI image freshness sweep".into(),
            opened: at("2026-09-08T12:00:00Z"), // two days
            guard: "NOT open_job_exists(...)".into(),
        };
        let v = explained_by(base, &block, now);
        assert!(!v.is_finding(), "{v:?}");
        assert!(v.is_explained(), "{v:?}");
    }

    /// A young block must not bury an old silence: quiet that predates
    /// the block is quiet the block cannot account for.
    #[test]
    fn a_silence_that_started_before_the_block_still_alarms() {
        let now = at("2026-09-10T12:00:00Z");
        let base = Verdict::Silent {
            interval_min: 1440,
            age_min: 14_400, // ten days
            last: "2026-08-31T12:00:00+00:00".into(),
        };
        let block = Block {
            job_id: "open-2".into(),
            title: "filed by hand an hour ago".into(),
            opened: at("2026-09-10T11:00:00Z"),
            guard: "NOT open_job_exists(...)".into(),
        };
        let v = explained_by(base, &block, now);
        assert!(
            v.is_finding(),
            "ten days of silence with a one-hour block is still a finding: {v:?}"
        );
    }

    /// `NeverFiled` is never explained away: for every shipped guard the
    /// blocking packet IS a packet of the measured identity, so "no
    /// packet ever" plus "a packet is open" is a contradiction to report.
    #[test]
    fn a_kind_that_never_filed_is_not_explained_by_a_block() {
        let now = at("2026-09-10T12:00:00Z");
        let block = Block {
            job_id: "open-3".into(),
            title: "x".into(),
            opened: at("2026-01-01T00:00:00Z"),
            guard: "g".into(),
        };
        let v = explained_by(Verdict::NeverFiled { interval_min: 1440 }, &block, now);
        assert_eq!(v, Verdict::NeverFiled { interval_min: 1440 });
        assert!(v.is_finding());
    }

    /// The block named is the OLDEST open packet — the one nobody
    /// drained, which is both the longest-standing block and the
    /// actionable remedy.
    #[test]
    fn the_oldest_open_packet_is_the_named_block() {
        let rows = vec![
            json!({"id": "newer", "title": "n", "metadata": {"opened_at": "2026-09-08T00:00:00+00:00"}}),
            json!({"id": "oldest", "title": "o", "metadata": {"opened_at": "2026-09-01T00:00:00+00:00"}}),
            json!({"id": "unreadable", "title": "u"}),
        ];
        let block = oldest_open_block(&rows, "g").expect("a block");
        assert_eq!(block.job_id, "oldest");
    }

    #[test]
    fn the_suppression_window_is_three_intervals_with_the_same_floor() {
        assert_eq!(suppression_window_min(1440), 4320);
        assert_eq!(suppression_window_min(5), MIN_SILENCE_WINDOW_MIN);
        assert!(
            suppression_window_min(1440) > silence_window_min(1440),
            "an explained silence gets more rope than an unexplained one"
        );
    }

    #[test]
    fn an_undetermined_declaration_files_a_distinct_alarm() {
        let now = at("2026-09-08T12:00:00Z");
        let v = Verdict::Undetermined {
            reason: "declared interval is a string, not a positive number of minutes".into(),
        };
        let body = alarm_body(
            &Watched::from_arg(
                "maintenance-mystery",
                Declared::Undetermined("unreadable".into()),
            ),
            &v,
            "evidence",
            now,
        );
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
