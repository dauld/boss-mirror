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
//! THE FEE RIDES THE BALANCE TRANSACTION (backlog 21eb9516, decided
//! by design 18cf4272). A charge states the gross; what Stripe kept
//! and what reached the balance are on the balance transaction it
//! created. So for every charge with a `balance_transaction`, one
//! more read — `GET /v1/balance_transactions/<id>` — and its `fee`
//! and `net` go onto the reading under [`FEE_CENTS`] and
//! [`NET_CENTS`] (null when the charge has no transaction yet), and
//! from there onto the packet, where the tenant's posting rule reads
//! `/metadata/fee_cents` for the fee lines.
//!
//! A SECONDARY READ THAT FAILS IS NOT A FAILED POLL. Consent and the
//! fee are optional; the payment is not. A 401/403 on the session
//! listing (a restricted key without `checkout_sessions: read`) or on
//! the transaction read (without `balance_transactions: read`) is a
//! standing condition: named ONCE per poll — one log line, and the
//! same note ([`SPONSOR_ROLL_NOTE`], [`BALANCE_TRANSACTION_NOTE`]) on
//! every reading it left null — and not asked again until the next
//! poll. The two standings are separate: a key with one scope and not
//! the other still reads everything the scope it has allows. Any
//! other non-2xx or transport failure is weather on that one charge:
//! the reading carries null and a note naming the status, and the
//! next charge is asked.
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

/// The fee Stripe kept on the charge, and what reached the balance
/// (`amount - fee`), in the smallest currency unit — the keys the
/// reading and the packet carry them under. Read from the charge's
/// balance transaction (backlog 21eb9516): the tenant's posting rule
/// for `finance.sponsorship.received` posts `/metadata/fee_cents` to
/// the fee expense, so the recognize step needs the fee on the packet.
pub const FEE_CENTS: &str = "fee_cents";
pub const NET_CENTS: &str = "net_cents";

/// On a reading whose fee could not be read: why, as the transaction
/// read answered it (a status and the path, never the key).
pub const BALANCE_TRANSACTION_NOTE: &str = "balance_transaction_note";

pub struct StripeCharges {
    client: reqwest::Client,
    base: String,
}

/// `GET /v1/charges` and `GET /v1/checkout/sessions` both list this
/// way (Stripe API reference, "Pagination").
#[derive(Debug, Deserialize)]
pub(crate) struct ChargePage {
    pub(crate) data: Vec<Json>,
    #[serde(default)]
    pub(crate) has_more: bool,
}

/// Why one secondary read — the session for a consent, the balance
/// transaction for a fee — gave nothing.
enum Miss {
    /// 401/403: the key lacks the scope — standing for the whole poll.
    Refused(String),
    /// Anything else — this one charge's weather.
    Failed(String),
}

/// One refusal, remembered for the rest of a poll: the first 401/403
/// a secondary read meets is logged once and carried as the note on
/// every later reading it leaves null, and that read is not asked
/// again until the next poll.
#[derive(Default)]
struct Standing(Option<String>);

impl Standing {
    /// `read` once, unless this poll already met the refusal: `Ok` is
    /// the value (or its honest absence), `Err` the note to carry.
    async fn ask<T, F>(&mut self, what: &str, id: &str, read: F) -> Result<Option<T>, String>
    where
        F: std::future::Future<Output = Result<Option<T>, Miss>>,
    {
        if let Some(note) = &self.0 {
            return Err(note.clone());
        }
        match read.await {
            Ok(v) => Ok(v),
            Err(Miss::Refused(note)) => {
                tracing::warn!(charge = %id, note = %note, "stripe: {what} unread this poll");
                self.0 = Some(note.clone());
                Err(note)
            }
            Err(Miss::Failed(note)) => Err(note),
        }
    }
}

