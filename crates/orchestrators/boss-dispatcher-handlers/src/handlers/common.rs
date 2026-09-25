//! Common helpers shared across step-completion handlers.
//!
//! All step-completion handlers follow the same shape: read the
//! triggering `step.done.<kind>` event payload, extract step
//! metadata + subject + day, build an HTTP body, POST it. These
//! helpers cut the boilerplate to ~5 lines per handler.

use boss_dispatcher::rules::handler::HandlerError;
use boss_jobs::channels::{InputChannel, RECORDED_KEY};
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
    /// The parent job's owner, when the event names one — `step.done`
    /// carries it (`job_owner_id`) so a wait-is-over signal reaches the
    /// person waiting on the packet when the step names no role
    /// (backlog 58f0b536). `""` and absent both read as `None`.
    pub job_owner_id: Option<&'a str>,
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
        let job_owner_id = obj
            .get("job_owner_id")
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
            job_owner_id,
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

/// Stamp the INPUT LANE onto a machine filer's packet metadata
/// (backlog b2d9b432).
///
/// Every backlog-item this crate files has a perfectly known origin —
/// an estate alarm is telemetry, a red chore line is a pipeline
/// failure, a failed ops verb is a pipeline failure — better known than
/// any human filer knows theirs. None of them recorded it, so from
/// `FIELD_LIVE_AT` (2026-09-21T00:00:00Z) every one would have read as
/// "the filer did not say", and the filed-without-a-lane count would
/// have measured the machines instead of the operators. A count that
/// mixes "somebody forgot" with "the machines do not participate" is
/// worse than no count.
///
/// The lane is a REQUIRED ARGUMENT, not an option with a default: a
/// filer that cannot say which lane it is has no business filing, and a
/// default would quietly re-create the silence this fixes. The key and
/// the labels come from `boss_jobs::channels`, the one definition the
/// CLI door reads too, so this cannot drift from what `boss job file
/// --channel` accepts.
pub(crate) fn with_lane(metadata: Value, lane: InputChannel) -> Value {
    let mut metadata = match metadata {
        Value::Object(m) => m,
        // A filer that handed us something that is not an object had no
        // metadata to keep; the lane is still worth recording.
        _ => serde_json::Map::new(),
    };
    metadata.insert(RECORDED_KEY.into(), Value::String(lane.label().into()));
    Value::Object(metadata)
}

/// The lane's recorded label, for a filer whose metadata is an inline
/// `json!` object rather than a value to wrap. Same one definition as
/// [`with_lane`]; see it for why the lane is never defaulted.
pub(crate) fn lane_label(lane: InputChannel) -> &'static str {
    lane.label()
}

/// The rows of a listing, or a refusal naming the reading — the ONE
/// place this handler crate decides what a missing `data` array means.
///
/// WHY THIS EXISTS (backlog 833e2d0a). The same chain —
/// `.get("data") … .unwrap_or_default()` — turns an ERROR SHAPE into an
/// empty list, so a dark, narrowed or restarted far side reads as
/// "nothing to do" and the pass reports healthy. It has been cured in
/// three handlers ONE AT A TIME, each found by accident by someone
/// working on something else: `retro.open`'s departments (80a77466),
/// `sensor.poll`'s sensors (6c4c432a), and its owed readings
/// (0767c830). A grep sweep is not the cure and that is measured: in
/// `sensor_poll.rs` alone the chain appears four times, of which one
/// was the bug, one is safe by accident of its own truncation check
/// and one is a different shape. So the JUDGEMENT moves here instead,
/// and a reading that takes it cannot drift from the other readings.
///
/// `.filter(is_array)` rather than `as_array().to_vec()` is the
/// load-bearing detail: `"data": null` and `"data": {}` refuse too,
/// rather than being read as zero rows.
///
/// An EMPTY array is honest and stays honest — a registry may
/// legitimately hold nothing, so this puts no floor on the count. The
/// refusal is a `String` so the caller decides its class; at every
/// current call site that is `HandlerError::Downstream`, because a
/// listing with no `data` is a bad answer from the far side rather
/// than a deterministic fault in the request, and a redelivery can
/// succeed once the service or the policy scope is right.
pub(crate) fn rows_or_refuse<T: serde::de::DeserializeOwned>(
    listing: &Value,
    what: &str,
) -> Result<Vec<T>, String> {
    let rows = listing
        .get("data")
        .filter(|d| d.is_array())
        .ok_or_else(|| format!("{what} answered no `data` array"))?;
    serde_json::from_value(rows.clone()).map_err(|e| format!("{what}: rows not in shape: {e}"))
}

