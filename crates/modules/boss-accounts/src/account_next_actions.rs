//! Next-best-action rules — read-side surface for the unified
//! account detail view.
//!
//! The endpoints read `ml_predictions` for the six
//! `mdl-next-action-*-v1` model rows seeded by
//! `boss-ml::bootstrap::seed_phase_two_candidates` and map each
//! prediction's payload into the `Action` shape the UI consumes.
//! Predictions are written by `boss-ml-api`'s daily infer-batch
//! cron — see `infra/ml/run-inference-batch.sh`.
//!
//! Six rules covered:
//!
//! 1. **contract-expiring** — declarative-rule (service_agreements
//!    within 60 days).
//! 2. **past-due-invoice** — declarative-rule (invoices status='past-due').
//! 3. **missing-primary-contact** — declarative-rule (accounts with no
//!    is_primary contact).
//! 4. **churn-risk** — declarative-rule joining ml_predictions for the
//!    `account-churn-risk-composite-v1` plugin.
//! 5. **stalled-ticket** — declarative-rule (open service Jobs
//!    untouched ≥3 days).
//! 6. **preventive-maintenance-due** — declarative-rule (installed assets beyond
//!    preventive_maintenance_interval_months).
//!
//! Adding a new rule is a new model row in
//! `seed_phase_two_candidates` plus an entry in `NEXT_ACTION_MODEL_IDS`
//! below; no Rust changes here.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_policy_client::CurrentUser;
use chrono::NaiveDate;
use serde::Serialize;
use sqlx::PgPool;

/// Model ids whose ml_predictions back the next-actions surface.
/// Adding a rule = adding a row in
/// `boss-ml::bootstrap::seed_phase_two_candidates` and an entry
/// here. No Rust handler changes per rule.
const NEXT_ACTION_MODEL_IDS: &[&str] = &[
    "mdl-next-action-contract-expiring-v1",
    "mdl-next-action-past-due-invoice-v1",
    "mdl-next-action-missing-primary-contact-v1",
    "mdl-next-action-high-churn-risk-v1",
    "mdl-next-action-stalled-service-ticket-v1",
    "mdl-next-action-preventive-maintenance-due-v1",
];

