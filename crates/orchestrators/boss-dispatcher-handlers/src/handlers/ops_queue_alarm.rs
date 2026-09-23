//! `ops.queue.alarm` — the reader the ops-runner's queue gauge was
//! built for (backlog a45b38c1, the half 2cfb4562 left unbuilt).
//!
//! WHY IT EXISTS. The runner on each host reads its own queue before it
//! walks it and writes what every answered request waited onto that
//! request (`queue_depth` / `queued_s`, 1ffb3305, made honest by
//! 2cfb4562). Nothing read it. `infra/estate/observe-units.sh` does not
//! watch `boss-ops-runner` on either host — it is not in roles.toml,
//! and the forge's UNITS roster is a host-local drop-in — so a stuck or
//! dead runner was invisible to the estate chain, and a runner's own
//! gauge cannot report its own death: a runner that stops writes
//! nothing, and nothing is exactly what a quiet day writes too.
//!
//! So this handler does NOT read the gauge. It reads the QUEUE — every
//! open ops-request, from the jobs API, paged on `total` — and judges
//! each host's oldest waiting request against a bound the gauge's own
//! series calibrated. The runner is the subject; the reading owes it
//! nothing (CLAUDE.md §Diagnosis: "an arm that needs the patient is not
//! an arm").
//!
//! THE CONDITION. A request is WAITING while its `execute` step is
//! `ready` or `active` — the two states the runner takes (a request
//! still pending its passkey approval is waiting on a person, not on
//! the runner, and its wait starts when the approval lands). A host's
//! queue has STALLED when its oldest waiting request has waited longer
//! than [`STALL_AFTER_S`]. One key per host, `ops_queue:<host>`, deduped
//! by `estate_finding` the sensor.poll way: raised once, refreshed only
//! when the diagnosis changes, withdrawn through
//! `common::retraction` (a2d8bad3) when the queue drains.
//!
//! THE DIAGNOSIS RIDES THE ALARM (detection is not diagnosis). When a
//! queue has stalled, one more read — the host's most recently opened
//! CLOSED requests — says when its runner last completed anything, and
//! that separates the two conditions an operator fixes differently:
//! - [`Condition::Silent`]: nothing answered for longer than the bound
//!   while requests waited — a DEAD runner, a wedged one (systemd will
//!   not start the oneshot while a verb still holds the last run), a
//!   host that is down, or a host no runner was ever installed on.
//! - [`Condition::NotDraining`]: the runner is completing requests, but
//!   the oldest is not among them — a long verb ahead of it in the
//!   serial walk, or a queue deeper than the runner's one page, which
//!   it walks newest first so the oldest starve (the fairness question
//!   2cfb4562 deferred).
//!
//! CALIBRATION — read from the series, not picked (2026-09-23, every
//! ops-request in the system of record: 1841, opened 2026-09-16..23,
//! 1005 of them carrying the gauge):
//! - `queued_s` on the 1005 gauged answers: p50 33 s, p90 59 s. NONE
//!   fell between 90 and 120 s; all 36 above 120 s belong to three
//!   episodes — 2026-09-21 ~18:00 (forge, 550 s), 2026-09-22
//!   00:47–02:55 (BOTH hosts, up to 7394 s: seventeen car probes and
//!   three converges held two hours) and 2026-09-22 20:20 (forge,
//!   403 s).
//! - The runner's silence while something waited, over all 1841: p99
//!   67 s on the forge and 84 s on boss-gcp, the healthiest-worst 91 s.
//!   Above 300 s: the same three episodes (786 s, 7394 s / 6950 s,
//!   727 s) and nothing else.
//! So the bound is [`STALL_CADENCES`] of the runner's own cadence: more
//! than three times the worst healthy wait, and it would have raised on
//! each of the three episodes and on no other request in the week —
//! the two-hour one within ten minutes instead of by hand.
//!
//! DEPTH IS EVIDENCE, NOT A TRIGGER. The packet's fix shape named depth
//! OR wait; the series says depth alone means nothing: the forge's
//! gauge read p50 10, p90 14, max 18 — one train's batch of car probes
//! — and a depth of 17 drained inside one cadence again and again. The
//! runner walks its WHOLE queue every run, so depth can only harm
//! through wait, and a queue past one page starves its oldest, which
//! this handler sees because it reads the queue itself, fully paged.
//! Depth rides the alarm; it does not raise one.
//!
//! AN ALARM, NOT A LIMIT. Nothing here refuses, holds, reorders or
//! re-routes any request. It files a packet and takes it back.

