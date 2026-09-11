//! Common helpers shared across step-completion handlers.
//!
//! All step-completion handlers follow the same shape: read the
//! triggering `step.done.<kind>` event payload, extract step
//! metadata + subject + day, build an HTTP body, POST it. These
//! helpers cut the boilerplate to ~5 lines per handler.

use boss_dispatcher::rules::handler::HandlerError;
use serde_json::Value;

/// Step-event payload fields the handlers commonly read.
///
/// The `step.done.<kind>` event published by jobs-api carries this
/// shape inside its `payload` envelope. The dispatcher unwraps the
/// envelope; handlers see this inner shape as `ctx.event_payload`.
#[derive(Debug)]
pub struct StepEvent<'a> {
    pub job_id: &'a str,
    pub step_id: &'a str,
    pub kind: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
    pub completed_on: Option<chrono::NaiveDate>,
    /// Who the step is assigned to, if anyone. A named assignee is a
    /// STRONGER routing signal than a role — a role says someone like
    /// you should do this, an assignee says you specifically.
    pub assignee_id: Option<&'a str>,
    pub metadata: &'a serde_json::Map<String, Value>,
}

impl<'a> StepEvent<'a> {
    /// Extract the canonical fields from a step.done payload.
    /// Returns a tightly-typed view that handlers consume; errors
    /// surface as HandlerError::Downstream with a clear shape-mismatch
    /// message for the operator.
    pub fn from_payload(payload: &'a Value) -> Result<Self, HandlerError> {
        let obj = payload
            .as_object()
            .ok_or_else(|| HandlerError::Downstream("step.done payload is not an object".into()))?;

        let job_id = obj
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing job_id".into()))?;
        let step_id = obj
            .get("step_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing step_id".into()))?;
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing kind".into()))?;
        let subject_kind = obj
            .get("subject_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subject_id = obj.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
        let assignee_id = obj
            .get("assignee_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let completed_on = obj
            .get("completed_on")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
        let metadata = obj
            .get("metadata")
            .and_then(|v| v.as_object())
            .ok_or_else(|| {
                HandlerError::Downstream("step.done payload missing metadata object".into())
            })?;

        Ok(StepEvent {
            job_id,
            step_id,
            kind,
            subject_kind,
            subject_id,
            completed_on,
            assignee_id,
            metadata,
        })
    }

    /// Convenience: pull a string field from step metadata, with a
    /// fallback closure for the common subject-derived defaults.
    pub fn meta_string_or<F: FnOnce(&Self) -> String>(&self, key: &str, fallback: F) -> String {
        self.metadata
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| fallback(self))
    }
}

/// Parse a `YYYY-MM-DD` string out of an optional JSON value, e.g. a
/// step-metadata field. Returns `None` when the value is absent, not a
/// string, or not a valid date — leaving the fallback to the caller.
pub(crate) fn parse_date(v: Option<&Value>) -> Option<chrono::NaiveDate> {
    v.and_then(|v| v.as_str())
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

/// The absorption fact's `source_id` for one driver:
/// `overhead-absorbed@{step_id}:{credit_account}`. Mirrors the id the
/// inventory absorption endpoint mints (`overhead_absorbed_handler`,
/// boss-inventory `http/items.rs`) — these two `format!`s are the
/// write/reconstruct halves of one contract; change them together.
/// The absorb side (`inventory.overhead.absorb`) posts one fact per
/// driver do; the drain side (`products.produce` drain-actual-wip)
/// reconstructs the same ids from its `overhead_accounts` rule arg —
/// both sets are rules data, and the brewery seed test asserts they
/// agree.
pub(crate) fn overhead_source_id(step_id: &str, credit_account: &str) -> String {
    format!("overhead-absorbed@{step_id}:{credit_account}")
}

/// The `x-sim-origin` value for a downstream call.
///
/// Reads the task-local the dispatch loop set from the TRIGGERING
/// event, so sim-ness is inherited rather than guessed. Downstream
/// services parse `"true"`/`"1"` as simulated and anything else as
/// real, so sending `"false"` explicitly is equivalent to omitting the
/// header — and saying it out loud is better than relying on absence,
/// because absence used to mean "ask the clock", which marked every
/// real user action on this deployment as simulated.
pub fn sim_origin_value() -> &'static str {
    if boss_core::sim_origin::is_in_sim_chain() {
        "true"
    } else {
        "false"
    }
}

