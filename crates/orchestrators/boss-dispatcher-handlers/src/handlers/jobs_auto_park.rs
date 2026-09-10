//! `jobs.auto-park` — file a car when a gate-run goes GREEN carrying a
//! park intent, so a gate-green branch never strands unparked.
//!
//! THE ACTOR HALF OF AUTO-PARK. `boss gate --park-*` stamps the park
//! prose onto the gate-run (`park_*` metadata; the input half). When the
//! gate goes green the `record-verdict` step completes as
//! `gate-verdict`, and a rule on `step.done.gate-verdict` fires this
//! handler, which reads that intent + the verbatim receipt and files the
//! ship-a-change car — exactly what a human does with `boss park`, at
//! computer speed and while the base is still current (a stranded green
//! decays: gated yesterday, unmergeable today — 2026-09-01).
//!
//! FILES THROUGH `boss_jobs::car`, the shared builder `boss park` uses,
//! so the receipt-copy contract cannot drift (CLAUDE.md §9a). The receipt
//! is copied VERBATIM — never rebuilt — which is the bug `boss park` was
//! created to kill.
//!
//! A NO-OP, NOT AN ERROR, when this is not an auto-park: the verdict is
//! not green, or no `--park-*` intent was stamped (a plain manual gate).
//! The rule's `when` filters most of those, but the handler re-checks so
//! it is correct on its own terms.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::car::{self, Receipt};

use super::common::{StepEvent, api_client, dispatcher_actor_header, get_json, write_json};

pub struct JobsAutoPark {
    client: reqwest::Client,
    jobs_base: String,
    /// A precise `now` for the car's step stamps — the gate step's
    /// `completed_at` is one end of the dock-queue-time measurement
    /// (`review − gate`), so it must be the real park instant, not a
    /// day-granular fallback. The dispatcher is not on the no-wallclock
    /// allowlist, so this comes from the clock service like every other
    /// record stamp.
    clock: Arc<dyn boss_clock_client::ClockClient>,
}

impl JobsAutoPark {
    pub fn new(jobs_base: impl Into<String>, clock_url: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            clock: Arc::new(boss_clock_client::ReqwestClockClient::new(clock_url)),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// BEST-EFFORT: state the linked item's ROUTE, because parking this
    /// car is what decided it.
    ///
    /// `--park-backlog-item <id>` says this car is that item's build.
    /// The item's `build` step only OPENS once its triage records
    /// `disposition = build`, so until now a car parked against an
    /// un-triaged item linked to a step nothing could advance: the
    /// change shipped, landed and was proven while the item still read
    /// as undecided work, and the arrival rule found nothing to
    /// complete (backlog ca76d8f9, measured twice in an hour on
    /// 2026-09-09). The DECISION is `car::triage_on_park`, shared with
    /// `boss park` so the two park paths cannot disagree about it, and
    /// idempotent — a triage a person already completed answers `None`.
    ///
    /// NOTHING HERE MAY FAIL THE PARK. By the time this runs the car is
    /// filed and its steps are complete; a routing write that cannot
    /// reach the system of record is worth a warning, not a redelivery
    /// that would re-do the whole park. CLAUDE.md records a fallible
    /// write inside a reconcile loop freezing ALL landings — so every
    /// failure here is logged and swallowed.
    async fn triage_linked_item(&self, item_id: &str, car_id: &str, branch: &str, rule: &str) {
        // A stored edge may be a >= 8-char prefix — seven cars have one
        // — and `GET /api/jobs/{prefix}` is a permanent 400. Same test
        // the arrival rule applies to the same edge, one definition
        // (CLAUDE.md §9a).
        if let Some(why) = super::jobs_complete_linked_step::unusable_link(item_id) {
            tracing::warn!(rule = %rule, car = %car_id,
                "park left the linked item un-routed — {why}");
            return;
        }
        let item = match get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), item_id),
            rule,
        )
        .await
        {
            Ok(item) => item,
            Err(e) => {
                tracing::warn!(rule = %rule, car = %car_id, item = %item_id,
                    "park could not read the linked item to route it: {e}");
                return;
            }
        };
        let Some(write) = car::triage_on_park(&item, car_id, branch) else {
            return;
        };
        let url = format!(
            "{}/api/jobs/{}/steps/{}",
            self.base(),
            item_id,
            write.step_id
        );
        match write_json(&self.client, reqwest::Method::PUT, &url, &write.body, rule).await {
            Ok(()) => tracing::info!(rule = %rule, car = %car_id, item = %item_id,
                "routed the linked item to `build`: the car that names it IS its build"),
            Err(e) => tracing::warn!(rule = %rule, car = %car_id, item = %item_id,
                "park could not route the linked item to `build`: {e}"),
        }
    }
}

/// The inputs a green-with-intent gate-run yields for filing its car.
#[derive(Debug, PartialEq, Eq)]
struct AutoParkInputs {
    branch: String,
    summary: String,
    excludes: String,
    test: String,
    verified: String,
    backlog_item: Option<String>,
    /// The item provenance that does NOT authorise a close: the item
    /// this car is one piece of (`car::PARTIAL_ITEM`), or the reason it
    /// names none (`car::NO_ITEM_REASON`). Copied onto the car verbatim
    /// and read by nothing downstream — which is the point, because the
    /// arrival rule follows `backlog_item` alone.
    item_provenance: serde_json::Map<String, Value>,
    delivery_channel: Option<String>,
    receipt: Receipt,
    /// The car's proof intent, copied VERBATIM from the gate-run's
    /// `park_probe` / `park_expect` / `park_proof_event` onto the car's
    /// `proof_*` keys (28ac45ab) — the same copy-don't-rebuild rule the
    /// receipt lives by. Empty for a car whose builder recorded none.
    proof: serde_json::Map<String, Value>,
}