use boss_jobs::channels::InputChannel;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    RECOVERED_AT, Retraction, api_client, get_json, open_jobs_of_kind, owner_for_filing, post_json,
    recovery_note, relapse_patch, retraction, rows_or_refuse, with_lane, write_json,
};
use super::sensor_poll::firing_instant;

/// How often a runner polls, in seconds — `OnUnitActiveSec=1min` in
/// `infra/ops/boss-ops-runner.timer`, the one unit both hosts install
/// (`infra/ops/install-ops-runner.sh`). A fact that lives twice, so it
/// is pinned: `the_cadence_is_the_timers` reads the unit file and fails
/// naming both numbers when they disagree (CLAUDE.md §9a).
pub const RUNNER_CADENCE_S: i64 = 60;

/// How many of its own cadences a host's oldest waiting request may
/// wait before its queue has stalled. Five: the worst healthy wait in
/// the calibration week was 91 s, one and a half cadences, and every
/// wait past 120 s was an incident (module docs).
pub const STALL_CADENCES: i64 = 5;

/// The bound, in seconds.
pub const STALL_AFTER_S: i64 = RUNNER_CADENCE_S * STALL_CADENCES;

/// The `estate_finding` prefix; the key is `ops_queue:<host>`.
pub const FINDING_PREFIX: &str = "ops_queue:";

/// The `scope` every alarm this handler files carries — what the open-
/// alarm read narrows on, so it reads this handler's packets and not
/// the whole backlog. Not an estate-comparison scope: `estate.recover`
/// matches its own three scopes and so never touches these.
pub const SCOPE: &str = "ops-queue";

/// Who a withdrawal is stamped as.
const CLEARED_BY: &str = "ops.queue.alarm";

/// The open-alarm page. This handler's own packets number at most one
/// per host; a `total` past the page HOLDS rather than twins.
const ALARM_PAGE: usize = 1000;

/// How many of a host's most recently opened closed requests the
/// last-answer read takes. The runner walks newest first, so the most
/// recent answer is among the most recently opened; twenty is a page of
/// one train's probes plus slack.
const ANSWER_PAGE: usize = 20;

/// The finding key for one host.
pub fn finding_key(host: &str) -> String {
    format!("{FINDING_PREFIX}{host}")
}

// ---------------------------------------------------------------------------
// The queue, as the jobs API holds it
// ---------------------------------------------------------------------------

/// One host's open ops-requests, judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostQueue {
    pub host: String,
    /// Every open request naming this host — the runner's own `depth`.
    pub open: usize,
    /// Of those, the ones whose `execute` step the runner may take.
    pub waiting: usize,
    /// Waiting requests with no readable start — counted, never aged
    /// (`date -d ''` answers midnight; so would a guess here).
    pub unaged: usize,
    /// The waiting request that has waited longest, and since when.
    pub oldest: Option<(String, DateTime<Utc>)>,
}

impl HostQueue {
    /// Seconds the oldest waiting request has waited at `now`.
    pub fn oldest_wait_s(&self, now: DateTime<Utc>) -> Option<i64> {
        self.oldest
            .as_ref()
            .map(|(_, since)| (now - *since).num_seconds().max(0))
    }

    /// Has this queue stalled at `now`?
    pub fn stalled(&self, now: DateTime<Utc>) -> bool {
        self.oldest_wait_s(now).is_some_and(|w| w > STALL_AFTER_S)
    }
}

fn parse_at(v: Option<&Json>) -> Option<DateTime<Utc>> {
    v.and_then(Json::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn step<'a>(job: &'a Json, slug: &str) -> Option<&'a Json> {
    job.get("steps")
        .and_then(Json::as_array)?
        .iter()
        .find(|s| s.get("spec_slug").and_then(Json::as_str) == Some(slug))
}

fn status_of(step: &Json) -> Option<&str> {
    step.get("status").and_then(Json::as_str)
}

/// Is this request the runner's to take? Its `execute` step is `ready`
/// or `active` — the two the runner walks (`ops-runner.sh`, the
/// `case "$step_status"` that skips everything else).
fn is_waiting(job: &Json) -> bool {
    step(job, "execute")
        .and_then(status_of)
        .is_some_and(|s| s == "ready" || s == "active")
}

/// When a waiting request started waiting on the RUNNER: the passkey
/// approval's completion when it needed one, else the filer's
/// `opened_at` stamp — the same stamp the runner's `queued_s` is taken
/// from, so this reads the wait the gauge would record.
fn waiting_since(job: &Json) -> Option<DateTime<Utc>> {
    let approved = step(job, "approve")
        .filter(|s| status_of(s) == Some("completed"))
        .and_then(|s| parse_at(s.get("completed_at")));
    approved.or_else(|| parse_at(job.pointer("/metadata/opened_at")))
}

