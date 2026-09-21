//! The second Stripe source for `sensor.poll` (backlog 21eb9516,
//! decided by design 18cf4272): `GET /v1/payouts`, paid payouts only,
//! one reading per payout — the money leaving the Stripe balance for
//! the bank, so the tenant's finance packet can post it (DR Bank / CR
//! Stripe balance). A sensor row with `source = "stripe-payouts"`
//! selects this adapter; the credential row is the same restricted
//! read-only key the charges source sends (`stripe-restricted-read`),
//! which needs `payouts: read` besides `charges: read`.
//!
//! THE READ. `?status=paid&limit=100&arrival_date[gte]=<cursor unix>`
//! and, to page, `&starting_after=<last id of the page>` while
//! `has_more`. Stripe's list semantics, as the API reference states
//! them and the stub below pins them: the listing is sorted by
//! `created`, newest first (every Stripe list is), `status` is a
//! filter (`pending | paid | failed | canceled`), and `arrival_date`
//! takes the same `gt/gte/lt/lte` range as `created`. The reading's
//! instant — and so the sensor's cursor — is the ARRIVAL date, not the
//! creation: a payout is created days before it lands, and the fact
//! the packet records is the money arriving. `gte`, not `gt`, for the
//! same reason as the charges: the cursor is the newest arrival already
//! read and a second payout landing that day must not be lost — the
//! readings door dedups the one re-read. Only `status == "paid"` is a
//! reading even though the query asks for paid: a page is the source's
//! answer, and the adapter checks what it was told.
//!
//! WHAT THE READING IS. Not the payout whole: the six facts the packet
//! needs — `amount_cents`, `currency`, `arrival_date` (`YYYY-MM-DD`,
//! the day the posting is dated), `payout_id`, `balance_transaction`
//! (the payout's own transaction on the balance, for a later
//! reconciliation), `status`. The balance transactions the payout
//! COVERS (the charges it sweeps to the bank) are a further paged
//! read (`GET /v1/balance_transactions?payout=<id>`) and not this car.
//!
//! WHAT THE PACKET SAYS. `describe` titles it "Payout: `<amount>` `<CUR>`
//! arriving `<date>`" and carries `amount_cents`, `currency`,
//! `arrival_date`, `stripe_payout_id`; the handler adds `sensor_id`
//! and `sensor_source`. The workflow it opens (`receive-a-payout`) is
//! the tenant's to declare; a sensor row naming a kind that is not
//! published meets the handler's unopenable alarm, exactly as the
//! sponsorship one does — pinned end-to-end in `sensor_poll`'s tests.
//!
//! ERRORS. 401 and 403 are UNREADABLE (the key is wrong, revoked, or
//! lacks `payouts: read`); everything else is TRANSIENT — the same two
//! legs as the charges source, the same page cap.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::sensor_poll::{Described, Observation, SensorSource, SourceError};
use super::stripe_charges::{ChargePage, MAX_PAGES, PAGE_LIMIT};

/// The key the packet carries the payout's id under.
pub const STRIPE_PAYOUT_ID: &str = "stripe_payout_id";

pub struct StripePayouts {
    client: reqwest::Client,
    base: String,
}

impl StripePayouts {
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
        let url = format!("{}/v1/payouts", self.base);
        let mut query: Vec<(&str, String)> = vec![
            ("status", "paid".to_string()),
            ("limit", PAGE_LIMIT.to_string()),
        ];
        if let Some(s) = since {
            query.push(("arrival_date[gte]", s.timestamp().to_string()));
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
                "stripe answered {status} to GET /v1/payouts: the restricted key is wrong, \
                 revoked, or lacks payouts: read"
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
            .map_err(|e| SourceError::Transient(format!("GET {url}: not a payout page: {e}")))
    }
}

/// One payout as an observation — `None` for a payout that is not a
/// reading (not paid, or without the fields a reading needs).
pub fn observation(payout: &Json) -> Option<Observation> {
    if payout.get("status").and_then(Json::as_str) != Some("paid") {
        return None;
    }
    let id = payout.get("id")?.as_str()?.to_string();
    let arrival = payout.get("arrival_date")?.as_i64()?;
    let observed_at = DateTime::<Utc>::from_timestamp(arrival, 0)?;
    let amount = payout.get("amount")?.as_i64()?;
    let currency = payout.get("currency")?.as_str()?;
    Some(Observation {
        external_id: id.clone(),
        observed_at,
        payload: json!({
            "amount_cents": amount,
            "currency": currency,
            "arrival_date": observed_at.format("%Y-%m-%d").to_string(),
            "payout_id": id,
            "balance_transaction": payout.get("balance_transaction").cloned().unwrap_or(Json::Null),
            "status": "paid",
        }),
    })
}

