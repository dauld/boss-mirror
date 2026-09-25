//! Warehouse-status projection endpoint.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use super::InventoryApiState;
use crate::port::InventoryRepository;
use crate::warehouse_status::{OutboundShipmentsRead, build_warehouse_status};

pub(super) async fn warehouse_status<R: InventoryRepository + 'static>(
    State(state): State<Arc<InventoryApiState<R>>>,
) -> Response {
    // Inventory-owned reads are cheap (in-memory or single SQL). The
    // cross-service fan-out is the expensive part; run them in
    // parallel so latency is the slowest single call, not the sum.
    //
    // Backlog 89cf07d8: only inventory's OWN reads may fail the whole
    // answer. The shipping leg — distribution's service, off on the
    // live instance — used to answer 503 when unwired and 502 when
    // down, taking parts stock and inbound POs with it. It now fails
    // alone, and the wire names why.
    let items_fut = state.inventory.all_items();
    let pos_fut = state.inventory.all_purchase_orders();
    let shipments_fut = async {
        match &state.clients {
            None => OutboundShipmentsRead::Unavailable {
                reason: "shipping client not configured".into(),
            },
            Some(clients) => match clients.shipping.outbound_shipment_summary().await {
                Ok(summary) => OutboundShipmentsRead::Ok { summary },
                Err(e) => OutboundShipmentsRead::Unavailable {
                    reason: e.to_string(),
                },
            },
        }
    };
    let (items_res, pos_res, shipments) = tokio::join!(items_fut, pos_fut, shipments_fut);

    let items = match items_res {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let pos = match pos_res {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let status = build_warehouse_status(
        &items,
        &pos,
        shipments,
        boss_clock_client::now_from(&state.clock).await,
    );
    Json(status).into_response()
}
