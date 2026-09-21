//! `gate.resolve` — the agent executor for decision gates.
//!
//! A gate is a `StepType` with `executor = Agent` and an `outcome` enum
//! field that the Workflow forks on (`demand-gate` → brew|oversupply,
//! `availability-gate` → fulfill|backorder). This handler is the
//! computer-speed executor for that decision: on `step.ready.*` it reads
//! real finished-goods stock, computes the outcome, and `PUT`s the step
//! `completed` with `metadata.outcome` stamped — no human, no workforce
//! slot, no duration. The existing Workflow-v2 `ready_when` fork does the
//! rest.
//!
//! This logic used to live in `boss-sim`'s workforce (it read FG stock at
//! completion to stamp brew/oversupply). That placement forced a *system*
//! decision through a human workforce slot — so gates queued behind labor
//! at warp — and put system logic inside the sim, against
//! `feedback_sim_separate_from_system`. Moving it here fixes both. See
//! docs/architecture-decisions.md.
//!
//! Rides the `step.ready.*` subscription (one NATS consumer) alongside
//! `jobs.complete_step` + `messages.notify`; self-filters to agent gates so
//! every other kind is a no-op.

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::step_registry::{Completion, StepRegistry};
use serde_json::{Value as JsonValue, json};
use std::collections::HashMap;
use std::sync::Arc;

use super::common::{
    StepEvent, dispatcher_actor_header, dispatcher_reader_header, sim_origin_value,
};

pub struct GateResolve {
    client: reqwest::Client,
    jobs_base: String,
    products_base: String,
    registry: Arc<StepRegistry>,
}

