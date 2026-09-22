//! `ledger.tax.accrue` — POST a standalone tax accrual to
//! `/api/ledger/tax-accruals` (DR an expense account / CR a liability
//! account) the moment a production step completes.
//!
//! Used for federal beer excise tax: each brew batch's taxed barrels
//! (`excise_bbl`, from the package step's metadata) post as
//! DR Excise Tax Expense / CR Excise Tax Payable, exactly the way sales
//! tax accrues per invoice line. The quarterly excise-tax-filing
//! Workflow later drains the liability to Cash. The liability is
//! credited by this production source fact, not at filing time, so the
//! filing's `period-excise` derive_basis sums that account's credit
//! balance for the period.
//!
//! THE ACCOUNTS ARE THE KIND'S (backlog c0b83e13). The rule's args
//! carry `kind` — a `tax_kinds` row on the instance — and the ledger
//! resolves both accounts from that row. Until 2026-09-22 they were
//! `liability_account = "2320"` / `expense_account = "6550"` args: the
//! demo tenant's two codes, spelled in its own rule file and in the
//! dispatcher schema seed — a third and fourth copy of a fact the
//! tenant's row already held.
//!
//! THE RATE IS REGISTRY DATA (brewery-fidelity Q4, decided
//! 2026-08-22). The handler sends the QUANTITY, never an amount: the
//! ledger resolves the jurisdiction's graduated tier schedule from
//! `excise_rate_schedules` against year-to-date taxed barrels — the
//! real TTB curve is $3.50/bbl for the first 60,000 bbl of the
//! calendar year, $16.00/bbl above, and the old flat-350 arg
//! understated the demo tenant's liability ~3.7×. The rule's
//! `rate_cents_per_bbl` arg survives as the flat FALLBACK the ledger
//! applies (loudly) when the jurisdiction has no registry row, so
//! pre-registry rules.toml deployments accrue exactly as before.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde_json::json;
use std::sync::Arc;

pub struct LedgerTaxAccrue {
    client: reqwest::Client,
    ledger_base: String,
}

impl LedgerTaxAccrue {
    pub fn new(ledger_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            ledger_base: ledger_base.into(),
        })
    }
}

#[async_trait]
impl Handler for LedgerTaxAccrue {
    fn name(&self) -> &'static str {
        "ledger.tax.accrue"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;

        // Taxable barrels for this batch live in step metadata
        // (`excise_bbl`, seeded per package step from the brew's
        // produces_products volume). A batch with no/zero taxable
        // barrels is a no-op — nothing to accrue.
        let excise_bbl = step
            .metadata
            .get("excise_bbl")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if excise_bbl <= 0 {
            return Ok(());
        }

        // Optional since the registry landed: present = the flat rate
        // the ledger falls back to for a jurisdiction with no
        // `excise_rate_schedules` row; absent = registry-or-400.
        let fallback_rate_cents_per_bbl = arg(args, "rate_cents_per_bbl").and_then(|v| match v {
            Value::Int(i) => Some(*i),
            _ => None,
        });
        let kind = arg(args, "kind")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| HandlerError::Downstream("kind arg missing or not a string".into()))?;
        let jurisdiction = arg(args, "jurisdiction")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                HandlerError::Downstream("jurisdiction arg missing or not a string".into())
            })?;

        let posted_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;

        let Some(body) = accrual_body(
            step.step_id,
            excise_bbl,
            fallback_rate_cents_per_bbl,
            &kind,
            &jurisdiction,
            posted_on,
        ) else {
            return Ok(());
        };

        let url = format!(
            "{}/api/ledger/tax-accruals",
            self.ledger_base.trim_end_matches('/')
        );
        common::post_json(&self.client, &url, &body, &ctx.rule_name).await
    }
}

/// Build the `/api/ledger/tax-accruals` request for one completed
/// production step, or `None` when there is nothing to accrue.
///
/// The handler ships the QUANTITY (`excise_bbl`) and lets the ledger
/// resolve the rate from the `excise_rate_schedules` registry — it
/// never computes an amount, because the graduated tier split needs
/// the jurisdiction's year-to-date taxed barrels, which only the
/// ledger can read transactionally. `fallback_rate_cents_per_bbl`
/// (the rule's legacy flat `rate_cents_per_bbl` arg) rides along only
/// when the rule declares it.
fn accrual_body(
    step_id: &str,
    excise_bbl: i64,
    fallback_rate_cents_per_bbl: Option<i64>,
    kind: &str,
    jurisdiction: &str,
    posted_on: chrono::NaiveDate,
) -> Option<serde_json::Value> {
    if excise_bbl <= 0 {
        return None;
    }
    let mut body = json!({
        "id": format!("excise-{step_id}"),
        "kind": kind,
        "excise_bbl": excise_bbl,
        "posted_on": posted_on,
        "jurisdiction": jurisdiction,
    });
    if let Some(rate) = fallback_rate_cents_per_bbl {
        body["fallback_rate_cents_per_bbl"] = json!(rate);
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()
    }

    #[test]
    fn zero_or_negative_barrels_build_no_body() {
        assert_eq!(
            accrual_body("s1", 0, Some(350), "production-tax", "US-FEDERAL", day()),
            None
        );
        assert_eq!(
            accrual_body("s1", -3, Some(350), "production-tax", "US-FEDERAL", day()),
            None
        );
    }

    #[test]
    fn body_carries_the_quantity_basis_and_the_flat_fallback() {
        let body = accrual_body(
            "step-9",
            105,
            Some(350),
            "production-tax",
            "US-FEDERAL",
            day(),
        )
        .unwrap();
        assert_eq!(body["id"], "excise-step-9");
        assert_eq!(body["excise_bbl"], 105);
        assert_eq!(body["fallback_rate_cents_per_bbl"], 350);
        assert_eq!(body["jurisdiction"], "US-FEDERAL");
        assert_eq!(body["posted_on"], "2026-03-01");
        // The ledger owns the rate: the handler must NOT send a
        // caller-computed amount, or the registry curve would never run.
        assert!(body.get("amount_cents").is_none());
    }

    #[test]
    fn the_body_names_the_kind_and_neither_account() {
        // The tax_kinds row is the one definition of both accounts
        // (c0b83e13): a handler that still spelled them would be a
        // fourth copy, and the door refuses the fields outright.
        let body = accrual_body(
            "step-9",
            105,
            Some(350),
            "production-tax",
            "US-FEDERAL",
            day(),
        )
        .unwrap();
        assert_eq!(body["kind"], "production-tax");
        assert!(body.get("liability_account").is_none());
        assert!(body.get("expense_account").is_none());
    }

    #[test]
    fn no_rate_arg_omits_the_fallback_entirely() {
        // A rule that trusts the registry can drop rate_cents_per_bbl;
        // the ledger then 400s (loudly) if the registry row is missing,
        // instead of accruing silently at a stale flat rate.
        let body =
            accrual_body("step-9", 105, None, "production-tax", "US-FEDERAL", day()).unwrap();
        assert!(body.get("fallback_rate_cents_per_bbl").is_none());
    }
}
