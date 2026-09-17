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
//! `stripe_charge_id` — identifiers and amounts, never the key.
//!
//! ERRORS. 401 and 403 are UNREADABLE (the key is wrong, revoked, or
//! lacks `charges: read`) — a standing condition the handler alarms;
//! everything else (transport, 5xx, 429, a body that is not JSON) is
//! TRANSIENT — weather the handler NAKs. A page walk is capped at
//! [`MAX_PAGES`]: a first read of a busy account reads that much and
//! the next poll continues from the new cursor.

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

pub struct StripeCharges {
    client: reqwest::Client,
    base: String,
}

#[derive(Debug, Deserialize)]
struct ChargePage {
    data: Vec<Json>,
    #[serde(default)]
    has_more: bool,
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
    Described {
        title,
        metadata: json!({
            "amount_cents": amount,
            "currency": charge.get("currency").cloned().unwrap_or(Json::Null),
            "customer_email": email,
            "customer_name": name,
            "description": charge.get("description").cloned().unwrap_or(Json::Null),
            "receipt_url": charge.get("receipt_url").cloned().unwrap_or(Json::Null),
            "stripe_charge_id": charge.get("id").cloned().unwrap_or(Json::Null),
            "stripe_customer": charge.get("customer").cloned().unwrap_or(Json::Null),
        }),
    }
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
        for _ in 0..MAX_PAGES {
            let page = self.page(credential, since, after.as_deref()).await?;
            out.extend(page.data.iter().filter_map(observation));
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

    // ----- a stub Stripe: two pages, the key checked, the query recorded -----

    type Seen = Arc<Mutex<Vec<(String, std::collections::HashMap<String, String>)>>>;

    async fn stub_stripe(answer: u16) -> (String, Seen) {
        use axum::extract::Query;
        use axum::http::{HeaderMap, StatusCode};
        use axum::{Json as AxJson, Router, routing::get};
        use std::collections::HashMap;

        let seen: Seen = Default::default();
        let s = seen.clone();
        let app = Router::new().route(
            "/v1/charges",
            get(
                move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| {
                    let s = s.clone();
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
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
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
