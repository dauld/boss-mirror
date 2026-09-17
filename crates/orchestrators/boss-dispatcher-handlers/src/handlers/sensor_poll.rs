//! `sensor.poll` — the first sensor: a declared poll of the world
//! outside BOSS, recorded outside the audit log, opened as work.
//!
//! Design 14c9b2ad (decided with David 2026-09-17: "Polling is fine,
//! use a restricted read-only key"), backlog 2d33e111. ONE platform
//! cadence rule (`sensors-poll-every-5-minutes`) fires this handler; it
//! reads the sensor registry (`GET /api/sensors`) and, for every
//! enabled sensor whose `every_minutes` has elapsed since its last
//! attempt, asks the source adapter for everything since the sensor's
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
    api_client, dispatcher_reader_header, get_json, post_json, sim_origin_value, triage_step,
};

/// The alarm key one unreadable sensor carries — the `estate_finding`
/// the dedup lens reads.
pub fn alarm_key(sensor_id: &str) -> String {
    format!("sensor_unreadable:{sensor_id}")
}

/// The actor a sensor's writes sign as. The packet's owner, its
/// `opened_by`, and the readings' provenance are all this one id.
pub fn sensor_actor(sensor_id: &str) -> String {
    format!("automation:sensor:{sensor_id}")
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

/// The alarm one unreadable sensor files. `reason` is what the read
/// answered — never a value.
pub fn alarm_body(sensor: &SensorRow, reason: &str) -> Json {
    json!({
        "kind": "backlog-item",
        "title": format!("ESTATE ALARM: sensor {} cannot read its source ({})", sensor.id, sensor.source),
        "subject": {"subject_kind": "custom", "id": sensor.id},
        "owner_id": "emp-david",
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": {
            "area": "estate",
            "estate_finding": alarm_key(&sensor.id),
            "scope": "sensor",
            "sensor_id": sensor.id,
            "source": sensor.source,
            "credential": sensor.credential,
            "reason": reason,
            "detail": format!(
                "Raised by sensor.poll (design 14c9b2ad): the sensor `{}` (source {}, credential \
                 `{}`) could not be read: {reason}. Until it can, no {} packet is opened for \
                 anything the source records. The poll retries every {} minutes and refreshes \
                 this packet only when the reason changes; the next good read closes it.",
                sensor.id, sensor.source, sensor.credential, sensor.opens_kind, sensor.every_minutes
            ),
        },
    })
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
    at: DateTime<Utc>,
) -> Json {
    let mut metadata = existing.clone();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "sensor.poll read sensor `{sensor_id}` successfully at {}; the condition this alarm \
             carried no longer holds. Closed by machine from the read, not by judgement.",
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
}

/// What one sensor's poll did — the line the firing logs per sensor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// `(readings recorded, packets opened)`.
    Read { recorded: usize, opened: usize },
    /// The alarm was raised, refreshed, or left as it was.
    Unreadable(&'static str),
}

impl SensorPoll {
    pub fn new(
        jobs_base: impl Into<String>,
        sources: HashMap<String, Arc<dyn SensorSource>>,
        credentials: CredentialValues,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            sources,
            credentials,
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

    /// A write signed as the sensor's own actor. Returns the body for
    /// the one caller that reads it (the packet's id).
    async fn write(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Json,
        sensor_id: &str,
    ) -> Result<Json, HandlerError> {
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
    /// or leave it when nothing changed.
    async fn raise_or_refresh(
        &self,
        sensor: &SensorRow,
        reason: &str,
    ) -> Result<&'static str, HandlerError> {
        let key = alarm_key(&sensor.id);
        match self.open_alarm_for(&key).await? {
            Some((id, held)) if held == reason => {
                tracing::info!(finding = %key, packet = %id, "sensor.poll: alarm already open with this reason");
                Ok("held")
            }
            Some((id, _)) => {
                self.write(
                    reqwest::Method::PATCH,
                    &format!("/api/jobs/{id}/metadata"),
                    &json!({ "reason": reason, "detail": alarm_body(sensor, reason)["metadata"]["detail"] }),
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
                    &alarm_body(sensor, reason),
                    &sensor.id,
                )
                .await?;
                tracing::warn!(finding = %key, reason = %reason, "sensor.poll raised an alarm");
                Ok("raised")
            }
        }
    }

    /// Close the open alarm, if one is, through its triage step.
    async fn recover(&self, sensor: &SensorRow, at: DateTime<Utc>) -> Result<(), HandlerError> {
        let key = alarm_key(&sensor.id);
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
            &recover_step_body(&existing, &sensor.id, at),
            &sensor.id,
        )
        .await?;
        tracing::info!(finding = %key, packet = %id, "sensor.poll closed the alarm: the sensor reads again");
        Ok(())
    }

    /// The whole obligation for one due sensor.
    async fn poll_one(
        &self,
        sensor: &SensorRow,
        now: DateTime<Utc>,
    ) -> Result<PollOutcome, HandlerError> {
        let unreadable = |reason: String| async move {
            let what = self.raise_or_refresh(sensor, &reason).await?;
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
        let owed: Vec<Reading> = self
            .get(&format!(
                "/api/sensors/{}/readings?unstamped=true",
                sensor.id
            ))
            .await?
            .get("data")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| HandlerError::Downstream(format!("readings not in shape: {e}")))?
            .unwrap_or_default();
        let mut opened = 0;
        for r in &owed {
            let described = source.describe(&r.payload);
            let created = self
                .write(
                    reqwest::Method::POST,
                    "/api/jobs",
                    &packet_body(sensor, r, &described),
                    &sensor.id,
                )
                .await?;
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
        let cursor = observations.iter().map(|o| o.observed_at).max();
        self.mark_polled(sensor, now, cursor).await?;
        self.recover(sensor, now).await?;
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
        let sensors: Vec<SensorRow> = listing
            .get("data")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| HandlerError::Downstream(format!("sensors not in shape: {e}")))?
            .unwrap_or_default();
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
        let b = alarm_body(&sensor(), "env BOSS_BROKER_STRIPE_KEY is empty");
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
        use axum::{Json as AxJson, Router, routing::get, routing::post};
        use std::collections::HashMap as Map;

        let captured: Captured = Default::default();
        let sensors = Arc::new(boss_jobs::sensors::InMemorySensors::new());
        let alarms = Arc::new(Mutex::new(open_alarms));
        let (c1, c2, c3) = (captured.clone(), captured.clone(), captured.clone());
        let (a1, a2, a3) = (alarms.clone(), alarms.clone(), alarms.clone());
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
                        async move {
                            let actor: Json = serde_json::from_str(
                                headers.get("x-boss-user").unwrap().to_str().unwrap(),
                            )
                            .unwrap();
                            let mut b = body.clone();
                            b["_actor"] = actor["id"].clone();
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
                            AxJson(json!({ "id": id }))
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
                        async move {
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