/// The packet a payout opens.
pub fn describe_payout(payload: &Json) -> Described {
    let amount = payload
        .get("amount_cents")
        .and_then(Json::as_i64)
        .unwrap_or(0);
    let currency = payload
        .get("currency")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_uppercase();
    let arrival = payload
        .get("arrival_date")
        .and_then(Json::as_str)
        .unwrap_or("an unknown date");
    let title = format!(
        "Payout: {}.{:02} {currency} arriving {arrival}",
        amount / 100,
        amount.rem_euclid(100)
    );
    let metadata = json!({
        "amount_cents": amount,
        "currency": payload.get("currency").cloned().unwrap_or(Json::Null),
        "arrival_date": payload.get("arrival_date").cloned().unwrap_or(Json::Null),
        STRIPE_PAYOUT_ID: payload.get("payout_id").cloned().unwrap_or(Json::Null),
    });
    Described { title, metadata }
}

#[async_trait]
impl SensorSource for StripePayouts {
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
                .and_then(|p| p.get("id"))
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
        describe_payout(payload)
    }
}

/// The stub Stripe the tests speak to — shared with `sensor_poll`'s
/// tests, which run the real adapter through the handler.
#[cfg(test)]
pub(crate) mod stub {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// One payout as `GET /v1/payouts` lists it, trimmed to what the
    /// adapter reads. The shape is Stripe's API reference for the
    /// payout object (https://docs.stripe.com/api/payouts/object):
    /// `amount` in the smallest currency unit, `arrival_date` and
    /// `created` as unix seconds, `balance_transaction` the payout's
    /// own transaction on the balance, `status` one of
    /// `pending | in_transit | paid | failed | canceled`.
    pub(crate) fn payout(id: &str, arrival: i64, status: &str, amount: i64) -> Json {
        json!({
            "id": id, "object": "payout", "amount": amount, "arrival_date": arrival,
            "automatic": true, "balance_transaction": format!("txn_{id}"),
            "created": arrival - 172_800, "currency": "usd", "description": "STRIPE PAYOUT",
            "destination": "ba_1", "failure_code": null, "failure_message": null,
            "livemode": true, "method": "standard", "source_type": "card",
            "statement_descriptor": null, "status": status, "type": "bank_account"
        })
    }

    pub(crate) type Seen = Arc<Mutex<Vec<(String, std::collections::HashMap<String, String>)>>>;

