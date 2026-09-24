//! Warehouse-status projection endpoint.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use super::InventoryApiState;
use crate::port::InventoryRepository;
use crate::warehouse_status::build_warehouse_status;

pub(super) async fn warehouse_status<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
) -> Response {
    let Some(clients) = &state.clients else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "warehouse-status requires the shipping client — not configured",
        )
            .into_response();
    };

    // Inventory-owned reads are cheap (in-memory or single SQL). The
    // cross-service fan-out is the expensive part; run them in
    // parallel so latency is the slowest single call, not the sum.
    let items_fut = state.inventory.all_items();
    let pos_fut = state.inventory.all_purchase_orders();
    let shipments_fut = clients.shipping.outbound_shipment_summary();
    let (items_res, pos_res, shipments_res) = tokio::join!(items_fut, pos_fut, shipments_fut);

    let items = match items_res {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let pos = match pos_res {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let shipments = match shipments_res {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("shipping: {e}")).into_response(),
    };

    let status = build_warehouse_status(
        &items,
        &pos,
        shipments,
        boss_clock_client::now_from(&state.clock).await,
    );
    Json(status).into_response()
}