/// Every host's queue, from the open ops-request rows. A row naming no
/// host is nobody's queue (the runner matches `metadata.host` exactly).
pub fn queues(open: &[Json]) -> BTreeMap<String, HostQueue> {
    let mut out: BTreeMap<String, HostQueue> = BTreeMap::new();
    for job in open {
        let Some(host) = job
            .pointer("/metadata/host")
            .and_then(Json::as_str)
            .filter(|h| !h.is_empty())
        else {
            continue;
        };
        let q = out.entry(host.to_string()).or_insert_with(|| HostQueue {
            host: host.to_string(),
            open: 0,
            waiting: 0,
            unaged: 0,
            oldest: None,
        });
        q.open += 1;
        if !is_waiting(job) {
            continue;
        }
        q.waiting += 1;
        let id = job.get("id").and_then(Json::as_str).unwrap_or_default();
        match waiting_since(job) {
            None => q.unaged += 1,
            Some(since) => {
                if q.oldest.as_ref().is_none_or(|(_, o)| since < *o) {
                    q.oldest = Some((id.to_string(), since));
                }
            }
        }
    }
    out
}

/// When the runner on a host last completed a request, from its most
/// recently opened closed ones: the newest `execute` completion among
/// them. An answer and a refusal both complete `execute` — either one
/// is the runner alive. `None` is no completion on the page at all.
///
/// A listing with no `data` array is NO ANSWER and refuses by name
/// (`common::rows_or_refuse`, backlog 833e2d0a) — read as zero rows it
/// would call a live runner silent.
pub fn last_answer(listing: &Json) -> Result<Option<DateTime<Utc>>, String> {
    let rows: Vec<Json> = rows_or_refuse(listing, "the last-answer read")?;
    Ok(rows
        .iter()
        .filter_map(|j| step(j, "execute"))
        .filter(|s| status_of(s) == Some("completed"))
        .filter_map(|s| parse_at(s.get("completed_at")))
        .max())
}

// ---------------------------------------------------------------------------
// The two diagnoses
// ---------------------------------------------------------------------------

/// Which of the two stalls a host's queue is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// Nothing completed for longer than the bound while requests
    /// waited.
    Silent,
    /// Requests are completing, but the oldest waiting one is not.
    NotDraining,
}

impl Condition {
    /// The alarm's `reason` — STABLE per condition, so a persisting
    /// stall is one packet and not a metadata write every firing; the
    /// measured numbers ride `detail`, taken at the raise or refresh.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Silent => "silent: the runner completed nothing while requests waited",
            Self::NotDraining => {
                "not draining: the runner is completing requests, but not the oldest"
            }
        }
    }

    /// The machine-readable name.
    pub fn code(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::NotDraining => "not-draining",
        }
    }
}

/// The diagnosis of a stalled queue: the runner has been silent while
/// something waited if it completed nothing within the bound — counted
/// from the later of its last completion and the moment the oldest
/// request began to wait, because silence while nothing waited is a
/// quiet day, not a dead runner (boss-gcp answered nothing for twenty
/// hours on 2026-09-17 with nothing asked of it).
pub fn diagnose(
    oldest_since: DateTime<Utc>,
    last_answer: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Condition {
    let quiet_from = last_answer.map_or(oldest_since, |a| a.max(oldest_since));
    if (now - quiet_from).num_seconds() > STALL_AFTER_S {
        Condition::Silent
    } else {
        Condition::NotDraining
    }
}

/// One stalled host, as the alarm reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub queue: HostQueue,
    pub condition: Condition,
    pub oldest_wait_s: i64,
    pub last_answer: Option<DateTime<Utc>>,
}