/// True iff `user` may see next-best-actions for `account_id`.
///
/// Three paths to access:
///   1. Role is broadly scoped (C-suite / VP / manager).
///   2. User is the account's `territory_rep_id`.
///   3. User has a row in `account_team_members` for this account.
///
/// The SQL is one round trip; we short-circuit on the role check.
async fn user_can_see_account_nba(
    pool: &PgPool,
    user_id: &str,
    role: &str,
    account_id: &str,
) -> Result<bool, String> {
    if boss_core::roles::has_broad_account_access(role) {
        return Ok(true);
    }
    let (can_see,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (
             SELECT 1 FROM accounts WHERE id = $1 AND territory_rep_id = $2
         ) OR EXISTS (
             SELECT 1 FROM account_team_members WHERE account_id = $1 AND employee_id = $2
         )",
    )
    .bind(account_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(can_see)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

impl Severity {
    /// Numeric rank for sorting — lower is more urgent.
    fn rank(&self) -> u8 {
        match self {
            Severity::Critical => 0,
            Severity::Warning => 1,
            Severity::Info => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Action {
    /// Stable id per (rule, subject). Lets the frontend dedupe
    /// between refreshes and gives a key for "dismiss this action"
    /// functionality later.
    pub id: String,
    /// The rule that produced this action. Matches the registered
    /// rule slug so the frontend can group, filter, or style by rule.
    pub rule: String,
    pub severity: Severity,
    pub headline: String,
    pub context: String,
    /// Frontend route to the subject entity — the action card is a
    /// click-through.
    pub deep_link: String,
    pub due_on: Option<NaiveDate>,
}

#[derive(Clone)]
pub struct NextActionsState {
    pub pool: Arc<PgPool>,
    /// Source `today` for the prediction-freshness cutoff from this
    /// clock so sim-mode reads sim-today.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn next_actions_router(pool: PgPool, clock: Arc<dyn boss_clock_client::ClockClient>) -> Router {
    let state = NextActionsState {
        pool: Arc::new(pool),
        clock,
    };
    Router::new()
        .route(
            "/api/people/accounts/{account_id}/next-actions",
            get(list_next_actions),
        )
        .route("/api/people/my-day/actions", get(my_day_actions))
        .with_state(state)
}

async fn list_next_actions(
    State(state): State<NextActionsState>,
    CurrentUser(user): CurrentUser,
    Path(account_id): Path<String>,
) -> Response {
    // NBAs are policy-scoped. Callers without a direct or role-based
    // relationship to the account get an empty list, not a 403 — the
    // rest of the account page is
    // still viewable, just the action panel is gated.
    match user_can_see_account_nba(&state.pool, &user.id, &user.role, &account_id).await {
        Ok(true) => {}
        Ok(false) => return Json(Vec::<Action>::new()).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }

    // Source `today` from ClockClient so the "yesterday's stale
    // predictions" cutoff respects sim-time.
    let today = state.clock.now().await.now.date_naive();
    match read_actions(&state.pool, &[account_id.as_str()], today).await {
        Ok(mut actions) => {
            sort_actions(&mut actions);
            Json(actions).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// =============================================================================
// Read path — turns ml_predictions rows into Action structs.
// =============================================================================

/// Read every today-scoped prediction across the six next-action
/// model ids for the given accounts and map each to an `Action`.
/// The `today` filter cuts off stale rows from yesterday's runs
/// that the current cron has not yet superseded.
async fn read_actions(
    pool: &PgPool,
    account_ids: &[&str],
    today: chrono::NaiveDate,
) -> Result<Vec<Action>, String> {
    if account_ids.is_empty() {
        return Ok(Vec::new());
    }
    let model_ids: Vec<String> = NEXT_ACTION_MODEL_IDS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let owned: Vec<String> = account_ids.iter().map(|s| s.to_string()).collect();
    let rows: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT model_id, entity_id, payload \
         FROM ml_predictions \
         WHERE model_id = ANY($1::text[]) \
           AND entity_type = 'account' \
           AND entity_id = ANY($2::text[]) \
           AND created_at >= $3::date",
    )
    .bind(&model_ids)
    .bind(&owned)
    .bind(today)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("ml_predictions read: {e}"))?;

    let mut out = Vec::with_capacity(rows.len());
    for (model_id, entity_id, payload) in rows {
        if let Some(action) = action_from_payload(&model_id, &entity_id, &payload) {
            out.push(action);
        }
    }
    Ok(out)
}

fn action_from_payload(
    model_id: &str,
    entity_id: &str,
    payload: &serde_json::Value,
) -> Option<Action> {
    let rule = payload
        .get("rule")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let severity = match payload
        .get("severity")
        .and_then(|v| v.as_str())
        .unwrap_or("info")
    {
        "critical" => Severity::Critical,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    };
    let headline = payload
        .get("headline")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let context = payload
        .get("context")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let deep_link = payload
        .get("deep_link")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let due_on = payload
        .get("due_on")
        .and_then(|v| v.as_str())
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());

    // Stable id of the shape `{rule}/{ref}` where `ref` is the
    // contextual entity (agreement / invoice / job / system). When the
    // rule has no row-level discriminator (e.g. churn-risk,
    // missing-primary-contact), fall back to the account id.
    let row_ref = ["agreement_id", "invoice_id", "job_id", "asset_id"]
        .into_iter()
        .find_map(|k| {
            payload
                .get(k)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| entity_id.to_string());
    let id = format!("{rule}/{row_ref}");

    if headline.is_empty() && deep_link.is_empty() {
        // Defensive: malformed payload, skip.
        tracing::warn!(
            model_id,
            entity_id,
            "next-action prediction payload is empty"
        );
        return None;
    }
    Some(Action {
        id,
        rule,
        severity,
        headline,
        context,
        deep_link,
        due_on,
    })
}

/// Sort by severity rank then by due date. None sorts to the end
/// so critical items without an explicit deadline still beat
/// non-critical items that do have one.
fn sort_actions(actions: &mut [Action]) {
    actions.sort_by(|a, b| {
        a.severity
            .rank()
            .cmp(&b.severity.rank())
            .then_with(|| match (a.due_on, b.due_on) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
    });
}

async fn my_day_actions(
    State(state): State<NextActionsState>,
    CurrentUser(user): CurrentUser,
) -> Response {
    // employee_id is the authenticated caller, not an untrusted query
    // param — otherwise any caller could request any employee's My
    // Day. Sourced from the
    // `x-boss-user` header via the CurrentUser extractor.
    let limit: usize = 20;

    // Ownership union: accounts where the employee is either the
    // territory rep OR carries an account-team role (customer-success,
    // service-manager, finance-contact, executive-sponsor). DISTINCT
    // guards against double-counting when an employee covers both
    // relationships on the same account.
    let accounts: Vec<(String,)> = match sqlx::query_as(
        "SELECT id FROM accounts WHERE territory_rep_id = $1 \
         UNION \
         SELECT account_id FROM account_team_members WHERE employee_id = $1",
    )
    .bind(&user.id)
    .fetch_all(state.pool.as_ref())
    .await
    {
        Ok(rows) => rows,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let owned_ids: Vec<String> = accounts.into_iter().map(|(s,)| s).collect();
    let owned_refs: Vec<&str> = owned_ids.iter().map(String::as_str).collect();
    // Source `today` from ClockClient so the prediction-freshness
    // cutoff respects sim-time.
    let today = state.clock.now().await.now.date_naive();
    let mut actions = match read_actions(state.pool.as_ref(), &owned_refs, today).await {
        Ok(a) => a,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    sort_actions(&mut actions);
    actions.truncate(limit);

    // Deduplicate by (rule, deep_link) just in case two accounts
    // surface the same action (shouldn't happen today but cheap to
    // guard). Order-preserving.
    let mut seen: HashMap<(String, String), ()> = HashMap::new();
    let deduped: Vec<Action> = actions
        .into_iter()
        .filter(|a| {
            seen.insert((a.rule.clone(), a.deep_link.clone()), ())
                .is_none()
        })
        .collect();

    Json(deduped).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ranks_critical_before_warning_before_info() {
        assert!(Severity::Critical.rank() < Severity::Warning.rank());
        assert!(Severity::Warning.rank() < Severity::Info.rank());
    }

    #[test]
    fn severity_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Severity::Critical).unwrap(),
            "\"critical\""
        );
    }

    #[test]
    fn action_serializes_with_snake_case_fields() {
        let action = Action {
            id: "contract-expiring/SA-123".to_string(),
            rule: "contract-expiring".to_string(),
            severity: Severity::Warning,
            headline: "Contract SA-123 expires in 28 days".to_string(),
            context: "Annual value $12K/yr".to_string(),
            deep_link: "/sales/agreements/SA-123".to_string(),
            due_on: NaiveDate::from_ymd_opt(2026, 5, 10),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("\"deep_link\":\"/sales/agreements/SA-123\""));
        assert!(json.contains("\"severity\":\"warning\""));
        assert!(json.contains("\"due_on\":\"2026-05-10\""));
    }
}
