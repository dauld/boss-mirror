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

/// THE FLOOR under every handler that fans out over a roster read at
/// fire time (backlog 86ebf7fc): a roster that answers ZERO rows is
/// refused, never reported as a quiet pass.
///
/// WHY IT IS A REFUSAL AND NOT A WARNING. A wrong, dark or
/// policy-narrowed read answers `total: 0` instead of erroring — the
/// most expensive recurring shape in this codebase (CLAUDE.md §Doors,
/// "a wrong target answers instead of erroring"). The layer below can
/// be made honest — `GET /api/departments` refuses rather than flatten
/// a registry error to an empty list (80a77466) — and that is only
/// half: the consumer still has to refuse the EMPTY it is honestly
/// handed, because the rosters this applies to are ones whose zero
/// means the system is broken, not that the week was quiet. A warning
/// is not a refusal: it fires into a log nobody is reading at 00:00 on
/// a Monday, and the cadence passes with no packet anyone acts on.
///
/// WHAT REFUSING BUYS. `Permanent` terminates the firing at once
/// rather than burning the redelivery budget on a roster that will
/// answer empty identically every time, and the dead-letter is counted
/// on the dispatcher's liveness surface, where the estate chain reads
/// unrecorded dead-letters and files a finding (8834804a) — a packet
/// someone acts on, which is exactly what the quiet pass never made.
///
/// `why_zero_is_broken` is the caller's sentence, because the floor is
/// generic and the reason never is.
pub(crate) fn empty_roster_refusal(
    handler: &str,
    roster: &str,
    why_zero_is_broken: &str,
) -> HandlerError {
    HandlerError::Permanent(format!(
        "{handler}: the {roster} roster is empty — {why_zero_is_broken}. Firing successfully \
         over zero rows is absence treated as an answer, and it hides the fault for a whole \
         cadence at a time, so this refuses instead: a fan-out over an empty roster is a check \
         that is not running"
    ))
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

/// The owner a handler files a packet with: the platform owner as the
/// port answers it, or NOBODY with the refusal in the journal under the
/// rule's name (backlog 3c23662d — every alarm handler wrote
/// `"emp-david"` until 2026-09-18). The filing goes ahead either way:
/// the jobs API resolves the kind's `owner_role` from the same registry
/// or refuses by name, and a handler that fell silent for want of an
/// owner would be the failure mode the alarms exist to end.
pub async fn owner_for_filing(
    port: &dyn boss_core::platform_owner::PlatformOwner,
    rule_name: &str,
) -> String {
    boss_core::platform_owner::owner_for_filing(port, |e| {
        tracing::warn!(
            rule = rule_name,
            "{e}; filing with no owner named — the jobs API resolves the kind's owner_role, or refuses"
        )
    })
    .await
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

/// POST a packet and read the id the jobs API minted for it — the one
/// thing [`post_json`] does not return, and the thing a judging
/// handler's note on the judged packet names. Same 422 contract.
///
/// Lived in `ops_judge` until `maintenance.chore.file_reds` needed the
/// same POST-and-read (ac3270c7) — one definition rather than a second
/// copy (CLAUDE.md §9a).
pub(crate) async fn post_json_minted_id(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<String, HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            HandlerError::Permanent(format!("POST {url} returned {status}: {text}"))
        } else {
            HandlerError::Downstream(format!("POST {url} returned {status}: {text}"))
        });
    }
    let created: Value = resp
        .json()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url} answer not JSON: {e}")))?;
    created
        .get("id")
        .or_else(|| created.get("data").and_then(|d| d.get("id")))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| HandlerError::Downstream(format!("POST {url} answered no id: {created}")))
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

/// Every open Job of `kind`, steps inline, paged on the list's `total`
/// so a packet sorted past one page is still found — a capped page is
/// a false negative that grows with the board's age.
///
/// Lived in `jobs_run_car_probes` until `jobs.complete_step_matching`
/// needed the same walk (c34583cb) — one definition rather than a
/// second copy (CLAUDE.md §9a).
pub(crate) async fn open_jobs_of_kind(
    client: &reqwest::Client,
    jobs_base: &str,
    kind: &str,
    rule_name: &str,
) -> Result<Vec<Value>, HandlerError> {
    open_jobs(client, jobs_base, Some(kind), rule_name).await
}

/// The same walk with the kind OPTIONAL: `None` is the whole open
/// board. A handler asking a question about claimed STEPS rather than
/// about packets of one kind needs it — `jobs.reclaim_abandoned_step`
/// looks for steps held by a dead executor run, and those sit on
/// packets of every kind an agent block appears on (a3397b01).
pub(crate) async fn open_jobs(
    client: &reqwest::Client,
    jobs_base: &str,
    kind: Option<&str>,
    rule_name: &str,
) -> Result<Vec<Value>, HandlerError> {
    const PAGE: usize = 500;
    let kind_q = match kind {
        Some(k) => format!("kind={k}&"),
        None => String::new(),
    };
    let mut rows: Vec<Value> = Vec::new();
    loop {
        let body = get_json(
            client,
            &format!(
                "{}/api/jobs?{kind_q}status=open&limit={PAGE}&offset={}",
                jobs_base.trim_end_matches('/'),
                rows.len()
            ),
            rule_name,
        )
        .await?;
        let total = body.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
        let page: Vec<Value> = body
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let got = page.len();
        rows.extend(page);
        if got == 0 || rows.len() >= total {
            break;
        }
    }
    Ok(rows)
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

/// The step a machine completes to close a `backlog-item` alarm it
/// raised. The kind routes on `triage.disposition`, and `stale` is the
/// terminal whose title is literally "Closed — the claim no longer
/// holds" (infra/platform/workflows/backlog-item.toml).
pub(crate) const TRIAGE_SLUG: &str = "triage";

/// The `triage` step of one alarm packet, as (id, existing metadata).
///
/// Lived in `cadence_silence` until `estate.recover` closed alarms the
/// same way (backlog ef421cd3) — one definition of "the step that
/// closes an alarm", not a second copy (CLAUDE.md §9a). The existing
/// metadata rides back because PUT on a step REPLACES top-level
/// metadata, and `authority_role` living there is what keeps the step
/// gated.
pub(crate) fn triage_step(job: &Value) -> Option<(String, serde_json::Map<String, Value>)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// THE FLOOR, stated once: a roster read that answers zero rows is
    /// refused, and the refusal names the handler, the roster and why
    /// zero means broken — the three things an operator reading a
    /// dead-letter has to be told.
    #[test]
    fn an_empty_roster_is_a_permanent_refusal_that_names_the_roster() {
        let e = empty_roster_refusal(
            "retro.open",
            "departments",
            "a company with no departments is broken in a way a retro cannot fix",
        );
        assert!(
            e.is_permanent(),
            "a roster that is empty is empty identically on every redelivery"
        );
        let text = e.to_string();
        assert!(text.contains("retro.open"), "{text}");
        assert!(text.contains("departments roster is empty"), "{text}");
        assert!(
            text.contains("broken in a way a retro cannot fix"),
            "the caller's reason rides the refusal: {text}"
        );
        assert!(
            text.contains("a check that is not running"),
            "the floor's own sentence, stated here and not per handler: {text}"
        );
    }

    #[test]
    fn overhead_source_id_matches_endpoint_contract() {
        assert_eq!(
            overhead_source_id("step-1", "6100"),
            "overhead-absorbed@step-1:6100"
        );
    }
}