/// The alarm one stalled host files.
pub fn alarm_body(f: &Finding, owner: &str, now: DateTime<Utc>) -> Json {
    let host = &f.queue.host;
    let (oldest_id, _) = f.queue.oldest.clone().unwrap_or_default();
    let last = f.last_answer.map_or_else(
        || "no completion on its recent requests".to_string(),
        |a| a.to_rfc3339(),
    );
    let why = match f.condition {
        Condition::Silent => format!(
            "The runner has completed nothing for longer than {STALL_AFTER_S} s while requests \
             waited (last completion: {last}). That is a dead runner, a wedged one (systemd does \
             not start the oneshot while a verb still holds the last run), a host that is down, \
             or a host no runner is installed on — `boss-ops-runner.timer` on {host}, and the \
             `ops-runner` role in infra/estate/estate.toml, which is what installs one."
        ),
        Condition::NotDraining => format!(
            "The runner is completing requests (last completion: {last}), but not the oldest. \
             Its walk is serial and newest first: a long verb ahead of the oldest request holds \
             it, and a queue deeper than the runner's one page (1000) is never walked to its \
             oldest at all."
        ),
    };
    let detail = format!(
        "Raised by ops.queue.alarm (backlog a45b38c1) at {}: the ops-request queue for host \
         `{host}` has {} open, {} waiting on the runner, and the oldest waiting request \
         ({oldest_id}) has waited {} s — past the {STALL_AFTER_S} s bound ({STALL_CADENCES} of \
         the runner's {RUNNER_CADENCE_S} s cadence, calibrated on the week's queued_s series). \
         {why} This alarm refuses nothing; it is withdrawn when the host's oldest waiting \
         request is back under the bound.",
        now.to_rfc3339(),
        f.queue.open,
        f.queue.waiting,
        f.oldest_wait_s,
    );
    let metadata = json!({
        "area": "ops-runner",
        "estate_finding": finding_key(host),
        "scope": SCOPE,
        "host": host,
        "condition": f.condition.code(),
        "reason": f.condition.reason(),
        "detail": detail,
        "queue_depth": f.queue.open,
        "waiting": f.queue.waiting,
        "unaged": f.queue.unaged,
        "oldest_request": oldest_id,
        "oldest_wait_s": f.oldest_wait_s,
        "last_answered_at": f.last_answer.map(|a| a.to_rfc3339()),
        "stall_after_s": STALL_AFTER_S,
        "measured_at": now.to_rfc3339(),
    });
    json!({
        "kind": "backlog-item",
        "title": format!(
            "ESTATE ALARM: ops-requests for {host} have waited past {} minutes",
            STALL_AFTER_S / 60
        ),
        "subject": {"subject_kind": "custom", "id": host},
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

// ---------------------------------------------------------------------------
// This handler's open alarms
// ---------------------------------------------------------------------------

/// One open alarm, as the listing row holds it — steps inline, which is
/// what `common::retraction` reads.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAlarm {
    pub id: String,
    pub reason: String,
    /// It carries a standing recovery note, which a relapse withdraws.
    pub told: bool,
    pub row: Json,
}

fn told(packet: &Json) -> bool {
    packet
        .pointer(&format!("/metadata/{RECOVERED_AT}"))
        .is_some_and(|v| !v.is_null())
}

/// This handler's open alarms, by host. A page that did not hold the
/// whole `total` is a refusal: an alarm judged absent from a truncated
/// page would be twinned (the sensor.poll dedup rule, 445c1494).
pub fn open_alarms(listing: &Json) -> Result<BTreeMap<String, OpenAlarm>, String> {
    let rows: Vec<Json> = rows_or_refuse(listing, "the ops-queue open-alarm read")?;
    let total = listing.get("total").and_then(Json::as_u64);
    if total.is_none_or(|t| (rows.len() as u64) < t) {
        return Err(format!(
            "the ops-queue open-alarm read held {} rows of total {total:?}; the alarms are \
             held for retry rather than twinned",
            rows.len()
        ));
    }
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let host = row
                .pointer("/metadata/estate_finding")
                .and_then(Json::as_str)?
                .strip_prefix(FINDING_PREFIX)?
                .to_string();
            let id = row.get("id").and_then(Json::as_str)?.to_string();
            let reason = row
                .pointer("/metadata/reason")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string();
            Some((
                host,
                OpenAlarm {
                    id,
                    reason,
                    told: told(&row),
                    row,
                },
            ))
        })
        .collect())
}

/// The completion that withdraws a recovered alarm at the step it is
/// waiting on (`common::retraction`). `existing` is that step's own
/// metadata: a step PUT replaces it wholesale.
pub fn recover_step_body(
    existing: &serde_json::Map<String, Json>,
    evidence: &str,
    at: DateTime<Utc>,
) -> Json {
    let mut metadata = existing.clone();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "{evidence} Closed by machine from the reading, not by judgement."
        )),
    );
    metadata.insert("cleared_by".into(), json!(CLEARED_BY));
    metadata.insert(RECOVERED_AT.into(), json!(at.to_rfc3339()));
    json!({"status": "completed", "metadata": metadata})
}