    /// A stub Stripe answering `/v1/payouts`: `answer` the status
    /// every read gets (200 checks the key), `payouts` one page in
    /// place of the two-page default (newest first, as Stripe lists;
    /// the second page reached by `starting_after` the first's last
    /// id). Every read's bearer and query are recorded.
    pub(crate) async fn stub_stripe(answer: u16, payouts: Option<Vec<Json>>) -> (String, Seen) {
        use axum::extract::Query;
        use axum::http::{HeaderMap, StatusCode};
        use axum::{Json as AxJson, Router, routing::get};
        use std::collections::HashMap;

        let seen: Seen = Default::default();
        let s = seen.clone();
        let app = Router::new().route(
            "/v1/payouts",
            get(
                move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| {
                    let s = s.clone();
                    let payouts = payouts.clone();
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
                        // The status filter is Stripe's, applied here
                        // so a test that lists an unpaid payout sees it
                        // filtered the way the real listing would.
                        let wanted = q.get("status").cloned();
                        let keep = |p: &Json| {
                            wanted
                                .as_deref()
                                .is_none_or(|w| p["status"].as_str() == Some(w))
                        };
                        if let Some(data) = payouts {
                            let data: Vec<Json> = data.into_iter().filter(keep).collect();
                            return (
                                StatusCode::OK,
                                AxJson(json!({"object": "list", "has_more": false, "data": data})),
                            );
                        }
                        let page = match q.get("starting_after").map(String::as_str) {
                            None => json!({
                                "object": "list", "has_more": true,
                                "data": [payout("po_3", 1_700_300_000, "paid", 30_000),
                                         payout("po_2", 1_700_200_000, "paid", 20_000)]
                            }),
                            Some("po_2") => json!({
                                "object": "list", "has_more": false,
                                "data": [payout("po_1", 1_700_100_000, "paid", 10_000)]
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
}

#[cfg(test)]
mod tests {
    use super::stub::{payout, stub_stripe};
    use super::*;

    #[test]
    fn only_a_paid_payout_is_a_reading_dated_by_its_arrival() {
        // 1_700_000_000 is 2023-11-14T22:13:20Z.
        let ok = observation(&payout("po_1", 1_700_000_000, "paid", 12_345)).unwrap();
        assert_eq!(ok.external_id, "po_1");
        assert_eq!(ok.observed_at.timestamp(), 1_700_000_000);
        assert_eq!(
            ok.payload,
            json!({
                "amount_cents": 12_345, "currency": "usd", "arrival_date": "2023-11-14",
                "payout_id": "po_1", "balance_transaction": "txn_po_1", "status": "paid",
            }),
            "the six facts, nothing else of the payout"
        );
        for status in ["pending", "in_transit", "failed", "canceled"] {
            assert!(
                observation(&payout("po_2", 1, status, 1)).is_none(),
                "{status}"
            );
        }
        assert!(
            observation(&json!({"status": "paid", "id": "po_3"})).is_none(),
            "no arrival date"
        );
    }

    #[test]
    fn the_packet_says_the_amount_the_currency_and_the_arrival_and_never_a_key() {
        let d = describe_payout(
            &observation(&payout("po_1", 1_700_000_000, "paid", 12_345))
                .unwrap()
                .payload,
        );
        assert_eq!(d.title, "Payout: 123.45 USD arriving 2023-11-14");
        assert_eq!(
            d.metadata,
            json!({
                "amount_cents": 12_345, "currency": "usd",
                "arrival_date": "2023-11-14", STRIPE_PAYOUT_ID: "po_1",
            })
        );
        let bare = describe_payout(&json!({}));
        assert_eq!(bare.title, "Payout: 0.00  arriving an unknown date");
        assert_eq!(bare.metadata[STRIPE_PAYOUT_ID], Json::Null);
    }

    #[tokio::test]
    async fn a_read_pages_paid_payouts_from_the_arrival_cursor_inclusive_with_the_key_as_bearer() {
        let (base, seen) = stub_stripe(200, None).await;
        let src = StripePayouts::new(base);
        let since = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let obs = src.read("rk_test_good", Some(since)).await.unwrap();
        assert_eq!(
            obs.iter()
                .map(|o| o.external_id.as_str())
                .collect::<Vec<_>>(),
            ["po_3", "po_2", "po_1"],
            "both pages, newest first as listed"
        );
        assert_eq!(obs[0].observed_at.timestamp(), 1_700_300_000);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "Bearer rk_test_good");
        assert_eq!(seen[0].1["status"], "paid");
        assert_eq!(seen[0].1["limit"], "100");
        assert_eq!(seen[0].1["arrival_date[gte]"], "1700000000");
        assert!(!seen[0].1.contains_key("created[gte]"));
        assert!(!seen[0].1.contains_key("starting_after"));
        assert_eq!(
            seen[1].1["starting_after"], "po_2",
            "the raw page's last id"
        );
    }

    #[tokio::test]
    async fn no_cursor_reads_from_the_beginning_and_an_unpaid_payout_listed_is_not_a_reading() {
        let (base, seen) = stub_stripe(
            200,
            Some(vec![
                payout("po_9", 1_700_900_000, "in_transit", 90_000),
                payout("po_8", 1_700_800_000, "paid", 80_000),
            ]),
        )
        .await;
        let obs = StripePayouts::new(base)
            .read("rk_test_good", None)
            .await
            .unwrap();
        assert_eq!(
            obs.iter()
                .map(|o| o.external_id.as_str())
                .collect::<Vec<_>>(),
            ["po_8"]
        );
        assert!(!seen.lock().unwrap()[0].1.contains_key("arrival_date[gte]"));
    }

    #[tokio::test]
    async fn a_401_is_unreadable_naming_the_scope_and_no_key() {
        let (base, _) = stub_stripe(200, None).await;
        let err = StripePayouts::new(base)
            .read("rk_test_wrong", None)
            .await
            .unwrap_err();
        match err {
            SourceError::Unreadable(why) => {
                assert!(why.contains("401"), "{why}");
                assert!(why.contains("payouts: read"), "{why}");
                assert!(!why.contains("rk_test_wrong"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        let (base, _) = stub_stripe(403, None).await;
        assert!(matches!(
            StripePayouts::new(base).read("k", None).await,
            Err(SourceError::Unreadable(_))
        ));
    }

    #[tokio::test]
    async fn a_5xx_and_an_unreachable_host_are_transient() {
        let (base, _) = stub_stripe(503, None).await;
        assert!(matches!(
            StripePayouts::new(base).read("k", None).await,
            Err(SourceError::Transient(_))
        ));
        assert!(matches!(
            StripePayouts::new("http://127.0.0.1:9")
                .read("k", None)
                .await,
            Err(SourceError::Transient(_))
        ));
    }
}
