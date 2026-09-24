//! `account-churn-risk-composite-v1` — heuristic-formula plugin that
//! scores four account-activity signals into a churn-risk composite.
//! The score is normalized to 0.0..=1.0 (the dispatcher's `score:
//! f64` contract); the integer 0..=100 score and the human-readable
//! factor labels are carried in the prediction payload, which is the
//! watchlist response shape `boss-accounts::account_risk_scores`
//! reads back.

use async_trait::async_trait;
use chrono::NaiveDate;

use boss_ml::{InferContext, InferError, InferOutput, InferencePlugin};

/// `(days_since_invoice, open_tickets, has_active_contract,
///  days_since_note)` factors per account, scored into a 0..=100
/// composite.
#[derive(Debug, Clone)]
pub struct RiskFactors {
    pub days_since_last_invoice: Option<i64>,
    pub open_ticket_count: i64,
    pub has_active_contract: bool,
    pub days_since_last_note: Option<i64>,
}

/// Apply the v1 bump rules. Returns `(score_0_to_100,
/// top_factor_label)`.
pub fn score_for(f: &RiskFactors) -> (i32, String) {
    let mut bumps: Vec<(i32, &'static str)> = Vec::new();

    match f.days_since_last_invoice {
        Some(d) if d > 180 => bumps.push((25, "no invoice in 180+ days")),
        Some(d) if d > 90 => bumps.push((15, "no invoice in 90+ days")),
        Some(d) if d > 30 => bumps.push((5, "no invoice in 30+ days")),
        None => bumps.push((20, "never invoiced")),
        _ => {}
    }

    if f.open_ticket_count > 5 {
        bumps.push((20, "5+ open tickets"));
    } else if f.open_ticket_count > 2 {
        bumps.push((10, "3+ open tickets"));
    }

    if !f.has_active_contract {
        bumps.push((15, "no active service contract"));
    }

    match f.days_since_last_note {
        Some(d) if d > 180 => bumps.push((15, "no contact in 180+ days")),
        Some(d) if d > 90 => bumps.push((10, "no contact in 90+ days")),
        None => bumps.push((10, "no recorded contact")),
        _ => {}
    }

    let score: i32 = bumps.iter().map(|(n, _)| *n).sum::<i32>().min(100);
    let top = bumps
        .iter()
        .max_by_key(|(n, _)| *n)
        .map(|(_, l)| (*l).to_string())
        .unwrap_or_else(|| "healthy".to_string());
    (score, top)
}

pub struct AccountChurnRiskV1;

impl AccountChurnRiskV1 {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AccountChurnRiskV1 {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InferencePlugin for AccountChurnRiskV1 {
    fn name(&self) -> &'static str {
        "account-churn-risk-composite-v1"
    }

    async fn infer(
        &self,
        entity_type: &str,
        entity_id: &str,
        ctx: &InferContext<'_>,
    ) -> Result<InferOutput, InferError> {
        if entity_type != "account" {
            return Err(InferError::BadRequest(format!(
                "expected entity_type=account, got {entity_type}"
            )));
        }
        let factors = collect_factors(ctx, entity_id).await?;
        let (score_int, top_factor) = score_for(&factors);
        let payload = serde_json::json!({
            "rule": "account-churn-risk",
            "score": score_int,
            "top_factor": top_factor,
            "factors": {
                "days_since_last_invoice": factors.days_since_last_invoice,
                "open_ticket_count": factors.open_ticket_count,
                "has_active_contract": factors.has_active_contract,
                "days_since_last_note": factors.days_since_last_note,
            }
        });
        Ok(InferOutput {
            score: score_int as f64 / 100.0,
            payload,
        })
    }
}

/// Collect the four signals for a single account, scoped to one row,
/// so per-entity infer paths are a single query each. Batch callers
/// pay 4N queries per run; today's account count makes that trivial.
/// Future tuning can fold these into a single LATERAL join.
async fn collect_factors(
    ctx: &InferContext<'_>,
    account_id: &str,
) -> Result<RiskFactors, InferError> {
    // Source `today` from the dispatcher-provided ctx.now (itself
    // ClockClient-routed) instead of Utc::now(). In sim mode, ctx.now
    // is sim-today.
    let today = ctx.now.date_naive();

    let last_invoice: Option<NaiveDate> =
        sqlx::query_scalar("SELECT MAX(issued_on) FROM invoices WHERE account_id = $1")
            .bind(account_id)
            .fetch_one(ctx.pool)
            .await
            .map_err(|e| InferError::Storage(format!("invoices: {e}")))?;

    let open_tickets: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM jobs \
         WHERE subject_kind = 'account' \
           AND subject_id = $1 \
           AND status = 'open'",
    )
    .bind(account_id)
    .fetch_one(ctx.pool)
    .await
    .map_err(|e| InferError::Storage(format!("jobs: {e}")))?;

    // Bind today instead of using CURRENT_DATE so sim-mode sees
    // sim-today, not Postgres wallclock.
    let active_contract_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM service_agreements \
         WHERE account_id = $1 \
           AND status = 'active' \
           AND end_date >= $2::date",
    )
    .bind(account_id)
    .bind(today)
    .fetch_one(ctx.pool)
    .await
    .map_err(|e| InferError::Storage(format!("service_agreements: {e}")))?;

    let last_note_at: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT MAX(occurred_at) FROM account_notes \
         WHERE account_id = $1 AND deleted_at IS NULL",
    )
    .bind(account_id)
    .fetch_one(ctx.pool)
    .await
    .map_err(|e| InferError::Storage(format!("account_notes: {e}")))?;

    Ok(RiskFactors {
        days_since_last_invoice: last_invoice.map(|d| (today - d).num_days()),
        open_ticket_count: open_tickets,
        has_active_contract: active_contract_count > 0,
        days_since_last_note: last_note_at.map(|ts| (today - ts.date_naive()).num_days()),
    })
}

// =============================================================================
// Tests — pure score_for unit tests + a TestDb integration test.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_account_scores_zero() {
        let (score, top) = score_for(&RiskFactors {
            days_since_last_invoice: Some(7),
            open_ticket_count: 1,
            has_active_contract: true,
            days_since_last_note: Some(14),
        });
        assert_eq!(score, 0);
        assert_eq!(top, "healthy");
    }

    #[test]
    fn dormant_account_scores_high() {
        let (score, top) = score_for(&RiskFactors {
            days_since_last_invoice: Some(200),
            open_ticket_count: 0,
            has_active_contract: false,
            days_since_last_note: Some(200),
        });
        // 25 (invoice 180+) + 15 (no contract) + 15 (no contact 180+) = 55
        assert_eq!(score, 55);
        assert_eq!(top, "no invoice in 180+ days");
    }

    #[test]
    fn score_clamps_to_100() {
        let (score, _) = score_for(&RiskFactors {
            days_since_last_invoice: Some(9999),
            open_ticket_count: 50,
            has_active_contract: false,
            days_since_last_note: Some(9999),
        });
        assert!(score <= 100);
    }

    #[test]
    fn top_factor_picks_biggest_bump() {
        let (_, top) = score_for(&RiskFactors {
            days_since_last_invoice: Some(45),
            open_ticket_count: 6,
            has_active_contract: true,
            days_since_last_note: Some(100),
        });
        assert_eq!(top, "5+ open tickets");
    }

    // The algorithm is covered by the score_for tests above; the
    // dispatcher plumbing by boss-ml::inference::tests. End-to-end
    // TestDb coverage flows through the
    // /api/people/accounts/risk-scores integration test, which reads
    // ml_predictions populated by this plugin.
}
