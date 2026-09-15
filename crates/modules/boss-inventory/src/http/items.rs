//! Inventory-item handlers: stock reads, consume/receive, batch upsert,
//! overhead absorption, and the dispatcher lookup helpers.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use boss_policy_client::CurrentUser;

use super::InventoryApiState;
use crate::port::{InventoryError, InventoryRepository};

pub(super) async fn list_items<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
) -> Response {
    match state.inventory.all_items().await {
        Ok(items) => Json(items).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn get_item<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(part_sku): Path<String>,
) -> Response {
    match state.inventory.item_by_sku(&part_sku).await {
        Ok(Some(item)) => Json(item).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no inventory item with SKU {part_sku}"),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/inventory/items/{sku}/open-po-exists — dispatcher helper.
/// Returns `{exists: bool}` for the rule predicate
/// `NOT open_po_exists(part_sku)`.
pub(super) async fn open_po_exists_for_sku<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(part_sku): Path<String>,
) -> Response {
    match state.inventory.open_po_exists_for_part(&part_sku).await {
        Ok(exists) => Json(serde_json::json!({ "exists": exists })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/inventory/items/{sku}/primary-vendor — dispatcher helper.
/// Returns `{vendor_id: "..."}` for the rule arg
/// `vendor_for(part_sku)`. 404 when the SKU has never been ordered;
/// the dispatcher's rule should be authored to handle that
/// gracefully (e.g. by not firing when no vendor exists).
pub(super) async fn primary_vendor_for_sku<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(part_sku): Path<String>,
) -> Response {
    match state.inventory.primary_vendor_for_part(&part_sku).await {
        Ok(Some(vendor_id)) => Json(serde_json::json!({ "vendor_id": vendor_id })).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no PO history for {part_sku}; vendor unknown"),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct ConsumeRequest {
    qty: u32,
    #[serde(default)]
    reason: Option<String>,
    /// Deterministic idempotency key from the triggering step
    /// (`{step_id}:{part_sku}`). Becomes the consume's `source_id` so a
    /// redelivered step-effect event (at-least-once JetStream delivery)
    /// resolves to the SAME fact and the relative `on_hand -= qty` is
    /// applied exactly once. Absent for direct callers (manual ops,
    /// tests) → a random source_id, i.e. no cross-call dedup.
    #[serde(default)]
    idempotency_key: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ConsumeResponse {
    ok: bool,
    item: crate::types::InventoryItem,
    alert_sent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    alert_detail: Option<String>,
}

pub(super) async fn consume_part<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(part_sku): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<ConsumeRequest>,
) -> Response {
    // `now` resolved by the Clock service. Production wires a
    // wall-mode clock; demo wires a sim-mode clock. The handler
    // never inspects the request to learn which.
    let now = boss_clock_client::now_from(&state.clock).await;
    // source_id: shared between the in-tx financial_fact and the
    // post-commit audit_log event so bundle round-trip rebuild lands on
    // the same row. Prefer the caller's deterministic idempotency key
    // (`{step_id}:{part_sku}`) so a redelivered step-effect re-resolves
    // to the same fact and `consume_part_at` skips the relative
    // `on_hand -= qty` on replay. Direct callers without a key get a
    // random source_id (no cross-call dedup). The fallback must never be
    // time-based: under sim-time threading a day's midnight rfc3339 is
    // identical for every consume on that day, which collides on the
    // unique `(kind, source_table, source_id)` index and silently drops
    // facts — so the fallback stays random.
    let consume_source_id = body
        .idempotency_key
        .clone()
        .unwrap_or_else(|| format!("{}@{}", part_sku, uuid::Uuid::new_v4()));
    // Outbox phase 2: ITEM_CONSUMED + INVENTORY_TRANSFERRED record
    // inside the repository transaction via the stamp.
    let stamp = super::event_stamp(&state, &user).await;
    let applied = match state
        .inventory
        .consume_part_at(&part_sku, body.qty, now, &consume_source_id, &stamp)
        .await
    {
        Ok(applied) => applied,
        Err(InventoryError::InsufficientStock(sku, have, need)) => {
            // Auto-restock is not spawned inline. The 409 stands; the
            // boss-dispatcher rule registry consumes the
            // inventory.item.consumed event (which fires for successful
            // consumes below) and evaluates the canonical
            // reorder-threshold rule:
            //   on_hand <= reorder_point AND NOT open_po_exists(part_sku)
            // On the insufficient-stock path no consume happened, so no
            // event fires, so no rule fires — correct: there's nothing
            // for inventory-api to deterministically do beyond reporting
            // the shortfall. The caller (a step's side-effect handler)
            // decides whether to retry, alert, or abort.
            return (
                StatusCode::CONFLICT,
                format!("insufficient stock: {sku} has {have}, need {need}"),
            )
                .into_response();
        }
        Err(InventoryError::NotFound(sku)) => {
            return (StatusCode::NOT_FOUND, format!("part not found: {sku}")).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let crate::types::ConsumeApplied {
        item,
        fact_payload: _,
    } = applied;

    // Check if stock dropped below reorder threshold. The threshold
    // counts (on_hand - allocated + inbound reservations) — Jobs whose
    // receiving step hasn't fired yet are real upcoming supply, and
    // opening a *second* restock when sufficient supply is already
    // in-flight just floods the warehouse. inbound_reserved naturally
    // dedups concurrent auto-restock triggers: the first race winner
    // creates the Job, and its receiving step's expected_items make
    // every losing-side check land above the threshold.
    let available = item.on_hand.saturating_sub(item.allocated);
    let inbound = state
        .inventory
        .inbound_reserved_for_part(&part_sku)
        .await
        .unwrap_or(0);
    let effective_supply = available as i64 + inbound;
    let alert_sent = effective_supply <= item.reorder_point as i64;
    let alert_detail = if alert_sent {
        // Signal the warehouse manager. The auto-restock Job spawn is
        // owned by the dispatcher rule registry: the
        // inventory.item.consumed event emitted below triggers the
        // canonical reorder-threshold rule, which evaluates
        // `on_hand <= reorder_point AND NOT open_po_exists(part_sku)` and
        // fires the jobs.spawn handler with subject =
        // vendor_for(part_sku). Idempotency is the open-PO predicate.
        let msg_body = format!(
            "Part {} is now at {} on-hand ({} available) — below reorder point of {}. \
             Consumed {} unit(s){}.",
            part_sku,
            item.on_hand,
            available,
            item.reorder_point,
            body.qty,
            body.reason
                .as_ref()
                .map(|r| format!(" for: {r}"))
                .unwrap_or_default(),
        );

        // Best-effort alert via the messages API.
        let alert_result = send_low_stock_alert(&part_sku, &msg_body).await;
        Some(match alert_result {
            Ok(()) => format!(
                "Alert sent: {available} available + {inbound} inbound, reorder point {}",
                item.reorder_point
            ),
            Err(e) => format!("Alert failed to send: {e}"),
        })
    } else {
        None
    };

    (
        StatusCode::OK,
        Json(ConsumeResponse {
            ok: true,
            item,
            alert_sent,
            alert_detail,
        }),
    )
        .into_response()
}

/// Send a low-stock system signal to the warehouse manager's inbox.
async fn send_low_stock_alert(part_sku: &str, body: &str) -> Result<(), String> {
    let client = reqwest::Client::new();
    let msg = serde_json::json!({
        "sender_id": "automation:inventory",
        "recipient_id": "emp-091-mgr",
        "subject": format!("Low stock alert: {part_sku}"),
        "body": body,
        "kind": "signal",
        "entity_ref": {
            "entity_type": "part",
            "entity_id": part_sku,
            "entity_path": format!("/parts/{part_sku}"),
        },
    });
    let resp = client
        .post("http://127.0.0.1:7200/api/messages/send")
        .json(&msg)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("messages API returned {}", resp.status()))
    }
}

pub(super) async fn batch_upsert_items<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<Vec<crate::types::InventoryItem>>,
) -> Response {
    let mut inserted = 0usize;
    let mut opening_jes = 0usize;
    let batch_now = boss_clock_client::now_from(&state.clock).await;
    // Outbox phase 2: one stamp for the whole batch; ITEM_UPSERTED and
    // the opening-JE event record inside each repository transaction.
    let stamp = super::event_stamp(&state, &user).await;
    for item in &body {
        // Row-touch column binds the stamp's wall time; the opening-JE
        // happened_on below stays on the authoritative clock's date.
        if let Err(e) = state
            .inventory
            .upsert_item_at(item, stamp.timestamp, &stamp)
            .await
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        // Atomic opening-balance JE. When a batch upsert lands a row
        // carrying value, post DR 1300 / CR 3000 sized at exactly
        // value_cents — the conserved quantity the row now holds, so
        // the GL and the projection agree by construction (PR 6a;
        // avg_cost_cents is derived display and never sizes a JE).
        // Idempotent on `(source_table, source_id)` so re-runs of the
        // same seed bundle no-op. Posting the JE atomically at the API
        // layer closes the hole for every batch caller, not just
        // brewery-engine.
        let total_cost = item.value_cents;
        if total_cost > 0 {
            let memo = format!(
                "Opening balance — {} × {} (raw materials ← retained earnings)",
                item.on_hand, item.part_sku
            );
            let source_id = format!("opening-raw-{}", item.part_sku);
            match state
                .inventory
                .record_inventory_je(
                    total_cost,
                    "1300",
                    "3000",
                    &memo,
                    "brewery_seed_opening_balance",
                    &source_id,
                    batch_now.date_naive(),
                    &stamp,
                )
                .await
            {
                Ok(_je) => {
                    // The fact's WRITER owns its rebuild source: the
                    // adapter records the event in-tx, only when the
                    // call inserted the fact. (2026-07-09 regression:
                    // the seed's second, conflicting writer owned this
                    // emit, and #86's inserted-gating correctly muted
                    // it — every opening then vanished on rebuild.
                    // That gate is structural now.)
                    opening_jes += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        sku = %item.part_sku,
                        error = %e,
                        "batch_upsert: opening JE post failed (row upserted)"
                    );
                }
            }
        }
        inserted += 1;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "inserted": inserted,
            "opening_jes_posted": opening_jes,
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct ReceiveRequest {
    qty: u32,
    /// Per-unit cost in cents for this receive lot, used to update
    /// the part's weighted moving-average cost basis. Optional —
    /// when the caller doesn't carry cost data the avg stays put
    /// and only on_hand moves.
    #[serde(default)]
    unit_cost_cents: Option<i64>,
    /// Deterministic idempotency key from the triggering step
    /// (`{step_id}:{part_sku}`). Becomes the receive's `source_id` so a
    /// redelivered step-effect event (at-least-once JetStream delivery)
    /// resolves to the SAME proof-fact and the relative `on_hand += qty`
    /// is applied exactly once. Absent for direct callers (manual ops,
    /// tests) → a random source_id, i.e. no cross-call dedup.
    #[serde(default)]
    idempotency_key: Option<String>,
}

pub(super) async fn receive_part_handler<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    Path(part_sku): Path<String>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<ReceiveRequest>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    // Prefer the caller's deterministic idempotency key
    // (`{step_id}:{part_sku}`) so a redelivered step-effect re-resolves
    // to the same proof-fact and `receive_part_at` skips the relative
    // `on_hand += qty` on replay. Direct callers without a key get a
    // random source_id (no cross-call dedup). The fallback must never be
    // time-based: under sim-time threading a day's midnight rfc3339 is
    // identical for every receive on that day, which collides on the
    // unique `(kind, source_table, source_id)` index and would skip every
    // receive after the first — so the fallback stays random. (Same
    // rationale as the consume handler.)
    let receive_source_id = body
        .idempotency_key
        .clone()
        .unwrap_or_else(|| format!("{}@{}", part_sku, uuid::Uuid::new_v4()));
    // Outbox phase 2: ITEM_UPSERTED (post-receive row state) +
    // ITEM_RECEIVED (the exact proof-fact payload) record inside the
    // repository transaction; a redelivered receive records nothing.
    let stamp = super::event_stamp(&state, &user).await;
    let applied = match state
        .inventory
        .receive_part_at(
            &part_sku,
            body.qty,
            body.unit_cost_cents,
            now,
            &receive_source_id,
            &stamp,
        )
        .await
    {
        Ok(applied) => applied,
        Err(InventoryError::NotFound(sku)) => {
            return (StatusCode::NOT_FOUND, format!("part not found: {sku}")).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let crate::types::ReceiveApplied {
        item,
        receipt_payload: _,
    } = applied;

    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "item": item})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct OverheadAbsorbedRequest {
    /// Amount of one production-overhead driver being capitalized into
    /// WIP, in cents. The journal entry will be DR `debit_account` /
    /// CR `credit_account` for this amount. One POST per driver.
    total_cost_cents: i64,
    /// WIP inventory account (1310 in the canonical brewery chart).
    debit_account: String,
    /// Expense account this driver credits — the account IS the driver
    /// key (direct labor → 6100, process utilities → 6300, production
    /// depreciation → 6900 in the canonical brewery chart). The credit
    /// reduces the period expense by what gets absorbed into inventory,
    /// and uniquely keys the fact's `source_id` per (step, driver).
    credit_account: String,
    /// Free-form annotation surfaced on the GL entry. Defaults to a
    /// generic "Production overhead absorbed into WIP" string.
    #[serde(default)]
    memo: Option<String>,
    /// Provenance: the production-consume step that triggered this.
    /// Threaded into the fact's `source_id` so rebuild idempotency
    /// matches on a stable key.
    #[serde(default)]
    step_id: Option<String>,
    /// Date the absorption is booked. Defaults to today.
    #[serde(default)]
    happened_on: Option<chrono::NaiveDate>,
}

/// POST `/api/inventory/overhead-absorbed` — record one production-overhead
/// driver (direct labor, process utilities, production depreciation, …)
/// being capitalized into WIP at production-consume time. Balances burden
/// absorption: production-produce credits 1310 WIP for the full FG cost
/// basis, so production-consume must debit it for both raw materials
/// (the raw → WIP transfer) and the overhead drivers booked here.
///
/// Three writes in one repository tx (outbox phase 2):
///   1. `finance.inventory.transferred` financial_fact inserted
///      directly + journal entry posted via the ledger's
///      `post_fact_in_tx` (same shape as the raw → WIP transfer fact,
///      with a different debit/credit pair).
///   2. The `inventory.overhead.absorbed` audit event, recorded in
///      the SAME transaction — provenance + replay material.
///
/// On rebuild from audit_log alone, the `inventory.overhead.absorbed` →
/// `finance.inventory.transferred` projection rule re-creates the
/// fact; the matching journal entry follows from `post_fact_in_tx`.
pub(super) async fn overhead_absorbed_handler<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<OverheadAbsorbedRequest>,
) -> Response {
    if body.total_cost_cents <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            "total_cost_cents must be positive".to_string(),
        )
            .into_response();
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    let happened_on = body.happened_on.unwrap_or_else(|| now.date_naive());
    let memo = body
        .memo
        .clone()
        .unwrap_or_else(|| "Production overhead absorbed into WIP".to_string());
    let step_id = body.step_id.clone();

    // Source-id strategy: when a step_id is supplied (the typical
    // production-consume case), key on
    // `overhead-absorbed@<step_id>:<credit_account>`. Folding in the
    // credit account makes the id unique *per driver*, so a step that
    // absorbs several granular drivers (direct labor → 6100, process
    // utilities → 6300, production depreciation → 6900) lands one fact
    // each instead of colliding on the (kind, source_table, source_id)
    // unique key; re-emits for the same (step, account) — e.g. rebuild
    // replays — still collapse. The drain's `drain-actual-wip` basis
    // reconstructs the same key to sum what was capitalized. Without a
    // step_id we fall back to timestamp:account — best-effort
    // idempotency for ad-hoc absorption posts. The account suffix is
    // load-bearing there too: `now` is sim-time quantized and cached
    // ~100ms by the clock client, so back-to-back multi-driver posts
    // share one timestamp, and without the suffix the later drivers
    // would silently collapse onto the first's fact.
    let source_id = match &step_id {
        Some(s) => format!("overhead-absorbed@{s}:{}", body.credit_account),
        None => format!(
            "overhead-absorbed@{}:{}",
            now.to_rfc3339(),
            body.credit_account
        ),
    };

    // Live posting — financial_fact + journal entry + the audit event
    // via the repository, all in one tx. The adapter builds the ONE
    // payload construction (fact and event byte-identical — the
    // fact-level replay-check compares payloads; `step_id` is NOT a
    // payload field: it is already encoded in `source_id`) and records
    // `inventory.overhead.absorbed` gated on the fact being newly
    // inserted, so an idempotent replay records nothing.
    let stamp = super::event_stamp(&state, &user).await;
    let canonical_fact_id = match state
        .inventory
        .record_overhead_absorbed(
            body.total_cost_cents,
            &body.debit_account,
            &body.credit_account,
            &memo,
            &source_id,
            happened_on,
            &stamp,
        )
        .await
    {
        Ok(outcome) => outcome,
        // Deterministic request-data error (unknown account code) → 422,
        // so a seed typo reads as a client error with the offending code
        // in the body, not as service trouble. The dispatcher still NAKs
        // (it can't tell permanent from transient), but the redelivery
        // warns and the eventual dead-letter carry the precise cause.
        Err(e @ InventoryError::InvalidAccount(_)) => {
            return (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let (canonical_fact_id, _fact_inserted) = canonical_fact_id;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "fact_id": canonical_fact_id,
            "happened_on": happened_on,
        })),
    )
        .into_response()
}