/// The one row a single-row read answered, or a refusal naming the
/// reading and what the body carried instead — the single-row
/// neighbour of [`rows_or_refuse`], and the one place this crate
/// decides what such a body means.
///
/// WHY THIS EXISTS (backlog f2eac973). Seven reads spelled
/// `.get("data").cloned().unwrap_or(job)`: unwrap an envelope, else take
/// the body. Measured against the servers, no envelope is ever sent —
/// `GET /api/jobs/{id}` answers a flattened `JobDetail` and `GET
/// /api/credentials/{id}` a bare `CredentialRow` — so the hedge never
/// fired and its fallback was the only live path: ANY 200 body was the
/// row. `maintenance.sweep.inspect` and `dns.observe` then failed a kind
/// check on it and returned `Ok(())`, a ready step skipped without a
/// word. The helper therefore does not unwrap an envelope either (that
/// would be the same guess, kept); it reads the row bare and requires
/// the one field every such row carries, a non-empty string `id`. An
/// envelope, an error body, `null` or a list all refuse, and the
/// refusal lists the body's top-level keys so the next reader does not
/// have to re-fetch it to learn what came back.
///
/// The refusal is a `String` for the same reason as `rows_or_refuse`:
/// the caller picks the class, and at every current site that is
/// `HandlerError::Downstream` — a bad answer from the far side, which a
/// redelivery can outlive.
pub(crate) fn row_or_refuse(body: Value, what: &str) -> Result<Value, String> {
    if body
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return Ok(body);
    }
    let carried = match &body {
        Value::Object(m) => format!(
            "keys [{}]",
            m.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
        ),
        Value::Array(_) => "a list".to_string(),
        Value::Null => "null".to_string(),
        _ => "a scalar".to_string(),
    };
    Err(format!(
        "{what} answered no row (no string `id`; the body carried {carried})"
    ))
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
    let kind_q = match kind {
        Some(k) => format!("kind={k}&"),
        None => String::new(),
    };
    jobs_where(
        client,
        jobs_base,
        &format!("{kind_q}status=open"),
        rule_name,
    )
    .await
}

