//! The Stripe adapter for `sensor.poll` (design 14c9b2ad, backlog
//! 2d33e111): `GET /v1/charges` with a restricted read-only key, paged
//! from the sensor's cursor, succeeded charges only.
//!
//! ONE ADAPTER, ONE PORT. This is the only place Stripe's HTTP shape
//! is known; the handler speaks [`SensorSource`] and the tests speak
//! the in-memory one. The base URL is `BOSS_STRIPE_API_BASE` (default
//! `https://api.stripe.com`) so a stub can stand in; the key is the
//! bearer on every request and appears nowhere else — not in a log
//! line, not in an error, not on a packet.
//!
//! THE READ. `?limit=100&created[gte]=<cursor unix>` and, to page,
//! `&starting_after=<last id of the page>` while `has_more`; Stripe
//! lists newest first, so paging walks toward the cursor. `gte`, not
//! `gt`: the cursor is the newest `created` already read, and a charge
//! created in that same second must not be lost — the readings door
//! dedups the one re-read. Only `status == "succeeded"` is a reading:
//! a pending or failed charge is not a sponsorship received. Refunds
//! and disputes are a later sensor row (`charge.refunded`), not this
//! one.
//!
//! WHAT THE PACKET SAYS. `describe` maps a charge to the metadata the
//! receive-a-sponsorship protocol reads: `amount_cents`, `currency`,
//! `customer_email`, `customer_name`, `description`, `receipt_url`,
//! `stripe_charge_id`, `sponsor_roll_name` — identifiers, amounts and
//! the sponsor's own consent, never the key.
//!
//! THE CONSENT RIDES THE PAYMENT (design "The sponsor roll", David
//! 2026-09-17; backlog 75198b15). A charge carries no custom fields;
//! the Checkout Session that produced it does. So for every charge
//! with a `payment_intent`, one more read —
//! `GET /v1/checkout/sessions?payment_intent=<id>&limit=1` — and the
//! trimmed `text.value` of the custom field keyed
//! [`SPONSOR_ROLL_FIELD`] goes onto the reading payload under that
//! key (null when the field is absent or blank, when no session lists
//! for the intent, or when the charge has no intent). The reading is
//! the charge whole plus that key, so a reading recorded before this
//! read simply has no key and describes as null. Nothing else on the
//! session is read, and the billing name is never the roll name: the
//! consent is the sponsor's to give, in that field, or not at all.
//!
//! A SESSION READ THAT FAILS IS NOT A FAILED POLL. Consent is optional;
//! the payment is not. A 401/403 on the session listing (a restricted
//! key without `checkout_sessions: read`) is a standing condition:
//! named ONCE per poll — one log line, and the same
//! [`SPONSOR_ROLL_NOTE`] on every reading it left null — and not asked
//! again until the next poll. Any other non-2xx or transport failure
//! is weather on that one charge: the reading carries null and a note
//! naming the status, and the next charge is asked.
//!
//! ERRORS. 401 and 403 on the charges listing are UNREADABLE (the key
//! is wrong, revoked, or lacks `charges: read`) — a standing condition
//! the handler alarms; everything else (transport, 5xx, 429, a body
//! that is not JSON) is TRANSIENT — weather the handler NAKs. A page
//! walk is capped at [`MAX_PAGES`]: a first read of a busy account
//! reads that much and the next poll continues from the new cursor.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::sensor_poll::{Described, Observation, SensorSource, SourceError};

/// Stripe's page ceiling.
pub const PAGE_LIMIT: u32 = 100;

/// Pages one read walks at most — 10,000 charges. The next poll
/// continues from the cursor the first advanced.
pub const MAX_PAGES: usize = 100;

/// The Checkout custom field a sponsor consents to the roll in — its
/// `key` on the session, and the key the reading and the packet carry
/// its trimmed value under.
pub const SPONSOR_ROLL_FIELD: &str = "sponsor_roll_name";

/// On a reading whose consent could not be read: why, as the session
/// listing answered it (a status and the path, never the key).
pub const SPONSOR_ROLL_NOTE: &str = "sponsor_roll_name_note";