/// The dispatcher's identity for a READ.
///
/// Writes stamp the specific rule (`dispatcher_actor_header`) because
/// the rule is provenance on the event that results. A read produces
/// no event, so the honest actor is the dispatcher itself — and a
/// read still has to present SOMEBODY, or it breaks the day the
/// service it calls grows a policy gate. That is not hypothetical:
/// one unstamped ledger read halted the whole WIP→FG→COGS chain when
/// `/api/ledger/*` became gated.
pub fn dispatcher_reader_header() -> String {
    serde_json::json!({
        "id": "automation:dispatcher",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// The client every handler's production constructor uses: a plain
/// reqwest client that carries the machine token as a default header
/// when the process has one configured (7fcd78fa phase 1). One
/// definition, so a new handler cannot forget the token by writing
/// `Client::new()` out of habit -- the gate's 401 names the header if
/// one does.
pub fn api_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    boss_core::machine_token::attach(&mut headers);
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .expect("reqwest client always builds")
}

/// Re-exported, not defined here: the rules runner in core writes as
/// this same actor when it lands a dead-letter on a packet
/// (`boss_dispatcher::rules::dead_letter`), and an identity that exists
/// twice drifts — a policy refusal that reads as missing data. One
/// definition, in `boss_dispatcher::rules::actor` (CLAUDE.md §9a); every
/// call site here is unchanged.
pub use boss_dispatcher::rules::actor::dispatcher_actor_header;

/// POST a JSON body to a downstream service, stamping the dispatcher's
/// rule-as-actor `x-boss-user` header, and map a non-2xx response into a
/// `HandlerError::Downstream`.
///
/// This is the shared epilogue every step-completion handler ends with:
/// build the POST, attach `content-type: application/json` +
/// `x-boss-user: dispatcher_actor_header(rule_name)`, send, and turn a
/// transport failure or non-success status into a `Downstream` error with
/// the URL/status/body baked into the message. Handlers whose epilogue
/// differs (a PUT, a response-body read, a lenient no-fail webhook, or an
/// omitted header) keep their inline call.
pub(crate) async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<(), HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // House contract: 422 = deterministic request-data error (a
        // seed typo, a malformed body) — identical on every redelivery,
        // so the runner Terms immediately instead of burning the NAK
        // budget. Convergent conflicts (409 insufficient-stock, 404
        // not-yet-projected) stay retryable.
        if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Err(HandlerError::Permanent(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        return Err(HandlerError::Downstream(format!(
            "POST {url} returned {status}: {body}"
        )));
    }
    Ok(())
}

/// GET a JSON document from a downstream service, stamping the same
/// rule-as-actor `x-boss-user` header as [`post_json`], mapping
/// transport failures and non-2xx responses into
/// `HandlerError::Downstream`.
pub(crate) async fn get_json(
    client: &reqwest::Client,
    url: &str,
    rule_name: &str,
) -> Result<Value, HandlerError> {
    let resp = client
        .get(url)
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // Same 422 contract as post_json.
        if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Err(HandlerError::Permanent(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        return Err(HandlerError::Downstream(format!(
            "GET {url} returned {status}: {body}"
        )));
    }
    resp.json()
        .await
        .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
}

/// PUT or PATCH a body, mapping non-2xx the same way [`post_json`]
/// does. Completing a step is a PUT and merging job metadata is a
/// PATCH on the metadata door; the shared POST helper covers neither.
///
/// Lived in `jobs_auto_park` until `cadence.silence.sweep` needed the
/// same two verbs — one definition rather than a second copy
/// (CLAUDE.md 9a: collapse it if you can).
pub(crate) async fn write_json(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<(), HandlerError> {
    let verb = method.to_string();
    let resp = client
        .request(method, url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("{verb} {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            HandlerError::Permanent(format!("{verb} {url} returned {status}: {text}"))
        } else {
            HandlerError::Downstream(format!("{verb} {url} returned {status}: {text}"))
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overhead_source_id_matches_endpoint_contract() {
        assert_eq!(
            overhead_source_id("step-1", "6100"),
            "overhead-absorbed@step-1:6100"
        );
    }
}