/// The fee and the net of a balance transaction, as Stripe states
/// them (`fee`, `net`; both in the smallest currency unit) — `None`
/// when either is missing. The shape is Stripe's balance transaction
/// object (https://docs.stripe.com/api/balance_transactions/object),
/// pinned by the `balance_transaction` fixture in the tests below.
pub fn fee_and_net(txn: &Json) -> Option<(i64, i64)> {
    Some((txn.get("fee")?.as_i64()?, txn.get("net")?.as_i64()?))
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

/// Put one secondary read's result onto a reading's payload: the
/// `fields` (or null) always, the note under `note_key` only when the
/// read failed.
fn annotate(payload: &mut Json, fields: &[(&str, Json)], note_key: &str, note: Option<&str>) {
    if let Some(m) = payload.as_object_mut() {
        for (k, v) in fields {
            m.insert((*k).into(), v.clone());
        }
        if let Some(n) = note {
            m.insert(note_key.into(), json!(n));
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

    /// One secondary read beside the charge — `GET {path}` with the
    /// key as bearer — as JSON, or why not. `named` is the endpoint as
    /// the note says it (the path without any id, so one refusal's
    /// note reads the same on every reading it is carried by); `scope`
    /// is the restricted-key permission a 401/403 means is missing,
    /// and `what` is what the reading goes without.
    async fn secondary(
        &self,
        key: &str,
        path: &str,
        named: &str,
        query: &[(&str, &str)],
        scope: &str,
        what: &str,
    ) -> Result<Json, Miss> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(key)
            .query(query)
            .send()
            .await
            .map_err(|e| Miss::Failed(format!("GET {named}: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Miss::Refused(format!(
                "stripe answered {status} to GET {named}: the restricted key lacks {scope}, \
                 so {what} could not be read"
            )));
        }
        if !status.is_success() {
            return Err(Miss::Failed(format!(
                "stripe answered {status} to GET {named}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| Miss::Failed(format!("GET {named}: not JSON: {e}")))
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
    ) -> Result<Option<String>, Miss> {
        let path = "/v1/checkout/sessions";
        let listing = self
            .secondary(
                key,
                path,
                path,
                &[("payment_intent", payment_intent), ("limit", "1")],
                "checkout_sessions: read",
                "the sponsor's consent",
            )
            .await?;
        Ok(sponsor_roll_name(&listing))
    }

    /// The fee and net on the charge's balance transaction (Stripe API
    /// reference, "Retrieve a balance transaction"). A body without
    /// them is this charge's weather, named, not a silent zero.
    async fn balance_transaction(&self, key: &str, id: &str) -> Result<Option<(i64, i64)>, Miss> {
        let named = "/v1/balance_transactions";
        let txn = self
            .secondary(
                key,
                &format!("{named}/{id}"),
                named,
                &[],
                "balance_transactions: read",
                "the fee",
            )
            .await?;
        fee_and_net(&txn)
            .map(Some)
            .ok_or_else(|| Miss::Failed(format!("GET {named}: no fee and net on {id}")))
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
        // The fee and net as `read` put them on the reading — null for
        // a reading recorded before the fee was read at all.
        FEE_CENTS: charge.get(FEE_CENTS).cloned().unwrap_or(Json::Null),
        NET_CENTS: charge.get(NET_CENTS).cloned().unwrap_or(Json::Null),
    });
    for note_key in [SPONSOR_ROLL_NOTE, BALANCE_TRANSACTION_NOTE] {
        if let Some(note) = charge.get(note_key).filter(|n| !n.is_null()) {
            metadata[note_key] = note.clone();
        }
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
        // The one refusal each secondary read has met this poll: named
        // once, carried on every reading it leaves null, never asked
        // again this read. Two reads, two standings — a key with
        // `balance_transactions: read` but not `checkout_sessions:
        // read` still reads every fee.
        let mut sessions = Standing::default();
        let mut txns = Standing::default();
        for _ in 0..MAX_PAGES {
            let page = self.page(credential, since, after.as_deref()).await?;
            for charge in &page.data {
                let Some(mut obs) = observation(charge) else {
                    continue;
                };
                let id = obs.external_id.clone();
                if let Some(pi) = charge.get("payment_intent").and_then(Json::as_str) {
                    let (name, note) = match sessions
                        .ask("consent", &id, self.session_consent(credential, pi))
                        .await
                    {
                        Ok(name) => (name, None),
                        Err(note) => (None, Some(note)),
                    };
                    annotate(
                        &mut obs.payload,
                        &[(SPONSOR_ROLL_FIELD, json!(name))],
                        SPONSOR_ROLL_NOTE,
                        note.as_deref(),
                    );
                } else {
                    annotate(
                        &mut obs.payload,
                        &[(SPONSOR_ROLL_FIELD, Json::Null)],
                        SPONSOR_ROLL_NOTE,
                        None,
                    );
                }
                if let Some(txn) = charge.get("balance_transaction").and_then(Json::as_str) {
                    let (fee_net, note) = match txns
                        .ask("fee", &id, self.balance_transaction(credential, txn))
                        .await
                    {
                        Ok(v) => (v, None),
                        Err(note) => (None, Some(note)),
                    };
                    let (fee, net) =
                        fee_net.map_or((Json::Null, Json::Null), |(f, n)| (json!(f), json!(n)));
                    annotate(
                        &mut obs.payload,
                        &[(FEE_CENTS, fee), (NET_CENTS, net)],
                        BALANCE_TRANSACTION_NOTE,
                        note.as_deref(),
                    );
                } else {
                    annotate(
                        &mut obs.payload,
                        &[(FEE_CENTS, Json::Null), (NET_CENTS, Json::Null)],
                        BALANCE_TRANSACTION_NOTE,
                        None,
                    );
                }
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

    /// One balance transaction as `GET /v1/balance_transactions/{id}`
    /// answers it, trimmed to what the adapter reads. The shape is
    /// Stripe's API reference for the balance transaction object
    /// (https://docs.stripe.com/api/balance_transactions/object):
    /// `amount`, `fee` and `net` in the smallest currency unit with
    /// `net = amount - fee`; `fee_details[]` itemises the fee; `source`
    /// is the charge it was created by.
    fn balance_transaction(id: &str, amount: i64, fee: i64) -> Json {
        json!({
            "id": id, "object": "balance_transaction", "amount": amount,
            "available_on": 1_700_100_000, "created": 1_700_000_000, "currency": "usd",
            "description": "Sponsorship", "exchange_rate": null, "fee": fee,
            "fee_details": [{"amount": fee, "application": null, "currency": "usd",
                             "description": "Stripe processing fees", "type": "stripe_fee"}],
            "net": amount - fee, "reporting_category": "charge", "source": "ch_1",
            "status": "available", "type": "charge"
        })
    }

    #[test]
    fn the_fee_and_net_are_the_transactions_fee_and_net() {
        assert_eq!(
            fee_and_net(&balance_transaction("txn_1", 2550, 104)),
            Some((104, 2446))
        );
        assert_eq!(
            fee_and_net(&json!({"id": "txn_1", "fee": 1})),
            None,
            "no net"
        );
        assert_eq!(
            fee_and_net(&json!({"id": "txn_1", "net": 1})),
            None,
            "no fee"
        );
    }

    #[test]
    fn the_packet_carries_the_fee_and_net_from_the_reading_and_null_without_them() {
        let mut c = charge("ch_1", 1, "succeeded", 2550);
        let d = describe_charge(&c);
        assert_eq!(
            d.metadata[FEE_CENTS],
            Json::Null,
            "a reading recorded before the fee read has no key: null, not absent"
        );
        assert_eq!(d.metadata[NET_CENTS], Json::Null);
        assert!(d.metadata.get(BALANCE_TRANSACTION_NOTE).is_none());
        c[FEE_CENTS] = json!(104);
        c[NET_CENTS] = json!(2446);
        c[BALANCE_TRANSACTION_NOTE] = json!("x");
        let d = describe_charge(&c);
        assert_eq!(d.metadata[FEE_CENTS], 104);
        assert_eq!(d.metadata[NET_CENTS], 2446);
        assert_eq!(d.metadata[BALANCE_TRANSACTION_NOTE], "x");
        assert_eq!(
            d.metadata["amount_cents"], 2550,
            "the gross is still the gross"
        );
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

    /// What the stub's `/v1/balance_transactions/{id}` answers.
    #[derive(Clone)]
    enum Txns {
        /// The transaction for each id asked; an id not here is 404,
        /// as Stripe answers an unknown id.
        Listing(std::collections::HashMap<String, Json>),
        /// Every transaction read answers this status.
        Answer(u16),
    }

    async fn stub_stripe(answer: u16) -> (String, Seen) {
        let (base, charges, _, _) = stub_stripe_with(
            answer,
            None,
            Sessions::Answer(200),
            Txns::Listing(Default::default()),
        )
        .await;
        (base, charges)
    }

    /// `charges` overrides the two-page default with one page.
    async fn stub_stripe_with(
        answer: u16,
        charges: Option<Vec<Json>>,
        sessions: Sessions,
        txns: Txns,
    ) -> (String, Seen, Seen, Seen) {
        use axum::extract::{Path, Query};
        use axum::http::{HeaderMap, StatusCode};
        use axum::{Json as AxJson, Router, routing::get};
        use std::collections::HashMap;

        let seen: Seen = Default::default();
        let s = seen.clone();
        let seen_sessions: Seen = Default::default();
        let ss = seen_sessions.clone();
        let seen_txns: Seen = Default::default();
        let st = seen_txns.clone();
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
            )
            .route(
                "/v1/balance_transactions/{id}",
                get(move |headers: HeaderMap, Path(id): Path<String>| {
                    let st = st.clone();
                    let txns = txns.clone();
                    async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let mut q = HashMap::new();
                        q.insert("id".to_string(), id.clone());
                        st.lock().unwrap().push((auth, q));
                        match txns {
                            Txns::Answer(code) => (
                                StatusCode::from_u16(code).unwrap(),
                                AxJson(json!({"error": {"type": "invalid_request_error",
                                    "message": "This API key does not have the required permissions"}})),
                            ),
                            Txns::Listing(by_id) => match by_id.get(&id) {
                                Some(body) => (StatusCode::OK, AxJson(body.clone())),
                                None => (
                                    StatusCode::NOT_FOUND,
                                    AxJson(json!({"error": {"type": "invalid_request_error",
                                        "message": format!("No such balance transaction: '{id}'")}})),
                                ),
                            },
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen, seen_sessions, seen_txns)
    }

    /// A charge with the `balance_transaction` its fee is read from.
    fn settled_charge(id: &str, created: i64, txn: &str) -> Json {
        let mut c = charge(id, created, "succeeded", 2550);
        c["balance_transaction"] = json!(txn);
        c
    }

    fn fee(obs: &[Observation], id: &str) -> (Json, Json, Option<Json>) {
        let o = obs.iter().find(|o| o.external_id == id).unwrap();
        (
            o.payload[FEE_CENTS].clone(),
            o.payload[NET_CENTS].clone(),
            o.payload.get(BALANCE_TRANSACTION_NOTE).cloned(),
        )
    }

    #[tokio::test]
    async fn a_charge_with_a_balance_transaction_reads_its_fee_and_net_onto_the_reading() {
        let txns = [("txn_1", balance_transaction("txn_1", 2550, 104))]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let charges = vec![
            settled_charge("ch_1", 1_700_000_100, "txn_1"),
            settled_charge("ch_2", 1_700_000_200, "txn_missing"),
            charge("ch_3", 1_700_000_300, "succeeded", 300),
            charge("ch_4", 1_700_000_400, "pending", 400),
        ];
        let (base, _, _, seen) = stub_stripe_with(
            200,
            Some(charges),
            Sessions::Answer(200),
            Txns::Listing(txns),
        )
        .await;
        let obs = StripeCharges::new(base)
            .read("rk_test_good", None)
            .await
            .unwrap();
        assert_eq!(obs.len(), 3, "the pending charge is still not a reading");
        assert_eq!(
            fee(&obs, "ch_1"),
            (json!(104), json!(2446), None),
            "fee and net from the transaction"
        );
        let (f, n, note) = fee(&obs, "ch_2");
        assert_eq!((f, n), (Json::Null, Json::Null));
        let note = note.unwrap();
        let note = note.as_str().unwrap();
        assert!(
            note.contains("404") && note.contains("/v1/balance_transactions"),
            "an unknown transaction is this charge's weather, named: {note}"
        );
        assert_eq!(
            fee(&obs, "ch_3"),
            (Json::Null, Json::Null, None),
            "no balance_transaction: no read, null, no note"
        );
        assert_eq!(
            obs.iter()
                .find(|o| o.external_id == "ch_1")
                .unwrap()
                .payload["amount"],
            2550,
            "the charge rides whole beside the fee"
        );
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.iter()
                .map(|(_, q)| q["id"].as_str())
                .collect::<Vec<_>>(),
            ["txn_1", "txn_missing"],
            "one transaction read per charge with one, none for ch_3 / ch_4"
        );
        assert!(seen.iter().all(|(auth, _)| auth == "Bearer rk_test_good"));
    }

    #[tokio::test]
    async fn a_refused_transaction_read_is_still_a_reading_with_a_note_once_per_poll() {
        let charges = vec![
            settled_charge("ch_1", 1_700_000_100, "txn_1"),
            settled_charge("ch_2", 1_700_000_200, "txn_2"),
            charge("ch_3", 1_700_000_300, "succeeded", 300),
        ];
        let (base, _, _, seen) =
            stub_stripe_with(200, Some(charges), Sessions::Answer(200), Txns::Answer(403)).await;
        let obs = StripeCharges::new(base)
            .read("rk_test_good", None)
            .await
            .expect("the fee is optional; the payment is not");
        assert_eq!(obs.len(), 3);
        let (f, n, note) = fee(&obs, "ch_1");
        assert_eq!((f, n), (Json::Null, Json::Null));
        let note = note.unwrap();
        let note = note.as_str().unwrap();
        assert!(
            note.contains("403") && note.contains("/v1/balance_transactions"),
            "{note}"
        );
        assert!(note.contains("balance_transactions: read"), "{note}");
        assert!(!note.contains("rk_test_good"), "{note}");
        assert_eq!(
            fee(&obs, "ch_2"),
            (Json::Null, Json::Null, Some(json!(note))),
            "the same note on each"
        );
        assert_eq!(
            fee(&obs, "ch_3"),
            (Json::Null, Json::Null, None),
            "no transaction: nothing was refused"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "a refusal is standing: named once, the rest of the poll does not ask again"
        );
    }

    #[tokio::test]
    async fn any_other_failed_transaction_read_notes_the_status_and_keeps_the_reading() {
        let charges = vec![
            settled_charge("ch_1", 1_700_000_100, "txn_1"),
            settled_charge("ch_2", 1_700_000_200, "txn_2"),
        ];
        let (base, _, _, seen) =
            stub_stripe_with(200, Some(charges), Sessions::Answer(200), Txns::Answer(503)).await;
        let obs = StripeCharges::new(base)
            .read("rk_test_good", None)
            .await
            .unwrap();
        assert_eq!(obs.len(), 2);
        let (f, _, note) = fee(&obs, "ch_1");
        assert_eq!(f, Json::Null);
        assert!(note.unwrap().as_str().unwrap().contains("503"));
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "weather is not a refusal: the next charge is asked"
        );
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
        let (base, _, seen, _) = stub_stripe_with(
            200,
            Some(charges),
            Sessions::Listing(sessions),
            Txns::Listing(Default::default()),
        )
        .await;
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
        let (base, _, seen, _) = stub_stripe_with(
            200,
            Some(charges),
            Sessions::Answer(403),
            Txns::Listing(Default::default()),
        )
        .await;
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
        let (base, _, seen, _) = stub_stripe_with(
            200,
            Some(charges),
            Sessions::Answer(503),
            Txns::Listing(Default::default()),
        )
        .await;
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
