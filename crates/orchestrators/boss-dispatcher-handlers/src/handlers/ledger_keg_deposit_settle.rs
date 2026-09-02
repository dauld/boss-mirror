//! `ledger.keg_deposit.settle` — a reconciled keg fleet settles its
//! deposit on the books (93f936b9; David's Q1 decision, 2026-08-22:
//! "Let's go for the full balance-sheet model").
//!
//! Fires on `jobs.job.closed` gated `kind = "keg-return" AND outcome =
//! "completed"`. The keg-return protocol's field-bearing steps are
//! plain `task` kind, so there is no precise `step.done.<kind>` topic
//! to ride — the packet's terminal close is the honest trigger (the
//! spawn-car-on-sweep-remediated reasoning), and it delivers both legs
//! at once:
//!
//! - `log-fleet-out` carries `kegs_out` + `deposit_cents` and the date
//!   the fleet shipped (its own `completed_on`);
//! - `receive-returns` carries `kegs_returned` + `kegs_lost` and the
//!   date the fleet came back.
//!
//! One POST to `/api/ledger/keg-deposit-settlements` books both:
//! DR 1000 / CR 2400 Keg Deposits Payable at the fleet-out date, then
//! DR 2400 / CR 1000 refund + CR 4150 forfeiture at the return date —
//! each fact stamped with its own `happened_on`, so the ledger
//! timeline carries the in-field window even though posting happens at
//! reconciliation. The cost of the close-time trigger is that a fleet
//! still in the field has no liability booked yet; per-step posting
//! needs dedicated StepType kinds on the protocol, which is registry
//! data, not code — a protocol edit away if the lag matters.
//!
//! ## Idempotence
//!
//! JetStream is at-least-once. The ledger keys both facts on the job
//! id (`keg-charge-<job_id>` / `keg-release-<job_id>`) through the
//! financial_facts `(kind, source_table, source_id)` unique index, so
//! a redelivered close re-POSTs and gets a no-op 200.
//!
//! ## Failure semantics
//!
//! A closed-`completed` keg-return whose steps are missing their
//! required-at-done fields cannot happen through the step API
//! (validators run at completion), so a payload that fails to build is
//! malformed history — `HandlerError::Permanent`, identical on every
//! redelivery. The ledger's own 422 (e.g. counts that don't conserve)
//! Terms the same way via `post_json`'s house contract.

use super::common::{self, dispatcher_reader_header, sim_origin_value};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use serde_json::json;
use std::sync::Arc;

pub struct LedgerKegDepositSettle {
    client: reqwest::Client,
    jobs_base: String,
    ledger_base: String,
}

impl LedgerKegDepositSettle {
    pub fn new(jobs_base: impl Into<String>, ledger_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
            ledger_base: ledger_base.into(),
        })
    }

    async fn get_job(&self, job_id: &str, rule: &str) -> Result<serde_json::Value, HandlerError> {
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
            .map_err(|e| HandlerError::Downstream(format!("GET {url} (rule {rule}): {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
    }
}

#[async_trait]
impl Handler for LedgerKegDepositSettle {
    fn name(&self) -> &'static str {
        "ledger.keg_deposit.settle"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let job_id = ctx
            .event_payload
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Permanent("jobs.job.closed payload missing id".into()))?;

        let job = self.get_job(job_id, &ctx.rule_name).await?;
        let body = settlement_body(&job).map_err(HandlerError::Permanent)?;

        let url = format!(
            "{}/api/ledger/keg-deposit-settlements",
            self.ledger_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}

/// Read one required integer metadata field off a step.
fn step_i64(step: &serde_json::Value, slug: &str, key: &str) -> Result<i64, String> {
    step.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("step {slug} missing integer metadata {key}"))
}

/// Find a completed step by `spec_slug`.
fn completed_step<'a>(
    job: &'a serde_json::Value,
    slug: &str,
) -> Result<&'a serde_json::Value, String> {
    job.get("steps")
        .and_then(|s| s.as_array())
        .and_then(|steps| {
            steps.iter().find(|s| {
                s.get("spec_slug").and_then(|v| v.as_str()) == Some(slug)
                    && s.get("status").and_then(|v| v.as_str()) == Some("completed")
            })
        })
        .ok_or_else(|| format!("no completed `{slug}` step on the job"))
}