fn recovery_evidence(q: Option<&HostQueue>, host: &str, now: DateTime<Utc>) -> String {
    let state = match q {
        None => "no open request".to_string(),
        Some(q) => match q.oldest_wait_s(now) {
            Some(w) => format!(
                "{} waiting, the oldest for {w} s — under the {STALL_AFTER_S} s bound",
                q.waiting
            ),
            None => format!("{} open and none of them waiting on the runner", q.open),
        },
    };
    format!(
        "ops.queue.alarm read the ops-request queue for host `{host}` at {}: {state}; the \
         condition this alarm carried no longer holds.",
        now.to_rfc3339()
    )
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

pub struct OpsQueueAlarm {
    client: reqwest::Client,
    jobs_base: String,
    /// Who the packets this handler files are owned by — the platform
    /// owner through the port (backlog 3c23662d); never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl OpsQueueAlarm {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    fn url(&self, params: &[(&str, String)]) -> Result<String, HandlerError> {
        reqwest::Url::parse_with_params(&format!("{}/api/jobs", self.base()), params)
            .map(String::from)
            .map_err(|e| HandlerError::Permanent(format!("jobs API base is not a URL: {e}")))
    }

    async fn last_answer_for(
        &self,
        host: &str,
        rule: &str,
    ) -> Result<Option<DateTime<Utc>>, HandlerError> {
        let url = self.url(&[
            ("kind", "ops-request".into()),
            ("status", "closed".into()),
            ("metadata", json!({"host": host}).to_string()),
            ("limit", ANSWER_PAGE.to_string()),
        ])?;
        let listing = get_json(&self.client, &url, rule).await?;
        last_answer(&listing).map_err(HandlerError::Downstream)
    }

    async fn raise_or_refresh(
        &self,
        f: &Finding,
        open: Option<&OpenAlarm>,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<&'static str, HandlerError> {
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let body = alarm_body(f, &owner, now);
        match open {
            Some(a) if a.reason == f.condition.reason() => {
                if a.told {
                    // The condition is back on a routed alarm that was
                    // told it had recovered: that note is now false.
                    write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &format!("{}/api/jobs/{}/metadata", self.base(), a.id),
                        &relapse_patch(),
                        rule,
                    )
                    .await?;
                    return Ok("relapsed");
                }
                Ok("held")
            }
            Some(a) => {
                let mut patch = body["metadata"].clone();
                if let (Some(m), Json::Object(relapse)) = (patch.as_object_mut(), relapse_patch()) {
                    m.extend(relapse);
                }
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{}/metadata", self.base(), a.id),
                    &patch,
                    rule,
                )
                .await?;
                Ok("refreshed")
            }
            None => {
                post_json(
                    &self.client,
                    &format!("{}/api/jobs", self.base()),
                    &body,
                    rule,
                )
                .await?;
                Ok("raised")
            }
        }
    }

    async fn recover(
        &self,
        host: &str,
        alarm: &OpenAlarm,
        q: Option<&HostQueue>,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<&'static str, HandlerError> {
        let evidence = recovery_evidence(q, host, now);
        match retraction(&alarm.row) {
            None => {
                tracing::warn!(host, packet = %alarm.id, "ops.queue.alarm: the open alarm has no triage step to close");
                Ok("unclosable")
            }
            Some(Retraction::Complete {
                step_id, metadata, ..
            }) => {
                write_json(
                    &self.client,
                    reqwest::Method::PUT,
                    &format!("{}/api/jobs/{}/steps/{step_id}", self.base(), alarm.id),
                    &recover_step_body(&metadata, &evidence, now),
                    rule,
                )
                .await?;
                Ok("withdrawn")
            }
            // A packet already told is not told again on every firing.
            Some(Retraction::Annotate { .. }) if alarm.told => Ok("told"),
            Some(Retraction::Annotate { why_open }) => {
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{}/metadata", self.base(), alarm.id),
                    &Json::Object(recovery_note(
                        &evidence,
                        CLEARED_BY,
                        &now.to_rfc3339(),
                        &why_open,
                    )),
                    rule,
                )
                .await?;
                Ok("told")
            }
        }
    }
}

#[async_trait]
impl Handler for OpsQueueAlarm {
    fn name(&self) -> &'static str {
        "ops.queue.alarm"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let now = firing_instant(&ctx.event_payload);
        let open = open_jobs_of_kind(&self.client, self.base(), "ops-request", rule).await?;
        let queues = queues(&open);
        let alarms_url = self.url(&[
            ("kind", "backlog-item".into()),
            ("status", "open".into()),
            ("metadata", json!({"scope": SCOPE}).to_string()),
            ("limit", ALARM_PAGE.to_string()),
        ])?;
        let alarms = open_alarms(&get_json(&self.client, &alarms_url, rule).await?)
            .map_err(HandlerError::Downstream)?;

