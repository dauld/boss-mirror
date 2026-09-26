use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

type EntrySummaryRow = (
    Uuid,
    Uuid,
    NaiveDate,
    Option<String>,
    i32,
    String,
    Option<String>,
    Option<String>,
);
type EntryDetailRow = (
    Uuid,
    Uuid,
    NaiveDate,
    Option<String>,
    i32,
    String,
    serde_json::Value,
    Option<String>,
    Option<String>,
);
type EntryLineRow = (String, String, i64, i64, Option<String>, i16);

// --- entry list + detail --------------------------------------------------

#[derive(Deserialize)]
pub(super) struct EntriesQuery {
    account_code: Option<String>,
    fact_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct EntrySummary {
    pub id: Uuid,
    pub fact_id: Uuid,
    pub posted_on: NaiveDate,
    pub memo: Option<String>,
    pub rule_version: i32,
    pub fact_kind: String,
    pub fact_source_table: Option<String>,
    pub fact_source_id: Option<String>,
}

pub(super) async fn list_entries(
    State(state): State<Arc<LedgerApiState>>,
    Query(q): Query<EntriesQuery>,
) -> Response {
    if q.account_code.is_none() && q.fact_id.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "one of account_code or fact_id is required",
        )
            .into_response();
    }

    let account_id: Option<Uuid> = if let Some(code) = q.account_code {
        match sqlx::query_scalar::<_, Uuid>("SELECT id FROM gl_accounts WHERE code = $1")
            .bind(&code)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(Some(id)) => Some(id),
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    format!("unknown account code {code}"),
                )
                    .into_response();
            }
            Err(e) => return storage_err(e),
        }
    } else {
        None
    };

    let rows: Result<Vec<EntrySummaryRow>, _> = sqlx::query_as(
        "SELECT DISTINCT e.id, e.fact_id, e.posted_on, e.memo, rv.version, \
                f.kind, f.source_table, f.source_id \
         FROM gl_journal_entries e \
         JOIN gl_rule_versions rv ON rv.id = e.rule_version_id \
         JOIN financial_facts f ON f.id = e.fact_id \
         LEFT JOIN gl_journal_lines l ON l.journal_entry_id = e.id \
         WHERE ($1::uuid IS NULL OR l.account_id = $1) \
           AND ($2::uuid IS NULL OR e.fact_id = $2) \
         ORDER BY e.posted_on DESC, e.id \
         LIMIT $3",
    )
    .bind(account_id)
    .bind(q.fact_id)
    .bind(q.limit)
    .fetch_all(&state.pool)
    .await;

    match rows {
        Ok(rows) => {
            let entries: Vec<EntrySummary> = rows
                .into_iter()
                .map(
                    |(
                        id,
                        fact_id,
                        posted_on,
                        memo,
                        rule_version,
                        fact_kind,
                        fact_source_table,
                        fact_source_id,
                    )| EntrySummary {
                        id,
                        fact_id,
                        posted_on,
                        memo,
                        rule_version,
                        fact_kind,
                        fact_source_table,
                        fact_source_id,
                    },
                )
                .collect();
            Json(entries).into_response()
        }
        Err(e) => storage_err(e),
    }
}

#[derive(Serialize)]
pub struct EntryDetail {
    pub id: Uuid,
    pub fact_id: Uuid,
    pub posted_on: NaiveDate,
    pub memo: Option<String>,
    pub rule_version: i32,
    pub fact_kind: String,
    pub fact_payload: serde_json::Value,
    pub fact_source_table: Option<String>,
    pub fact_source_id: Option<String>,
    pub lines: Vec<EntryLine>,
}

#[derive(Serialize)]
pub struct EntryLine {
    pub account_code: String,
    pub account_name: String,
    pub debit_cents: i64,
    pub credit_cents: i64,
    #[serde(default = "default_currency")]
    pub currency: String,
    pub memo: Option<String>,
    pub sort_order: i16,
}

pub(super) async fn get_entry(
    State(state): State<Arc<LedgerApiState>>,
    Path(id): Path<Uuid>,
) -> Response {
    let entry: Result<Option<EntryDetailRow>, _> = sqlx::query_as(
        "SELECT e.id, e.fact_id, e.posted_on, e.memo, rv.version, \
                f.kind, f.payload, f.source_table, f.source_id \
         FROM gl_journal_entries e \
         JOIN gl_rule_versions rv ON rv.id = e.rule_version_id \
         JOIN financial_facts f ON f.id = e.fact_id \
         WHERE e.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await;

    let entry_row = match entry {
        Ok(Some(row)) => row,
        Ok(None) => return (StatusCode::NOT_FOUND, "entry not found").into_response(),
        Err(e) => return storage_err(e),
    };

    let lines_result: Result<Vec<EntryLineRow>, _> = sqlx::query_as(
        "SELECT a.code, a.name, l.debit_cents, l.credit_cents, l.memo, l.sort_order \
             FROM gl_journal_lines l \
             JOIN gl_accounts a ON a.id = l.account_id \
             WHERE l.journal_entry_id = $1 \
             ORDER BY l.sort_order",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await;

    let lines = match lines_result {
        Ok(rows) => rows
            .into_iter()
            .map(
                |(account_code, account_name, debit_cents, credit_cents, memo, sort_order)| {
                    EntryLine {
                        account_code,
                        account_name,
                        debit_cents,
                        credit_cents,
                        currency: "USD".to_string(),
                        memo,
                        sort_order,
                    }
                },
            )
            .collect(),
        Err(e) => return storage_err(e),
    };

    Json(EntryDetail {
        id: entry_row.0,
        fact_id: entry_row.1,
        posted_on: entry_row.2,
        memo: entry_row.3,
        rule_version: entry_row.4,
        fact_kind: entry_row.5,
        fact_payload: entry_row.6,
        fact_source_table: entry_row.7,
        fact_source_id: entry_row.8,
        lines,
    })
    .into_response()
}