/// PURE: read the auto-park inputs from a gate-run packet and its
/// verdict step's metadata. `None` = not an auto-park (verdict not green,
/// or no park intent stamped) — a no-op the caller returns `Ok(())` for.
/// Split out so the gating and extraction are unit-tested without HTTP.
fn auto_park_inputs(
    gate_run: &Value,
    verdict_meta: &serde_json::Map<String, Value>,
) -> Option<AutoParkInputs> {
    // Green only. A failed or lost gate does not park.
    if verdict_meta.get("verdict").and_then(Value::as_str) != Some("green") {
        return None;
    }
    let md = gate_run.get("metadata").and_then(Value::as_object)?;
    // No `park_summary` = a manual gate (no `--park-*` intent). Do not
    // auto-park; the branch is gated but its author did not ask for it.
    let summary = md.get("park_summary").and_then(Value::as_str)?.to_string();
    let branch = md.get("branch").and_then(Value::as_str)?.to_string();
    let field = |k: &str| {
        md.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    // The receipt rides on the verdict step, VERBATIM as a JSON string;
    // head and mode are read out of it for the car builder without
    // rebuilding the string.
    let raw = verdict_meta
        .get("receipt")
        .and_then(Value::as_str)?
        .to_string();
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let receipt = Receipt {
        raw,
        head: parsed
            .get("head")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        mode: parsed
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };
    Some(AutoParkInputs {
        branch,
        summary,
        excludes: field("park_excludes"),
        test: field("park_test"),
        verified: field("park_verified"),
        delivery_channel: md
            .get("delivery_channel")
            .and_then(Value::as_str)
            .map(str::to_string),
        backlog_item: md
            .get("park_backlog_item")
            .and_then(Value::as_str)
            .map(str::to_string),
        // The two answers that are NOT the closing edge, copied under
        // the core keys nothing downstream follows. Read here so the
        // gate's `park_partial_item` / `park_no_item` cannot silently go
        // missing between the gate-run and the car.
        item_provenance: car::item_provenance(
            md.get("park_partial_item").and_then(Value::as_str),
            md.get("park_no_item").and_then(Value::as_str),
        ),
        receipt,
        proof: car::proof_intent(
            md.get("park_probe").and_then(Value::as_str),
            md.get("park_expect").and_then(Value::as_str),
            md.get("park_proof_event").and_then(Value::as_str),
        ),
    })
}

/// PURE: the record written on the gate-run INSTEAD of a car, when a car
/// for this branch already carried it to main. `None` = nothing landed;
/// file as usual.
///
/// A LANDED BRANCH GETS NO TWIN. Measured 2026-09-08 (backlog 610537b2):
/// a re-gate of fix/boot-never-refuses-over-an-unviable-workflow — already
/// on main via train #259 — reused gate-run 57f49a80, which still carried
/// the park intent stamped before the landing. On green this handler
/// filed twin car dfb98d07 for content already merged; the conductor
/// boarded it as train #261, whose CI went red on an empty diff, and the
/// train was cancelled and the twin abandoned by hand. Stale intent on a
/// reused packet is real; the defence is here, at the one place a car is
/// filed. The dispatcher runs without git, so "landed" is read from the
/// system of record (`boss_jobs::car::landed_car_for`, the same test
/// `boss gate` runs), not from ancestry.
fn landed_skip(cars: &[Value], branch: &str) -> Option<Value> {
    let car = car::landed_car_for(cars, branch)?;
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let md = |k: &str| {
        car.pointer(&format!("/metadata/{k}"))
            .and_then(Value::as_str)
            .unwrap_or("?")
    };
    let train = md("train");
    Some(json!({
        "park_skipped": "landed",
        "park_skipped_note": format!(
            "no car filed: car {} already carried {branch} to main as {} (train {}) — \
             this green re-gated landed content, and a twin would board an empty diff \
             (backlog 610537b2)",
            &id[..8.min(id.len())],
            md("merge_ref"),
            &train[..8.min(train.len())],
        ),
    }))
}

/// PURE: the record written on the gate-run INSTEAD of a car, when this
/// branch's car is already ABOARD a train. `None` = nothing is riding;
/// carry on.
///
/// A BRANCH IN TRANSIT GETS NO TWIN EITHER. Measured 2026-09-08
/// (backlog 02165b1d): car d08a6418 for
/// feat/a-human-only-step-refuses-an-agent boarded train #274 at
/// 20:30:54, and at 20:31:20 a duplicate gate-run went green and this
/// handler filed car ad54e95c for the same branch — the builder session
/// that owned the branch had ended, but its detached retry loop kept
/// re-gating. The twin sat on the loading dock while the real car rode,
/// and was abandoned by hand. `parked_car_for` answered `None` — truly,
/// the first car was no longer parked — and "no parked car" was read as
/// "no car". The landed guard did not cover it: the branch was green and
/// in transit, not yet merged. So the question asked here is the wider
/// one, `open_car_for`.
fn boarded_skip(cars: &[Value], branch: &str) -> Option<Value> {
    let car = car::open_car_for(cars, branch).filter(|c| car::is_boarded(c))?;
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let train = car
        .pointer("/metadata/train")
        .and_then(Value::as_str)
        .unwrap_or("?");
    Some(json!({
        "park_skipped": "boarded",
        "park_skipped_note": format!(
            "no car filed: car {} for {branch} is already aboard train {} — a second car \
             for a branch in transit sits on the dock until someone abandons it by hand \
             (2026-09-08, cars d08a6418/ad54e95c; backlog 02165b1d)",
            &id[..8.min(id.len())],
            &train[..8.min(train.len())],
        ),
    }))
}

/// PURE: what this green does about `branch` — the whole decision, in
/// one place, so "does it ever file a second car" is a unit test rather
/// than a live incident.
///
/// `open` is the branch's open cars; `all` adds the closed ones (a
/// landing is usually closed). The order is the order the failures were
/// measured in: landed first (610537b2 — the operator needs the merge
/// ref), then boarded (02165b1d), then the parked refresh (d052afad),
/// and only a branch with no live car at all files one.
#[derive(Debug)]
enum ParkAction<'a> {
    /// File nothing; record this on the gate-run instead.
    Skip(Value),
    /// Refresh the car already at the dock with the fresh receipt.
    Refresh(&'a Value),
    /// FINISH the car the builder opened at build start: complete the
    /// steps their open left open, and carry the gate's own facts onto
    /// the packet that already exists.
    Adopt(&'a Value),
    /// No live car for this branch: file one.
    File,
}

fn park_action<'a>(open: &'a [Value], all: &[Value], branch: &str) -> ParkAction<'a> {
    if let Some(patch) = landed_skip(all, branch) {
        return ParkAction::Skip(patch);
    }
    if let Some(patch) = boarded_skip(open, branch) {
        return ParkAction::Skip(patch);
    }
    if let Some(car) = car::parked_car_for(open, branch) {
        return ParkAction::Refresh(car);
    }
    // A GREEN FINISHES THE CAR THE BUILDER OPENED (backlog be025b44).
    // Asked last, so every guard measured before it answers first — and
    // asked at all, because with `boss car open` in the builders' hands
    // this is the COMMON case, not an edge: the branch's packet exists
    // from the build's first minute and its `gate` step has not
    // reported, so none of the questions above matches it. `File` here
    // would be the fourth twin of the same shape.
    match car::building_car_for(open, branch) {
        Some(car) => ParkAction::Adopt(car),
        None => ParkAction::File,
    }
}

/// PURE: the job-metadata patch an ADOPT owes the car it finishes.
///
/// A car filed at green gets the gate's facts in its `car_body`; a car
/// opened at build start was filed before those facts existed, so they
/// arrive here instead — the proof intent copied verbatim from the
/// gate-run's `park_*` keys (`boss prove --from-car` and the arrival rule
/// read it back), the item provenance that does NOT authorise a close
/// (`car::PARTIAL_ITEM` / `car::NO_ITEM_REASON`), and the delivery
/// channel classified from the diff.
///
/// THE PROVENANCE IS CARRIED BY ALL THREE PARK PATHS OR BY NONE. The
/// file path merges it into `car_body_with_proof` and the refresh path
/// into `regate_patch`; an adopt that did not would silently drop a
/// `--park-partial-item` answer for every car a builder opened — which
/// is to say, for every car, once they do. The textual merge of these
/// two changes did not catch that, because the two edits are in
/// different functions.
///
/// NOTHING IS NULLED and nothing the open recorded is restated, with the
/// one deliberate exception `supersede` below carries. The job-metadata
/// door DELETES a key set to null, so a channel the open recorded must
/// not be stripped by a green that could not classify one.
///
/// THE CLOSING EDGE IS NOT ADDED HERE, and that is deliberate —
/// [`adopt_edge_patch`] says why.
fn adopt_patch(car: &Value, inputs: &AutoParkInputs) -> Value {
    let mut patch = serde_json::Map::new();
    patch.extend(inputs.proof.clone());
    patch.extend(inputs.item_provenance.clone());
    if let Some(dc) = inputs.delivery_channel.as_deref() {
        patch.insert("delivery_channel".to_string(), json!(dc));
    }
    for (k, v) in supersede(car, inputs) {
        patch.insert(k, v);
    }
    Value::Object(patch)
}

/// PURE: the one case where an adopt OVERWRITES what the open recorded —
/// the builder's two item answers contradict each other.
///
/// HOW IT ARISES. `boss car open --backlog-item X` writes the CLOSING
/// edge on the car at build start, ref-checked at the POST. The gate is
/// then given `--park-partial-item X` or `--park-no-item <reason>`,
/// which both say the opposite: do not close that item. Nothing rejects
/// the pair, because the two statements are made by two verbs an hour
/// apart, and the arrival rule follows `metadata.backlog_item` alone —
/// so left as-is the car would close an item whose builder had just
/// said it has work outstanding. That is the exact harm e1325456 was
/// filed for (item cf0f5e2d was three pieces; closing it would have
/// buried piece 3, which was waiting on an operator's yes).
///
/// WHY THE GATE'S ANSWER WINS. It is the later statement, made with the
/// finished diff in hand, and the two errors are not symmetrical: an
/// item left open when it could have closed costs one human close,
/// while an item closed with work outstanding buries the work. So the
/// closing edge is deleted (present-and-null — the metadata door's
/// delete) and the same id is preserved under `PARTIAL_ITEM`, the key
/// the landed design put there for exactly this provenance-without-a-
/// close purpose. Nothing is lost: the car still names the item, under
/// a name no rule follows.
///
/// NOT SILENT. `item_answer_superseded` records the contradiction in
/// prose on the car, and the handler logs it — a reader who wonders why
/// the edge they typed at open is gone finds the answer on the packet,
/// rather than re-deriving it.
///
/// EMPTY whenever there is no contradiction: a gate that names the
/// closing edge (the three `--park-*` answers are mutually exclusive, so
/// provenance is then empty anyway), a gate with no provenance, or an
/// opened car that carries no edge to supersede.
fn supersede(car: &Value, inputs: &AutoParkInputs) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    // A gate that DOES name the closing edge is not a contradiction —
    // and must never lose it. Belt and braces: the flags are mutually
    // exclusive at the gate, so this can only fire on a packet edited by
    // hand, where the closing edge is the safer thing to keep.
    if inputs.backlog_item.is_some() || inputs.item_provenance.is_empty() {
        return out;
    }
    let Some(open_edge) = car
        .pointer("/metadata/backlog_item")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    else {
        return out;
    };
    let answer = if let Some(reason) = inputs
        .item_provenance
        .get(car::NO_ITEM_REASON)
        .and_then(Value::as_str)
    {
        format!("--park-no-item \"{reason}\"")
    } else {
        let partial = inputs
            .item_provenance
            .get(car::PARTIAL_ITEM)
            .and_then(Value::as_str)
            .unwrap_or("?");
        format!("--park-partial-item {}", &partial[..8.min(partial.len())])
    };
    out.insert("backlog_item".to_string(), Value::Null);
    // Keep the id the open named, unless the gate already named one.
    if !inputs.item_provenance.contains_key(car::PARTIAL_ITEM) {
        out.insert(car::PARTIAL_ITEM.to_string(), json!(open_edge));
    }
    out.insert(
        "item_answer_superseded".to_string(),
        json!(format!(
            "the open named backlog_item {} (the edge the arrival rule closes) and the \
             gate then answered {answer}, which says the opposite. The gate's answer is \
             the later one and made with the finished diff, and closing an item with work \
             outstanding buries that work (e1325456), so the closing edge was removed and \
             the id kept under `{}` — provenance without a close. If the item really is \
             this car's whole build, re-gate with --park-backlog-item.",
            &open_edge[..8.min(open_edge.len())],
            car::PARTIAL_ITEM,
        )),
    );
    out
}

