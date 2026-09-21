//! `sensor.poll` — the first sensor: a declared poll of the world
//! outside BOSS, recorded outside the audit log, opened as work.
//!
//! Design 14c9b2ad (decided with David 2026-09-17: "Polling is fine,
//! use a restricted read-only key"), backlog 2d33e111. ONE platform
//! cadence rule (`sensors-poll-every-5-minutes`) fires this handler; it
//! reads the sensor registry (`GET /api/sensors`) and, for every
//! enabled sensor whose `every_minutes` has elapsed since its last
//! attempt (`SensorRow::due_at` — never true of a push-only source
//! such as `site`, whose readings the gateway records itself,
//! 0b5c5081), asks the source adapter for everything since the sensor's
//! cursor, records the readings (insert-if-absent), opens ONE packet
//! per reading that still owes one, stamps it, and marks the poll.
//! The product knows how to read a source; WHAT to open for it is the
//! sensor row — tenant data.
//!
//! THE READING IS NOT THE FACT; THE PACKET IS. David, 2026-09-16: "the
//! audit_log just needs to capture all work that is done; sensors
//! record data that don't flow into the audit_log." Nothing here
//! writes the log directly. The packet the poll opens — signed
//! `automation:sensor:<id>`, subject `<subject_kind>/<external id>` —
//! is the audit-log fact, and the reading is the evidence it was opened
//! from.
//!
//! IDEMPOTENT BY EXTERNAL ID. The source is read from the cursor
//! INCLUSIVE (a charge created in the cursor's own second must not be
//! lost), so a redelivery re-reads what it read before; the readings
//! door inserts nothing twice, and a reading is opened only while its
//! `packet_id` is null and stamped before the next one is opened. So a
//! firing that died between opening and stamping opens that ONE
//! packet again on the next pass — the honest bound; a firing that
//! died anywhere else opens nothing twice. Pinned by the test that
//! runs the handler twice and counts one packet.
//!
//! FAILURE IS LOUD ON A PACKET. A sensor has no packet of its own to
//! dead-letter on, so an UNREADABLE credential — the env var unset, a
//! 401/403 from the source, a source the product has no adapter for —
//! files the estate-style alarm `sensor_unreadable:<id>` (an urgent
//! backlog-item deduped by `estate_finding`, REFRESHED only when the
//! reason changes, so a persisting condition is one packet and not a
//! metadata write every five minutes) and marks the poll so the next
//! attempt waits the sensor's own period. The next GOOD read closes it
//! through its triage step (`disposition = stale`, the estate.recover
//! idiom). A network blip is a `Downstream` error: the firing NAKs, the
//! poll is NOT marked, and the next tick retries.
//!
//! AN UNOPENABLE PACKET IS THE SAME SHAPE, ONE STEP LATER (backlog
//! f50a9ec1, 2026-09-17). A source that reads fine can still declare a
//! packet the jobs API refuses — the `opens_kind` not published, its
//! `subject_kind` unknown to the registry, the described metadata
//! failing the kind's schema. Until this car that refusal was only
//! logged: a 400 rode the transient lane and retried every tick, a 422
//! terminated the whole firing, and nothing on any packet said why the
//! reading sat unstamped. Now a refusal (a 4xx that is not a miss, a
//! conflict or a throttle — `is_refusal`) files `sensor_unopenable:<id>`
//! through the SAME raise / refresh / recover mechanics as the
//! unreadable alarm, carrying the API's answer verbatim (status + body,
//! bounded by `REFUSAL_BOUND`) and the reading's id. The reading stays
//! unstamped, so it is still owed; the poll is marked so the next
//! attempt waits the period; and the first open that succeeds closes
//! the alarm. The two are two findings on one sensor, each recovered by
//! its own later success: a source that reads again closes the
//! unreadable one on the read even while its packets are still refused.
//!
//! THE VALUE IS NEVER READ EXCEPT TO SEND IT. The handler holds the
//! credential values the deployment handed it by registry id
//! (`BOSS_BROKER_STRIPE_KEY` for `stripe-restricted-read`, the same
//! optional-secretKeyRef idiom as the Cloudflare root) and passes one
//! to the adapter as a bearer. It is never logged, never on a packet,
//! never in an error string.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::sensors::{NewReading, PollStamp, Reading, SensorRow};
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    api_client, dispatcher_reader_header, get_json, owner_for_filing, post_json, sim_origin_value,
    triage_step,
};

/// The two standing conditions a sensor alarms on. Each is its own
/// `estate_finding` (so one sensor can carry both at once — a source
/// that reads but whose packets are refused is exactly the second and
/// not the first) and each is recovered by its own later success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// The credential or the source cannot be read.
    Unreadable,
    /// The source reads, but the packet a reading declares is refused
    /// by the jobs API (backlog f50a9ec1).
    Unopenable,
}

impl Condition {
    /// The alarm key one sensor carries for this condition — the
    /// `estate_finding` the dedup lens reads.
    pub fn key(self, sensor_id: &str) -> String {
        match self {
            Self::Unreadable => format!("sensor_unreadable:{sensor_id}"),
            Self::Unopenable => format!("sensor_unopenable:{sensor_id}"),
        }
    }

    /// What a later success proves about this condition, for the
    /// recovery evidence.
    fn recovered_by(self) -> &'static str {
        match self {
            Self::Unreadable => "read",
            Self::Unopenable => "opened a packet for",
        }
    }
}

/// How much of a refusal body an alarm carries verbatim. A jobs-API
/// refusal is one line naming the kind or the field; a few hundred
/// chars holds all of it, and the bound keeps a proxy's HTML error
/// page off a packet.
pub const REFUSAL_BOUND: usize = 400;

/// What one alarm carries: the condition, the reason (what the failing
/// call answered — never a value), and for an unopenable packet the
/// reading it was about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alarm {
    pub condition: Condition,
    pub reason: String,
    pub external_id: Option<String>,
}

impl Alarm {
    pub fn unreadable(reason: impl Into<String>) -> Self {
        Self {
            condition: Condition::Unreadable,
            reason: reason.into(),
            external_id: None,
        }
    }

    /// The API's refusal verbatim — status and body — bounded, and
    /// saying how much was cut when it was. The reading's id is in the
    /// reason too, so a NEW reading meeting the same refusal reads as a
    /// changed reason and refreshes the packet once.
    pub fn unopenable(external_id: &str, status: u16, body: &str) -> Self {
        let total = body.chars().count();
        let shown: String = body.chars().take(REFUSAL_BOUND).collect();
        let body = if total > REFUSAL_BOUND {
            format!("{shown} …[{total} chars; first {REFUSAL_BOUND} shown]")
        } else {
            shown
        };
        Self {
            condition: Condition::Unopenable,
            reason: format!("reading {external_id}: POST /api/jobs answered {status}: {body}"),
            external_id: Some(external_id.to_string()),
        }
    }
}

/// Which answers to a packet open are a standing refusal — request
/// data the API will refuse identically on every retry — and which are
/// weather. By the house contract on `HandlerError::Permanent`, 404
/// (not yet projected), 409 (convergent conflict), 408 and 429 stay
/// retryable; every other 4xx is the API saying no to THIS body.
pub fn is_refusal(status: reqwest::StatusCode) -> bool {
    status.is_client_error()
        && !matches!(
            status,
            reqwest::StatusCode::NOT_FOUND
                | reqwest::StatusCode::REQUEST_TIMEOUT
                | reqwest::StatusCode::CONFLICT
                | reqwest::StatusCode::TOO_MANY_REQUESTS
        )
}

/// The actor a sensor's writes sign as. The packet's owner, its
/// `opened_by`, and the readings' provenance are all this one id —
/// spelled once, in boss-jobs, because the readings door admits a
/// polled sensor's readings from this actor only (0b5c5081).
pub fn sensor_actor(sensor_id: &str) -> String {
    boss_jobs::sensors::sensor_actor(sensor_id)
}

fn sensor_actor_header(sensor_id: &str) -> String {
    json!({
        "id": sensor_actor(sensor_id),
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// The dedup read's page. Open backlog-items number in the tens; a
/// truncated page HOLDS the raise (never twins blind).
const DEDUP_PAGE: usize = 1000;

// ---------------------------------------------------------------------------
// The port — one source adapter per `source`
// ---------------------------------------------------------------------------

/// Why a source could not be read. The two legs the handler tells
/// apart: an UNREADABLE source is a standing condition (alarmed,
/// retried at the sensor's own period); a TRANSIENT one is weather
/// (NAKed, retried at the next tick).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    Unreadable(String),
    Transient(String),
}

/// One observation as the source reports it. `payload` is the source's
/// own record (a Stripe charge), kept whole; the packet's title and
/// metadata are derived from it by [`SensorSource::describe`] so a
/// reading recorded on one firing can be opened on a later one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub external_id: String,
    pub observed_at: DateTime<Utc>,
    pub payload: Json,
}

/// What a reading's packet says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    pub title: String,
    pub metadata: Json,
}