/// The same paged walk over any `/api/jobs` filter — `filter` is the
/// query string without `limit`/`offset`. The open board is one filter
/// of it; the owed-proof obligation reads CLOSED cars by the key they
/// carry (`kind=ship-a-change&metadata_has=proof_owed`, b9005734), which
/// no open-only walk can reach.
pub(crate) async fn jobs_where(
    client: &reqwest::Client,
    jobs_base: &str,
    filter: &str,
    rule_name: &str,
) -> Result<Vec<Value>, HandlerError> {
    const PAGE: usize = 500;
    let mut rows: Vec<Value> = Vec::new();
    loop {
        let body = get_json(
            client,
            &format!(
                "{}/api/jobs?{filter}&limit={PAGE}&offset={}",
                jobs_base.trim_end_matches('/'),
                rows.len()
            ),
            rule_name,
        )
        .await?;
        let total = body.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
        // A page with no `data` array is no answer, and reading it as
        // zero rows broke the loop below on `got == 0` and returned an
        // EMPTY BOARD to every rule that walks it (backlog 833e2d0a).
        let page: Vec<Value> =
            rows_or_refuse(&body, "GET /api/jobs").map_err(HandlerError::Downstream)?;
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

/// The writes that complete step `sid` on packet `jid` carrying
/// `fields`, in order: the fields through the step's MERGE door, then
/// the status alone through the step PUT. Paths only — the caller
/// prefixes its base. Empty `fields` is the flip alone. Pure, so the
/// shape is pinned without a socket.
///
/// WHY TWO WRITES AND NOT THE ONE PUT EVERY HANDLER HERE USED TO SEND
/// (backlog e39a9d2a, stage 2 of design 93d2bddb). A handler read the
/// step, merged its own keys into what it read, and PUT `{status,
/// metadata}` back. The step PUT replaces metadata wholesale, so that
/// read-merge-write is correct only while nothing writes the step
/// between the handler's read and its PUT: stage 1 refuses such a PUT
/// 409 when it omits a key someone added meanwhile, and David's decided
/// end state refuses ANY metadata body on the PUT — the tighten is one
/// block in `update_step`, waiting on writers like these. The merge
/// door lands the keys against the row as it stands, so it can neither
/// race nor shed a key it does not name, and a handler no longer needs
/// the step's metadata at all to write its own. It goes FIRST because
/// required-at-done fields are judged when the step flips; if the flip
/// is then refused the step stays open carrying the fields, and a
/// redelivery merges the same keys again — the outcome a refused PUT
/// left. The same shape as `boss_jobs::car::StepWrite` (the auto-park
/// path) and boss-cli's `train::step_completion_writes`.
pub(crate) fn step_completion_writes(
    jid: &str,
    sid: &str,
    fields: serde_json::Map<String, Value>,
) -> Vec<(reqwest::Method, String, Value)> {
    let path = format!("/api/jobs/{jid}/steps/{sid}");
    let merge = (!fields.is_empty()).then(|| {
        (
            reqwest::Method::PATCH,
            format!("{path}/metadata"),
            Value::Object(fields),
        )
    });
    merge
        .into_iter()
        .chain(std::iter::once((
            reqwest::Method::PUT,
            path,
            serde_json::json!({ "status": "completed" }),
        )))
        .collect()
}

/// Complete step `sid` on packet `jid` with `fields`: the
/// [`step_completion_writes`], each through [`write_json`] against
/// `base`, stopping at the first refusal.
pub(crate) async fn complete_step(
    client: &reqwest::Client,
    base: &str,
    jid: &str,
    sid: &str,
    fields: serde_json::Map<String, Value>,
    rule_name: &str,
) -> Result<(), HandlerError> {
    for (method, path, body) in step_completion_writes(jid, sid, fields) {
        write_json(client, method, &format!("{base}{path}"), &body, rule_name).await?;
    }
    Ok(())
}

/// The step a machine completes to close a `backlog-item` alarm it
/// raised. The kind routes on `triage.disposition`, and `stale` is the
/// terminal whose title is literally "Closed — the claim no longer
/// holds" (infra/platform/workflows/backlog-item.toml).
pub(crate) const TRIAGE_SLUG: &str = "triage";

/// The id of the `triage` step of one alarm packet.
///
/// Lived in `cadence_silence` until `estate.recover` closed alarms the
/// same way (backlog ef421cd3) — one definition of "the step that
/// closes an alarm", not a second copy (CLAUDE.md §9a). It used to hand
/// back the step's metadata too, for a completion PUT that replaced it
/// wholesale; a completion now merges its own keys through the step
/// merge door ([`complete_step`], e39a9d2a), so nothing needs it.
pub(crate) fn triage_step(job: &Value) -> Option<String> {
    job.get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(TRIAGE_SLUG))
        .and_then(|s| s.get("id").and_then(Value::as_str).map(str::to_string))
}

/// The routed steps a machine may complete to withdraw an alarm once a
/// person has triaged it — the two whose fields carry a `stale`
/// disposition (infra/platform/workflows/backlog-item.toml: `measure`
/// since 2802ba8c, `build` since 6c114a23). The design route has none:
/// `draft-design` needs a design id and `design-review` is a decision.
const WITHDRAWING_SLUGS: [&str; 2] = ["measure", "build"];

/// How a machine withdraws an alarm it raised once the condition has
/// cleared (backlog a2d8bad3).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Retraction {
    /// Complete this step with `disposition = stale`, through
    /// [`complete_step`] — the step's own keys are not carried, because
    /// the merge door keeps every key it is not sent (e39a9d2a).
    Complete { slug: String, step_id: String },
    /// No step the machine may complete: say on the PACKET that the
    /// condition cleared, and why it is still open.
    Annotate { why_open: String },
}