/// PURE: the backlog edge an ADOPT adds, when the gate's intent named one
/// the opened car does not already carry. `None` = nothing to write.
///
/// A SEPARATE, BEST-EFFORT WRITE, because `backlog_item` is a DECLARED
/// job edge: a DB trigger refuses an id that does not resolve, with a
/// **400**, and `write_json` maps anything but a 422 to `Downstream` —
/// which redelivers. So an unresolvable edge folded into `adopt_patch`
/// would retry the adopt forever and, worse, cost the car its proof
/// intent on every attempt. CLAUDE.md records what a fallible write
/// inside a loop does: a single observability write froze ALL landings.
/// The receipt and the steps are what a car must not lose; an edge that
/// will not resolve is worth a warning.
///
/// Skipped when the car already carries the same edge — an open that was
/// given `--backlog-item` wrote it at POST, where it was ref-checked.
fn adopt_edge_patch(car: &Value, inputs: &AutoParkInputs) -> Option<Value> {
    let item = inputs.backlog_item.as_deref()?;
    if car
        .pointer("/metadata/backlog_item")
        .and_then(Value::as_str)
        == Some(item)
    {
        return None;
    }
    Some(json!({ "backlog_item": item }))
}

/// How many cars one page of the open-car read asks for. The dock and
/// the trains together hold 22 open cars today, so one page answers —
/// but a page is still a page, and the read below walks `total`
/// (a-limit-is-not-a-filter).
const CARS_PAGE: usize = 200;

/// PURE: the listing query for the open cars a park decision reads.
///
/// KEYED ON THE BRANCH, NOT THE SUBJECT — and that is the whole of the
/// third measured twin (backlog 02165b1d, 2026-09-08 21:53 UTC). This
/// read used to narrow server-side with `subject_id={branch}`, on the
/// assumption that a car's subject IS its branch. It is only its
/// FILING branch: `boss rerail` repoints `metadata.branch` at the
/// re-railed branch and leaves the subject where the car was filed, so
/// car 538775dd (subject `feat/arrival-runs-the-probe`, branch
/// `feat/arrival-runs-the-probe-rerail`) never answered the narrowed
/// query. The parked car was invisible, "no parked car" read as "no
/// car" for the third time, and the handler filed twin ef4eff3c for a
/// branch that already had one.
///
/// So the query asks the wide question and the shared
/// `metadata.branch` predicates in `boss_jobs::car` decide — the same
/// way `boss gate`'s own launch guard already reads the dock
/// (`gate::all_open_cars`). One question, one answer, no field that
/// can drift out from under it (CLAUDE.md §9a).
fn open_cars_url(base: &str, offset: usize) -> String {
    format!("{base}/api/jobs?kind=ship-a-change&status=open&limit={CARS_PAGE}&offset={offset}")
}

/// PURE: the car body, with the proof intent and the item provenance
/// merged into its metadata. The shared builder owns the packet shape;
/// these keys are added here rather than threaded through its signature
/// because `boss park` (the hand verb auto-park replaces) has no probe
/// to pass.
fn car_body_with_proof(inputs: &AutoParkInputs) -> Value {
    let mut body = car::car_body(
        &inputs.branch,
        &inputs.summary,
        inputs.backlog_item.as_deref(),
        inputs.delivery_channel.as_deref(),
    );
    if let Some(md) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        md.extend(inputs.proof.clone());
        md.extend(inputs.item_provenance.clone());
    }
    body
}

/// POST a body and return the response JSON — the create needs the new
/// car's id back, which `common::post_json` (fire-and-forget) discards.
/// Same header + 422-is-permanent contract as the shared helpers.
async fn post_json_return(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<Value, HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", super::common::sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            HandlerError::Permanent(format!("POST {url} returned {status}: {text}"))
        } else {
            HandlerError::Downstream(format!("POST {url} returned {status}: {text}"))
        });
    }
    resp.json()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url} not JSON: {e}")))
}