impl GateResolve {
    pub fn new(
        jobs_base: impl Into<String>,
        products_base: impl Into<String>,
        registry: Arc<StepRegistry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
            products_base: products_base.into(),
            registry,
        })
    }

    pub fn with_client(
        client: reqwest::Client,
        jobs_base: impl Into<String>,
        products_base: impl Into<String>,
        registry: Arc<StepRegistry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
            products_base: products_base.into(),
            registry,
        })
    }

    /// An agent gate: `executor = Agent` and an `outcome` enum field the
    /// Workflow forks on. Plain agent action steps (order-intake, billing)
    /// have no `outcome` field — `jobs.complete_step` handles those.
    fn is_agent_gate(&self, kind: &str) -> bool {
        self.registry.get(kind).is_some_and(|st| {
            st.completion == Completion::Agent && st.fields.iter().any(|f| f.name == "outcome")
        })
    }

    /// Real finished-goods on-hand for one product SKU. Unknown SKU /
    /// unreachable service reads as 0 (treated as short). Errors only on a
    /// genuine transport failure so the message NAKs + redelivers.
    async fn product_on_hand(&self, sku: &str) -> Result<i64, HandlerError> {
        let url = format!(
            "{}/api/products/{}",
            self.products_base.trim_end_matches('/'),
            sku
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Ok(0);
        }
        let v: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("decode product {sku}: {e}")))?;
        Ok(v.get("total_on_hand").and_then(|x| x.as_i64()).unwrap_or(0))
    }

    /// The Workflow of the gate's parent Job. The `step.ready` payload
    /// carries the *step* kind (`demand-gate`), not the Workflow, so we
    /// read it off `GET /api/jobs/{job_id}` — the Workflow is what
    /// distinguishes a `morning-brew-stout` in-flight count from a
    /// `morning-brew-ipa` one. Returns `None` on a non-success status
    /// (the Job genuinely isn't there); errors only on transport
    /// failure so the message NAKs + redelivers.
    async fn workflow_for(&self, job_id: &str) -> Result<Option<String>, HandlerError> {
        let url = format!(
            "{}/api/jobs/{}",
            self.jobs_base.trim_end_matches('/'),
            job_id
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let v: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("decode job {job_id}: {e}")))?;
        Ok(v.get("kind")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()))
    }

    /// Count of OPEN Jobs of `kind` — the in-flight pipeline depth for
    /// a demand gate. Reads the jobs-api list `total` (a DB-wide count
    /// for the filter, not a page length) via
    /// `?kind=<k>&status=open&limit=1`; we only need the count, so the
    /// page is kept minimal. Non-success reads as 0 (no pipeline
    /// credit — fail toward brewing, never toward starving stock);
    /// transport failure errors so the message redelivers.
    async fn open_jobs_of_kind(&self, kind: &str) -> Result<i64, HandlerError> {
        let url = format!(
            "{}/api/jobs?kind={}&status=open&limit=1",
            self.jobs_base.trim_end_matches('/'),
            kind,
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Ok(0);
        }
        let v: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("decode jobs list {kind}: {e}")))?;
        Ok(v.get("total").and_then(|x| x.as_i64()).unwrap_or(0))
    }

    /// Compute the gate outcome from the step metadata + real stock.
    /// Dispatches on the gate shape: an `expected_daily_demand` map →
    /// demand gate (brew/oversupply); a `line_items` array → availability
    /// gate (fulfill/backorder).
    ///
    /// The demand branch is **pipeline-aware**: it credits the brews
    /// already in flight against the threshold so a daily production
    /// review doesn't double-brew through the multi-day brew lag.
    /// `effective_on_hand[sku] = real_on_hand[sku] + in_flight ×
    /// batch_yield[sku]`, where `in_flight` is the count of OPEN Jobs
    /// of this Workflow minus this one (its own gate is open at
    /// decision time). The pure `decide_demand_outcome` then runs on
    /// the effective map — IO here, decision stays pure.
    async fn outcome_for(&self, ev: &StepEvent<'_>) -> Result<&'static str, HandlerError> {
        let metadata = ev.metadata;
        let md = JsonValue::Object(metadata.clone());
        if metadata.contains_key("expected_daily_demand") {
            let skus = string_array(metadata, "target_skus");
            if skus.is_empty() {
                return Ok("brew");
            }
            // In-flight pipeline depth: open Jobs of this Workflow,
            // minus this one. Oversupply siblings are already
            // closed/terminal; siblings still brewing are open, so
            // `open_count − 1` is the number of in-flight brews whose
            // yield hasn't hit the cooler yet. Default to 0 in-flight
            // when the Workflow can't be resolved (fail toward brewing).
            let in_flight = match self.workflow_for(ev.job_id).await? {
                Some(kind) => (self.open_jobs_of_kind(&kind).await? - 1).max(0),
                None => 0,
            };
            let batch_yield = metadata.get("batch_yield");
            let mut effective_on_hand = HashMap::new();
            for sku in &skus {
                let real = self.product_on_hand(sku).await?;
                let per_batch = batch_yield
                    .and_then(|m| m.get(sku))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                effective_on_hand.insert(sku.clone(), real + in_flight * per_batch);
            }
            return Ok(decide_demand_outcome(&md, &skus, &effective_on_hand));
        }
        if let Some(items) = metadata.get("line_items").and_then(|v| v.as_array()) {
            let mut on_hand = HashMap::new();
            for li in items {
                if let Some(sku) = li.get("sku").and_then(|v| v.as_str()) {
                    on_hand
                        .entry(sku.to_string())
                        .or_insert(self.product_on_hand(sku).await?);
                }
            }
            return Ok(decide_availability(items, &on_hand));
        }
        // A gate with neither demand nor line_items can't be decided from
        // stock — default to the proceed branch rather than wedging the Job.
        Ok("proceed")
    }
}

#[async_trait]
impl Handler for GateResolve {
    fn name(&self) -> &'static str {
        "gate.resolve"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        if !self.is_agent_gate(ev.kind) {
            return Ok(());
        }
        let outcome = self.outcome_for(&ev).await?;