pub struct StripeCharges {
    client: reqwest::Client,
    base: String,
}

/// `GET /v1/charges` and `GET /v1/checkout/sessions` both list this
/// way (Stripe API reference, "Pagination").
#[derive(Debug, Deserialize)]
struct ChargePage {
    data: Vec<Json>,
    #[serde(default)]
    has_more: bool,
}

/// Why one session read gave no name.
enum SessionMiss {
    /// 401/403: the key lacks the scope — standing for the whole poll.
    Refused(String),
    /// Anything else — this one charge's weather.
    Failed(String),
}

/// The trimmed `text.value` of the custom field keyed
/// [`SPONSOR_ROLL_FIELD`] on the first session of a listing — `None`
/// when the listing is empty, the field is absent, or its value is
/// blank. Reads nothing else. The shape is Stripe's Checkout Session
/// object (https://docs.stripe.com/api/checkout/sessions/object,
/// `custom_fields[]`: `{key, label, optional, type, text: {value, ..}}`),
/// pinned by the `session` fixture in the tests below.
pub fn sponsor_roll_name(sessions: &Json) -> Option<String> {
    sessions
        .pointer("/data/0/custom_fields")?
        .as_array()?
        .iter()
        .find(|f| f.get("key").and_then(Json::as_str) == Some(SPONSOR_ROLL_FIELD))?
        .pointer("/text/value")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Put the consent onto a reading's payload: the name (or null) always,
/// the note only when a read failed.
fn annotate(payload: &mut Json, name: Option<String>, note: Option<&str>) {
    if let Some(m) = payload.as_object_mut() {
        m.insert(SPONSOR_ROLL_FIELD.into(), json!(name));
        if let Some(n) = note {
            m.insert(SPONSOR_ROLL_NOTE.into(), json!(n));
        }
    }
}

impl StripeCharges {
    /// `base` is the API origin, e.g. `https://api.stripe.com`.
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    async fn page(
        &self,
        key: &str,
        since: Option<DateTime<Utc>>,
        starting_after: Option<&str>,
    ) -> Result<ChargePage, SourceError> {
        let url = format!("{}/v1/charges", self.base);
        let mut query: Vec<(&str, String)> = vec![("limit", PAGE_LIMIT.to_string())];
        if let Some(s) = since {
            query.push(("created[gte]", s.timestamp().to_string()));
        }
        if let Some(id) = starting_after {
            query.push(("starting_after", id.to_string()));
        }
        let resp = self
            .client
            .get(&url)
            .bearer_auth(key)
            .query(&query)
            .send()
            .await
            .map_err(|e| SourceError::Transient(format!("GET {url}: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(SourceError::Unreadable(format!(
                "stripe answered {status} to GET /v1/charges: the restricted key is wrong, \
                 revoked, or lacks charges: read"
            )));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SourceError::Transient(format!(
                "GET {url} returned {status}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        resp.json()
            .await
            .map_err(|e| SourceError::Transient(format!("GET {url}: not a charge page: {e}")))
    }

    /// The consent on the Checkout Session that produced
    /// `payment_intent` (Stripe API reference, "List all Checkout
    /// Sessions": `payment_intent` returns the one session for that
    /// intent). `Ok(None)` is the honest absence — no session, no
    /// field, or blank — and an error is what the listing answered.
    async fn session_consent(
        &self,
        key: &str,
        payment_intent: &str,
    ) -> Result<Option<String>, SessionMiss> {
        let path = "/v1/checkout/sessions";
        let url = format!("{}{path}", self.base);
        let query = [("payment_intent", payment_intent), ("limit", "1")];
        let resp = self
            .client
            .get(&url)
            .bearer_auth(key)
            .query(&query)
            .send()
            .await
            .map_err(|e| SessionMiss::Failed(format!("GET {path}: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(SessionMiss::Refused(format!(
                "stripe answered {status} to GET {path}: the restricted key lacks \
                 checkout_sessions: read, so the sponsor's consent could not be read"
            )));
        }
        if !status.is_success() {
            return Err(SessionMiss::Failed(format!(
                "stripe answered {status} to GET {path}"
            )));
        }
        let listing: Json = resp
            .json()
            .await
            .map_err(|e| SessionMiss::Failed(format!("GET {path}: not a session listing: {e}")))?;
        Ok(sponsor_roll_name(&listing))
    }
}

/// One charge as an observation — `None` for a charge that is not a
/// reading (not succeeded, or without the fields a reading needs).
pub fn observation(charge: &Json) -> Option<Observation> {
    if charge.get("status").and_then(Json::as_str) != Some("succeeded") {
        return None;
    }
    let id = charge.get("id")?.as_str()?.to_string();
    let created = charge.get("created")?.as_i64()?;
    let observed_at = DateTime::<Utc>::from_timestamp(created, 0)?;
    Some(Observation {
        external_id: id,
        observed_at,
        payload: charge.clone(),
    })
}

/// The packet a charge opens.
pub fn describe_charge(charge: &Json) -> Described {
    let amount = charge.get("amount").and_then(Json::as_i64).unwrap_or(0);
    let currency = charge
        .get("currency")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_uppercase();
    let billing = charge.get("billing_details");
    let email = billing
        .and_then(|b| b.get("email"))
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty());
    let name = billing
        .and_then(|b| b.get("name"))
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty());
    let who = name.or(email).unwrap_or("an anonymous sponsor");
    let title = format!(
        "Sponsorship: {}.{:02} {currency} from {who}",
        amount / 100,
        amount.rem_euclid(100)
    );
    let mut metadata = json!({
        "amount_cents": amount,
        "currency": charge.get("currency").cloned().unwrap_or(Json::Null),
        "customer_email": email,
        "customer_name": name,
        "description": charge.get("description").cloned().unwrap_or(Json::Null),
        "receipt_url": charge.get("receipt_url").cloned().unwrap_or(Json::Null),
        "stripe_charge_id": charge.get("id").cloned().unwrap_or(Json::Null),
        "stripe_customer": charge.get("customer").cloned().unwrap_or(Json::Null),
        // The sponsor's consent as `read` put it on the reading — null
        // for a reading recorded before consent was read at all.
        SPONSOR_ROLL_FIELD: charge.get(SPONSOR_ROLL_FIELD).cloned().unwrap_or(Json::Null),
    });
    if let Some(note) = charge.get(SPONSOR_ROLL_NOTE).filter(|n| !n.is_null()) {
        metadata[SPONSOR_ROLL_NOTE] = note.clone();
    }
    Described { title, metadata }
}