#[async_trait]
impl Handler for JobsAutoPark {
    fn name(&self) -> &'static str {
        "jobs.auto-park"
    }

    async fn invoke(
        &self,
        // The rule engine's Value, not serde_json's — this handler takes
        // no args (it reads everything from the event + the gate-run).
        _args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;

        // Cheap reject before the fetch: only a green verdict parks.
        if ev.metadata.get("verdict").and_then(Value::as_str) != Some("green") {
            return Ok(());
        }

        // The gate-run packet carries the branch + the `park_*` intent.
        let gate_run = get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), ev.job_id),
            &ctx.rule_name,
        )
        .await?;

        let Some(inputs) = auto_park_inputs(&gate_run, ev.metadata) else {
            // Green, but no park intent — a manual gate. Nothing to do.
            return Ok(());
        };

        // A RE-GATE NEVER FILES A TWIN. Measured 2026-09-05 (backlog
        // d052afad): this handler filed a car per green gate, so the dock
        // held 10 cars for 6 branches and each first car sat with a
        // receipt for a vanished head, left behind by every train. A car
        // still at the dock is refreshed instead — the fresh receipt
        // rides it as `regate_receipt` (the rerail write, one builder in
        // core) and the skip clears. A car already ABOARD a train is left
        // alone entirely (02165b1d). Only a closed or past-review car is
        // spent history; then, and only then, a new car.
        //
        // ONE READ OF THE OPEN CARS serves all three questions below.
        // Every open car, paged — see `open_cars_url` for why it does
        // not narrow by subject.
        let mut cars: Vec<Value> = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = get_json(
                &self.client,
                &open_cars_url(self.base(), offset),
                &ctx.rule_name,
            )
            .await?;
            let rows: Vec<Value> = page
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let got = rows.len();
            cars.extend(rows);
            let total = page
                .get("total")
                .and_then(Value::as_u64)
                .unwrap_or(cars.len() as u64) as usize;
            // Stop on a short page as well as on the count: a `total`
            // that never shrinks would otherwise spin forever.
            if got == 0 || cars.len() >= total {
                break;
            }
            offset += CARS_PAGE;
        }

        // A LANDED BRANCH GETS NO TWIN — checked before the parked-car
        // refresh, because a still-parked car for a landed branch IS a
        // twin and must not be kept fresh either. Closed cars are the
        // usual landing; an open car with the `merged` marker is one
        // the dispatcher has not closed yet (see `landed_skip`).
        //
        // THIS read stays narrowed by subject, and deliberately: 807
        // ship-a-change packets are closed (measured 2026-09-08), so
        // reading them all per green gate is not a trade worth making
        // for a hole the open read now covers. What it leaves: a
        // RE-RAILED car that has both landed AND been closed answers
        // neither `subject_id={branch}` nor the open read, so a stray
        // re-gate of that branch could still file one twin. The window
        // is minutes wide — the conductor stamps `merged` on the car
        // while it is still open, and `landed_car_for` reads that
        // marker off the open page above — and it has not been
        // measured. Say so rather than pay for it.
        let closed = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind=ship-a-change&status=closed&subject_id={}&limit=50",
                self.base(),
                inputs.branch
            ),
            &ctx.rule_name,
        )
        .await?;
        let all: Vec<Value> = cars
            .iter()
            .cloned()
            .chain(
                closed
                    .get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            )
            .collect();
        // ONE DECISION, taken on what the SoR holds: skip (landed, or
        // aboard a train), refresh the car at the dock, finish the car a
        // builder opened, or file.
        let (parked, adopt) = match park_action(&cars, &all, &inputs.branch) {
            ParkAction::Skip(patch) => {
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{}/metadata", self.base(), ev.job_id),
                    &patch,
                    &ctx.rule_name,
                )
                .await?;
                return Ok(());
            }
            ParkAction::Refresh(car) => (Some(car), None),
            ParkAction::Adopt(car) => (None, Some(car.clone())),
            ParkAction::File => (None, None),
        };

        if let Some(parked) = parked {
            let id = parked.get("id").and_then(Value::as_str).ok_or_else(|| {
                HandlerError::Downstream("auto-park: parked car has no id".into())
            })?;
            let note = format!(
                "re-gated in place: green at {} (gate-run {}) — receipt machine-copied to \
                 regate_receipt by the auto-park handler; the frozen gate step stays as the \
                 original head's record",
                &inputs.receipt.head[..12.min(inputs.receipt.head.len())],
                ev.job_id
            );
            // A re-gate carries the CURRENT proof intent too: the
            // builder may have written (or fixed) the probe on the
            // re-gate, and the parked car should carry what its
            // latest green stamped, not what its first one did.
            let mut patch =
                car::regate_patch(&inputs.receipt, &note, inputs.delivery_channel.as_deref());
            if let Some(m) = patch.as_object_mut() {
                m.extend(inputs.proof.clone());
                // The provenance too: a re-gate may be the run where the
                // builder first said which item this car is a piece of,
                // and the refreshed car should carry what its latest
                // green stamped.
                m.extend(inputs.item_provenance.clone());
            }
            write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{}/metadata", self.base(), id),
                &patch,
                &ctx.rule_name,
            )
            .await?;
            // A re-gate restates the route too: the item may still be
            // un-triaged from the first park (this is new), and a
            // second pass over an item already routed writes nothing.
            if let Some(item) = inputs.backlog_item.as_deref() {
                self.triage_linked_item(item, id, &inputs.branch, &ctx.rule_name)
                    .await;
            }
            return Ok(());
        }

        let now = boss_clock_client::now_from(&self.clock).await;

        // ONE CAR, TWO WAYS IN. Either the builder opened it when the
        // build started and this green FINISHES it, or no car exists and
        // this green files one. Past this point the sequence is identical:
        // read the packet, then complete the steps it has not already
        // completed — which is why the step writes are decided in core
        // (`car::finish_writes`), shared with `boss park`.
        let car_id = match &adopt {
            Some(existing) => {
                let id = existing
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        HandlerError::Downstream("auto-park: the building car has no id".into())
                    })?
                    .to_string();
                // The gate's own facts — proof intent, item provenance,
                // delivery channel — onto the packet that already exists.
                // Before the steps, so a car whose arrival rule fires off
                // the gate completion already carries its probe AND the
                // item answer that decides whether it closes anything.
                let url = format!("{}/api/jobs/{}/metadata", self.base(), id);
                let patch = adopt_patch(existing, &inputs);
                if let Some(why) = patch
                    .pointer("/item_answer_superseded")
                    .and_then(Value::as_str)
                {
                    tracing::warn!(rule = %ctx.rule_name, car = %id, "{why}");
                }
                if patch.as_object().is_some_and(|m| !m.is_empty()) {
                    write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &url,
                        &patch,
                        &ctx.rule_name,
                    )
                    .await?;
                }
                // The declared edge, separately and best-effort: an id
                // that will not resolve is a 400, which redelivers, and
                // a redelivery over an edge must not cost the car the
                // write above (see `adopt_edge_patch`).
                if let Some(edge) = adopt_edge_patch(existing, &inputs)
                    && let Err(e) = write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &url,
                        &edge,
                        &ctx.rule_name,
                    )
                    .await
                {
                    tracing::warn!(rule = %ctx.rule_name, car = %id,
                        "the gate's backlog edge would not attach to the opened car: {e} — \
                         the car is finished; link it by hand");
                }
                tracing::info!(rule = %ctx.rule_name, car = %id, branch = %inputs.branch,
                    "finishing the car its builder opened at build start — no second car");
                id
            }
            None => {
                let body = car_body_with_proof(&inputs);
                let created = post_json_return(
                    &self.client,
                    &format!("{}/api/jobs", self.base()),
                    &body,
                    &ctx.rule_name,
                )
                .await?;
                created
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        HandlerError::Downstream("auto-park: create returned no id".into())
                    })?
                    .to_string()
            }
        };
        let car_id = car_id.as_str();

        let car = get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), car_id),
            &ctx.rule_name,
        )
        .await?;

        // In order (scope → build → gate): each completion re-evaluates
        // readiness so the next is ready. A step the OPEN already
        // completed is skipped — the step API refuses a metadata write to
        // a completed step, so re-sending `scope` would 409 on every car
        // a builder opened.
        for w in car::finish_writes(
            &car,
            &inputs.summary,
            &inputs.excludes,
            &inputs.test,
            &inputs.verified,
            &inputs.receipt,
            now,
        )
        .map_err(HandlerError::Downstream)?
        {
            write_json(
                &self.client,
                reqwest::Method::PUT,
                &format!("{}/api/jobs/{}/steps/{}", self.base(), car_id, w.step_id),
                &w.body,
                &ctx.rule_name,
            )
            .await?;
        }

        // THE CAR IS THE ITEM'S BUILD — say so on the item, last, so a
        // failure here costs the routing and not the car.
        if let Some(item) = inputs.backlog_item.as_deref() {
            self.triage_linked_item(item, car_id, &inputs.branch, &ctx.rule_name)
                .await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate_run(extra_md: Value) -> Value {
        let mut md = json!({ "branch": "fix/x", "sha": "abc" });
        if let (Some(dst), Some(src)) = (md.as_object_mut(), extra_md.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        json!({ "kind": "gate-run", "metadata": md })
    }

    fn green_step_meta() -> serde_json::Map<String, Value> {
        json!({
            "verdict": "green",
            "receipt": "{\"verdict\":\"green\",\"head\":\"deadbeef\",\"mode\":\"full\",\"fails\":[]}"
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn a_green_gate_with_full_intent_yields_a_car() {
        let gr = gate_run(json!({
            "park_summary": "does a thing. and more.",
            "park_excludes": "not that",
            "park_test": "ran it",
            "park_verified": "seen working",
            "park_backlog_item": "7c9e376d",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("green+intent parks");
        assert_eq!(got.branch, "fix/x");
        assert_eq!(got.summary, "does a thing. and more.");
        assert_eq!(got.backlog_item.as_deref(), Some("7c9e376d"));
        // Receipt copied verbatim; head/mode read out of it.
        assert_eq!(got.receipt.head, "deadbeef");
        assert_eq!(got.receipt.mode, "full");
        assert!(got.receipt.raw.contains("\"fails\":[]"));
    }

    /// THE WIDE RECEIPT PARKS THE SAME WAY.
    ///
    /// The gate runner reduced `infra/gate.sh`'s account of a run to
    /// `{verdict, head, mode, fails}` before reporting it until
    /// 2026-09-09; it now reports the whole receipt. Auto-park copies it
    /// VERBATIM onto the car and reads only `head` and `mode` out, so the
    /// widening must be a no-op here — a claim worth a test, because this
    /// handler and `boss park` are the two verbs that parse the receipt
    /// on the way to a train.
    #[test]
    fn a_wide_receipt_parks_verbatim_like_the_four_field_one() {
        const WIDE: &str = "{\"verdict\":\"green\",\"mode\":\"auto\",\"scope\":\"\",\
            \"head\":\"deadbeef\",\"dirty\":false,\"host\":\"gate-runner-abc\",\"ci\":true,\
            \"free_gb\":91,\"unverifiable\":[],\
            \"report\":{\"attempts\":4,\"waited_s\":63,\"sor_unreachable\":true},\
            \"checks\":[{\"name\":\"fmt\",\"result\":\"pass\",\"seconds\":3}]}";
        let meta = json!({ "verdict": "green", "receipt": WIDE })
            .as_object()
            .unwrap()
            .clone();
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
        }));
        let got = auto_park_inputs(&gr, &meta).expect("a wide green receipt still parks");
        assert_eq!(got.receipt.raw, WIDE, "verbatim, never rebuilt from parts");
        assert_eq!(got.receipt.head, "deadbeef");
        assert_eq!(got.receipt.mode, "auto");
    }

    /// THE CAR CARRIES ITS PROBE (28ac45ab). The gate's `park_probe` /
    /// `park_expect` land on the car as `proof_probe` / `proof_expect`,
    /// verbatim, under the keys core defines — the pair `boss prove
    /// --from-car` and the arrival rule read back.
    #[test]
    fn a_park_probe_rides_onto_the_car_verbatim() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_probe": "kubectl -n boss-dev exec deploy/boss-conductor -- boss gate --help | grep -m1 park-probe",
            "park_expect": "park-probe",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        let body = car_body_with_proof(&got);
        let md = &body["metadata"];
        assert_eq!(
            md[car::PROOF_PROBE],
            "kubectl -n boss-dev exec deploy/boss-conductor -- boss gate --help | grep -m1 park-probe"
        );
        assert_eq!(md[car::PROOF_EXPECT], "park-probe");
        assert!(md.get(car::PROOF_EVENT).is_none());
        // The rest of the body is the shared builder's, untouched.
        assert_eq!(md["branch"], "fix/x");
        assert_eq!(body["kind"], "ship-a-change");
    }

    /// An event-bound car records the event; a car with no proof
    /// intent records nothing (no `proof_probe: null` for a reader to
    /// trip on).
    #[test]
    fn an_event_bound_or_unprobed_car_records_exactly_what_it_was_given() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_proof_event": "event-bound — needs a stalled train",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        let md = car_body_with_proof(&got)["metadata"].clone();
        assert_eq!(md[car::PROOF_EVENT], "event-bound — needs a stalled train");
        assert!(md.get(car::PROOF_PROBE).is_none());

        let plain = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
        }));
        let got = auto_park_inputs(&plain, &green_step_meta()).expect("parks");
        assert!(got.proof.is_empty());
        let md = car_body_with_proof(&got)["metadata"].clone();
        for k in [car::PROOF_PROBE, car::PROOF_EXPECT, car::PROOF_EVENT] {
            assert!(md.get(k).is_none(), "{k} must be absent, not null");
        }
    }

    /// THE PARTIAL EDGE RIDES ONTO THE CAR, AND THE CLOSING KEY DOES
    /// NOT (e1325456).
    ///
    /// `--park-partial-item` says this car is one piece of a
    /// several-piece item: record which item, do not authorise its
    /// close. The inertness is structural, not a second rule — the
    /// arrival rule reads `metadata.backlog_item` and nothing else, and
    /// `triage_linked_item` is called only for `inputs.backlog_item`, so
    /// a `None` there is the whole guarantee that parking writes nothing
    /// on the item either.
    #[test]
    fn a_partial_item_rides_onto_the_car_without_the_closing_edge() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_partial_item": "cf0f5e2d-0000-0000-0000-000000000000",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(
            got.backlog_item, None,
            "a partial item is not the closing edge — and this None is what stops the \
             park-time triage write too"
        );
        let md = car_body_with_proof(&got)["metadata"].clone();
        assert_eq!(
            md[car::PARTIAL_ITEM],
            "cf0f5e2d-0000-0000-0000-000000000000"
        );
        assert!(
            md.get("backlog_item").is_none(),
            "the key the arrival rule follows must be absent: {md}"
        );
    }

    /// AN ITEM-LESS CAR CARRIES ITS REASON. `--park-no-item` is the
    /// escape for a car that genuinely answers no item, and the reason
    /// is what distinguishes it from a forgotten edge when someone reads
    /// the car later.
    #[test]
    fn an_item_less_car_carries_the_reason_it_names_none() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_no_item": "David asked for this in conversation",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(got.backlog_item, None);
        let md = car_body_with_proof(&got)["metadata"].clone();
        assert_eq!(
            md[car::NO_ITEM_REASON],
            "David asked for this in conversation"
        );
        assert!(md.get(car::PARTIAL_ITEM).is_none());
    }

    /// A car that IS an item's build is unchanged, and carries neither
    /// provenance key — absent, not null, like the proof keys.
    #[test]
    fn the_closing_edge_still_rides_alone() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v",
            "park_backlog_item": "7c9e376d-0000-0000-0000-000000000000",
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks");
        assert_eq!(
            got.backlog_item.as_deref(),
            Some("7c9e376d-0000-0000-0000-000000000000")
        );
        assert!(got.item_provenance.is_empty());
        let md = car_body_with_proof(&got)["metadata"].clone();
        assert_eq!(md["backlog_item"], "7c9e376d-0000-0000-0000-000000000000");
        for k in [car::PARTIAL_ITEM, car::NO_ITEM_REASON] {
            assert!(md.get(k).is_none(), "{k} must be absent, not null");
        }
    }

    #[test]
    fn a_non_green_verdict_is_a_no_op() {
        let gr = gate_run(json!({ "park_summary": "does a thing" }));
        let mut meta = green_step_meta();
        meta.insert("verdict".into(), json!("failed"));
        assert!(auto_park_inputs(&gr, &meta).is_none());
    }

    #[test]
    fn a_green_gate_with_no_park_intent_is_a_no_op() {
        // A plain `boss gate` (manual) stamps no `park_*` keys.
        let gr = gate_run(json!({}));
        assert!(auto_park_inputs(&gr, &green_step_meta()).is_none());
    }

    /// THE TWIN (610537b2). A merged car for the branch means this green
    /// re-gated landed content: no car, and the gate-run says why.
    #[test]
    fn a_landed_branch_records_a_skip_instead_of_a_twin() {
        let landed = json!({
            "id": "670087f4-0000-0000-0000-000000000000",
            "status": "closed",
            "metadata": {
                "branch": "fix/x", "merged": "true", "merge_ref": "b641f3adcf47",
                "outcome": "merged", "train": "3a476b50-7a3a-409f-a9bc-59266e44f331"
            }
        });
        let patch = landed_skip(&[landed], "fix/x").expect("a landed branch is skipped");
        assert_eq!(patch["park_skipped"], "landed");
        let note = patch["park_skipped_note"].as_str().unwrap();
        assert!(note.contains("670087f4"), "names the car: {note}");
        assert!(note.contains("b641f3adcf47"), "names the merge: {note}");
        assert!(note.contains("3a476b50"), "names the train: {note}");
    }

    /// An abandoned twin or a parked car is not a landing — the branch
    /// still needs its car (a parked one is refreshed, not duplicated).
    #[test]
    fn spent_or_parked_cars_do_not_block_the_park() {
        let abandoned = json!({
            "id": "dfb98d07", "status": "closed",
            "metadata": { "branch": "fix/x", "outcome": "abandoned" }
        });
        let parked = json!({
            "id": "p", "status": "open",
            "metadata": { "branch": "fix/x" },
            "steps": [{"spec_slug": "review", "title": car::REVIEW, "status": "ready"}]
        });
        assert!(landed_skip(&[abandoned, parked], "fix/x").is_none());
        assert!(landed_skip(&[], "fix/x").is_none());
    }

    #[test]
    fn a_backlog_item_is_optional_but_the_receipt_is_not() {
        let gr = gate_run(json!({
            "park_summary": "s", "park_excludes": "e", "park_test": "t", "park_verified": "v"
        }));
        let got = auto_park_inputs(&gr, &green_step_meta()).expect("parks without a backlog item");
        assert_eq!(got.backlog_item, None);

        // No receipt on the verdict step → cannot file a car (the whole
        // point of a car is the receipt), so it is a no-op rather than a
        // car with an empty receipt.
        let mut no_receipt = green_step_meta();
        no_receipt.remove("receipt");
        assert!(auto_park_inputs(&gr, &no_receipt).is_none());
    }
}

