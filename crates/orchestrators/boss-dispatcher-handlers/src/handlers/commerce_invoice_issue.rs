//! `commerce.invoice.issue` — POST a fully-shaped Invoice to
//! `/api/commerce/invoices/batch`. Reads line_items from step
//! metadata; threads optional tax_rate_bps + jurisdiction from
//! args.

use super::common::{self, StepEvent};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct LineItemInput {
    #[serde(default)]
    revenue_category: Option<String>,
    amount_cents: i64,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    ref_id: Option<String>,
    #[serde(default)]
    sku: Option<String>,
    #[serde(default)]
    qty: Option<i32>,
}

pub struct CommerceInvoiceIssue {
    client: reqwest::Client,
    commerce_base: String,
}

impl CommerceInvoiceIssue {
    pub fn new(commerce_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            commerce_base: commerce_base.into(),
        })
    }
}

#[async_trait]
impl Handler for CommerceInvoiceIssue {
    fn name(&self) -> &'static str {
        "commerce.invoice.issue"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = StepEvent::from_payload(&ctx.event_payload)?;
        let account_id = step.subject_id.to_string();

        let raw_lines = step
            .metadata
            .get("line_items")
            .ok_or_else(|| HandlerError::Downstream("step metadata missing line_items".into()))?;
        let lines: Vec<LineItemInput> = serde_json::from_value(raw_lines.clone())
            .map_err(|e| HandlerError::Downstream(format!("decode line_items: {e}")))?;
        if lines.is_empty() {
            return Err(HandlerError::Downstream(
                "step metadata line_items is empty".into(),
            ));
        }

        let due_days = arg(args, "due_days")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(30);
        let default_revenue_category = arg(args, "default_revenue_category")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "uncategorized".to_string());
        let tax_rate_bps = arg(args, "tax_rate_bps")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(0);
        let tax_jurisdiction = arg(args, "tax_jurisdiction").and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            _ => None,
        });
        if tax_rate_bps > 0 && tax_jurisdiction.is_none() {
            return Err(HandlerError::Downstream(
                "tax_rate_bps > 0 requires tax_jurisdiction".into(),
            ));
        }
        // Sales tax applies only to taxable revenue categories — retail /
        // taproom end-user sales. Wholesale + distribution are resale-exempt.
        // The taxable-category set is tenant policy (a rule arg); an empty set
        // means nothing is taxed, so a rate with no categories is a no-op.
        let taxable_categories: std::collections::HashSet<String> = arg(args, "taxable_categories")
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .map(|s| {
                s.split(',')
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let issued_on = step.completed_on.ok_or_else(|| {
            HandlerError::Downstream("step.done payload missing completed_on".into())
        })?;
        let due_on = issued_on + chrono::Duration::days(due_days);
        let invoice_id = format!("inv-step-{}", step.step_id);

        let mut invoice_lines: Vec<serde_json::Value> = Vec::with_capacity(lines.len());
        let mut line_sum: i64 = 0;
        let mut taxable_sum: i64 = 0;
        for (idx, l) in lines.into_iter().enumerate() {
            let category = l
                .revenue_category
                .unwrap_or_else(|| default_revenue_category.clone());
            line_sum += l.amount_cents;
            if taxable_categories.contains(&category) {
                taxable_sum += l.amount_cents;
            }
            invoice_lines.push(json!({
                "id": format!("{invoice_id}-line-{idx}"),
                "invoice_id": invoice_id,
                "revenue_category": category,
                "amount_cents": l.amount_cents,
                "currency": l.currency.unwrap_or_else(|| "USD".to_string()),
                "description": l.description.unwrap_or_default(),
                "ref_id": l.ref_id,
                "sku": l.sku,
                "qty": l.qty,
                "cost_basis_cents": null,
            }));
        }

        let tax_cents = if tax_rate_bps > 0 {
            (taxable_sum * tax_rate_bps) / 10_000
        } else {
            0
        };
        let currency = invoice_lines
            .first()
            .and_then(|l| l.get("currency"))
            .and_then(|v| v.as_str())
            .unwrap_or("USD")
            .to_string();

        let invoice = json!({
            "id": invoice_id,
            "account_id": account_id,
            "issued_on": issued_on,
            "due_on": due_on,
            "paid_on": null,
            "status": "outstanding",
            "amount_cents": line_sum + tax_cents,
            "currency": currency,
            "tax_cents": tax_cents,
            "tax_jurisdiction": tax_jurisdiction,
            "payment_method": null,
            "line_items": invoice_lines,
        });

        let url = format!(
            "{}/api/commerce/invoices/batch",
            self.commerce_base.trim_end_matches('/')
        );
        // Inline POST so we can inspect the batch RESULT, not just the
        // HTTP status (common::post_json checks status only). batch_invoices
        // returns 200 even when it REJECTS a row — the loss is reported in
        // `skipped[]`, not as a non-2xx — so a status-only check would ACK a
        // dropped invoice (lost FG drawdown + revenue, no redelivery). We
        // send exactly one invoice, so anything but "it exists as sent"
        // (`batch_holds_the_invoice`) is a hard failure → NAK so JetStream
        // redelivers it.
        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header(
                "x-boss-user",
                common::dispatcher_actor_header(&ctx.rule_name),
            )
            .header("x-sim-origin", common::sim_origin_value())
            .json(&json!([invoice]))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "POST {url} returned {status}: {text}"
            )));
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| {
            HandlerError::Downstream(format!("POST {url}: decode batch response: {e}"))
        })?;
        batch_holds_the_invoice(&body, &invoice_id)
            .map_err(|why| HandlerError::Downstream(format!("POST {url}: {why}")))
    }
}