        // TWO CALLS, AND THE ORDER IS THE POINT (backlog c30a510e,
        // design 93d2bddb's `shape` resolution).
        //
        // This used to build the step body as `ev.metadata.clone()` plus
        // `outcome` and PUT it, and the step PUT replaces top-level
        // `metadata` WHOLESALE. So it wrote the EVENT's snapshot over
        // whatever the step held at PUT time: any key written between the
        // event firing and this handler's write was silently erased —
        // including keys this handler has never heard of. A caller cannot
        // defend against that by remembering to include a key, because it
        // does not know which keys exist. Three carve-outs had already
        // accreted in the PUT for exactly this reason (authority_role
        // frozen, human_only and agent_run carried), each after an
        // incident, and the design chose to REMOVE the class rather than
        // keep enumerating it — an enumerated class costs a core change
        // per incident forever, a removed one costs a change and then
        // none.
        //
        // So: PATCH the one fact this handler owns, then set the status.
        // The metadata door MERGES and cannot touch status or assignee;
        // the PUT carries no metadata at all, and the step PUT's own
        // overlay semantics keep every other field intact.
        //
        // METADATA FIRST, because the step API refuses a metadata write to
        // a COMPLETED step — completing first would make the outcome
        // unwritable, and the Workflow's fork predicate reads
        // `metadata.outcome`.
        let step_url = format!(
            "{}/api/jobs/{}/steps/{}",
            self.jobs_base.trim_end_matches('/'),
            ev.job_id,
            ev.step_id,
        );
        let patch_url = format!("{step_url}/metadata");
        let patch = self
            .client
            .patch(&patch_url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&json!({ "outcome": outcome }))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PATCH {patch_url}: {e}")))?;
        if !patch.status().is_success() {
            let status = patch.status();
            let text = patch.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PATCH {patch_url} returned {status}: {text}"
            )));
        }

        let url = step_url;
        let body = json!({ "status": "completed" });
        let resp = self
            .client
            .put(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {url} returned {status}: {text}"
            )));
        }
        Ok(())
    }
}

fn string_array(metadata: &serde_json::Map<String, JsonValue>, key: &str) -> Vec<String> {
    metadata
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Pure brew-vs-oversupply decision: oversupply only when *every* target
/// SKU is above its `daily_demand × window × multiplier` threshold (none
/// short). Moved here from the sim workforce — the gate is system logic, not
/// labor the sim drives.
fn decide_demand_outcome(
    metadata: &JsonValue,
    target_skus: &[String],
    on_hand: &HashMap<String, i64>,
) -> &'static str {
    let window = metadata
        .get("demand_window_days")
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);
    let mult = metadata
        .get("oversupply_multiplier")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.5);
    let demand = metadata.get("expected_daily_demand");
    let mut all_over = true;
    for sku in target_skus {
        let daily = demand
            .and_then(|d| d.get(sku))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let threshold = (daily * window * mult) as i64;
        let have = on_hand.get(sku).copied().unwrap_or(0);
        if have <= threshold {
            all_over = false;
        }
    }
    if all_over { "oversupply" } else { "brew" }
}