#[cfg(test)]
mod twin_tests {
    use super::*;

    const BRANCH: &str = "feat/a-human-only-step-refuses-an-agent";

    /// Car d08a6418 at 20:31:20 UTC on 2026-09-08: boarded train #274
    /// twenty-six seconds earlier, review still waiting.
    fn boarded() -> Value {
        json!({
            "id": "d08a6418-e7af-484b-82bf-ab043229bfd6",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": {
                "branch": BRANCH,
                "boarded_head": "cd0c4f7bdadf2306a589c922f240a7ff963f70a7",
                "train": "d72ecdb9-c5a1-4d16-8a00-d934fd565002"
            },
            "steps": [
                {"spec_slug": "gate", "title": car::GATE, "status": "completed"},
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// Car ad54e95c — the twin this handler filed for the same branch,
    /// as it looked on the dock before it was abandoned by hand.
    fn parked() -> Value {
        json!({
            "id": "ad54e95c-b48a-4a4c-87f8-7aa8e4e19eff",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": BRANCH },
            "steps": [
                {"spec_slug": "gate", "title": car::GATE, "status": "completed"},
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// THE MEASURED CASE (02165b1d). A car already aboard a train means
    /// the branch has its car: nothing is filed, and the gate-run says
    /// which car and which train.
    #[test]
    fn a_boarded_car_is_never_twinned() {
        let open = vec![boarded()];
        match park_action(&open, &open, BRANCH) {
            ParkAction::Skip(patch) => {
                assert_eq!(patch["park_skipped"], "boarded");
                let note = patch["park_skipped_note"].as_str().unwrap();
                assert!(note.contains("d08a6418"), "names the car: {note}");
                assert!(note.contains("d72ecdb9"), "names the train: {note}");
                assert!(note.contains(BRANCH), "names the branch: {note}");
            }
            other => panic!("a boarded branch must file nothing: {other:?}"),
        }
    }

    /// A car still at the dock is REFRESHED — one car, a fresh receipt
    /// on it. The behaviour that already held, pinned: whatever else
    /// changes, this branch never ends with two cars.
    #[test]
    fn a_parked_car_is_refreshed_never_duplicated() {
        let open = vec![parked()];
        match park_action(&open, &open, BRANCH) {
            ParkAction::Refresh(car) => {
                assert_eq!(car["id"], "ad54e95c-b48a-4a4c-87f8-7aa8e4e19eff")
            }
            other => panic!("a parked car is refreshed, not twinned: {other:?}"),
        }
        // And with the twin already there beside the boarded car — the
        // 20:31:20 state — the answer is still never `File`.
        let both = vec![parked(), boarded()];
        assert!(
            !matches!(park_action(&both, &both, BRANCH), ParkAction::File),
            "a branch with any live car never files a second one"
        );
    }

    #[test]
    fn a_branch_with_no_car_files_one() {
        assert!(matches!(park_action(&[], &[], BRANCH), ParkAction::File));
        let elsewhere = vec![boarded()];
        assert!(matches!(
            park_action(&elsewhere, &elsewhere, "feat/other"),
            ParkAction::File
        ));
    }

    /// The landed skip (610537b2) is asked FIRST: a branch already on
    /// main is reported as landed, not as boarded, because the operator
    /// needs the merge ref, not the train that carried it.
    #[test]
    fn a_landed_branch_is_still_reported_as_landed() {
        let landed = json!({
            "id": "670087f4-0000-0000-0000-000000000000",
            "status": "open",
            "metadata": {
                "branch": BRANCH, "merged": "true", "merge_ref": "75e4d3c59387",
                "train": "d72ecdb9-c5a1-4d16-8a00-d934fd565002"
            },
            "steps": [
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        });
        let all = vec![landed];
        match park_action(&all, &all, BRANCH) {
            ParkAction::Skip(patch) => {
                assert_eq!(patch["park_skipped"], "landed");
                assert!(
                    patch["park_skipped_note"]
                        .as_str()
                        .unwrap()
                        .contains("75e4d3c59387")
                );
            }
            other => panic!("a landed branch records the landing: {other:?}"),
        }
    }
}

/// THE THIRD SHAPE (02165b1d, annotation `third_instance_2026_09_08_2200Z`):
/// a PARKED car whose branch head MOVED. The operator's documented
/// recovery for "gate receipt is for X but the branch boards Y" is to
/// re-gate with `--park-*`, and that re-gate filed a second car instead
/// of refreshing the first — for the third time, by a route the two
/// landed guards do not cover.
#[cfg(test)]
mod a_moved_head_tests {
    use super::*;

    /// The branch of the re-railed car, measured 2026-09-08 21:53 UTC.
    const BRANCH: &str = "feat/arrival-runs-the-probe-rerail";
    /// Its SUBJECT — the branch it was FILED under, before `boss rerail`
    /// repointed `metadata.branch`. The two disagree, and the read that
    /// trusted them to agree is the defect.
    const SUBJECT: &str = "feat/arrival-runs-the-probe";
    /// The head its receipt vouched for, and the head the branch moved
    /// to when a colliding migration prefix was renumbered and pushed.
    const OLD_HEAD: &str = "fb973bd0d1d0da9c25fd64a22263de3f4baa1fef";
    const NEW_HEAD: &str = "0ec4521fd4a5c632e01d6e6ed9926b1bf300fe6e";

    /// Car 538775dd as it stood when the re-gate went green: parked at
    /// the dock, receipt for a head the branch had left, boarding
    /// refusing it with a `skip_reason` — and its subject still the
    /// pre-rerail branch.
    fn rerailed_parked_car() -> Value {
        json!({
            "id": "538775dd-64ee-4897-b9d6-b49137eb6512",
            "kind": "ship-a-change",
            "status": "open",
            "subject": { "subject_kind": "custom", "id": SUBJECT },
            "metadata": {
                "branch": BRANCH,
                "regate_receipt": format!(
                    "{{\"verdict\": \"green\", \"head\": \"{OLD_HEAD}\", \"mode\": \"full\", \"fails\": []}}"
                ),
                "skip_reason":
                    "gate receipt is for fb973bd0 but the branch boards 0ec4521f — gated, then changed",
            },
            "steps": [
                {"spec_slug": "gate", "title": car::GATE, "status": "completed"},
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// Another branch's car, so the page proves a branch FILTER and not
    /// merely "the one row we asked the server for".
    fn someone_elses_car() -> Value {
        json!({
            "id": "aaaaaaaa-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "open",
            "subject": { "subject_kind": "custom", "id": "feat/other" },
            "metadata": { "branch": "feat/other" },
            "steps": [
                {"spec_slug": "gate", "title": car::GATE, "status": "completed"},
                {"spec_slug": "review", "title": car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// THE MEASURED CASE. Given the whole open page, the car whose head
    /// moved is REFRESHED. This is the ordinary state of affairs after
    /// a rebase, a re-rail, or a renumbered migration — not an edge.
    #[test]
    fn a_parked_car_whose_head_moved_is_refreshed_not_twinned() {
        let page = [someone_elses_car(), rerailed_parked_car()];
        match park_action(&page, &page, BRANCH) {
            ParkAction::Refresh(car) => assert_eq!(
                car["id"], "538775dd-64ee-4897-b9d6-b49137eb6512",
                "the moved-head car at the dock is the one refreshed"
            ),
            other => panic!("a re-gate after the head moved refreshes the parked car: {other:?}"),
        }
    }

    /// WHAT THE HANDLER USED TO SEE, and why it filed twin ef4eff3c.
    /// The open read was narrowed server-side by `subject_id`, which a
    /// re-railed car does not answer to. The narrowed page is empty,
    /// "no parked car" reads as "no car", and a second car is filed.
    #[test]
    fn the_subject_keyed_read_is_what_filed_the_twin() {
        let page = [someone_elses_car(), rerailed_parked_car()];
        let subject_keyed: Vec<Value> = page
            .iter()
            .filter(|c| c.pointer("/subject/id").and_then(Value::as_str) == Some(BRANCH))
            .cloned()
            .collect();
        assert!(
            subject_keyed.is_empty(),
            "a re-railed car does not answer subject_id={BRANCH} — that is the miss"
        );
        assert!(
            matches!(
                park_action(&subject_keyed, &subject_keyed, BRANCH),
                ParkAction::File
            ),
            "the subject-narrowed page files a twin — which is what happened at 21:53"
        );
    }

    /// So the query must not narrow by subject, and must page.
    #[test]
    fn the_open_car_query_is_keyed_on_the_branch_not_the_subject() {
        let url = open_cars_url("http://jobs", 0);
        assert!(
            !url.contains("subject_id"),
            "the open-car read must not narrow by subject: {url}"
        );
        assert!(url.contains("kind=ship-a-change"), "{url}");
        assert!(url.contains("status=open"), "{url}");
        assert!(url.contains("offset=0"), "{url}");
        assert!(
            open_cars_url("http://jobs", CARS_PAGE).contains(&format!("offset={CARS_PAGE}")),
            "a dock deeper than one page is still read whole (a-limit-is-not-a-filter)"
        );
    }

    /// The refresh writes the CURRENT receipt and DELETES the stale
    /// skip — `skip_reason` present-and-null, which the metadata door
    /// removes. Without both, the car keeps refusing to board for a
    /// reason that stopped being true.
    #[test]
    fn the_refresh_supersedes_the_receipt_and_clears_the_skip() {
        let fresh = Receipt {
            raw: format!(
                "{{\"verdict\": \"green\", \"head\": \"{NEW_HEAD}\", \"mode\": \"full\", \"fails\": []}}"
            ),
            head: NEW_HEAD.to_string(),
            mode: "full".to_string(),
        };
        let patch = car::regate_patch(&fresh, "re-gated in place", None);
        assert_eq!(patch["regate_receipt"], fresh.raw);
        assert!(
            patch["regate_receipt"].as_str().unwrap().contains(NEW_HEAD),
            "the fresh head, not {OLD_HEAD}"
        );
        assert_eq!(
            patch["skip_reason"],
            Value::Null,
            "a null key is deleted by the metadata door — the stale skip goes with the stale receipt"
        );
    }
}

/// THE CAR IS THE ITEM'S BUILD (backlog ca76d8f9).
#[cfg(test)]
mod park_routes_its_item_tests {
    use super::*;

    const ITEM: &str = "5942f205-0f0e-4a51-9a31-2f8f3b0b7a11";
    const CAR_ID: &str = "9442139b-1616-4b8d-8a7a-d1e34ff96486";

    /// Stand-in for jobs-api holding one un-triaged backlog item, and
    /// recording every step PUT so the routing write can be read back.
    /// `status` is the code the item GET answers with — 500 stands in
    /// for an SoR that cannot be reached.
    async fn mock_item(
        item: Option<Value>,
        status: axum::http::StatusCode,
    ) -> (String, Arc<std::sync::Mutex<Vec<(String, Value)>>>) {
        use axum::{Json, Router, extract::Path, routing::get};
        let puts: Arc<std::sync::Mutex<Vec<(String, Value)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let put_log = puts.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move || {
                    let item = item.clone();
                    async move {
                        match item {
                            Some(item) if status.is_success() => Ok(Json(item)),
                            _ => Err(status),
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                axum::routing::put(
                    move |Path((_id, step_id)): Path<(String, String)>, Json(body): Json<Value>| {
                        let puts = put_log.clone();
                        async move {
                            puts.lock().unwrap().push((step_id, body));
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), puts)
    }

    fn untriaged_item() -> Value {
        json!({
            "id": ITEM,
            "kind": "backlog-item",
            "status": "open",
            "steps": [
                { "id": "s-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
                { "id": "s-triage", "spec_slug": "triage", "status": "ready", "metadata": {} },
                { "id": "s-build", "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    /// THE WRITE. Parking a car that names an un-triaged item completes
    /// that item's triage with `disposition = build` and evidence naming
    /// the car — so the `build` step the arrival rule completes actually
    /// opens, instead of the item sitting at triage while the change is
    /// live.
    #[tokio::test]
    async fn parking_routes_the_linked_item_to_build() {
        let (base, puts) = mock_item(Some(untriaged_item()), axum::http::StatusCode::OK).await;
        let h = JobsAutoPark::new(base, "http://unused");
        h.triage_linked_item(ITEM, CAR_ID, "fix/x", "jobs.auto-park")
            .await;

        let puts = puts.lock().unwrap().clone();
        assert_eq!(puts.len(), 1, "one routing write: {puts:?}");
        let (step_id, body) = &puts[0];
        assert_eq!(step_id, "s-triage");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["disposition"], "build");
        let evidence = body["metadata"]["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains("9442139b") && evidence.contains("fix/x"),
            "the evidence names the car and its branch: {evidence}"
        );
    }

    /// IDEMPOTENT ON A RE-GATE. The refresh path runs this too, and an
    /// item a person already routed keeps their disposition — nothing is
    /// written at all.
    #[tokio::test]
    async fn a_regate_never_re_routes_an_item_someone_decided() {
        let mut decided = untriaged_item();
        decided["steps"][1]["status"] = json!("completed");
        decided["steps"][1]["metadata"] = json!({ "disposition": "verify", "evidence": "by hand" });
        let (base, puts) = mock_item(Some(decided), axum::http::StatusCode::OK).await;
        let h = JobsAutoPark::new(base, "http://unused");
        h.triage_linked_item(ITEM, CAR_ID, "fix/x", "jobs.auto-park")
            .await;
        assert!(
            puts.lock().unwrap().is_empty(),
            "a person's disposition is never overwritten"
        );
    }

    /// BEST-EFFORT, AND THAT IS THE POINT. An unreachable system of
    /// record costs the routing write, never the park: the car is
    /// already filed and green by the time this runs, and a fallible
    /// write in a loop like this one froze every landing once
    /// (CLAUDE.md §Diagnosis). The same holds for an edge stored as an
    /// 8-char prefix, which would be a permanent 400.
    #[tokio::test]
    async fn a_routing_write_that_cannot_land_does_not_fail_the_park() {
        let (base, puts) = mock_item(None, axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
        let h = JobsAutoPark::new(base.clone(), "http://unused");
        // Returns () — there is no error for the park to propagate.
        h.triage_linked_item(ITEM, CAR_ID, "fix/x", "jobs.auto-park")
            .await;
        assert!(puts.lock().unwrap().is_empty());

        let h = JobsAutoPark::new(base, "http://unused");
        h.triage_linked_item("5942f205", CAR_ID, "fix/x", "jobs.auto-park")
            .await;
        assert!(
            puts.lock().unwrap().is_empty(),
            "a prefix edge is not followed — it is a permanent 400"
        );
    }
}

/// A GREEN FINISHES THE CAR THE BUILDER OPENED (backlog be025b44).
///
/// `boss car open` files the `ship-a-change` packet at build START, so
/// by the time this handler sees a green the branch usually ALREADY has
/// a car — one whose `gate` step has not reported, which is to say one
/// that is neither parked nor boarded. Every earlier guard answered that
/// shape with `File`, and filing there would be the fourth twin of the
/// same kind (d052afad, 02165b1d, 6790175e): a packet recording a build
/// that already has one, on the dock beside it.
#[cfg(test)]
mod building_car_tests {
    use super::*;

    const BRANCH: &str = "feat/a-car-opens-when-the-build-starts";

    /// A car as `boss car open` leaves it: scope declared with the brief,
    /// build CLAIMED by the builder, gate and review still ahead.
    fn building() -> Value {
        json!({
            "id": "be025b44-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "open",
            "subject": { "subject_kind": "custom", "id": BRANCH },
            "metadata": {
                "branch": BRANCH,
                "build_started_at": "2026-09-10T18:00:00Z",
                "built_by": "claude@algedonic.dev",
                "build_host": "boss-dev-0",
            },
            "steps": [
                {"id": "s-opened", "spec_slug": "opened", "title": car::OPENED,
                 "status": "completed"},
                {"id": "s-scope", "spec_slug": "scope", "title": car::SCOPE,
                 "status": "completed",
                 "metadata": {"summary": "s", "excludes": "e"}},
                {"id": "s-build", "spec_slug": "build", "title": car::BUILD,
                 "status": "active", "assignee_id": "claude@algedonic.dev"},
                {"id": "s-gate", "spec_slug": "gate", "title": car::GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": car::REVIEW,
                 "status": "pending"},
            ],
        })
    }

    /// THE CAR THIS CAR EXISTS FOR. A green whose branch already has a
    /// building car ADOPTS it — it does not file a second.
    #[test]
    fn a_green_adopts_the_car_the_builder_opened() {
        let open = [building()];
        match park_action(&open, &open, BRANCH) {
            ParkAction::Adopt(car) => {
                assert_eq!(car["id"], "be025b44-0000-0000-0000-000000000000")
            }
            other => panic!("a building car is finished, not twinned: {other:?}"),
        }
    }

    /// And what it writes onto that car is exactly the steps the open
    /// did NOT already complete — the step API refuses a metadata write
    /// to a completed step, so re-sending `scope` would 409 every time.
    #[test]
    fn adopting_writes_the_steps_the_open_left_open() {
        let inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "ran the tests".into(),
            verified: "seen working".into(),
            backlog_item: None,
            item_provenance: serde_json::Map::new(),
            delivery_channel: Some("software".into()),
            receipt: Receipt {
                raw: "{\"verdict\":\"green\",\"head\":\"deadbeef\",\"mode\":\"full\"}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: car::proof_intent(Some("echo hi"), Some("hi"), None),
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-10T18:30:00Z")
            .unwrap()
            .into();
        let writes = car::finish_writes(
            &building(),
            &inputs.summary,
            &inputs.excludes,
            &inputs.test,
            &inputs.verified,
            &inputs.receipt,
            now,
        )
        .expect("an opened car can be finished");
        let ids: Vec<&str> = writes.iter().map(|w| w.step_id.as_str()).collect();
        assert_eq!(ids, vec!["s-build", "s-gate"], "scope is already declared");
        assert_eq!(writes[1].body["metadata"]["receipt"], inputs.receipt.raw);

        // The proof intent and the channel ride the JOB, not a step, so
        // an adopted car carries what a freshly filed one would.
        let patch = adopt_patch(&building(), &inputs);
        assert_eq!(patch[car::PROOF_PROBE], "echo hi");
        assert_eq!(patch[car::PROOF_EXPECT], "hi");
        assert_eq!(patch["delivery_channel"], "software");
        assert!(
            patch.get("summary").is_none(),
            "the open already recorded the summary; an adopt does not restate it"
        );
    }

    /// THE MERGE HAZARD, PINNED. The file path merges the item
    /// provenance into `car_body_with_proof` and the refresh path into
    /// `regate_patch`; the adopt path must too, or a
    /// `--park-partial-item` answer is silently dropped for every car a
    /// builder opened. The two changes landed in different functions, so
    /// a textual merge could not see it — this is the test that can.
    #[test]
    fn an_adopt_carries_the_item_provenance_every_other_park_path_carries() {
        let inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "t".into(),
            verified: "v".into(),
            backlog_item: None,
            item_provenance: car::item_provenance(
                Some("cf0f5e2d-0000-0000-0000-000000000000"),
                None,
            ),
            delivery_channel: None,
            receipt: Receipt {
                raw: "{}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: serde_json::Map::new(),
        };
        let patch = adopt_patch(&building(), &inputs);
        assert_eq!(
            patch[car::PARTIAL_ITEM],
            "cf0f5e2d-0000-0000-0000-000000000000"
        );
        assert!(
            patch.get("backlog_item").is_none(),
            "a partial item must never become the key the arrival rule closes on: {patch}"
        );
        // And the same for a deliberately item-less car.
        let mut none = inputs;
        none.item_provenance = car::item_provenance(None, Some("David asked in conversation"));
        let patch = adopt_patch(&building(), &none);
        assert_eq!(patch[car::NO_ITEM_REASON], "David asked in conversation");
        assert!(patch.get("backlog_item").is_none());
    }

    /// THE TWO ANSWERS CAN CONTRADICT EACH OTHER, and the contradiction
    /// must not default to closing an item with work outstanding.
    ///
    /// `boss car open --backlog-item X` writes the CLOSING edge at build
    /// start; the gate is then given `--park-partial-item` or
    /// `--park-no-item`, which say the opposite. The gate's answer is the
    /// later one and made with the finished diff, and the two errors are
    /// not symmetrical — an item left open costs one human close, an item
    /// closed with work outstanding buries the work (e1325456). So the
    /// edge goes, the id survives under `PARTIAL_ITEM`, and the car says
    /// in prose why.
    #[test]
    fn a_gate_that_says_do_not_close_supersedes_an_edge_the_open_wrote() {
        let opened = {
            let mut c = building();
            c["metadata"]["backlog_item"] = json!("cf0f5e2d-0000-0000-0000-000000000000");
            c
        };
        let mut inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "t".into(),
            verified: "v".into(),
            backlog_item: None,
            item_provenance: car::item_provenance(None, Some("one piece of a bigger item")),
            delivery_channel: None,
            receipt: Receipt {
                raw: "{}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: serde_json::Map::new(),
        };
        let patch = adopt_patch(&opened, &inputs);
        assert!(
            patch["backlog_item"].is_null(),
            "present-and-null is the metadata door's DELETE — the arrival rule must find \
             no edge to close: {patch}"
        );
        assert_eq!(
            patch[car::PARTIAL_ITEM],
            "cf0f5e2d-0000-0000-0000-000000000000",
            "the id the open named survives under the key no rule follows"
        );
        let why = patch["item_answer_superseded"].as_str().unwrap_or_default();
        assert!(why.contains("cf0f5e2d"), "names the item: {why}");
        assert!(
            why.contains("--park-no-item") && why.contains("--park-backlog-item"),
            "names what was answered and how to undo it: {why}"
        );

        // A gate that names its own partial item keeps ITS id, not the
        // open's — the later, more specific statement.
        inputs.item_provenance =
            car::item_provenance(Some("aaaa1111-0000-0000-0000-000000000000"), None);
        let patch = adopt_patch(&opened, &inputs);
        assert_eq!(
            patch[car::PARTIAL_ITEM],
            "aaaa1111-0000-0000-0000-000000000000"
        );
        assert!(patch["backlog_item"].is_null());
    }

    /// AND NOTHING IS SUPERSEDED WHEN THE ANSWERS AGREE. The common case
    /// must not touch the edge: a destructive write that fires when it
    /// should not is worse than the drop it was written to prevent.
    #[test]
    fn an_agreeing_or_absent_answer_supersedes_nothing() {
        let opened = {
            let mut c = building();
            c["metadata"]["backlog_item"] = json!("cf0f5e2d-0000-0000-0000-000000000000");
            c
        };
        let base = |backlog_item: Option<&str>,
                    provenance: serde_json::Map<String, Value>|
         -> AutoParkInputs {
            AutoParkInputs {
                branch: BRANCH.to_string(),
                summary: "s".into(),
                excludes: "e".into(),
                test: "t".into(),
                verified: "v".into(),
                backlog_item: backlog_item.map(str::to_string),
                item_provenance: provenance,
                delivery_channel: None,
                receipt: Receipt {
                    raw: "{}".into(),
                    head: "deadbeef".into(),
                    mode: "full".into(),
                },
                proof: serde_json::Map::new(),
            }
        };
        // The gate names the closing edge too: agreement, not conflict.
        let agree = base(
            Some("cf0f5e2d-0000-0000-0000-000000000000"),
            serde_json::Map::new(),
        );
        assert!(supersede(&opened, &agree).is_empty());
        // The gate carries no provenance at all.
        assert!(supersede(&opened, &base(None, serde_json::Map::new())).is_empty());
        // A partial answer, but the open named no edge — nothing to
        // supersede, and the provenance rides on its own.
        let partial = base(
            None,
            car::item_provenance(Some("cf0f5e2d-0000-0000-0000-000000000000"), None),
        );
        assert!(supersede(&building(), &partial).is_empty());
        // Both set at once can only come from a hand-edited packet. The
        // closing edge is the safer thing to keep, so it is kept.
        let both = base(
            Some("cf0f5e2d-0000-0000-0000-000000000000"),
            car::item_provenance(Some("aaaa1111"), None),
        );
        assert!(supersede(&opened, &both).is_empty());
    }

    /// THE DECLARED EDGE RIDES ITS OWN WRITE. `backlog_item` is
    /// ref-checked by a DB trigger that answers 400, and 400 maps to
    /// `Downstream`, which REDELIVERS — so an unresolvable edge folded
    /// into the proof-intent patch would retry the adopt forever and cost
    /// the car its probe on every attempt.
    #[test]
    fn the_backlog_edge_is_a_separate_best_effort_write() {
        let inputs = AutoParkInputs {
            branch: BRANCH.to_string(),
            summary: "s".into(),
            excludes: "e".into(),
            test: "t".into(),
            verified: "v".into(),
            backlog_item: Some("be025b44-2725-4db5-90d9-f16aba3844c6".into()),
            item_provenance: serde_json::Map::new(),
            delivery_channel: None,
            receipt: Receipt {
                raw: "{}".into(),
                head: "deadbeef".into(),
                mode: "full".into(),
            },
            proof: car::proof_intent(Some("echo hi"), Some("hi"), None),
        };
        let patch = adopt_patch(&building(), &inputs);
        assert!(
            patch.get("backlog_item").is_none(),
            "a refused edge must not be able to cost the car its proof intent"
        );
        assert_eq!(patch[car::PROOF_PROBE], "echo hi");
        // Nothing is NULLED: the metadata door deletes a null key, and a
        // delivery channel the open recorded must survive a green that
        // could not classify the diff.
        assert!(patch.get("delivery_channel").is_none());

        let edge = adopt_edge_patch(&building(), &inputs).expect("the edge is written");
        assert_eq!(edge["backlog_item"], "be025b44-2725-4db5-90d9-f16aba3844c6");

        // An open given `--backlog-item` already wrote it, ref-checked, at
        // the POST. Nothing to re-write.
        let mut linked = building();
        linked["metadata"]["backlog_item"] = json!("be025b44-2725-4db5-90d9-f16aba3844c6");
        assert!(adopt_edge_patch(&linked, &inputs).is_none());

        // And a green carrying no intent writes no edge at all.
        let mut unlinked = inputs;
        unlinked.backlog_item = None;
        assert!(adopt_edge_patch(&building(), &unlinked).is_none());
    }

    /// The earlier answers do not move. A car aboard a train, at the
    /// dock, or landed is still reported as such — the building case is
    /// asked LAST, after every guard that was measured first.
    #[test]
    fn the_guards_measured_first_still_answer_first() {
        let mut boarded = building();
        boarded["metadata"]["train"] = json!("d72ecdb9");
        boarded["steps"][3]["status"] = json!("completed");
        boarded["steps"][4]["status"] = json!("ready");
        let open = [building(), boarded];
        assert!(
            matches!(park_action(&open, &open, BRANCH), ParkAction::Skip(_)),
            "a branch in transit is still skipped"
        );

        let mut landed = building();
        landed["metadata"]["merged"] = json!("true");
        let all = [landed];
        match park_action(&all, &all, BRANCH) {
            ParkAction::Skip(p) => assert_eq!(p["park_skipped"], "landed"),
            other => panic!("a landed branch records the landing: {other:?}"),
        }
    }

    /// A RERAILED building car answers on the branch it CARRIES, not the
    /// one it was filed under — the read this handler was already fixed
    /// once for (car 235157a5, backlog 02165b1d).
    #[test]
    fn a_rerailed_building_car_is_adopted_not_twinned() {
        let mut rerailed = building();
        rerailed["metadata"]["branch"] = json!("feat/rerailed");
        let open = [rerailed];
        match park_action(&open, &open, "feat/rerailed") {
            ParkAction::Adopt(c) => {
                assert_eq!(c["id"], "be025b44-0000-0000-0000-000000000000");
                assert_eq!(
                    c["subject"]["id"], BRANCH,
                    "its subject is still the filing branch — which is why the read \
                     must not narrow by subject"
                );
            }
            other => panic!("the rerailed building car is the one to finish: {other:?}"),
        }
    }
}
