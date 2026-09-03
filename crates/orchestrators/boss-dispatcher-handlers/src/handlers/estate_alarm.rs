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
//!   `disk_tight` (a host below the floor a gate needs), and
//!   `units_unhealthy` (a watched unit the observer derived sick).
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
//! The raise is an URGENT packet on the operator's queue naming host +
//! condition + the latest evidence excerpt. Delivery beyond the queue
//! (push, phone) is deliberately NOT built here — that is channel
//! work for the harness, and the packet is the system-of-record raise
//! it would deliver.
//!
//! A no-op, not an error, when findings are absent, not yet
//! persistent, or already raised.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{api_client, get_json, post_json};
use super::estate_compare::{HOST_SCOPE, KNOWN_SCOPE, UNITS_SCOPE};

/// Consecutive same-series comparisons a hard finding must survive to
/// raise. Three: at the tightened 15-minute observer cadence that is
/// ~30-45 minutes of a node being gone or NotReady — slower than a
/// pager, far faster than "David asked hours later", and immune to a
/// single flapped reading. (The five-minute unit series raises in ~15
/// minutes; the daily host-disk series in three readings, which is
/// what its cadence affords — tightening that is a timer-file change,
/// not an alarm change.)
const PERSIST_N: usize = 3;

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
const WATCHED_SERIES: [(&str, bool); 3] = [
    (KNOWN_SCOPE, false),
    (HOST_SCOPE, true),
    (UNITS_SCOPE, true),
];

pub struct EstateAlarm {
    client: reqwest::Client,
    jobs_base: String,
    /// The `now` the silence sweep measures observation ages against.
    /// The dispatcher is not on the no-wallclock allowlist, so this
    /// comes from the clock service like every other stamp.
    clock: Arc<dyn boss_clock_client::ClockClient>,
}

impl EstateAlarm {
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

/// Just the keys of [`hard_findings`] — the set the persistence
/// intersection runs over.
fn hard_finding_keys(comparison: &Value) -> BTreeSet<String> {
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
/// entry per stale series: host, scope, last_observed_at, cadence_s,
/// age_s — the evidence the raise will carry.
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
            out.push(json!({
                "host": id,
                "scope": scope,
                "last_observed_at": times[0].to_rfc3339(),
                "cadence_s": cadence,
                "age_s": age,
            }));
        }
    }
    out
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
fn alarm_body(key: &str, scope: &str, host: Option<&str>, evidence: &str, excerpt: &str) -> Value {
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
        "owner_id": "emp-david",
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": metadata,
    })
}

/// The dedup key of one stale series: `unobserved:<host>` — one
/// condition per quiet host, even when both of its series go dark.
fn unobserved_key(stale: &Value) -> String {
    format!(
        "unobserved:{}",
        stale
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    )
}

/// The urgent packet one quiet series becomes. No persistence count in
/// the title — the staleness window IS the persistence.
fn staleness_body(stale: &Value, evidence: &str) -> Value {
    let host = stale
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let scope = stale.get("scope").and_then(Value::as_str).unwrap_or("?");
    json!({
        "kind": "backlog-item",
        "title": format!(
            "ESTATE ALARM: {host} unobserved — {scope} series quiet past \
             {STALE_MULTIPLIER}x cadence"
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": "emp-david",
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": {
            "area": "estate",
            "estate_finding": unobserved_key(stale),
            "scope": scope,
            "host": host,
            "detail": format!(
                "Raised by estate.alarm's silence sweep (a7a19a1a: an alarm that only \
                 hears what its sources say dies with its patient — an expected series \
                 that stops arriving IS a finding). The `{scope}` observation series \
                 for `{host}` went quiet: {evidence_json}. Either the observer died \
                 (the quiet-observer class) or the host itself is down; either way \
                 nothing downstream of this series can see that host any more. \
                 Evidence: {evidence}. The series rides \
                 /api/estate/observations?scope={scope}.",
                evidence_json = excerpt(stale),
            ),
        },
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

        // First-key-wins map: two stale series on one host collapse to
        // one raise before the dedup fetch ever runs.
        let mut to_raise: BTreeMap<String, Value> = BTreeMap::new();

        // --- The persistence half: does the TRIGGERING comparison's
        // finding survive the last PERSIST_N of its own series? Only
        // worth a fetch when it found something hard at all.
        let hard = hard_findings(comparison);
        if !hard.is_empty() {
            // The recorded series IS the state (the handler keeps
            // none). Scope travels down in the query — a page across
            // all scopes is spent by whichever series ticks fastest.
            let recent = get_json(
                &self.client,
                &format!(
                    "{}/api/estate/comparisons?scope={scope}&limit=20",
                    self.base()
                ),
                &ctx.rule_name,
            )
            .await?;
            let rows: Vec<Value> = recent
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // Rows are event envelopes; the comparison rides in
            // `payload` (recorded verbatim by the dumb door). Fall back
            // to the row itself so a flattened future shape keeps
            // working.
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
                let body = alarm_body(&key, scope, host, &evidence, &latest);
                to_raise.entry(key).or_insert(body);
            }
        }

        // --- The silence half, on EVERY firing: any surviving series'
        // heartbeat is the clock that notices a dead neighbor. Gating
        // this on the triggering comparison having findings would make
        // the sweep run least when the estate looks healthiest — which
        // is exactly when a dead observer is lying loudest.
        let now = boss_clock_client::now_from(&self.clock).await;
        for (watched_scope, per_host) in WATCHED_SERIES {
            let obs = get_json(
                &self.client,
                &format!(
                    "{}/api/estate/observations?scope={watched_scope}&limit=50",
                    self.base()
                ),
                &ctx.rule_name,
            )
            .await?;
            let rows: Vec<Value> = obs
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for stale in stale_series(&rows, per_host, watched_scope, now) {
                let body = staleness_body(&stale, &evidence);
                to_raise.entry(unobserved_key(&stale)).or_insert(body);
            }
        }

        if to_raise.is_empty() {
            return Ok(());
        }

        let open = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind=backlog-item&status=open&limit=200",
                self.base()
            ),
            &ctx.rule_name,
        )
        .await?;
        let open_rows: Vec<Value> = open
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let raised = already_raised(&open_rows);

        for (key, body) in to_raise {
            if raised.contains(&key) {
                continue;
            }
            post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &body,
                &ctx.rule_name,
            )
            .await?;
            tracing::info!(finding = %key, scope, "estate.alarm raised a packet");
        }
        Ok(())
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
    fn the_alarm_packet_is_urgent_and_carries_the_key() {
        let b = alarm_body("not_ready:cp-2", "kubernetes-nodes", None, "evt", "");
        assert_eq!(b["priority"], "urgent");
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
        assert_eq!(stale[0]["host"], "kubernetes-nodes");
    }

    #[test]
    fn the_staleness_packet_is_urgent_and_names_host_and_condition() {
        let stale = json!({
            "host": "boss-gcp", "scope": "host-units",
            "last_observed_at": "2026-09-03T11:20:00+00:00",
            "cadence_s": 300, "age_s": 2400,
        });
        let b = staleness_body(&stale, "evt");
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
        let a = json!({"host": "boss-gcp", "scope": "host"});
        let b = json!({"host": "boss-gcp", "scope": "host-units"});
        assert_eq!(unobserved_key(&a), unobserved_key(&b));
    }
}