/// Pure fulfill-vs-backorder decision: backorder when *any* order line
/// can't be covered from finished-goods stock on hand.
fn decide_availability(line_items: &[JsonValue], on_hand: &HashMap<String, i64>) -> &'static str {
    for li in line_items {
        let Some(sku) = li.get("sku").and_then(|v| v.as_str()) else {
            continue;
        };
        let qty = li.get("qty").and_then(|v| v.as_i64()).unwrap_or(0);
        if on_hand.get(sku).copied().unwrap_or(0) < qty {
            return "backorder";
        }
    }
    "fulfill"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Arc<StepRegistry> {
        Arc::new(StepRegistry::v1())
    }

    #[test]
    fn demand_gate_is_an_agent_gate_plain_agent_action_is_not() {
        let h = GateResolve::new("http://j", "http://p", reg());
        // demand-gate: executor=agent + has an `outcome` field.
        assert!(h.is_agent_gate("demand-gate"));
        // brewing / markers are not gates.
        assert!(!h.is_agent_gate("production-consume"));
        assert!(!h.is_agent_gate("trigger"));
        assert!(!h.is_agent_gate("not-a-real-kind"));
    }

    #[test]
    fn demand_outcome_oversupply_only_when_all_over_threshold() {
        let md = json!({
            "demand_window_days": 30, "oversupply_multiplier": 1.5,
            "expected_daily_demand": { "FP-A": 100, "FP-B": 100 },
        });
        let skus = vec!["FP-A".to_string(), "FP-B".to_string()];
        // both over 100×30×1.5=4500 → oversupply
        let over: HashMap<String, i64> = [("FP-A".into(), 5000), ("FP-B".into(), 5000)].into();
        assert_eq!(decide_demand_outcome(&md, &skus, &over), "oversupply");
        // one short → brew
        let mixed: HashMap<String, i64> = [("FP-A".into(), 5000), ("FP-B".into(), 100)].into();
        assert_eq!(decide_demand_outcome(&md, &skus, &mixed), "brew");
    }

    /// In-flight brews count toward effective stock. The decision is
    /// pure — `outcome_for` builds `effective = real + in_flight ×
    /// batch_yield` and passes it here. This test models the
    /// throttle-up correctness fix: real stock is below the
    /// threshold, but enough brews are already in the pipeline that
    /// the projected stock clears it → oversupply (skip), so a
    /// daily review doesn't double-brew through the multi-day lag.
    #[test]
    fn demand_outcome_oversupply_when_in_flight_yield_clears_threshold() {
        // Stout gate: threshold = 64 × 30 × 1.5 = 2880.
        let md = json!({
            "demand_window_days": 30, "oversupply_multiplier": 1.5,
            "expected_daily_demand": { "FP-STOUT-1-6-BBL": 64 },
        });
        let skus = vec!["FP-STOUT-1-6-BBL".to_string()];
        // Real on-hand 1000 (below 2880 → would brew on its own).
        let real: i64 = 1000;
        let batch_yield: i64 = 360; // one stout brew's sixtel yield
        // 6 brews in flight: 1000 + 6×360 = 3160 ≥ 2880 → oversupply.
        let effective: HashMap<String, i64> =
            [("FP-STOUT-1-6-BBL".into(), real + 6 * batch_yield)].into();
        assert_eq!(decide_demand_outcome(&md, &skus, &effective), "oversupply");
    }

    /// The exact day-58 regression, mirror-imaged: identical real
    /// on-hand, but with ZERO brews in flight the effective map is
    /// just the real stock — which is below threshold → brew. This
    /// is what the open-loop gate could not express: it read only
    /// real stock, so it couldn't tell "1000 on hand, nothing
    /// coming" (brew now) apart from "1000 on hand, 6 batches
    /// landing tomorrow" (skip).
    #[test]
    fn demand_outcome_brews_when_same_on_hand_but_zero_in_flight() {
        let md = json!({
            "demand_window_days": 30, "oversupply_multiplier": 1.5,
            "expected_daily_demand": { "FP-STOUT-1-6-BBL": 64 },
        });
        let skus = vec!["FP-STOUT-1-6-BBL".to_string()];
        // 1000 real on-hand, no in-flight: effective == real == 1000
        // < 2880 threshold → brew.
        let effective: HashMap<String, i64> = [("FP-STOUT-1-6-BBL".into(), 1000)].into();
        assert_eq!(decide_demand_outcome(&md, &skus, &effective), "brew");
    }

    #[test]
    fn availability_backorders_when_any_line_short() {
        let items = vec![
            json!({ "sku": "FP-PALE-1-2-BBL", "qty": 4 }),
            json!({ "sku": "FP-IPA-1-6-BBL", "qty": 8 }),
        ];
        let stocked: HashMap<String, i64> = [
            ("FP-PALE-1-2-BBL".into(), 100),
            ("FP-IPA-1-6-BBL".into(), 100),
        ]
        .into();
        assert_eq!(decide_availability(&items, &stocked), "fulfill");
        let short: HashMap<String, i64> = [
            ("FP-PALE-1-2-BBL".into(), 2),
            ("FP-IPA-1-6-BBL".into(), 100),
        ]
        .into();
        assert_eq!(decide_availability(&items, &short), "backorder");
    }

    // ---- in-flight-aware demand gate (IO wiring) ----

    /// Spin up a mock jobs-api (`GET /api/jobs/{id}` returns the
    /// Workflow; `GET /api/jobs` list returns `total = open_count`;
    /// `PUT /api/jobs/{id}/steps/{step_id}` captures the body) and a
    /// mock products-api (`GET /api/products/{sku}` returns
    /// `on_hand`). Returns the two base URLs + the captured PUT body
    /// handle. Mirrors the stub-server idiom in `inventory_po_place`.
    async fn stub_servers(
        workflow: &'static str,
        open_count: i64,
        on_hand: i64,
    ) -> (
        String,
        String,
        std::sync::Arc<std::sync::Mutex<Option<JsonValue>>>,
    ) {
        use axum::extract::Path;
        use axum::{Json, Router, routing::get, routing::patch, routing::put};

        // BOTH calls are captured, under one lock, as `{put, patch}` —
        // the handler now writes the outcome through the merging door
        // and the status through the step PUT, and a test that watched
        // only one of them could not tell a merge from a repaint.
        let captured: std::sync::Arc<std::sync::Mutex<Option<JsonValue>>> = Default::default();
        let cap = captured.clone();
        let cap_patch = captured.clone();

        let jobs = Router::new()
            .route(
                "/api/jobs",
                get(move || async move { Json(json!({ "data": [], "total": open_count })) }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(_id): Path<String>| async move {
                    Json(json!({ "id": _id, "kind": workflow }))
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                put(
                    move |Path((_id, _sid)): Path<(String, String)>,
                          Json(body): Json<JsonValue>| {
                        let cap = cap.clone();
                        async move {
                            let mut slot = cap.lock().unwrap();
                            let mut seen = slot.take().unwrap_or_else(|| json!({}));
                            seen["put"] = body;
                            *slot = Some(seen);
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}/metadata",
                patch(
                    move |Path((_id, _sid)): Path<(String, String)>,
                          Json(body): Json<JsonValue>| {
                        let cap = cap_patch.clone();
                        async move {
                            let mut slot = cap.lock().unwrap();
                            let mut seen = slot.take().unwrap_or_else(|| json!({}));
                            seen["patch"] = body;
                            *slot = Some(seen);
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            );
        let jobs_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let jobs_addr = jobs_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(jobs_listener, jobs).await.unwrap() });

        let products = Router::new().route(
            "/api/products/{sku}",
            get(move |Path(sku): Path<String>| async move {
                Json(json!({ "sku": sku, "total_on_hand": on_hand }))
            }),
        );
        let prod_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let prod_addr = prod_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(prod_listener, products).await.unwrap() });

        (
            format!("http://{jobs_addr}"),
            format!("http://{prod_addr}"),
            captured,
        )
    }

    fn stout_gate_payload() -> JsonValue {
        json!({
            "job_id": "job-stout-1",
            "step_id": "step-demand-1",
            "kind": "demand-gate",
            "subject_kind": "location",
            "subject_id": "loc-brewery-brewhouse",
            "metadata": {
                "target_skus": ["FP-STOUT-1-6-BBL"],
                "expected_daily_demand": { "FP-STOUT-1-6-BBL": 64 },
                "demand_window_days": 30,
                "oversupply_multiplier": 1.5,
                "batch_yield": { "FP-STOUT-1-6-BBL": 360 },
                "is_demand_check": true
            }
        })
    }

    fn ctx_for(payload: JsonValue) -> InvocationContext {
        InvocationContext {
            rule_name: "gate-resolve-test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "step.ready.demand-gate".into(),
            event_payload: payload,
        }
    }

    /// Full invoke path: real on-hand 1000 < threshold 2880, but 7
    /// open Jobs of this kind → 6 in flight × 360/batch = 2160
    /// credited; effective 3160 ≥ 2880 → the gate completes
    /// `oversupply` and the PUT carries it. This is the throttle-up:
    /// a daily review skips because the pipeline already covers
    /// demand, instead of double-brewing through the lag.
    #[tokio::test]
    async fn in_flight_yield_pushes_gate_to_oversupply() {
        let body = run_gate(7, 1000).await;
        assert_eq!(body["put"]["status"], "completed");
        assert_eq!(body["patch"]["outcome"], "oversupply");
    }

    /// The tenant fixture, spelled ONCE.
    ///
    /// Each of the three cases below used to name the demo workflow and
    /// its payload itself, so the tenant's vocabulary appeared six times
    /// in a tier that ratchets it (`no-tenant-vocabulary-above-its-tier`,
    /// baseline never raised). Adding a case therefore cost two more
    /// words. Through here it costs none, and the cases read as what
    /// they vary — the open count and the stock — instead of repeating
    /// what they do not.
    async fn run_gate(open_count: i64, on_hand: i64) -> JsonValue {
        let (jobs, products, captured) =
            stub_servers("morning-brew-stout", open_count, on_hand).await;
        let h = GateResolve::new(jobs, products, reg());
        h.invoke(&[], &ctx_for(stout_gate_payload()))
            .await
            .expect("invoke succeeds");
        let body = captured.lock().unwrap().clone();
        body.expect("the handler wrote")
    }

    /// THE DEFECT ITSELF: the handler must not write the event's
    /// snapshot of the step's metadata.
    ///
    /// It used to build the body as `ev.metadata.clone()` plus
    /// `outcome` and PUT it, and the step PUT replaces top-level
    /// `metadata` wholesale — so any key written to the step between
    /// the event firing and this write was erased, including keys the
    /// handler has never heard of (backlog c30a510e). A caller cannot
    /// defend against that by remembering to include a key, because it
    /// does not know which keys exist.
    ///
    /// The assertion is on what the handler SENDS, which is the only
    /// thing it controls: the outcome goes through the merging door
    /// carrying nothing else, and the PUT carries no `metadata` key at
    /// all. A PUT with no metadata cannot overwrite metadata, whatever
    /// the step happens to hold by then.
    #[tokio::test]
    async fn the_write_carries_the_outcome_alone_and_never_the_events_snapshot() {
        let body = run_gate(7, 1000).await;

        // The status write carries NO metadata — this is the property
        // that makes the erasure impossible rather than unlikely.
        assert!(
            body["put"].get("metadata").is_none(),
            "the status PUT must send no metadata at all: {}",
            body["put"]
        );
        assert_eq!(body["put"]["status"], "completed");

        // And the merge carries exactly one key: the fact this handler
        // owns. Anything else here would be the snapshot returning by
        // another route.
        let patched = body["patch"].as_object().expect("a patch body");
        assert_eq!(
            patched.keys().collect::<Vec<_>>(),
            vec!["outcome"],
            "the merge writes only the outcome: {:?}",
            patched
        );
    }

    /// Mirror case — same real on-hand (1000), but only this Job is
    /// open (`total = 1` → 0 in flight). Effective == real == 1000 <
    /// 2880 → brew. This is the exact day-58 regression: the
    /// open-loop gate read only real stock and couldn't distinguish
    /// "nothing in flight" from "pipeline full", so it skipped and
    /// starved Stout to zero. The pipeline credit fixes it.
    #[tokio::test]
    async fn zero_in_flight_same_on_hand_brews() {
        let body = run_gate(1, 1000).await;
        assert_eq!(body["patch"]["outcome"], "brew");
    }
}