#[async_trait]
pub trait SensorSource: Send + Sync {
    /// Every observation at or after `since` (inclusive — the caller
    /// dedups), in any order. `credential` is the value to send.
    async fn read(
        &self,
        credential: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Observation>, SourceError>;

    /// The packet one recorded payload opens. Pure.
    fn describe(&self, payload: &Json) -> Described;
}

/// In-memory source: the readings it will answer, or the error it
/// will raise, set by the test. Every `read` is recorded.
pub struct InMemorySource {
    pub answer: std::sync::Mutex<Result<Vec<Observation>, SourceError>>,
    pub reads: std::sync::Mutex<Vec<(String, Option<DateTime<Utc>>)>>,
}

impl InMemorySource {
    pub fn answering(obs: Vec<Observation>) -> Arc<Self> {
        Arc::new(Self {
            answer: std::sync::Mutex::new(Ok(obs)),
            reads: Default::default(),
        })
    }

    pub fn failing(e: SourceError) -> Arc<Self> {
        Arc::new(Self {
            answer: std::sync::Mutex::new(Err(e)),
            reads: Default::default(),
        })
    }

    pub fn set(&self, answer: Result<Vec<Observation>, SourceError>) {
        *self.answer.lock().expect("source lock") = answer;
    }
}

#[async_trait]
impl SensorSource for InMemorySource {
    async fn read(
        &self,
        credential: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Observation>, SourceError> {
        self.reads
            .lock()
            .expect("reads lock")
            .push((credential.to_string(), since));
        self.answer.lock().expect("source lock").clone()
    }

    fn describe(&self, payload: &Json) -> Described {
        Described {
            title: format!(
                "Reading {}",
                payload.get("id").and_then(Json::as_str).unwrap_or("?")
            ),
            metadata: json!({ "amount_cents": payload.get("amount").cloned().unwrap_or(Json::Null) }),
        }
    }
}

// ---------------------------------------------------------------------------
// Credential values — by registry id, from the deployment
// ---------------------------------------------------------------------------

/// The credential values the deployment handed this process, keyed by
/// `credentials` registry id, each beside the env var it came from so
/// an absent one is alarmed BY NAME. Built once in the binary from
/// config; the tests build it directly.
#[derive(Default, Clone)]
pub struct CredentialValues {
    values: BTreeMap<String, (String, Option<String>)>,
}

impl CredentialValues {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare that registry id `credential` is read from env var
    /// `env_name`, whose value (if the deployment set it) is `value`.
    pub fn with(mut self, credential: &str, env_name: &str, value: Option<String>) -> Self {
        self.values.insert(
            credential.to_string(),
            (env_name.to_string(), value.filter(|v| !v.trim().is_empty())),
        );
        self
    }