        let hosts: BTreeSet<&String> = queues.keys().chain(alarms.keys()).collect();
        for host in hosts {
            let q = queues.get(host);
            let alarm = alarms.get(host);
            let stalled = q.filter(|q| q.stalled(now));
            let outcome = match (stalled, alarm) {
                (Some(q), _) => {
                    let (_, since) = q.oldest.clone().unwrap_or((String::new(), now));
                    let last = self.last_answer_for(host, rule).await?;
                    let finding = Finding {
                        queue: q.clone(),
                        condition: diagnose(since, last, now),
                        oldest_wait_s: q.oldest_wait_s(now).unwrap_or_default(),
                        last_answer: last,
                    };
                    self.raise_or_refresh(&finding, alarm, now, rule).await?
                }
                (None, Some(a)) => self.recover(host, a, q, now, rule).await?,
                (None, None) => "ok",
            };
            tracing::info!(
                host = %host,
                open = q.map_or(0, |q| q.open),
                oldest_wait_s = ?q.and_then(|q| q.oldest_wait_s(now)),
                outcome,
                "ops.queue.alarm"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const NOW: &str = "2026-09-23T10:00:00+00:00";

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "ops-runner-queue-watched-every-5-minutes".into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-09-23", "_at": NOW}),
        }
    }

    /// An open ops-request as the list read returns it: steps inline.
    fn request(id: &str, host: &str, opened_at: &str, execute: &str) -> Json {
        json!({
            "id": id, "kind": "ops-request", "status": "open",
            "metadata": {"host": host, "verb": "df", "opened_at": opened_at},
            "steps": [
                {"id": format!("{id}-filed"), "spec_slug": "filed", "status": "completed"},
                {"id": format!("{id}-approve"), "spec_slug": "approve", "status": "skipped"},
                {"id": format!("{id}-execute"), "spec_slug": "execute", "status": execute},
            ],
        })
    }

