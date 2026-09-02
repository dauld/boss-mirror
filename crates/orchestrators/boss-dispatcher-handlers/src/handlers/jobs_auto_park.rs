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

use super::common::{StepEvent, api_client, dispatcher_actor_header, get_json};

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
    receipt: Receipt,
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
        backlog_item: md
            .get("park_backlog_item")
            .and_then(Value::as_str)
            .map(str::to_string),
        receipt,
    })
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

/// PUT a body, mapping non-2xx the same way. Completing a car step is a
/// PUT, which the shared POST helper does not cover.
async fn put_json(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<(), HandlerError> {
    let resp = client
        .put(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", super::common::sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("PUT {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            HandlerError::Permanent(format!("PUT {url} returned {status}: {text}"))
        } else {
            HandlerError::Downstream(format!("PUT {url} returned {status}: {text}"))
        });
    }
    Ok(())
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

        let now = boss_clock_client::now_from(&self.clock).await;

        // File the car: POST the packet, then complete its three steps
        // with the shared builder — the same sequence `boss park::run`
        // performs, receipt verbatim.
        let body = car::car_body(
            &inputs.branch,
            &inputs.summary,
            inputs.backlog_item.as_deref(),
        );
        let created = post_json_return(
            &self.client,
            &format!("{}/api/jobs", self.base()),
            &body,
            &ctx.rule_name,
        )
        .await?;
        let car_id = created
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| HandlerError::Downstream("auto-park: create returned no id".into()))?;

        let car = get_json(
            &self.client,
            &format!("{}/api/jobs/{}", self.base(), car_id),
            &ctx.rule_name,
        )
        .await?;
        let steps = car
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| HandlerError::Downstream("auto-park: car has no steps".into()))?;

        // In order (scope → build → gate): each completion re-evaluates
        // readiness so the next is ready, the same order `boss park` uses.
        for (title, meta) in car::step_fields(
            &inputs.summary,
            &inputs.excludes,
            &inputs.test,
            &inputs.verified,
            &inputs.receipt,
            now,
        ) {
            let step_id = steps
                .iter()
                .find(|s| s.get("title").and_then(Value::as_str) == Some(title))
                .and_then(|s| s.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    HandlerError::Downstream(format!("auto-park: car missing step '{title}'"))
                })?;
            put_json(
                &self.client,
                &format!("{}/api/jobs/{}/steps/{}", self.base(), car_id, step_id),
                &json!({"status": "completed", "metadata": meta}),
                &ctx.rule_name,
            )
            .await?;
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