    /// The value to send, or the reason it cannot be: no env var is
    /// declared for this registry id, or the declared one is unset.
    pub fn value(&self, credential: &str) -> Result<String, String> {
        match self.values.get(credential) {
            None => Err(format!(
                "no env var carries credential `{credential}` into the dispatcher — the product \
                 knows {} — declare it in the binary beside the others",
                if self.values.is_empty() {
                    "none".to_string()
                } else {
                    self.values.keys().cloned().collect::<Vec<_>>().join(", ")
                }
            )),
            Some((env, None)) => Err(format!(
                "credential `{credential}` is unset: env {env} is empty (the Secret key the \
                 registry row names has not been minted, or the pod was not rolled after it was)"
            )),
            Some((_, Some(v))) => Ok(v.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure pieces — bodies and decisions
// ---------------------------------------------------------------------------

/// The packet one reading opens: the sensor's declared kind, the
/// reading as subject, the source's description, and the sensor's
/// provenance on the metadata.
pub fn packet_body(sensor: &SensorRow, r: &Reading, d: &Described) -> Json {
    let mut metadata = d.metadata.clone();
    if let Some(m) = metadata.as_object_mut() {
        m.insert("sensor_id".into(), json!(sensor.id));
        m.insert("sensor_source".into(), json!(sensor.source));
        m.insert("external_id".into(), json!(r.external_id));
        m.insert("observed_at".into(), json!(r.observed_at.to_rfc3339()));
    }
    json!({
        "kind": sensor.opens_kind,
        "subject": {"subject_kind": sensor.subject_kind, "id": r.external_id},
        "title": d.title,
        "owner_id": sensor_actor(&sensor.id),
        "priority": "standard",
        "status": "open",
        "metadata": metadata,
        "tags": ["sensor"],
    })
}

/// The alarm one troubled sensor files. The reason is what the failing
/// call answered — never a value.
pub fn alarm_body(sensor: &SensorRow, alarm: &Alarm, owner: &str) -> Json {
    let reason = &alarm.reason;
    let (title, detail) = match alarm.condition {
        Condition::Unreadable => (
            format!(
                "ESTATE ALARM: sensor {} cannot read its source ({})",
                sensor.id, sensor.source
            ),
            format!(
                "Raised by sensor.poll (design 14c9b2ad): the sensor `{}` (source {}, credential \
                 `{}`) could not be read: {reason}. Until it can, no {} packet is opened for \
                 anything the source records. The poll retries every {} minutes and refreshes \
                 this packet only when the reason changes; the next good read closes it.",
                sensor.id,
                sensor.source,
                sensor.credential,
                sensor.opens_kind,
                sensor.every_minutes
            ),
        ),
        Condition::Unopenable => (
            format!(
                "ESTATE ALARM: sensor {} cannot open its {} packet",
                sensor.id, sensor.opens_kind
            ),
            format!(
                "Raised by sensor.poll (backlog f50a9ec1): the sensor `{}` (source {}) read its \
                 source and recorded the reading, but the jobs API refused the `{}` packet it \
                 declares (subject_kind `{}`): {reason}. The reading is recorded and unstamped, \
                 so it is still owed a packet; the poll retries the open every {} minutes and \
                 refreshes this packet only when the refusal changes; the first open that \
                 succeeds closes it. The usual causes: the workflow `{}` is not published, its \
                 subject_kinds do not admit `{}`, or the described metadata fails its schema.",
                sensor.id,
                sensor.source,
                sensor.opens_kind,
                sensor.subject_kind,
                sensor.every_minutes,
                sensor.opens_kind,
                sensor.subject_kind,
            ),
        ),
    };
    let mut metadata = json!({
        "area": "estate",
        "estate_finding": alarm.condition.key(&sensor.id),
        "scope": "sensor",
        "sensor_id": sensor.id,
        "source": sensor.source,
        "credential": sensor.credential,
        "reason": reason,
        "detail": detail,
    });
    if alarm.condition == Condition::Unopenable
        && let Some(m) = metadata.as_object_mut()
    {
        m.insert("opens_kind".into(), json!(sensor.opens_kind));
        m.insert("subject_kind".into(), json!(sensor.subject_kind));
        m.insert("external_id".into(), json!(alarm.external_id));
    }
    json!({
        "kind": "backlog-item",
        "title": title,
        "subject": {"subject_kind": "custom", "id": sensor.id},
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": metadata,
    })
}

/// The sensor registry as `GET /api/sensors` answered it.
///
/// NO `data` ARRAY IS A REFUSAL, NOT AN EMPTY LIST (backlog 6c4c432a,
/// the same reading `retro_open::departments` takes of departments,
/// 80a77466). An error shape, a changed contract or a policy-narrowed
/// answer carries no `data` key, and reading that as zero sensors
/// polls nothing and reports the pass healthy — the whole class of
/// outside-world input going dark with the only signal an absence of
/// readings. An EMPTY array is honest: sensors are a registry, not a
/// work queue, and a deployment may legitimately declare none, so it
/// keeps working and no floor is put on the count.
///
/// The refusal is RETRYABLE (`HandlerError::Downstream` at the call
/// site, as the malformed-row case already was): a listing with no
/// `data` is a bad answer from the jobs API, not a deterministic fault
/// in the request this handler sent, so a redelivery can succeed once
/// the service or the policy scope is right, and the firing naks
/// loudly instead of terminating.
pub fn sensors(listing: &Json) -> Result<Vec<SensorRow>, String> {
    super::common::rows_or_refuse(listing, "GET /api/sensors")
}

/// The readings a sensor still owes a packet, from its unstamped
/// listing — the same rule as `sensors` above, one call later and with
/// more at stake. A missing `data` read as zero here means "no reading
/// owes a packet", so the poll completes happily, the sensor looks
/// alive and the packets those readings owe never open. An EMPTY array
/// is honest (a sensor may genuinely owe nothing); a missing or
/// non-array one is no answer and refuses.
pub fn owed_readings(listing: &Json) -> Result<Vec<Reading>, String> {
    super::common::rows_or_refuse(listing, "GET /api/sensors/{id}/readings")
}

/// The open alarm carrying `key`, as `(id, reason)`, if any — and
/// whether the page can be trusted (a truncated page holds).
pub fn open_alarm(listing: &Json, key: &str) -> Result<Option<(String, String)>, String> {
    let rows: Vec<&Json> = listing
        .get("data")
        .and_then(Json::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let total = listing
        .get("total")
        .and_then(Json::as_u64)
        .map(|t| usize::try_from(t).unwrap_or(usize::MAX));
    match total {
        Some(t) if rows.len() >= t => {}
        _ => {
            return Err(format!(
                "dedup read truncated ({} rows, total {total:?}); the alarm is held for retry rather than twinned",
                rows.len()
            ));
        }
    }
    Ok(rows
        .iter()
        .find(|j| j.pointer("/metadata/estate_finding").and_then(Json::as_str) == Some(key))
        .and_then(|j| {
            Some((
                j.get("id")?.as_str()?.to_string(),
                j.pointer("/metadata/reason")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ))
        }))
}

/// The triage completion that closes a recovered alarm.
pub fn recover_step_body(
    existing: &serde_json::Map<String, Json>,
    sensor_id: &str,
    condition: Condition,
    at: DateTime<Utc>,
) -> Json {
    let mut metadata = existing.clone();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "sensor.poll {} sensor `{sensor_id}` successfully at {}; the condition this alarm \
             carried no longer holds. Closed by machine from the success, not by judgement.",
            condition.recovered_by(),
            at.to_rfc3339()
        )),
    );
    metadata.insert("cleared_by".into(), json!("sensor.poll"));
    metadata.insert("recovered_at".into(), json!(at.to_rfc3339()));
    json!({"status": "completed", "metadata": metadata})
}

/// The instant a firing reads the world at: the tick's `_at` when the
/// clock path fired it (provenance), else the one real clock.
pub fn firing_instant(payload: &Json) -> DateTime<Utc> {
    payload
        .get("_at")
        .and_then(Json::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(boss_clock_client::wall_now)
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

pub struct SensorPoll {
    client: reqwest::Client,
    jobs_base: String,
    sources: HashMap<String, Arc<dyn SensorSource>>,
    credentials: CredentialValues,
    /// Who the packets this handler files are owned by — the platform
    /// owner through the port (backlog 3c23662d), resolved once per
    /// invocation by `common::owner_for_filing`; never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

/// What one sensor's poll did — the line the firing logs per sensor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// `(readings recorded, packets opened)`.
    Read { recorded: usize, opened: usize },
    /// The alarm was raised, refreshed, or left as it was.
    Unreadable(&'static str),
    /// The source read, but a packet open was refused: what happened
    /// to the alarm, and what was recorded and opened before it.
    Unopenable {
        alarm: &'static str,
        recorded: usize,
        opened: usize,
    },
}

impl SensorPoll {
    pub fn new(
        jobs_base: impl Into<String>,
        sources: HashMap<String, Arc<dyn SensorSource>>,
        credentials: CredentialValues,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            sources,
            credentials,
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn get(&self, path: &str) -> Result<Json, HandlerError> {
        let url = format!("{}{path}", self.base());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {st}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
    }

    /// A write signed as the sensor's own actor, answered as the API
    /// answered it: the status and the body, whatever they were. Only
    /// transport failure is an error here; the caller judges the
    /// status, because one caller (the packet open) needs to tell a
    /// refusal from weather and keep the refusal's text.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Json,
        sensor_id: &str,
    ) -> Result<(reqwest::StatusCode, String), HandlerError> {
        let url = format!("{}{path}", self.base());
        let verb = method.to_string();
        let resp = self
            .client
            .request(method, &url)
            .header("content-type", "application/json")
            .header("x-boss-user", sensor_actor_header(sensor_id))
            .header("x-sim-origin", sim_origin_value())
            .json(body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{verb} {url}: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }

    /// A write that must succeed. Returns the body for the one caller
    /// that reads it (the packet's id).
    async fn write(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Json,
        sensor_id: &str,
    ) -> Result<Json, HandlerError> {
        let url = format!("{}{path}", self.base());
        let verb = method.to_string();
        let (status, text) = self.send(method, path, body, sensor_id).await?;
        if !status.is_success() {
            return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
                HandlerError::Permanent(format!("{verb} {url} returned {status}: {text}"))
            } else {
                HandlerError::Downstream(format!("{verb} {url} returned {status}: {text}"))
            });
        }
        Ok(serde_json::from_str(&text).unwrap_or(Json::Null))
    }

    async fn mark_polled(
        &self,
        sensor: &SensorRow,
        polled_at: DateTime<Utc>,
        cursor_at: Option<DateTime<Utc>>,
    ) -> Result<(), HandlerError> {
        let stamp = PollStamp {
            polled_at,
            cursor_at,
        };
        self.write(
            reqwest::Method::POST,
            &format!("/api/sensors/{}/polled", sensor.id),
            &serde_json::to_value(stamp).unwrap_or(Json::Null),
            &sensor.id,
        )
        .await?;
        Ok(())
    }

    async fn open_alarm_for(&self, key: &str) -> Result<Option<(String, String)>, HandlerError> {
        let listing = self
            .get(&format!(
                "/api/jobs?kind=backlog-item&status=open&limit={DEDUP_PAGE}"
            ))
            .await?;
        open_alarm(&listing, key).map_err(HandlerError::Downstream)
    }

    /// File the alarm, or refresh the open one when the reason moved,
    /// or leave it when nothing changed. One mechanism for both
    /// conditions; the alarm's key is what tells them apart.
    async fn raise_or_refresh(
        &self,
        sensor: &SensorRow,
        alarm: &Alarm,
    ) -> Result<&'static str, HandlerError> {
        let key = alarm.condition.key(&sensor.id);
        let reason = &alarm.reason;
        let owner = owner_for_filing(self.owner.as_ref(), self.name()).await;
        match self.open_alarm_for(&key).await? {
            Some((id, held)) if held == *reason => {
                tracing::info!(finding = %key, packet = %id, "sensor.poll: alarm already open with this reason");
                Ok("held")
            }
            Some((id, _)) => {
                // The fresh body's metadata, whole: a merge on the
                // packet, so every key the reason moved with (the
                // detail, the reading's id) moves with it.
                self.write(
                    reqwest::Method::PATCH,
                    &format!("/api/jobs/{id}/metadata"),
                    &alarm_body(sensor, alarm, &owner)["metadata"],
                    &sensor.id,
                )
                .await?;
                tracing::info!(finding = %key, packet = %id, "sensor.poll refreshed the open alarm");
                Ok("refreshed")
            }
            None => {
                self.write(
                    reqwest::Method::POST,
                    "/api/jobs",
                    &alarm_body(sensor, alarm, &owner),
                    &sensor.id,
                )
                .await?;
                tracing::warn!(finding = %key, reason = %reason, "sensor.poll raised an alarm");
                Ok("raised")
            }
        }
    }

    /// Close the open alarm for `condition`, if one is, through its
    /// triage step.
    async fn recover(
        &self,
        sensor: &SensorRow,
        condition: Condition,
        at: DateTime<Utc>,
    ) -> Result<(), HandlerError> {
        let key = condition.key(&sensor.id);
        let Some((id, _)) = self.open_alarm_for(&key).await? else {
            return Ok(());
        };
        let job = self.get(&format!("/api/jobs/{id}")).await?;
        let Some((step_id, existing)) = triage_step(&job) else {
            tracing::warn!(finding = %key, packet = %id, "sensor.poll: the open alarm has no triage step to close");
            return Ok(());
        };
        self.write(
            reqwest::Method::PUT,
            &format!("/api/jobs/{id}/steps/{step_id}"),
            &recover_step_body(&existing, &sensor.id, condition, at),
            &sensor.id,
        )
        .await?;
        tracing::info!(finding = %key, packet = %id, "sensor.poll closed the alarm: the condition no longer holds");
        Ok(())
    }

    /// The whole obligation for one due sensor.
    async fn poll_one(
        &self,
        sensor: &SensorRow,
        now: DateTime<Utc>,
    ) -> Result<PollOutcome, HandlerError> {
        let unreadable = |reason: String| async move {
            let what = self
                .raise_or_refresh(sensor, &Alarm::unreadable(reason))
                .await?;
            self.mark_polled(sensor, now, None).await?;
            Ok::<_, HandlerError>(PollOutcome::Unreadable(what))
        };
        let credential = match self.credentials.value(&sensor.credential) {
            Ok(v) => v,
            Err(why) => return unreadable(why).await,
        };
        let Some(source) = self.sources.get(&sensor.source) else {
            return unreadable(format!(
                "no source adapter named `{}` — the product reads: {}",
                sensor.source,
                self.sources.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
            .await;
        };
        let observations = match source.read(&credential, sensor.cursor_at).await {
            Ok(o) => o,
            Err(SourceError::Unreadable(why)) => return unreadable(why).await,
            Err(SourceError::Transient(why)) => {
                return Err(HandlerError::Downstream(format!(
                    "sensor {}: {why}",
                    sensor.id
                )));
            }
        };
        let readings: Vec<NewReading> = observations
            .iter()
            .map(|o| NewReading {
                external_id: o.external_id.clone(),
                observed_at: o.observed_at,
                payload: o.payload.clone(),
            })
            .collect();
        let mut recorded = 0;
        if !readings.is_empty() {
            let out = self
                .write(
                    reqwest::Method::POST,
                    &format!("/api/sensors/{}/readings", sensor.id),
                    &serde_json::to_value(&readings).unwrap_or(Json::Null),
                    &sensor.id,
                )
                .await?;
            recorded = out
                .get("inserted")
                .and_then(Json::as_u64)
                .map(|n| n as usize)
                .unwrap_or(0);
        }
        // Open a packet per reading still owed one — the ones just
        // recorded and any an earlier firing recorded but could not
        // open — and stamp each before the next.
        let listing = self
            .get(&format!(
                "/api/sensors/{}/readings?unstamped=true",
                sensor.id
            ))
            .await?;
        let owed: Vec<Reading> = owed_readings(&listing).map_err(HandlerError::Downstream)?;
        // The cursor is the newest observation whether or not every
        // packet opens: the readings are recorded, and an owed one is
        // found by its missing stamp, not by re-reading the source.
        let cursor = observations.iter().map(|o| o.observed_at).max();
        let mut opened = 0;
        for r in &owed {
            let described = source.describe(&r.payload);
            let (status, text) = self
                .send(
                    reqwest::Method::POST,
                    "/api/jobs",
                    &packet_body(sensor, r, &described),
                    &sensor.id,
                )
                .await?;
            if is_refusal(status) {
                // The unopenable leg: the same raise / mark as the
                // unreadable one, one step later. The reading stays
                // unstamped (still owed), and the source having read
                // is itself the unreadable alarm's recovery.
                let alarm = Alarm::unopenable(&r.external_id, status.as_u16(), &text);
                let what = self.raise_or_refresh(sensor, &alarm).await?;
                self.mark_polled(sensor, now, cursor).await?;
                self.recover(sensor, Condition::Unreadable, now).await?;
                return Ok(PollOutcome::Unopenable {
                    alarm: what,
                    recorded,
                    opened,
                });
            }
            if !status.is_success() {
                return Err(HandlerError::Downstream(format!(
                    "POST /api/jobs for reading {} returned {status}: {text}",
                    r.external_id
                )));
            }
            let created: Json = serde_json::from_str(&text).unwrap_or(Json::Null);
            let packet_id = created
                .get("id")
                .or_else(|| created.pointer("/data/id"))
                .and_then(Json::as_str)
                .ok_or_else(|| {
                    HandlerError::Downstream(format!(
                        "POST /api/jobs answered without an id for reading {}: {created}",
                        r.external_id
                    ))
                })?
                .to_string();
            self.write(
                reqwest::Method::PUT,
                &format!(
                    "/api/sensors/{}/readings/{}/packet",
                    sensor.id, r.external_id
                ),
                &json!({ "packet_id": packet_id }),
                &sensor.id,
            )
            .await?;
            opened += 1;
        }
        self.mark_polled(sensor, now, cursor).await?;
        self.recover(sensor, Condition::Unreadable, now).await?;
        self.recover(sensor, Condition::Unopenable, now).await?;
        Ok(PollOutcome::Read { recorded, opened })
    }
}

#[async_trait]
impl Handler for SensorPoll {
    fn name(&self) -> &'static str {
        "sensor.poll"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let now = firing_instant(&ctx.event_payload);
        let listing = get_json(
            &self.client,
            &format!("{}/api/sensors", self.base()),
            &ctx.rule_name,
        )
        .await?;
        let sensors = sensors(&listing).map_err(HandlerError::Downstream)?;
        let mut transient: Vec<String> = Vec::new();
        for sensor in sensors.iter().filter(|s| s.due_at(now)) {
            match self.poll_one(sensor, now).await {
                Ok(outcome) => {
                    tracing::info!(sensor = %sensor.id, ?outcome, "sensor.poll");
                }
                Err(HandlerError::Downstream(why)) => {
                    tracing::warn!(sensor = %sensor.id, error = %why, "sensor.poll: transient, will retry");
                    transient.push(why);
                }
                Err(e) => return Err(e),
            }
        }
        // The readings' retention sweep rides the same firing — one
        // sweep per pass, whatever the sensors did, signed as the rule
        // (it is the platform's chore, not any one sensor's).
        post_json(
            &self.client,
            &format!("{}/api/sensors/sweep", self.base()),
            &json!({}),
            &ctx.rule_name,
        )
        .await?;
        if transient.is_empty() {
            Ok(())
        } else {
            Err(HandlerError::Downstream(transient.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs::sensors::Sensors as _;
    use std::sync::Mutex;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sensor() -> SensorRow {
        SensorRow {
            id: "stripe-sponsorships".into(),
            source: "stripe".into(),
            credential: "stripe-restricted-read".into(),
            every_minutes: 15,
            opens_kind: "receive-a-sponsorship".into(),
            subject_kind: "custom".into(),
            enabled: true,
            tenant_id: "acme".into(),
            published_at: at("2026-09-17T00:00:00Z"),
            last_polled_at: None,
            cursor_at: None,
        }
    }

    fn obs(id: &str, when: &str, amount: i64) -> Observation {
        Observation {
            external_id: id.into(),
            observed_at: at(when),
            payload: json!({"id": id, "amount": amount, "status": "succeeded"}),
        }
    }

    // ----- pure pieces -----

    #[test]
    fn the_packet_is_the_sensors_declared_kind_about_the_reading_signed_by_the_sensor() {
        let r = Reading {
            sensor_id: "stripe-sponsorships".into(),
            external_id: "ch_1".into(),
            observed_at: at("2026-09-17T10:00:00Z"),
            payload: json!({"id": "ch_1", "amount": 500}),
            packet_id: None,
        };
        let d = Described {
            title: "Sponsorship: $5.00".into(),
            metadata: json!({"amount_cents": 500, "currency": "usd"}),
        };
        let b = packet_body(&sensor(), &r, &d);
        assert_eq!(b["kind"], "receive-a-sponsorship");
        assert_eq!(b["subject"]["subject_kind"], "custom");
        assert_eq!(b["subject"]["id"], "ch_1");
        assert_eq!(b["owner_id"], "automation:sensor:stripe-sponsorships");
        assert_eq!(b["metadata"]["amount_cents"], 500);
        assert_eq!(b["metadata"]["sensor_id"], "stripe-sponsorships");
        assert_eq!(b["metadata"]["external_id"], "ch_1");
        assert_eq!(b["title"], "Sponsorship: $5.00");
    }

    #[test]
    fn the_alarm_is_dedup_keyed_and_carries_the_reason_never_a_value() {
        let b = alarm_body(
            &sensor(),
            &Alarm::unreadable("env BOSS_BROKER_STRIPE_KEY is empty"),
            "emp-owner",
        );
        assert_eq!(b["owner_id"], "emp-owner", "the owner is the one handed in");
        assert_eq!(
            b["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        assert_eq!(b["metadata"]["area"], "estate");
        assert_eq!(b["priority"], "urgent");
        assert!(
            b["metadata"]["detail"]
                .as_str()
                .unwrap()
                .contains("BOSS_BROKER_STRIPE_KEY")
        );
        assert!(b["title"].as_str().unwrap().contains("stripe-sponsorships"));
    }

    #[test]
    fn the_open_alarm_is_found_by_key_and_a_truncated_page_holds() {
        let listing = json!({
            "data": [
                {"id": "other", "metadata": {"estate_finding": "dns_drift:x"}},
                {"id": "alarm-1", "metadata": {"estate_finding": "sensor_unreadable:stripe-sponsorships", "reason": "401"}}
            ],
            "total": 2
        });
        assert_eq!(
            open_alarm(&listing, "sensor_unreadable:stripe-sponsorships").unwrap(),
            Some(("alarm-1".into(), "401".into()))
        );
        assert_eq!(
            open_alarm(&listing, "sensor_unreadable:other").unwrap(),
            None
        );
        let truncated = json!({"data": [], "total": 5});
        assert!(
            open_alarm(&truncated, "k")
                .unwrap_err()
                .contains("truncated")
        );
    }

    /// Backlog 6c4c432a. A listing with NO `data` array is no answer —
    /// an error shape, a changed contract, a policy-narrowed read — and
    /// it must refuse, not poll nothing and report healthy. An EMPTY
    /// array is honest: a deployment may legitimately declare no
    /// sensors, so it keeps working. Same distinction, same words, as
    /// `retro_open::departments` (80a77466).
    #[test]
    fn a_listing_with_no_data_array_is_a_refusal_and_an_empty_one_is_honest() {
        assert_eq!(
            sensors(&json!({ "data": [], "total": 0 })).expect("empty is honest"),
            vec![]
        );
        let why = sensors(&json!({ "total": 0 })).expect_err("no data array is a refusal");
        assert!(why.contains("no `data` array"), "{why}");
        let why = sensors(&json!({ "error": "forbidden" })).expect_err("an error shape refuses");
        assert!(why.contains("no `data` array"), "{why}");
        let why = sensors(&json!({ "data": [{ "id": 7 }] })).expect_err("a bad row refuses");
        assert!(why.contains("not in shape"), "{why}");
    }

    /// The readings half of the same rule (0767c830). This one is
    /// sharper than the listing's: when the LISTING goes dark the pass
    /// polls nothing, which at least leaves an absence. Here the
    /// readings are recorded and in the record, the sensor looks alive
    /// and the cadence reports healthy — and a missing `data` read as
    /// zero means the packets they owe silently never open. The
    /// evidence of health is what makes the failure invisible, so an
    /// EMPTY array stays honest (a sensor may genuinely owe nothing)
    /// and a MISSING one refuses.
    #[test]
    fn owed_readings_refuse_a_missing_data_array_and_an_empty_one_is_honest() {
        assert_eq!(
            owed_readings(&json!({ "data": [], "total": 0 })).expect("empty is honest"),
            vec![]
        );
        let why = owed_readings(&json!({ "total": 0 })).expect_err("no data array is a refusal");
        assert!(why.contains("no `data` array"), "{why}");
        let why =
            owed_readings(&json!({ "error": "forbidden" })).expect_err("an error shape refuses");
        assert!(why.contains("no `data` array"), "{why}");
        let why = owed_readings(&json!({ "data": null })).expect_err("a null data refuses");
        assert!(why.contains("no `data` array"), "{why}");
        let why = owed_readings(&json!({ "data": { "external_id": "x" } }))
            .expect_err("an object data refuses");
        assert!(why.contains("no `data` array"), "{why}");
        let why = owed_readings(&json!({ "data": [{ "external_id": 7 }] }))
            .expect_err("a bad row refuses");
        assert!(why.contains("not in shape"), "{why}");
    }

    #[test]
    fn credential_values_name_the_env_var_when_unset_and_the_id_when_undeclared() {
        let c = CredentialValues::new()
            .with("stripe-restricted-read", "BOSS_BROKER_STRIPE_KEY", None)
            .with("other", "BOSS_OTHER", Some("secret-value".into()));
        let why = c.value("stripe-restricted-read").unwrap_err();
        assert!(why.contains("BOSS_BROKER_STRIPE_KEY"), "{why}");
        assert!(!why.contains("secret-value"));
        assert!(c.value("nobody").unwrap_err().contains("nobody"));
        assert_eq!(c.value("other").unwrap(), "secret-value");
        // Whitespace is unset.
        let c = CredentialValues::new().with("x", "X", Some("  ".into()));
        assert!(c.value("x").unwrap_err().contains("X is empty"));
    }

    #[test]
    fn the_firing_instant_is_the_ticks_when_the_clock_fired_it() {
        assert_eq!(
            firing_instant(&json!({"_at": "2026-09-17T10:05:00+00:00"})),
            at("2026-09-17T10:05:00Z")
        );
        let before = boss_clock_client::wall_now();
        assert!(firing_instant(&json!({})) >= before);
    }

    // ----- the stub jobs API: sensors door + jobs + alarms, in memory -----

    type Captured = Arc<Mutex<Vec<(String, Json)>>>;

    struct Stub {
        base: String,
        captured: Captured,
        sensors: Arc<boss_jobs::sensors::InMemorySensors>,
        /// Open backlog-items, as the listing answers them.
        alarms: Arc<Mutex<Vec<Json>>>,
        /// When set, every packet open (a POST /api/jobs that is not
        /// a backlog-item) is answered with this status and body.
        refuse_opens: Arc<Mutex<Option<(u16, String)>>>,
    }

    impl Stub {
        fn packets(&self) -> Vec<Json> {
            self.captured
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k == "POST /api/jobs")
                .map(|(_, b)| b.clone())
                .collect()
        }
        fn writes(&self, prefix: &str) -> Vec<(String, Json)> {
            self.captured
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .cloned()
                .collect()
        }
    }

    async fn stub_jobs_api(open_alarms: Vec<Json>) -> Stub {
        use axum::extract::{Path, Query};
        use axum::response::IntoResponse;
        use axum::{Json as AxJson, Router, routing::get, routing::post};
        use std::collections::HashMap as Map;

        let captured: Captured = Default::default();
        let sensors = Arc::new(boss_jobs::sensors::InMemorySensors::new());
        let alarms = Arc::new(Mutex::new(open_alarms));
        let refuse_opens: Arc<Mutex<Option<(u16, String)>>> = Default::default();
        let refuse = refuse_opens.clone();
        let (c1, c2, c3) = (captured.clone(), captured.clone(), captured.clone());
        let (a1, a2, a3, a4) = (
            alarms.clone(),
            alarms.clone(),
            alarms.clone(),
            alarms.clone(),
        );
        let app = Router::new()
            .merge(boss_jobs::sensors::http::router(
                boss_jobs::sensors::http::SensorsApiState {
                    repo: sensors.clone() as Arc<dyn boss_jobs::sensors::Sensors>,
                },
            ))
            .route(
                "/api/jobs",
                get(move |Query(q): Query<Map<String, String>>| {
                    let alarms = a1.clone();
                    async move {
                        assert_eq!(q.get("kind").map(String::as_str), Some("backlog-item"));
                        assert_eq!(q.get("status").map(String::as_str), Some("open"));
                        let rows = alarms.lock().unwrap().clone();
                        AxJson(json!({ "data": rows, "total": rows.len() }))
                    }
                })
                .post(
                    move |headers: axum::http::HeaderMap, AxJson(body): AxJson<Json>| {
                        let c = c1.clone();
                        let alarms = a2.clone();
                        let refuse = refuse.clone();
                        async move {
                            let actor: Json = serde_json::from_str(
                                headers.get("x-boss-user").unwrap().to_str().unwrap(),
                            )
                            .unwrap();
                            let mut b = body.clone();
                            b["_actor"] = actor["id"].clone();
                            let refusal = refuse.lock().unwrap().clone();
                            if let Some((status, text)) = refusal
                                && body["kind"] != "backlog-item"
                            {
                                c.lock()
                                    .unwrap()
                                    .push(("POST /api/jobs (refused)".into(), b));
                                return (axum::http::StatusCode::from_u16(status).unwrap(), text)
                                    .into_response();
                            }
                            c.lock().unwrap().push(("POST /api/jobs".into(), b));
                            let n = c.lock().unwrap().len();
                            let id = format!("job-{n}");
                            if body["kind"] == "backlog-item" {
                                let mut row = body.clone();
                                row["id"] = json!(id);
                                row["steps"] = json!([
                                    {"id": format!("{id}-triage"), "spec_slug": "triage", "status": "ready",
                                     "metadata": {"authority_role": "platform-admin"}}
                                ]);
                                alarms.lock().unwrap().push(row);
                            }
                            AxJson(json!({ "id": id })).into_response()
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let alarms = a3.clone();
                    async move {
                        let row = alarms
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|r| r["id"] == id)
                            .cloned()
                            .unwrap_or(json!({"id": id, "steps": []}));
                        AxJson(row)
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(move |Path(id): Path<String>, AxJson(body): AxJson<Json>| {
                    let c = c2.clone();
                    async move {
                        c.lock()
                            .unwrap()
                            .push((format!("PATCH /api/jobs/{id}/metadata"), body));
                        AxJson(json!({ "ok": true }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                axum::routing::put(
                    move |Path((id, sid)): Path<(String, String)>, AxJson(body): AxJson<Json>| {
                        let c = c3.clone();
                        let alarms = a4.clone();
                        async move {
                            // A completed triage step closes the
                            // packet, so it leaves the open listing —
                            // as the real API's terminal does.
                            if body["status"] == "completed" {
                                alarms.lock().unwrap().retain(|r| r["id"] != id);
                            }
                            c.lock()
                                .unwrap()
                                .push((format!("PUT /api/jobs/{id}/steps/{sid}"), body));
                            AxJson(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route("/health", post(|| async { "" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Stub {
            base: format!("http://{addr}"),
            captured,
            sensors,
            alarms,
            refuse_opens,
        }
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "sensors-poll-every-5-minutes".into(),
            triggering_event_id: "clock-tick:2026-09-17T10:05:00+00:00".into(),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-09-17", "_at": "2026-09-17T10:05:00+00:00"}),
        }
    }

    fn ctx_at(s: &str) -> InvocationContext {
        InvocationContext {
            event_payload: json!({"_day": "2026-09-17", "_at": s}),
            ..ctx()
        }
    }

    async fn declare(stub: &Stub) {
        stub.sensors
            .publish(
                "acme",
                &[boss_jobs::sensors::SensorInput {
                    id: "stripe-sponsorships".into(),
                    source: "stripe".into(),
                    credential: "stripe-restricted-read".into(),
                    every_minutes: 15,
                    opens: "receive-a-sponsorship".into(),
                    subject_kind: "custom".into(),
                    enabled: true,
                }],
            )
            .await
            .unwrap();
    }

    fn handler(stub: &Stub, source: Arc<InMemorySource>, key: Option<&str>) -> Arc<SensorPoll> {
        let mut sources: HashMap<String, Arc<dyn SensorSource>> = HashMap::new();
        sources.insert("stripe".into(), source);
        SensorPoll::new(
            stub.base.clone(),
            sources,
            CredentialValues::new().with(
                "stripe-restricted-read",
                "BOSS_BROKER_STRIPE_KEY",
                key.map(str::to_string),
            ),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    /// THE IDEMPOTENCE PIN. Two firings against the same source open
    /// ONE packet per reading, the reading is stamped with it, the
    /// cursor is the newest observation, and the packet is signed by
    /// the sensor.
    #[tokio::test]
    async fn a_poll_opens_one_packet_per_reading_and_a_second_poll_opens_none() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![
            obs("ch_1", "2026-09-17T09:00:00Z", 500),
            obs("ch_2", "2026-09-17T09:30:00Z", 2500),
        ]);
        let h = handler(&stub, source.clone(), Some("rk_test_x"));

        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 2, "{packets:#?}");
        assert_eq!(packets[0]["kind"], "receive-a-sponsorship");
        assert_eq!(packets[0]["subject"]["id"], "ch_1");
        assert_eq!(
            packets[0]["_actor"],
            "automation:sensor:stripe-sponsorships"
        );
        assert_eq!(packets[1]["subject"]["id"], "ch_2");
        let readings = stub.sensors.readings().await;
        assert_eq!(readings.len(), 2);
        assert!(
            readings.iter().all(|r| r.packet_id.is_some()),
            "{readings:?}"
        );
        let row = &stub.sensors.list().await.unwrap()[0];
        assert_eq!(row.cursor_at, Some(at("2026-09-17T09:30:00Z")));
        assert_eq!(row.last_polled_at, Some(at("2026-09-17T10:05:00Z")));
        // The source was asked from no cursor, with the key.
        let reads = source.reads.lock().unwrap().clone();
        assert_eq!(reads, vec![("rk_test_x".to_string(), None)]);

        // Second firing, 15 minutes on: due again, re-reads (from the
        // cursor, inclusive), opens nothing.
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 2, "a redelivery opens nothing twice");
        let reads = source.reads.lock().unwrap().clone();
        assert_eq!(reads[1].1, Some(at("2026-09-17T09:30:00Z")));
        assert_eq!(stub.sensors.readings().await.len(), 2);
        // No alarm was ever filed.
        assert!(stub.alarms.lock().unwrap().is_empty());
    }

    /// A sensor polled less than `every_minutes` ago is not read.
    #[tokio::test]
    async fn a_sensor_inside_its_period_is_not_read() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source.clone(), Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        h.invoke(&[], &ctx_at("2026-09-17T10:10:00+00:00"))
            .await
            .unwrap();
        assert_eq!(
            source.reads.lock().unwrap().len(),
            1,
            "five minutes on, not due"
        );
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(
            source.reads.lock().unwrap().len(),
            2,
            "fifteen minutes on, due"
        );
    }

    /// A reading recorded but never opened (a firing that died between
    /// the record and the open) is opened by the next pass — and only
    /// once.
    #[tokio::test]
    async fn a_reading_recorded_without_a_packet_is_opened_by_the_next_pass() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        stub.sensors
            .record(
                "stripe-sponsorships",
                &[NewReading {
                    external_id: "ch_old".into(),
                    observed_at: at("2026-09-16T12:00:00Z"),
                    payload: json!({"id": "ch_old", "amount": 100}),
                }],
            )
            .await
            .unwrap();
        let source = InMemorySource::answering(vec![]);
        let h = handler(&stub, source, Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0]["subject"]["id"], "ch_old");
        assert_eq!(packets[0]["title"], "Reading ch_old");
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1);
    }

    /// An unset key files the alarm NAMING the env var, marks the poll
    /// (so the next attempt waits the period), opens nothing; the same
    /// reason on the next pass leaves the alarm as it is; a changed
    /// reason refreshes it; a good read closes it through triage.
    #[tokio::test]
    async fn an_unreadable_credential_alarms_by_name_and_the_next_good_read_recovers() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source.clone(), None);

        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1, "{packets:#?}");
        assert_eq!(packets[0]["kind"], "backlog-item");
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        assert!(
            packets[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("BOSS_BROKER_STRIPE_KEY")
        );
        assert!(
            source.reads.lock().unwrap().is_empty(),
            "no read without a key"
        );
        let row = &stub.sensors.list().await.unwrap()[0];
        assert_eq!(row.last_polled_at, Some(at("2026-09-17T10:05:00Z")));
        assert_eq!(row.cursor_at, None);

        // Same reason, next period: held, not twinned, not patched.
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1);
        assert!(stub.writes("PATCH").is_empty());

        // A different unreadable reason (the source now answers 401):
        // the open alarm is refreshed, never twinned.
        let h2 = handler(&stub, source.clone(), Some("rk_bad"));
        source.set(Err(SourceError::Unreadable("stripe answered 401".into())));
        h2.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1);
        let patches = stub.writes("PATCH");
        assert_eq!(patches.len(), 1, "{patches:?}");
        assert_eq!(patches[0].1["reason"], "stripe answered 401");
        // The stub listing does not merge the patch, so the next pass
        // with the SAME reason would refresh again; it is the reason
        // comparison that is under test, not the stub's memory.

        // A good read: the reading lands, the packet opens, the alarm
        // is closed through its triage step with disposition stale.
        source.set(Ok(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]));
        h2.invoke(&[], &ctx_at("2026-09-17T10:50:00+00:00"))
            .await
            .unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[1]["kind"], "receive-a-sponsorship");
        let puts = stub.writes("PUT /api/jobs/job-1/steps/job-1-triage");
        assert_eq!(puts.len(), 1, "{:?}", stub.writes("PUT"));
        assert_eq!(puts[0].1["status"], "completed");
        assert_eq!(puts[0].1["metadata"]["disposition"], "stale");
        assert_eq!(
            puts[0].1["metadata"]["authority_role"], "platform-admin",
            "existing keys ride"
        );
        assert_eq!(puts[0].1["metadata"]["cleared_by"], "sensor.poll");
    }

    // ----- the unopenable path (backlog f50a9ec1) -----

    #[test]
    fn the_two_conditions_are_two_findings_on_one_sensor() {
        assert_eq!(
            Condition::Unreadable.key("stripe-sponsorships"),
            "sensor_unreadable:stripe-sponsorships"
        );
        assert_eq!(
            Condition::Unopenable.key("stripe-sponsorships"),
            "sensor_unopenable:stripe-sponsorships"
        );
    }

    #[test]
    fn the_unopenable_alarm_carries_the_refusal_verbatim_and_the_readings_id() {
        let alarm = Alarm::unopenable(
            "ch_1",
            400,
            "unknown or inactive job kind: receive-a-sponsorship",
        );
        let b = alarm_body(&sensor(), &alarm, "emp-owner");
        assert_eq!(
            b["metadata"]["estate_finding"],
            "sensor_unopenable:stripe-sponsorships"
        );
        assert_eq!(b["metadata"]["external_id"], "ch_1");
        assert_eq!(b["metadata"]["opens_kind"], "receive-a-sponsorship");
        assert_eq!(b["priority"], "urgent");
        let reason = b["metadata"]["reason"].as_str().unwrap();
        assert!(reason.contains("400"), "{reason}");
        assert!(
            reason.contains("unknown or inactive job kind: receive-a-sponsorship"),
            "{reason}"
        );
        assert!(reason.contains("ch_1"), "{reason}");
        assert!(
            b["title"].as_str().unwrap().contains("cannot open"),
            "{}",
            b["title"]
        );
        assert!(
            b["metadata"]["detail"]
                .as_str()
                .unwrap()
                .contains("receive-a-sponsorship"),
            "{}",
            b["metadata"]["detail"]
        );
        // The unreadable body still reads as it did.
        let b = alarm_body(&sensor(), &Alarm::unreadable("env X is empty"), "emp-owner");
        assert_eq!(
            b["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        assert_eq!(b["metadata"]["reason"], "env X is empty");
        assert!(b["metadata"].get("external_id").is_none());
    }

    /// A refusal body is carried verbatim up to a bound; past it the
    /// reason says how much was cut, so a packet never carries a
    /// megabyte of HTML error page.
    #[test]
    fn the_refusal_is_bounded_to_a_few_hundred_chars_and_says_so() {
        let short = Alarm::unopenable("ch_1", 422, "bad metadata");
        assert!(short.reason.ends_with("bad metadata"), "{}", short.reason);
        let long_body = "x".repeat(5000);
        let long = Alarm::unopenable("ch_1", 422, &long_body);
        assert!(
            long.reason.chars().count() < REFUSAL_BOUND + 120,
            "{}",
            long.reason.len()
        );
        assert!(long.reason.contains("5000"), "{}", long.reason);
        assert!(long.reason.contains("422"), "{}", long.reason);
    }

    /// Which answers to a packet open are a standing refusal (alarmed,
    /// retried at the sensor's period) and which are weather (NAKed).
    #[test]
    fn a_refusal_is_a_4xx_that_is_not_a_conflict_a_miss_or_a_throttle() {
        for s in [400u16, 401, 403, 422] {
            assert!(is_refusal(reqwest::StatusCode::from_u16(s).unwrap()), "{s}");
        }
        for s in [404u16, 408, 409, 429, 500, 502, 503] {
            assert!(
                !is_refusal(reqwest::StatusCode::from_u16(s).unwrap()),
                "{s}"
            );
        }
    }

    /// The source reads, the reading is recorded, but the jobs API
    /// refuses the packet (the declared kind is not published): the
    /// alarm `sensor_unopenable:<id>` is filed carrying the refusal
    /// and the reading's id, the reading stays unstamped, the poll is
    /// marked (the next attempt waits the period, and the cursor
    /// advances past what was recorded). The same refusal next period
    /// holds the packet; a changed refusal refreshes it; the first
    /// open that succeeds stamps the reading and closes the alarm
    /// through triage.
    #[tokio::test]
    async fn a_refused_packet_open_alarms_with_the_refusal_and_the_next_good_open_recovers() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 500)]);
        let h = handler(&stub, source.clone(), Some("rk_test_x"));
        *stub.refuse_opens.lock().unwrap() = Some((
            400,
            "unknown or inactive job kind: receive-a-sponsorship".into(),
        ));

        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1, "{packets:#?}");
        assert_eq!(packets[0]["kind"], "backlog-item");
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unopenable:stripe-sponsorships"
        );
        assert_eq!(packets[0]["metadata"]["external_id"], "ch_1");
        let reason = packets[0]["metadata"]["reason"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(reason.contains("400"), "{reason}");
        assert!(reason.contains("unknown or inactive job kind"), "{reason}");
        assert_eq!(
            packets[0]["_actor"],
            "automation:sensor:stripe-sponsorships"
        );
        assert_eq!(stub.writes("POST /api/jobs (refused)").len(), 1);
        let readings = stub.sensors.readings().await;
        assert_eq!(readings.len(), 1, "the reading was recorded");
        assert_eq!(readings[0].packet_id, None, "and stays unstamped");
        let row = &stub.sensors.list().await.unwrap()[0];
        assert_eq!(row.last_polled_at, Some(at("2026-09-17T10:05:00Z")));
        assert_eq!(row.cursor_at, Some(at("2026-09-17T09:00:00Z")));

        // Same refusal, next period: the open is attempted again (the
        // reading is still owed), refused again, the alarm held.
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1, "not twinned");
        assert_eq!(stub.writes("POST /api/jobs (refused)").len(), 2);
        assert!(stub.writes("PATCH").is_empty(), "not patched");

        // A different refusal (the kind is now published but the
        // metadata is refused): refreshed, not twinned.
        *stub.refuse_opens.lock().unwrap() =
            Some((422, "metadata: amount_cents must be an integer".into()));
        h.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1);
        let patches = stub.writes("PATCH");
        assert_eq!(patches.len(), 1, "{patches:?}");
        let reason = patches[0].1["reason"].as_str().unwrap();
        assert!(reason.contains("422"), "{reason}");
        assert!(reason.contains("amount_cents"), "{reason}");
        assert_eq!(patches[0].1["external_id"], "ch_1");

        // The open succeeds: the packet opens, the reading is stamped,
        // the alarm is closed through its triage step.
        *stub.refuse_opens.lock().unwrap() = None;
        h.invoke(&[], &ctx_at("2026-09-17T10:50:00+00:00"))
            .await
            .unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 2, "{packets:#?}");
        assert_eq!(packets[1]["kind"], "receive-a-sponsorship");
        assert_eq!(packets[1]["subject"]["id"], "ch_1");
        let readings = stub.sensors.readings().await;
        assert!(readings[0].packet_id.is_some(), "{readings:?}");
        // job-1 was the refused open; the alarm is job-2.
        let puts = stub.writes("PUT /api/jobs/job-2/steps/job-2-triage");
        assert_eq!(puts.len(), 1, "{:?}", stub.writes("PUT"));
        assert_eq!(puts[0].1["status"], "completed");
        assert_eq!(puts[0].1["metadata"]["disposition"], "stale");
        assert_eq!(puts[0].1["metadata"]["cleared_by"], "sensor.poll");
        assert!(
            puts[0].1["metadata"]["evidence"]
                .as_str()
                .unwrap()
                .contains("opened"),
            "{}",
            puts[0].1["metadata"]["evidence"]
        );

        // Nothing more to open, nothing more to close.
        h.invoke(&[], &ctx_at("2026-09-17T11:05:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 2);
        assert_eq!(stub.writes("PUT").len(), 1);
    }

    /// The payouts source (backlog 21eb9516) through the whole handler:
    /// a `stripe-payouts` sensor row, the real adapter against the stub
    /// Stripe, the same credential row as the sponsorships. The tenant
    /// has not published `receive-a-payout` yet: the reading is
    /// recorded, the open is refused, `sensor_unopenable:stripe-payouts`
    /// is filed naming the reading, and the cursor is the arrival date.
    /// Once the kind is published the same reading opens the packet
    /// `describe` says — title, amount, arrival, payout id, provenance.
    #[tokio::test]
    async fn a_payouts_sensor_whose_kind_is_not_published_alarms_and_opens_once_it_is() {
        use super::super::stripe_payouts::{StripePayouts, stub};

        let stub_jobs = stub_jobs_api(vec![]).await;
        stub_jobs
            .sensors
            .publish(
                "acme",
                &[boss_jobs::sensors::SensorInput {
                    id: "stripe-payouts".into(),
                    source: "stripe-payouts".into(),
                    credential: "stripe-restricted-read".into(),
                    every_minutes: 60,
                    opens: "receive-a-payout".into(),
                    subject_kind: "custom".into(),
                    enabled: true,
                }],
            )
            .await
            .unwrap();
        // 1_789_430_400 is 2026-09-15T00:00:00Z — inside the readings'
        // 90-day retention from the firing, so the sweep that rides
        // every firing leaves the reading to be read back.
        let (stripe, seen) = stub::stub_stripe(
            200,
            Some(vec![stub::payout("po_1", 1_789_430_400, "paid", 250_075)]),
        )
        .await;
        let mut sources: HashMap<String, Arc<dyn SensorSource>> = HashMap::new();
        sources.insert(
            "stripe-payouts".into(),
            Arc::new(StripePayouts::new(stripe)),
        );
        let h = SensorPoll::new(
            stub_jobs.base.clone(),
            sources,
            CredentialValues::new().with(
                "stripe-restricted-read",
                "BOSS_BROKER_STRIPE_KEY",
                Some("rk_test_good".into()),
            ),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        *stub_jobs.refuse_opens.lock().unwrap() =
            Some((400, "unknown or inactive job kind: receive-a-payout".into()));

        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub_jobs.packets();
        assert_eq!(packets.len(), 1, "{packets:#?}");
        assert_eq!(packets[0]["kind"], "backlog-item");
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unopenable:stripe-payouts"
        );
        assert_eq!(packets[0]["metadata"]["opens_kind"], "receive-a-payout");
        assert_eq!(packets[0]["metadata"]["external_id"], "po_1");
        assert!(
            packets[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("unknown or inactive job kind: receive-a-payout")
        );
        assert_eq!(packets[0]["_actor"], "automation:sensor:stripe-payouts");
        let readings = stub_jobs.sensors.readings().await;
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].packet_id, None, "still owed");
        assert_eq!(readings[0].payload["amount_cents"], 250_075);
        let row = &stub_jobs.sensors.list().await.unwrap()[0];
        assert_eq!(
            row.cursor_at,
            Some(at("2026-09-15T00:00:00Z")),
            "the cursor is the arrival date"
        );
        assert_eq!(
            seen.lock().unwrap()[0].0,
            "Bearer rk_test_good",
            "the same credential row, sent as the bearer"
        );

        // The tenant publishes the kind: the owed reading opens the
        // packet describe says, and the alarm closes.
        *stub_jobs.refuse_opens.lock().unwrap() = None;
        h.invoke(&[], &ctx_at("2026-09-17T11:05:00+00:00"))
            .await
            .unwrap();
        let packets = stub_jobs.packets();
        assert_eq!(packets.len(), 2, "{packets:#?}");
        let p = &packets[1];
        assert_eq!(p["kind"], "receive-a-payout");
        assert_eq!(p["title"], "Payout: 2500.75 USD arriving 2026-09-15");
        assert_eq!(
            p["subject"],
            json!({"subject_kind": "custom", "id": "po_1"})
        );
        assert_eq!(p["metadata"]["amount_cents"], 250_075);
        assert_eq!(p["metadata"]["currency"], "usd");
        assert_eq!(p["metadata"]["arrival_date"], "2026-09-15");
        assert_eq!(p["metadata"]["stripe_payout_id"], "po_1");
        assert_eq!(p["metadata"]["sensor_id"], "stripe-payouts");
        assert_eq!(p["metadata"]["sensor_source"], "stripe-payouts");
        assert_eq!(p["_actor"], "automation:sensor:stripe-payouts");
        assert!(stub_jobs.sensors.readings().await[0].packet_id.is_some());
        let puts = stub_jobs.writes("PUT /api/jobs/job-2/steps/job-2-triage");
        assert_eq!(
            puts.len(),
            1,
            "the alarm closed: {:?}",
            stub_jobs.writes("PUT")
        );
        assert_eq!(puts[0].1["metadata"]["disposition"], "stale");
    }

    /// A 5xx / 409 from the open is weather, exactly as it was: the
    /// firing NAKs, no alarm, the poll is not marked.
    #[tokio::test]
    async fn a_transient_answer_to_the_open_naks_without_an_alarm() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source, Some("k"));
        *stub.refuse_opens.lock().unwrap() =
            Some((503, "subject existence check unavailable".into()));
        let err = h.invoke(&[], &ctx()).await.unwrap_err();
        assert!(matches!(err, HandlerError::Downstream(_)), "{err}");
        assert!(stub.packets().is_empty(), "{:?}", stub.packets());
        assert_eq!(stub.sensors.list().await.unwrap()[0].last_polled_at, None);
    }

    /// The two alarms are two findings: a source that reads again
    /// closes the unreadable one on the read even while its packets
    /// are still refused, and the unopenable one stands until an open
    /// succeeds.
    #[tokio::test]
    async fn a_good_read_whose_open_is_refused_closes_only_the_unreadable_alarm() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        // First: no key, unreadable alarm.
        let h = handler(&stub, source.clone(), None);
        h.invoke(&[], &ctx()).await.unwrap();
        assert_eq!(
            stub.packets()[0]["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        // Then: a key, but the open is refused. The unreadable alarm
        // closes (the source read), the unopenable one is raised.
        *stub.refuse_opens.lock().unwrap() = Some((400, "unknown or inactive job kind".into()));
        let h2 = handler(&stub, source.clone(), Some("k"));
        h2.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 2, "{packets:#?}");
        assert_eq!(
            packets[1]["metadata"]["estate_finding"],
            "sensor_unopenable:stripe-sponsorships"
        );
        let puts = stub.writes("PUT /api/jobs/job-1/steps/job-1-triage");
        assert_eq!(puts.len(), 1, "the unreadable alarm closed on the read");
        assert_eq!(
            stub.writes("PUT").len(),
            1,
            "the unopenable one stands: {:?}",
            stub.writes("PUT")
        );
    }

    /// A source the product has no adapter for is unreadable by that
    /// name — a declaration error, alarmed, never a panic.
    #[tokio::test]
    async fn an_unknown_source_is_an_alarm_naming_it() {
        let stub = stub_jobs_api(vec![]).await;
        stub.sensors
            .publish(
                "acme",
                &[boss_jobs::sensors::SensorInput {
                    id: "weather".into(),
                    source: "noaa".into(),
                    credential: "stripe-restricted-read".into(),
                    every_minutes: 15,
                    opens: "k".into(),
                    subject_kind: "custom".into(),
                    enabled: true,
                }],
            )
            .await
            .unwrap();
        let h = handler(&stub, InMemorySource::answering(vec![]), Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1);
        assert!(
            packets[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("noaa")
        );
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unreadable:weather"
        );
    }

    /// A network blip NAKs: the firing fails Downstream, no alarm is
    /// filed, and the poll is not marked, so the next tick retries.
    #[tokio::test]
    async fn a_transient_error_naks_without_an_alarm_or_a_mark() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::failing(SourceError::Transient("connection reset".into()));
        let h = handler(&stub, source, Some("k"));
        let err = h.invoke(&[], &ctx()).await.unwrap_err();
        assert!(matches!(err, HandlerError::Downstream(_)), "{err}");
        assert!(err.to_string().contains("connection reset"));
        assert!(stub.packets().is_empty());
        assert_eq!(stub.sensors.list().await.unwrap()[0].last_polled_at, None);
    }

    /// A disabled sensor is never read.
    #[tokio::test]
    async fn a_disabled_sensor_is_left_alone() {
        let stub = stub_jobs_api(vec![]).await;
        stub.sensors
            .publish(
                "acme",
                &[boss_jobs::sensors::SensorInput {
                    id: "stripe-sponsorships".into(),
                    source: "stripe".into(),
                    credential: "stripe-restricted-read".into(),
                    every_minutes: 15,
                    opens: "receive-a-sponsorship".into(),
                    subject_kind: "custom".into(),
                    enabled: false,
                }],
            )
            .await
            .unwrap();
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source.clone(), Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        assert!(source.reads.lock().unwrap().is_empty());
        assert!(stub.packets().is_empty());
    }
}