#[async_trait]
impl SensorSource for StripeCharges {
    async fn read(
        &self,
        credential: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Observation>, SourceError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        // The one refusal this poll has seen: named once, carried on
        // every reading it leaves null, never asked again this read.
        let mut refused: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page = self.page(credential, since, after.as_deref()).await?;
            for charge in &page.data {
                let Some(mut obs) = observation(charge) else {
                    continue;
                };
                let intent = charge.get("payment_intent").and_then(Json::as_str);
                let (name, note) = match (intent, &refused) {
                    (None, _) => (None, None),
                    (Some(_), Some(note)) => (None, Some(note.clone())),
                    (Some(pi), None) => match self.session_consent(credential, pi).await {
                        Ok(name) => (name, None),
                        Err(SessionMiss::Refused(note)) => {
                            tracing::warn!(charge = %obs.external_id, note = %note, "stripe: consent unread this poll");
                            refused = Some(note.clone());
                            (None, Some(note))
                        }
                        Err(SessionMiss::Failed(note)) => (None, Some(note)),
                    },
                };
                annotate(&mut obs.payload, name, note.as_deref());
                out.push(obs);
            }
            let last = page
                .data
                .last()
                .and_then(|c| c.get("id"))
                .and_then(Json::as_str)
                .map(str::to_string);
            match (page.has_more, last) {
                (true, Some(id)) => after = Some(id),
                _ => break,
            }
        }
        Ok(out)
    }

    fn describe(&self, payload: &Json) -> Described {
        describe_charge(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn charge(id: &str, created: i64, status: &str, amount: i64) -> Json {
        json!({
            "id": id, "object": "charge", "amount": amount, "currency": "usd",
            "status": status, "created": created, "receipt_url": format!("https://r/{id}"),
            "description": "Sponsorship", "customer": "cus_1",
            "billing_details": {"email": "ada@example.org", "name": "Ada"}
        })
    }

    #[test]
    fn only_a_succeeded_charge_is_a_reading() {
        let ok = observation(&charge("ch_1", 1_700_000_000, "succeeded", 500)).unwrap();
        assert_eq!(ok.external_id, "ch_1");
        assert_eq!(ok.observed_at.timestamp(), 1_700_000_000);
        assert_eq!(ok.payload["amount"], 500);
        assert!(observation(&charge("ch_2", 1, "pending", 500)).is_none());
        assert!(observation(&charge("ch_3", 1, "failed", 500)).is_none());
        assert!(
            observation(&json!({"status": "succeeded"})).is_none(),
            "no id"
        );
    }

    #[test]
    fn the_packet_carries_the_sponsorship_fields_and_never_a_key() {
        let d = describe_charge(&charge("ch_1", 1, "succeeded", 2550));
        assert_eq!(d.title, "Sponsorship: 25.50 USD from Ada");
        assert_eq!(d.metadata["amount_cents"], 2550);
        assert_eq!(d.metadata["currency"], "usd");
        assert_eq!(d.metadata["customer_email"], "ada@example.org");
        assert_eq!(d.metadata["customer_name"], "Ada");
        assert_eq!(d.metadata["receipt_url"], "https://r/ch_1");
        assert_eq!(d.metadata["stripe_charge_id"], "ch_1");
        assert_eq!(d.metadata["description"], "Sponsorship");
        let anon =
            describe_charge(&json!({"amount": 100, "currency": "eur", "status": "succeeded"}));
        assert_eq!(
            anon.title,
            "Sponsorship: 1.00 EUR from an anonymous sponsor"
        );
    }

    /// A charge with the `payment_intent` a Checkout Session is found by.
    fn paid_charge(id: &str, created: i64, payment_intent: &str) -> Json {
        let mut c = charge(id, created, "succeeded", 500);
        c["payment_intent"] = json!(payment_intent);
        c
    }

    /// One Checkout Session as `GET /v1/checkout/sessions` lists it,
    /// trimmed to what the adapter reads. The `custom_fields` shape is
    /// Stripe's API reference for the Checkout Session object
    /// (https://docs.stripe.com/api/checkout/sessions/object, field
    /// `custom_fields`): an array of `{key, label: {type, custom},
    /// optional, type, text: {value, default_value, minimum_length,
    /// maximum_length}}` — `text.value` is what the customer typed, or
    /// null when the optional field was left empty.
    fn session(payment_intent: &str, custom_fields: Json) -> Json {
        json!({
            "id": format!("cs_test_{payment_intent}"),
            "object": "checkout.session",
            "payment_intent": payment_intent,
            "payment_status": "paid",
            "status": "complete",
            "custom_fields": custom_fields,
        })
    }

    fn text_field(key: &str, value: Json) -> Json {
        json!({
            "key": key,
            "label": {"type": "custom", "custom": "Name on the sponsor roll"},
            "optional": true,
            "type": "text",
            "text": {"value": value, "default_value": null,
                     "minimum_length": null, "maximum_length": 60}
        })
    }

    fn list(data: Vec<Json>) -> Json {
        json!({"object": "list", "has_more": false, "data": data})
    }

    #[test]
    fn the_sponsor_roll_name_is_the_named_custom_fields_text_trimmed() {
        let s = list(vec![session(
            "pi_1",
            json!([
                text_field("company", json!("Acme")),
                text_field(SPONSOR_ROLL_FIELD, json!("  Ada Lovelace  "))
            ]),
        )]);
        assert_eq!(sponsor_roll_name(&s).as_deref(), Some("Ada Lovelace"));
        // Blank, null, and a field of another key are no consent.
        for fields in [
            json!([text_field(SPONSOR_ROLL_FIELD, json!("   "))]),
            json!([text_field(SPONSOR_ROLL_FIELD, Json::Null)]),
            json!([text_field("company", json!("Acme"))]),
            json!([]),
        ] {
            assert_eq!(
                sponsor_roll_name(&list(vec![session("pi_1", fields)])),
                None
            );
        }
        assert_eq!(sponsor_roll_name(&list(vec![])), None, "no session");
    }

    #[test]
    fn the_packet_carries_the_consent_from_the_reading_and_null_without_it() {
        let mut c = charge("ch_1", 1, "succeeded", 100);
        assert_eq!(
            describe_charge(&c).metadata[SPONSOR_ROLL_FIELD],
            Json::Null,
            "a reading recorded before the consent read has no key: null, not absent"
        );
        assert!(
            describe_charge(&c)
                .metadata
                .get(SPONSOR_ROLL_NOTE)
                .is_none()
        );
        c[SPONSOR_ROLL_FIELD] = json!("Ada Lovelace");
        c[SPONSOR_ROLL_NOTE] = json!("x");
        let d = describe_charge(&c);
        assert_eq!(d.metadata[SPONSOR_ROLL_FIELD], "Ada Lovelace");
        assert_eq!(d.metadata[SPONSOR_ROLL_NOTE], "x");
        assert_eq!(
            d.metadata["customer_name"], "Ada",
            "the billing name is its own field, never the roll"
        );
    }

    // ----- a stub Stripe: two pages, the key checked, the query recorded -----

    type Seen = Arc<Mutex<Vec<(String, std::collections::HashMap<String, String>)>>>;

    /// What the stub's `/v1/checkout/sessions` answers.
    #[derive(Clone)]
    enum Sessions {
        /// The listing for each `payment_intent` asked; an intent not
        /// here lists empty.
        Listing(std::collections::HashMap<String, Json>),
        /// Every session read answers this status.
        Answer(u16),
    }

    async fn stub_stripe(answer: u16) -> (String, Seen) {
        let (base, charges, _) = stub_stripe_with(answer, None, Sessions::Answer(200)).await;
        (base, charges)
    }

    /// `charges` overrides the two-page default with one page.
    async fn stub_stripe_with(
        answer: u16,
        charges: Option<Vec<Json>>,
        sessions: Sessions,
    ) -> (String, Seen, Seen) {
        use axum::extract::Query;
        use axum::http::{HeaderMap, StatusCode};
        use axum::{Json as AxJson, Router, routing::get};
        use std::collections::HashMap;

        let seen: Seen = Default::default();
        let s = seen.clone();
        let seen_sessions: Seen = Default::default();
        let ss = seen_sessions.clone();
        let app = Router::new()
            .route(
                "/v1/charges",
                get(
                    move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| {
                        let s = s.clone();
                        let charges = charges.clone();
                        async move {
                            let auth = headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            s.lock().unwrap().push((auth.clone(), q.clone()));
                            if answer != 200 {
                                return (
                                    StatusCode::from_u16(answer).unwrap(),
                                    AxJson(json!({"error": {}})),
                                );
                            }
                            if auth != "Bearer rk_test_good" {
                                return (
                                    StatusCode::UNAUTHORIZED,
                                    AxJson(json!({"error": {"message": "Invalid API Key"}})),
                                );
                            }
                            if let Some(data) = charges {
                                return (StatusCode::OK, AxJson(list(data)));
                            }
                            let page = match q.get("starting_after").map(String::as_str) {
                                None => json!({
                                    "object": "list", "has_more": true,
                                    "data": [charge("ch_3", 1_700_000_300, "succeeded", 300),
                                             charge("ch_2", 1_700_000_200, "pending", 200)]
                                }),
                                Some("ch_2") => json!({
                                    "object": "list", "has_more": false,
                                    "data": [charge("ch_1", 1_700_000_100, "succeeded", 100)]
                                }),
                                Some(other) => panic!("unexpected starting_after {other}"),
                            };
                            (StatusCode::OK, AxJson(page))
                        }
                    },
                ),
            )
            .route(
                "/v1/checkout/sessions",
                get(
                    move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| {
                        let ss = ss.clone();
                        let sessions = sessions.clone();
                        async move {
                            let auth = headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            ss.lock().unwrap().push((auth, q.clone()));
                            match sessions {
                                Sessions::Answer(200) => (StatusCode::OK, AxJson(list(vec![]))),
                                Sessions::Answer(code) => (
                                    StatusCode::from_u16(code).unwrap(),
                                    AxJson(json!({"error": {"type": "invalid_request_error",
                                        "message": "This API key does not have the required permissions"}})),
                                ),
                                Sessions::Listing(by_intent) => {
                                    let pi = q.get("payment_intent").cloned().unwrap_or_default();
                                    let body = by_intent.get(&pi).cloned().unwrap_or(list(vec![]));
                                    (StatusCode::OK, AxJson(body))
                                }
                            }
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen, seen_sessions)
    }

    fn roll(obs: &[Observation], id: &str) -> (Json, Option<Json>) {
        let o = obs.iter().find(|o| o.external_id == id).unwrap();
        (
            o.payload[SPONSOR_ROLL_FIELD].clone(),
            o.payload.get(SPONSOR_ROLL_NOTE).cloned(),
        )
    }

    #[tokio::test]
    async fn a_charge_with_a_payment_intent_reads_its_sessions_custom_field_onto_the_reading() {
        let listing = [
            (
                "pi_1",
                list(vec![session(
                    "pi_1",
                    json!([
                        text_field("company", json!("Acme")),
                        text_field(SPONSOR_ROLL_FIELD, json!(" Ada Lovelace ")),
                    ]),
                )]),
            ),
            (
                "pi_2",
                list(vec![session(
                    "pi_2",
                    json!([text_field(SPONSOR_ROLL_FIELD, json!("  ")),]),
                )]),
            ),
            (
                "pi_4",
                list(vec![session(
                    "pi_4",
                    json!([text_field("company", json!("Acme")),]),
                )]),
            ),
        ];
        let sessions = listing
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let charges = vec![
            paid_charge("ch_1", 1_700_000_100, "pi_1"),
            paid_charge("ch_2", 1_700_000_200, "pi_2"),
            paid_charge("ch_3", 1_700_000_300, "pi_3"),
            paid_charge("ch_4", 1_700_000_400, "pi_4"),
            charge("ch_5", 1_700_000_500, "succeeded", 500),
            charge("ch_6", 1_700_000_600, "pending", 600),
        ];
        let (base, _, seen) =
            stub_stripe_with(200, Some(charges), Sessions::Listing(sessions)).await;
        let obs = StripeCharges::new(base)
            .read("rk_test_good", None)
            .await
            .unwrap();
        assert_eq!(obs.len(), 5, "the pending charge is still not a reading");
        assert_eq!(
            roll(&obs, "ch_1"),
            (json!("Ada Lovelace"), None),
            "present: trimmed"
        );
        assert_eq!(roll(&obs, "ch_2"), (Json::Null, None), "blank: no consent");
        assert_eq!(
            roll(&obs, "ch_3"),
            (Json::Null, None),
            "no session for the intent"
        );
        assert_eq!(
            roll(&obs, "ch_4"),
            (Json::Null, None),
            "another key is not consent"
        );
        assert_eq!(
            roll(&obs, "ch_5"),
            (Json::Null, None),
            "no payment_intent: no session read"
        );
        assert_eq!(
            obs.iter()
                .find(|o| o.external_id == "ch_1")
                .unwrap()
                .payload["billing_details"]["name"],
            "Ada",
            "the charge rides whole beside the consent"
        );
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.iter()
                .map(|(_, q)| q["payment_intent"].as_str())
                .collect::<Vec<_>>(),
            ["pi_1", "pi_2", "pi_3", "pi_4"],
            "one session read per charge with an intent, none for ch_5 / ch_6"
        );
        assert!(
            seen.iter()
                .all(|(auth, q)| auth == "Bearer rk_test_good" && q["limit"] == "1")
        );
    }

    #[tokio::test]
    async fn a_refused_session_read_is_still_a_reading_with_a_note_once_per_poll() {
        let charges = vec![
            paid_charge("ch_1", 1_700_000_100, "pi_1"),
            paid_charge("ch_2", 1_700_000_200, "pi_2"),
            charge("ch_3", 1_700_000_300, "succeeded", 300),
        ];
        let (base, _, seen) = stub_stripe_with(200, Some(charges), Sessions::Answer(403)).await;
        let obs = StripeCharges::new(base)
            .read("rk_test_good", None)
            .await
            .expect("consent is optional; the payment is not");
        assert_eq!(obs.len(), 3);
        let (name, note) = roll(&obs, "ch_1");
        assert_eq!(name, Json::Null);
        let note = note.unwrap();
        let note = note.as_str().unwrap();
        assert!(
            note.contains("403") && note.contains("/v1/checkout/sessions"),
            "{note}"
        );
        assert!(!note.contains("rk_test_good"), "{note}");
        assert_eq!(
            roll(&obs, "ch_2"),
            (Json::Null, Some(json!(note))),
            "the same note on each"
        );
        assert_eq!(
            roll(&obs, "ch_3"),
            (Json::Null, None),
            "no intent: nothing was refused"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "a refusal is standing: named once, the rest of the poll does not ask again"
        );
    }

    #[tokio::test]
    async fn any_other_failed_session_read_notes_the_status_and_keeps_the_reading() {
        let charges = vec![
            paid_charge("ch_1", 1_700_000_100, "pi_1"),
            paid_charge("ch_2", 1_700_000_200, "pi_2"),
        ];
        let (base, _, seen) = stub_stripe_with(200, Some(charges), Sessions::Answer(503)).await;
        let obs = StripeCharges::new(base)
            .read("rk_test_good", None)
            .await
            .unwrap();
        assert_eq!(obs.len(), 2);
        let (name, note) = roll(&obs, "ch_1");
        assert_eq!(name, Json::Null);
        assert!(note.unwrap().as_str().unwrap().contains("503"));
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "weather is not a refusal: the next charge is asked"
        );
    }

    #[tokio::test]
    async fn a_read_pages_from_the_cursor_inclusive_with_the_key_as_bearer() {
        let (base, seen) = stub_stripe(200).await;
        let src = StripeCharges::new(base);
        let since = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let obs = src.read("rk_test_good", Some(since)).await.unwrap();
        assert_eq!(
            obs.iter()
                .map(|o| o.external_id.as_str())
                .collect::<Vec<_>>(),
            ["ch_3", "ch_1"],
            "succeeded only, both pages"
        );
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "Bearer rk_test_good");
        assert_eq!(seen[0].1["limit"], "100");
        assert_eq!(seen[0].1["created[gte]"], "1700000000");
        assert!(!seen[0].1.contains_key("starting_after"));
        assert_eq!(
            seen[1].1["starting_after"], "ch_2",
            "the raw page's last id"
        );
    }

    #[tokio::test]
    async fn no_cursor_reads_from_the_beginning() {
        let (base, seen) = stub_stripe(200).await;
        let src = StripeCharges::new(base);
        src.read("rk_test_good", None).await.unwrap();
        assert!(!seen.lock().unwrap()[0].1.contains_key("created[gte]"));
    }

    #[tokio::test]
    async fn a_401_is_unreadable_and_names_no_key() {
        let (base, _) = stub_stripe(200).await;
        let src = StripeCharges::new(base);
        let err = src.read("rk_test_wrong", None).await.unwrap_err();
        match err {
            SourceError::Unreadable(why) => {
                assert!(why.contains("401"), "{why}");
                assert!(!why.contains("rk_test_wrong"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        let (base, _) = stub_stripe(403).await;
        assert!(matches!(
            StripeCharges::new(base).read("k", None).await,
            Err(SourceError::Unreadable(_))
        ));
    }

    #[tokio::test]
    async fn a_5xx_and_an_unreachable_host_are_transient() {
        let (base, _) = stub_stripe(503).await;
        assert!(matches!(
            StripeCharges::new(base).read("k", None).await,
            Err(SourceError::Transient(_))
        ));
        assert!(matches!(
            StripeCharges::new("http://127.0.0.1:9")
                .read("k", None)
                .await,
            Err(SourceError::Transient(_))
        ));
    }
}