/// THE step a recovered alarm withdraws at — one definition for every
/// machine that closes its own alarm (CLAUDE.md §9a).
///
/// WHY IT IS NOT ALWAYS `triage` (backlog a2d8bad3). Both closers
/// completed the triage step, which is right only while nobody has
/// routed the alarm. Once a person triaged it, that step was already
/// complete, the jobs API refused the PUT as a write to a terminal
/// step, and the machine had no other exit: alarm a6a4ae18 (CADENCE
/// SILENT: ops-request/github-mirror), routed to `build` on 2026-09-20,
/// had its cadence back on 2026-09-21 and sat open with `build` ready,
/// its measurement frozen at the raise, until a person closed it on
/// 2026-09-23.
///
/// So the machine withdraws at the step the packet is WAITING ON:
/// - `triage`, while it is open — unchanged;
/// - a READY `measure` or `build`, the step the route opened, which
///   carries `stale` for exactly this — the claim no longer holds;
/// - otherwise it annotates. An ACTIVE step has an executor on it, and
///   completing a dispatched run's step fires
///   `agent-run-delivers-when-its-step-is-done`, which lands the run as
///   `delivered` for work it did not deliver; the design route has no
///   withdrawal at all. The executor, or the person deciding, reads the
///   note and closes it — a machine alarm a human has touched can still
///   tell the human it is over.
///
/// A human's route is not overridden by this: the `stale` completion is
/// stamped `cleared_by`, which both raisers' settled-suppression reads
/// as a machine clear, so a condition that returns re-raises instead of
/// hiding behind the close — and a HUMAN's close still holds for its
/// settle window, exactly as before.
///
/// `None` is a packet with no triage step, which is not an alarm this
/// crate can judge.
pub(crate) fn retraction(job: &Value) -> Option<Retraction> {
    let steps: Vec<&Value> = job
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let slug_of = |s: &Value| {
        s.get("spec_slug")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let status_of = |s: &Value| s.get("status").and_then(Value::as_str).map(str::to_string);
    let triage_id = triage_step(job)?;
    let triage = steps
        .iter()
        .find(|s| slug_of(s).as_deref() == Some(TRIAGE_SLUG))?;
    // A triage with no status is read as open: the listing always
    // carries one, and the older fixtures do not.
    if !matches!(
        status_of(triage).as_deref(),
        Some("completed") | Some("skipped")
    ) {
        return Some(Retraction::Complete {
            slug: TRIAGE_SLUG.to_string(),
            step_id: triage_id,
        });
    }
    let routed = triage
        .pointer("/metadata/disposition")
        .and_then(Value::as_str)
        .unwrap_or("an unrecorded route");
    for slug in WITHDRAWING_SLUGS {
        let Some(step) = steps.iter().find(|s| slug_of(s).as_deref() == Some(slug)) else {
            continue;
        };
        match status_of(step).as_deref() {
            Some("ready") => {
                return Some(Retraction::Complete {
                    slug: slug.to_string(),
                    step_id: step.get("id").and_then(Value::as_str)?.to_string(),
                });
            }
            Some("active") => {
                return Some(Retraction::Annotate {
                    why_open: format!(
                        "triage routed it to `{routed}` and `{slug}` is active — an executor \
                         holds it, and the machine does not complete a step from under its \
                         executor"
                    ),
                });
            }
            _ => {}
        }
    }
    Some(Retraction::Annotate {
        why_open: format!(
            "triage routed it to `{routed}`, and no step that route has open carries a \
             withdrawal the machine may complete"
        ),
    })
}

/// The fields that withdraw a recovered alarm at the step
/// [`retraction`] names, for a machine that judged the condition from a
/// reading: `stale` — the backlog-item terminal "the claim no longer
/// holds" — with the evidence, who cleared it and when. ONLY these: they
/// ride the step merge door ([`complete_step`]), which keeps every key
/// the step holds (e39a9d2a).
///
/// One definition, since the queue alarm, the flight overdue and the
/// agent-step overdue each carried the same body a copy apiece, each
/// with the step's own metadata cloned in for a PUT that replaced it.
pub(crate) fn withdrawal_fields(
    evidence: &str,
    cleared_by: &str,
    at: &str,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("disposition".into(), Value::String("stale".into()));
    m.insert(
        "evidence".into(),
        Value::String(format!(
            "{evidence} Closed by machine from the reading, not by judgement."
        )),
    );
    m.insert("cleared_by".into(), Value::String(cleared_by.into()));
    m.insert(RECOVERED_AT.into(), Value::String(at.into()));
    m
}

/// The packet-metadata key a recovery is stamped under — the same key
/// `estate.recover` has written onto the packets it closes since
/// ef421cd3, so a reader asks one question of every alarm.
pub(crate) const RECOVERED_AT: &str = "recovered_at";

/// The merge that tells a person the condition is over when the machine
/// may not close the packet itself ([`Retraction::Annotate`]).
pub(crate) fn recovery_note(
    evidence: &str,
    cleared_by: &str,
    recovered_at: &str,
    why_open: &str,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert(RECOVERED_AT.into(), Value::String(recovered_at.into()));
    m.insert("recovered_by".into(), Value::String(cleared_by.into()));
    m.insert(
        "recovery".into(),
        Value::String(format!(
            "RECOVERED — {evidence} Still open because {why_open}. The condition this alarm \
             was raised for no longer holds; close it as `stale` unless the route it took is \
             still wanted without it (backlog a2d8bad3)."
        )),
    );
    m
}

/// The merge that withdraws a [`recovery_note`] when the condition comes
/// back: `null` deletes a key on `PATCH /api/jobs/{id}/metadata`, and a
/// packet whose condition returned must not go on saying RECOVERED.
pub(crate) fn relapse_patch() -> Value {
    serde_json::json!({RECOVERED_AT: null, "recovered_by": null, "recovery": null})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Backlog 833e2d0a — THE JUDGEMENT LIVES ONCE. A listing with no
    /// `data` array is no answer: an error shape, a changed contract, a
    /// policy-narrowed read, a far side that restarted. Reading it as
    /// zero rows makes a handler do nothing and report the pass
    /// healthy. Three handlers were cured of exactly this one at a
    /// time (80a77466 departments, 6c4c432a sensors, 0767c830
    /// readings) and each was found by accident. An EMPTY array is
    /// honest — a registry may legitimately hold nothing — so no floor
    /// is put on the count.
    #[test]
    fn a_listing_with_no_data_array_refuses_and_an_empty_one_is_honest() {
        let rows: Vec<Value> =
            rows_or_refuse(&json!({ "data": [], "total": 0 }), "GET /api/things")
                .expect("an empty array is honest");
        assert!(rows.is_empty());

        // `null` and an object refuse too: `as_array` alone reads both
        // as absent, and an absence that defaults is the whole disease.
        for dark in [
            json!({ "total": 0 }),
            json!({ "error": "forbidden" }),
            json!({ "data": null }),
            json!({ "data": {} }),
        ] {
            let why = rows_or_refuse::<Value>(&dark, "GET /api/things")
                .expect_err("no `data` array is a refusal");
            assert!(why.contains("no `data` array"), "{why}");
            assert!(why.contains("GET /api/things"), "{why}");
        }
    }

    /// A row out of the caller's shape is a refusal carrying the serde
    /// error and the reading's name — never a shorter list.
    #[test]
    fn a_row_out_of_shape_refuses_and_names_the_reading() {
        let ok: Vec<u64> =
            rows_or_refuse(&json!({ "data": [1, 2] }), "GET /api/things").expect("rows");
        assert_eq!(ok, vec![1, 2]);
        let why = rows_or_refuse::<u64>(&json!({ "data": ["x"] }), "GET /api/things")
            .expect_err("a row out of shape refuses");
        assert!(why.contains("GET /api/things"), "{why}");
        assert!(why.contains("not in shape"), "{why}");
    }

    /// Backlog f2eac973 — the single-row neighbour of 833e2d0a. Every
    /// single-row door these handlers read (`GET /api/jobs/{id}`, a
    /// flattened `JobDetail`; `GET /api/credentials/{id}`, a bare
    /// `CredentialRow`) answers the row BARE, so the old
    /// `.get("data").cloned().unwrap_or(job)` hedged for an envelope no
    /// server sends and its fallback was the only live path: any 200
    /// body was read as the row, and a kind check then skipped it
    /// silently. A row is what carries its `id`; everything else —
    /// an error body, an envelope, null, a list — refuses and says
    /// what it did carry.
    #[test]
    fn a_single_row_read_with_no_id_refuses_and_a_bare_row_is_the_answer() {
        let job = json!({ "id": "j-1", "kind": "maintenance-sweep", "steps": [] });
        assert_eq!(
            row_or_refuse(job.clone(), "GET /api/jobs/j-1").expect("a bare row is the answer"),
            job,
            "the row comes back whole"
        );
        for dark in [
            json!({ "error": "forbidden" }),
            json!({ "data": { "id": "j-1" } }),
            json!({ "id": "" }),
            json!({ "id": 7 }),
            json!(null),
            json!([{ "id": "j-1" }]),
        ] {
            let why = row_or_refuse(dark.clone(), "GET /api/jobs/j-1")
                .expect_err("a body with no row id is a refusal");
            assert!(why.contains("GET /api/jobs/j-1"), "{why}");
            assert!(why.contains("no row"), "{why}");
        }
        // The refusal names what the body DID carry, so the next
        // reader does not re-derive it (CLAUDE.md §Diagnosis).
        let why = row_or_refuse(json!({ "error": "forbidden" }), "GET /api/jobs/j-1")
            .expect_err("refuses");
        assert!(why.contains("error"), "{why}");
    }

    /// The judgement AT THE CONSUMING LAYER. `open_jobs` is the board
    /// walk almost every packet-reading rule goes through, and it held
    /// the same chain: a page with no `data` array left `got == 0`,
    /// which broke the paging loop and returned an EMPTY BOARD. Every
    /// caller then found nothing to do and said so calmly — the
    /// widest-blast-radius instance of 833e2d0a, and the fourth.
    async fn board_from(page: Value) -> Result<Vec<Value>, HandlerError> {
        use axum::{Json as AxJson, Router, routing::get};
        let app = Router::new().route(
            "/api/jobs",
            get(move || {
                let page = page.clone();
                async move { AxJson(page) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        open_jobs(
            &api_client(),
            &format!("http://{addr}"),
            Some("backlog-item"),
            "a-rule",
        )
        .await
    }

    #[tokio::test]
    async fn a_board_page_with_no_data_array_refuses_rather_than_reading_empty() {
        let board = board_from(json!({ "data": [], "total": 0 }))
            .await
            .expect("an empty board is honest");
        assert!(board.is_empty());

        let why = board_from(json!({ "total": 3 }))
            .await
            .expect_err("a page with no `data` array is a refusal");
        assert!(matches!(why, HandlerError::Downstream(_)), "{why:?}");
        assert!(format!("{why:?}").contains("no `data` array"), "{why:?}");
    }

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

    /// A step as the jobs API lists it back, for the retraction tests.
    fn step(slug: &str, status: &str, metadata: Value) -> Value {
        json!({"id": format!("s-{slug}"), "spec_slug": slug, "status": status, "metadata": metadata})
    }

    fn alarm(steps: Vec<Value>) -> Value {
        json!({"id": "alarm-1", "status": "open", "steps": steps})
    }

    /// Nobody has routed it: the machine withdraws it where it has
    /// always withdrawn it, at `triage`. A fixture with no `status` at
    /// all is read as open — the shape every pre-existing test uses.
    #[test]
    fn an_untriaged_alarm_is_withdrawn_at_its_triage_step() {
        for triage in [
            step(
                "triage",
                "ready",
                json!({"authority_role": "platform-admin"}),
            ),
            step(
                "triage",
                "active",
                json!({"authority_role": "platform-admin"}),
            ),
            json!({"id": "s-triage", "spec_slug": "triage", "metadata": {"authority_role": "platform-admin"}}),
        ] {
            let job = alarm(vec![step("filed", "completed", json!({})), triage]);
            match retraction(&job) {
                Some(Retraction::Complete { slug, step_id }) => {
                    assert_eq!(slug, TRIAGE_SLUG);
                    assert_eq!(step_id, "s-triage");
                }
                other => panic!("an untriaged alarm completes triage, got {other:?}"),
            }
        }
    }

    /// Backlog a2d8bad3 — THE WORKED EXAMPLE, a6a4ae18. The alarm was
    /// triaged to `build` on 2026-09-20 and its cadence came back on
    /// 2026-09-21; the sweep's only exit was the triage step, already
    /// complete, so the jobs API refused the PUT and the alarm sat open
    /// with `build` ready until a person closed it on 2026-09-23. The
    /// step the packet is WAITING ON is `build`, and `build` carries a
    /// `stale` disposition for exactly this — the claim no longer holds.
    #[test]
    fn an_alarm_routed_to_build_is_withdrawn_at_its_ready_build_step() {
        let job = alarm(vec![
            step("filed", "completed", json!({})),
            step(
                "triage",
                "completed",
                json!({"disposition": "build", "evidence": "routed by a human"}),
            ),
            step("measure", "skipped", json!({})),
            step("draft-design", "skipped", json!({})),
            step("design-review", "skipped", json!({})),
            step(
                "build",
                "ready",
                json!({"authority_role": "platform-admin", "agent_profile": "builder"}),
            ),
        ]);
        match retraction(&job) {
            Some(Retraction::Complete { slug, step_id }) => {
                assert_eq!(slug, "build");
                assert_eq!(step_id, "s-build");
            }
            other => panic!("a ready build is where the alarm withdraws, got {other:?}"),
        }
    }

    /// The verify route withdraws at `measure`, which carries the same
    /// `stale` disposition (2802ba8c).
    #[test]
    fn an_alarm_routed_to_verify_is_withdrawn_at_its_ready_measure_step() {
        let job = alarm(vec![
            step("triage", "completed", json!({"disposition": "verify"})),
            step("measure", "ready", json!({})),
            step("build", "skipped", json!({})),
        ]);
        assert!(matches!(
            retraction(&job),
            Some(Retraction::Complete { slug, .. }) if slug == "measure"
        ));
    }

    /// An executor HOLDS an active build: completing it from under a
    /// dispatched run fires `agent-run-delivers-when-its-step-is-done`
    /// and lands the run as `delivered` for work it did not deliver. So
    /// the machine does not complete it — it says on the packet that
    /// the condition cleared, and the executor's verify-the-claim reads
    /// that.
    #[test]
    fn a_build_an_executor_holds_is_annotated_not_completed() {
        let job = alarm(vec![
            step("triage", "completed", json!({"disposition": "build"})),
            step("build", "active", json!({})),
        ]);
        match retraction(&job) {
            Some(Retraction::Annotate { why_open }) => {
                assert!(why_open.contains("`build`"), "{why_open}");
                assert!(why_open.contains("active"), "{why_open}");
            }
            other => panic!("a held build is annotated, got {other:?}"),
        }
    }

    /// The design route has no withdrawal on it: `draft-design` needs a
    /// design id and `design-review` is a decision a person owns. The
    /// machine completes neither, and tells them instead.
    #[test]
    fn a_design_route_is_annotated_because_no_step_on_it_can_withdraw() {
        let job = alarm(vec![
            step("triage", "completed", json!({"disposition": "design"})),
            step("draft-design", "completed", json!({"design_id": "d-1"})),
            step("design-review", "ready", json!({})),
            step("build", "pending", json!({})),
        ]);
        match retraction(&job) {
            Some(Retraction::Annotate { why_open }) => {
                assert!(why_open.contains("design"), "{why_open}");
            }
            other => panic!("a design route is annotated, got {other:?}"),
        }
    }

    /// No triage step is not an alarm this crate can judge.
    #[test]
    fn a_packet_with_no_triage_step_has_no_retraction() {
        assert_eq!(
            retraction(&alarm(vec![step("filed", "completed", json!({}))])),
            None
        );
    }

    /// The note says it is over and why the packet is still open, and
    /// the relapse patch deletes exactly the keys the note wrote — a
    /// packet whose condition came back must not go on saying
    /// RECOVERED.
    #[test]
    fn the_recovery_note_and_its_relapse_patch_name_the_same_keys() {
        let note = recovery_note(
            "the cadence is arriving again",
            "cadence.silence.sweep",
            "2026-09-21T00:00:00+00:00",
            "triage routed it to `design`",
        );
        assert_eq!(note[RECOVERED_AT], "2026-09-21T00:00:00+00:00");
        assert_eq!(note["recovered_by"], "cadence.silence.sweep");
        let text = note["recovery"].as_str().expect("a sentence");
        assert!(text.contains("the cadence is arriving again"), "{text}");
        assert!(text.contains("triage routed it to `design`"), "{text}");
        let relapse = relapse_patch();
        let relapse = relapse.as_object().expect("an object");
        assert_eq!(
            relapse.keys().collect::<Vec<_>>(),
            note.keys().collect::<Vec<_>>()
        );
        assert!(
            relapse.values().all(Value::is_null),
            "null deletes on PATCH"
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

#[cfg(test)]
mod lane_pin {
    use super::*;

    /// EVERY MACHINE-FILED BACKLOG-ITEM IN THIS CRATE STAMPS ITS LANE
    /// (backlog b2d9b432).
    ///
    /// Stamping the eight sites that existed was the easy half; the half
    /// that lasts is that the NINTH cannot be written without one. From
    /// `FIELD_LIVE_AT` a packet with no lane reads as "the filer did not
    /// say", and a machine filer is the one filer whose origin is never
    /// in doubt — so a silent one does not merely lose a field, it
    /// poisons the count that is supposed to measure operators.
    ///
    /// This reads the crate's own source rather than exercising each
    /// handler, because the defect is a MISSING line: no amount of
    /// running the eight that are right can show that a ninth is wrong.
    /// The `mod tests` truncation is what keeps fixtures out — a test
    /// packet standing in for what the jobs API lists back is not a
    /// filing, and there are five of those.
    #[test]
    fn every_backlog_item_this_crate_files_stamps_an_input_lane() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/handlers");
        let mut filings = 0;
        let mut silent: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(dir).expect("handlers dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let src = std::fs::read_to_string(&path).expect("read handler");
            // Fixtures live under `mod tests`; a filing never does.
            let production = match src.find("\nmod tests {") {
                Some(i) => &src[..i],
                None => &src[..],
            };
            let lines: Vec<&str> = production.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !line.contains(r#""kind": "backlog-item""#) {
                    continue;
                }
                filings += 1;
                let window = lines[i..lines.len().min(i + 30)].join("\n");
                if !window.contains("input_channel")
                    && !window.contains("with_lane")
                    && !window.contains("lane_label")
                {
                    silent.push(format!("{name}:{}", i + 1));
                }
            }
        }
        assert!(
            silent.is_empty(),
            "these machine filers file a backlog-item with NO input lane, so from \
             FIELD_LIVE_AT each reads as `the filer did not say` though its origin is \
             known exactly: {silent:?}. Stamp it — `super::common::with_lane(metadata, \
             InputChannel::<lane>)` when the metadata is a value, or \
             `\"input_channel\": super::common::lane_label(InputChannel::<lane>)` inside \
             an inline object."
        );
        // Nine since `ops.queue.alarm` (a45b38c1), which files
        // `ops_queue:<host>` stamped `Telemetry`; ten since
        // `jobs.agent_step_overdue` (078ddcb0), whose alarm per late
        // real-work step is stamped `Telemetry` too; eleven since
        // `jobs.flight_overdue` (73c31776), whose stale-flight alarm is
        // `Telemetry` for the same reason; twelve since the door alarm
        // (`estate.alarm`'s `door_dark`, e6406701), stamped `Telemetry`
        // like the estate alarms beside it.
        assert_eq!(
            filings, 12,
            "the number of machine filing sites changed. That is fine — but check the new \
             one stamps a lane, then update this count, which exists so a filing that \
             DISAPPEARS from the scan (a renamed key, a reshaped body) cannot read as \
             `all clear`."
        );
    }

    #[test]
    fn with_lane_records_the_label_and_keeps_what_was_there() {
        let out = with_lane(
            serde_json::json!({"area": "estate", "host": "w-1"}),
            InputChannel::Telemetry,
        );
        assert_eq!(out["input_channel"], "telemetry/monitoring");
        assert_eq!(out["area"], "estate", "existing keys survive");
        assert_eq!(out["host"], "w-1");
        // The key is the one the CLI reads, not a second spelling.
        assert_eq!(RECORDED_KEY, "input_channel");
        // A filer with nothing to keep still records the lane.
        let bare = with_lane(serde_json::Value::Null, InputChannel::PipelineFailure);
        assert_eq!(bare["input_channel"], "pipeline-failure");
    }

    /// Backlog e39a9d2a: a completion is the fields through the step
    /// merge door FIRST (required-at-done fields are judged at the
    /// flip), then a PUT whose body carries the status and nothing
    /// else — the one form that survives the step PUT refusing any
    /// metadata body.
    #[test]
    fn a_completion_merges_its_fields_then_flips_the_status_alone() {
        use serde_json::json;
        let mut fields = serde_json::Map::new();
        fields.insert("disposition".into(), json!("stale"));
        let writes = step_completion_writes("j1", "s1", fields);
        assert_eq!(
            writes,
            vec![
                (
                    reqwest::Method::PATCH,
                    "/api/jobs/j1/steps/s1/metadata".to_string(),
                    json!({ "disposition": "stale" }),
                ),
                (
                    reqwest::Method::PUT,
                    "/api/jobs/j1/steps/s1".to_string(),
                    json!({ "status": "completed" }),
                ),
            ]
        );
        assert_eq!(
            step_completion_writes("j1", "s1", serde_json::Map::new()),
            vec![(
                reqwest::Method::PUT,
                "/api/jobs/j1/steps/s1".to_string(),
                json!({ "status": "completed" }),
            )],
            "no fields is the flip alone — never an empty merge"
        );
    }
}