    fn answered(id: &str, host: &str, completed_at: &str) -> Json {
        json!({
            "id": id, "kind": "ops-request", "status": "closed",
            "metadata": {"host": host},
            "steps": [{"id": format!("{id}-execute"), "spec_slug": "execute",
                       "status": "completed", "completed_at": completed_at}],
        })
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    /// The pin (CLAUDE.md §9a): the cadence the bound is counted in is
    /// the one the runner's timer declares.
    #[test]
    fn the_cadence_is_the_timers() {
        let path = boss_testing::repo_root().join("infra/ops/boss-ops-runner.timer");
        let unit = std::fs::read_to_string(&path).unwrap();
        let declared = unit
            .lines()
            .find_map(|l| l.trim().strip_prefix("OnUnitActiveSec="))
            .unwrap_or_else(|| panic!("{} declares no OnUnitActiveSec", path.display()));
        let secs = match declared.trim() {
            s if s.ends_with("min") => s.trim_end_matches("min").parse::<i64>().unwrap() * 60,
            s if s.ends_with('s') => s.trim_end_matches('s').parse::<i64>().unwrap(),
            s => panic!("OnUnitActiveSec={s}: a unit this pin cannot read"),
        };
        assert_eq!(
            secs, RUNNER_CADENCE_S,
            "infra/ops/boss-ops-runner.timer polls every {secs} s but RUNNER_CADENCE_S is \
             {RUNNER_CADENCE_S}: the stall bound is counted in the runner's own cadence"
        );
    }

    // The calibration week's worst healthy wait was 91 s; the shortest
    // of the three incidents it held waited 403 s. A bound retuned out
    // of that window stops compiling rather than drifting silently.
    const _: () = assert!(STALL_AFTER_S > 3 * 91 && STALL_AFTER_S < 403);

    #[test]
    fn the_bound_is_five_cadences() {
        assert_eq!(STALL_AFTER_S, 300);
    }

    #[test]
    fn a_queue_is_per_host_and_ages_only_what_the_runner_may_take() {
        let open = vec![
            request("a", "forge", "2026-09-23T09:50:00Z", "ready"),
            request("b", "forge", "2026-09-23T09:58:00Z", "active"),
            // Waiting on a passkey, not on the runner.
            request("c", "forge", "2026-09-23T08:00:00Z", "pending"),
            request("d", "boss-gcp", "2026-09-23T09:59:00Z", "ready"),
            json!({"id": "e", "metadata": {}, "steps": []}),
        ];
        let q = queues(&open);
        assert_eq!(q.len(), 2, "a request naming no host is nobody's queue");
        let forge = &q["forge"];
        assert_eq!((forge.open, forge.waiting, forge.unaged), (3, 2, 0));
        assert_eq!(forge.oldest.as_ref().unwrap().0, "a");
        assert_eq!(forge.oldest_wait_s(at(NOW)), Some(600));
        assert!(forge.stalled(at(NOW)));
        assert!(!q["boss-gcp"].stalled(at(NOW)));
    }

    #[test]
    fn an_approved_request_waits_from_its_approval_and_an_unstamped_one_is_not_aged() {
        let mut approved = request("a", "forge", "2026-09-23T08:00:00Z", "ready");
        approved["steps"][1] = json!({"spec_slug": "approve", "status": "completed", "completed_at": "2026-09-23T09:59:00Z"});
        let mut unstamped = request("b", "forge", "", "ready");
        unstamped["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("opened_at");
        let q = &queues(&[approved, unstamped])["forge"];
        assert_eq!(q.unaged, 1);
        assert_eq!(q.oldest_wait_s(at(NOW)), Some(60));
        assert!(!q.stalled(at(NOW)));
    }

    #[test]
    fn the_bound_is_strict() {
        let q = &queues(&[request("a", "forge", "2026-09-23T09:55:00Z", "ready")])["forge"];
        assert_eq!(q.oldest_wait_s(at(NOW)), Some(STALL_AFTER_S));
        assert!(!q.stalled(at(NOW)), "at the bound is not past it");
    }

    #[test]
    fn silence_is_counted_only_while_something_waited() {
        let now = at(NOW);
        let since = at("2026-09-23T09:50:00Z");
        // Nothing completed since hours before the request arrived.
        assert_eq!(
            diagnose(since, Some(at("2026-09-23T02:00:00Z")), now),
            Condition::Silent
        );
        assert_eq!(diagnose(since, None, now), Condition::Silent);
        // It completed something a minute ago: alive, not draining.
        assert_eq!(
            diagnose(since, Some(at("2026-09-23T09:59:00Z")), now),
            Condition::NotDraining
        );
        // It completed something after the oldest arrived, then stopped.
        assert_eq!(
            diagnose(since, Some(at("2026-09-23T09:52:00Z")), now),
            Condition::Silent
        );
    }

    #[test]
    fn the_last_answer_is_the_newest_execute_completion_and_no_data_array_refuses() {
        let l = listing(vec![
            answered("x", "forge", "2026-09-23T09:10:00Z"),
            answered("y", "forge", "2026-09-23T09:40:00Z"),
            json!({"id": "z", "steps": [{"spec_slug": "execute", "status": "skipped"}]}),
        ]);
        assert_eq!(last_answer(&l).unwrap(), Some(at("2026-09-23T09:40:00Z")));
        assert_eq!(last_answer(&listing(vec![])).unwrap(), None);
        let why = last_answer(&listing_stub::no_data_array()).unwrap_err();
        assert!(why.contains("no `data` array"), "{why}");
    }

    #[test]
    fn the_alarm_is_keyed_per_host_urgent_and_carries_its_measurement() {
        let q = queues(&[request("a", "forge", "2026-09-23T09:50:00Z", "ready")])["forge"].clone();
        let f = Finding {
            queue: q,
            condition: Condition::Silent,
            oldest_wait_s: 600,
            last_answer: None,
        };
        let b = alarm_body(&f, "emp-owner", at(NOW));
        assert_eq!(b["kind"], "backlog-item");
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["metadata"]["estate_finding"], "ops_queue:forge");
        assert_eq!(b["metadata"]["scope"], SCOPE);
        assert_eq!(b["metadata"]["condition"], "silent");
        assert_eq!(b["metadata"]["reason"], Condition::Silent.reason());
        assert_eq!(b["metadata"]["oldest_request"], "a");
        assert_eq!(b["metadata"]["oldest_wait_s"], 600);
        assert_eq!(b["metadata"]["queue_depth"], 1);
        assert!(
            b["metadata"]["detail"]
                .as_str()
                .unwrap()
                .contains("dead runner")
        );
    }

    #[test]
    fn a_truncated_or_arrayless_alarm_read_holds() {
        assert!(open_alarms(&listing_stub::no_data_array()).is_err());
        assert!(open_alarms(&json!({"data": [], "total": 2})).is_err());
        assert!(open_alarms(&json!({"data": []})).is_err());
        let found = open_alarms(&listing(vec![json!({
            "id": "al-1",
            "metadata": {"estate_finding": "ops_queue:forge", "reason": "r"},
        })]))
        .unwrap();
        assert_eq!(found["forge"].id, "al-1");
    }

    // -- end to end, against the stub jobs API ---------------------------

    const OPEN_Q: &str = "/api/jobs?kind=ops-request&status=open";
    const ALARMS: &str = "/api/jobs?kind=backlog-item&status=open";
    const CLOSED_Q: &str = "/api/jobs?kind=ops-request&status=closed";

    fn handler(stub: &listing_stub::Stub) -> Arc<OpsQueueAlarm> {
        OpsQueueAlarm::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn alarm_row(reason: &str, triage: &str) -> Json {
        json!({
            "id": "al-1", "kind": "backlog-item", "status": "open",
            "metadata": {"estate_finding": "ops_queue:forge", "scope": SCOPE, "reason": reason},
            "steps": [{"id": "al-1-triage", "spec_slug": "triage", "status": triage, "metadata": {}}],
        })
    }

    #[tokio::test]
    async fn a_dead_runner_raises_one_silent_alarm() {
        let stub = listing_stub::serve(vec![
            (
                OPEN_Q,
                listing(vec![request("a", "forge", "2026-09-23T09:50:00Z", "ready")]),
            ),
            (ALARMS, listing(vec![])),
            (
                CLOSED_Q,
                listing(vec![answered("x", "forge", "2026-09-23T02:00:00Z")]),
            ),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(sent[0].1["metadata"]["condition"], "silent");
        assert_eq!(sent[0].1["metadata"]["estate_finding"], "ops_queue:forge");
    }

    #[tokio::test]
    async fn a_held_alarm_with_the_same_reason_writes_nothing() {
        let stub = listing_stub::serve(vec![
            (
                OPEN_Q,
                listing(vec![request("a", "forge", "2026-09-23T09:50:00Z", "ready")]),
            ),
            (
                ALARMS,
                listing(vec![alarm_row(Condition::Silent.reason(), "ready")]),
            ),
            (CLOSED_Q, listing(vec![])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn a_runner_that_wakes_but_does_not_drain_refreshes_the_diagnosis() {
        let stub = listing_stub::serve(vec![
            (
                OPEN_Q,
                listing(vec![request("a", "forge", "2026-09-23T09:50:00Z", "ready")]),
            ),
            (
                ALARMS,
                listing(vec![alarm_row(Condition::Silent.reason(), "ready")]),
            ),
            (
                CLOSED_Q,
                listing(vec![answered("x", "forge", "2026-09-23T09:59:00Z")]),
            ),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/metadata");
        assert_eq!(sent[0].1["condition"], "not-draining");
    }

    #[tokio::test]
    async fn a_drained_queue_withdraws_its_alarm_at_the_waiting_step() {
        let stub = listing_stub::serve(vec![
            (OPEN_Q, listing(vec![])),
            (
                ALARMS,
                listing(vec![alarm_row(Condition::Silent.reason(), "ready")]),
            ),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "PUT /api/jobs/al-1/steps/al-1-triage");
        assert_eq!(sent[0].1["metadata"]["disposition"], "stale");
        assert_eq!(sent[0].1["metadata"]["cleared_by"], CLEARED_BY);
    }

    #[tokio::test]
    async fn a_routed_alarm_is_told_once_and_a_relapse_withdraws_the_note() {
        let mut routed = alarm_row(Condition::Silent.reason(), "completed");
        routed["steps"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "al-1-build", "spec_slug": "build", "status": "active"}));
        // Recovered: told.
        let stub = listing_stub::serve(vec![
            (OPEN_Q, listing(vec![])),
            (ALARMS, listing(vec![routed.clone()])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/metadata");
        assert!(sent[0].1[RECOVERED_AT].is_string());
        // Told already: silent.
        routed["metadata"][RECOVERED_AT] = json!("2026-09-23T09:55:00+00:00");
        let stub = listing_stub::serve(vec![
            (OPEN_Q, listing(vec![])),
            (ALARMS, listing(vec![routed.clone()])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
        // The stall is back: the note is withdrawn.
        let stub = listing_stub::serve(vec![
            (
                OPEN_Q,
                listing(vec![request("a", "forge", "2026-09-23T09:50:00Z", "ready")]),
            ),
            (ALARMS, listing(vec![routed])),
            (CLOSED_Q, listing(vec![])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].1, relapse_patch());
    }

    #[tokio::test]
    async fn a_healthy_queue_writes_nothing_and_reads_no_answers() {
        let stub = listing_stub::serve(vec![
            (
                OPEN_Q,
                listing(vec![request("a", "forge", "2026-09-23T09:59:00Z", "ready")]),
            ),
            (ALARMS, listing(vec![])),
        ])
        .await;
        // No CLOSED_Q route: a read of it would 404 and fail the firing.
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn an_arrayless_queue_read_refuses_rather_than_reading_a_quiet_estate() {
        let stub = listing_stub::serve(vec![
            (OPEN_Q, listing_stub::no_data_array()),
            (
                ALARMS,
                listing(vec![alarm_row(Condition::Silent.reason(), "ready")]),
            ),
        ])
        .await;
        let res = handler(&stub).invoke(&[], &ctx()).await;
        listing_stub::assert_refused_by_name(res, "GET /api/jobs");
        assert!(stub.writes().is_empty(), "no alarm withdrawn on no answer");
    }
}