/// PURE: whether a one-invoice batch answer says `invoice_id` exists as
/// sent — written by this POST (`inserted` 1) or by an earlier delivery
/// of the same issue (`already_created` naming it; backlog 9d2af748).
/// Anything else, a skipped row above all, is a refusal the caller
/// NAKs so JetStream redelivers.
fn batch_holds_the_invoice(body: &serde_json::Value, invoice_id: &str) -> Result<(), String> {
    let inserted = body.get("inserted").and_then(|v| v.as_i64()).unwrap_or(0);
    let already = body
        .get("already_created")
        .and_then(|v| v.as_array())
        .is_some_and(|ids| ids.len() == 1 && ids[0] == invoice_id);
    let skipped_empty = body
        .get("skipped")
        .and_then(|v| v.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true);
    if skipped_empty && ((inserted == 1 && !already) || (inserted == 0 && already)) {
        return Ok(());
    }
    Err(format!(
        "invoice {invoice_id} rejected by batch (inserted={inserted}, already_created={}, skipped={})",
        body.get("already_created")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        body.get("skipped")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    ))
}

#[cfg(test)]
mod tests {
    use super::batch_holds_the_invoice;
    use serde_json::json;

    #[test]
    fn a_written_invoice_is_held() {
        let body = json!({"inserted": 1, "already_created": [], "skipped": []});
        assert_eq!(batch_holds_the_invoice(&body, "inv-step-1"), Ok(()));
        // An answer from before `already_created` existed reads the same.
        let body = json!({"inserted": 1, "skipped": []});
        assert_eq!(batch_holds_the_invoice(&body, "inv-step-1"), Ok(()));
    }

    /// Backlog 9d2af748: a redelivered issue finds its invoice already
    /// created — `inserted` 0, the id named — and converges instead of
    /// NAKing forever on a write that correctly did not happen.
    #[test]
    fn a_redelivered_issue_whose_invoice_exists_is_held() {
        let body = json!({"inserted": 0, "already_created": ["inv-step-1"], "skipped": []});
        assert_eq!(batch_holds_the_invoice(&body, "inv-step-1"), Ok(()));
    }

    #[test]
    fn a_skipped_or_missing_invoice_is_refused() {
        for body in [
            json!({"inserted": 0, "already_created": [], "skipped": [{"id": "inv-step-1", "error": "x"}]}),
            json!({"inserted": 0, "already_created": [], "skipped": []}),
            json!({"inserted": 0, "already_created": ["inv-step-other"], "skipped": []}),
        ] {
            let why = batch_holds_the_invoice(&body, "inv-step-1").expect_err("refused");
            assert!(why.contains("inv-step-1"), "{why}");
        }
    }
}