/// Build the `/api/ledger/keg-deposit-settlements` request from a
/// closed keg-return job, or say exactly which leg is malformed.
/// Pure — the unit tests below pin the contract.
fn settlement_body(job: &serde_json::Value) -> Result<serde_json::Value, String> {
    let job_id = job
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("job missing id")?;
    let account_id = job
        .get("subject")
        .and_then(|s| s.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let fleet_out = completed_step(job, "log-fleet-out")?;
    let returns = completed_step(job, "receive-returns")?;

    let shipped_on = fleet_out
        .get("completed_on")
        .and_then(|v| v.as_str())
        .ok_or("step log-fleet-out missing completed_on")?;
    let returned_on = returns
        .get("completed_on")
        .and_then(|v| v.as_str())
        .ok_or("step receive-returns missing completed_on")?;

    Ok(json!({
        "job_id": job_id,
        "account_id": account_id,
        "kegs_out": step_i64(fleet_out, "log-fleet-out", "kegs_out")?,
        "kegs_returned": step_i64(returns, "receive-returns", "kegs_returned")?,
        "kegs_lost": step_i64(returns, "receive-returns", "kegs_lost")?,
        "deposit_cents": step_i64(fleet_out, "log-fleet-out", "deposit_cents")?,
        "shipped_on": shipped_on,
        "returned_on": returned_on,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keg_return_job() -> serde_json::Value {
        json!({
            "id": "job-77",
            "kind": "keg-return",
            "subject": { "subject_kind": "account", "id": "account-00042" },
            "steps": [
                {
                    "spec_slug": "log-fleet-out",
                    "status": "completed",
                    "completed_on": "2026-03-01",
                    "metadata": { "kegs_out": 10, "deposit_cents": 30_000 },
                },
                {
                    "spec_slug": "receive-returns",
                    "status": "completed",
                    "completed_on": "2026-03-15",
                    "metadata": { "kegs_returned": 7, "kegs_lost": 3 },
                },
            ],
        })
    }

    #[test]
    fn body_carries_both_legs_with_their_own_dates() {
        let body = settlement_body(&keg_return_job()).unwrap();
        assert_eq!(body["job_id"], "job-77");
        assert_eq!(body["account_id"], "account-00042");
        assert_eq!(body["kegs_out"], 10);
        assert_eq!(body["kegs_returned"], 7);
        assert_eq!(body["kegs_lost"], 3);
        assert_eq!(body["deposit_cents"], 30_000);
        assert_eq!(body["shipped_on"], "2026-03-01");
        assert_eq!(body["returned_on"], "2026-03-15");
    }

    #[test]
    fn missing_returns_step_names_the_gap() {
        let mut job = keg_return_job();
        job["steps"].as_array_mut().unwrap().truncate(1);
        let err = settlement_body(&job).unwrap_err();
        assert!(err.contains("receive-returns"), "{err}");
    }

    #[test]
    fn incomplete_fleet_out_step_does_not_count() {
        let mut job = keg_return_job();
        job["steps"][0]["status"] = json!("active");
        let err = settlement_body(&job).unwrap_err();
        assert!(err.contains("log-fleet-out"), "{err}");
    }

    #[test]
    fn missing_count_field_names_step_and_field() {
        let mut job = keg_return_job();
        job["steps"][1]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("kegs_lost");
        let err = settlement_body(&job).unwrap_err();
        assert!(
            err.contains("receive-returns") && err.contains("kegs_lost"),
            "{err}"
        );
    }
}
