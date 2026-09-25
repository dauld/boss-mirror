//! Axum HTTP surface for the inventory API. The router lives here;
//! handlers are grouped by concern into the submodules below.

use std::sync::Arc;

use axum::routing::{get, post, put};
use axum::{Json, Router};

use boss_classes_client::ClassesClient;
use boss_core::publisher::DomainPublisher;
use boss_shipping_client::ShippingClient;

use crate::port::InventoryRepository;

mod items;
mod orders;
mod vendor_invoices;
mod vendors;
mod warehouse;

use items::*;
use orders::*;
use vendor_invoices::*;
use vendors::*;
use warehouse::*;

/// Cross-service clients the warehouse-status projection reads.
/// Bundled so the binary constructs once and passes a single Option —
/// `clients = None` answers the shipping leg as unavailable, not the read.
pub struct WarehouseClients {
    pub shipping: Arc<dyn ShippingClient>,
}

pub struct InventoryApiState<R: InventoryRepository> {
    pub inventory: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    pub clients: Option<WarehouseClients>,
    /// Optional Class registry for `DiscrepancyKind` validation. When
    /// configured, every vendor-invoice upsert that carries a
    /// `discrepancy_kind` checks that the code exists under
    /// `(subject_kind='vendor-invoice')` in the Class registry. When
    /// `None`, the API is permissive (matches the catalog
    /// `check_category` gate). An absent discrepancy_kind is always
    /// accepted — the field is optional on a clean match.
    pub classes_client: Option<Arc<dyn ClassesClient>>,
    /// Authoritative clock. See `boss-clock-client` for the
    /// shape; every handler reads `now` via `state.clock.now()`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

/// Build the enrichment stamp for in-tx event recording: the caller's
/// actor + the authoritative timestamp, with `_simulated` resolved by
/// the publisher's clock probe when one is wired (task-local sim
/// chain only otherwise — the test paths). Write handlers record
/// their events in the DOMAIN TRANSACTION via this stamp (outbox
/// phase 2); the publisher no longer publishes those kinds
/// post-commit.
pub(crate) async fn event_stamp<R: InventoryRepository>(
    state: &InventoryApiState<R>,
    user: &boss_policy_client::User,
) -> boss_core::publisher::EventStamp {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    match &state.publisher {
        Some(p) => p.stamp_with_actor(actor).await,
        None => boss_core::publisher::EventStamp::new("inventory", actor),
    }
}

pub fn router<R: InventoryRepository + 'static>(state: InventoryApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/inventory/health", get(health))
        .route("/api/inventory/items", get(list_items::<R>))
        .route("/api/inventory/items/{part_sku}", get(get_item::<R>))
        .route("/api/inventory/orders", get(list_orders::<R>))
        .route("/api/inventory/orders/{id}", get(get_order::<R>))
        .route(
            "/api/inventory/items/{part_sku}/consume",
            post(consume_part::<R>),
        )
        .route(
            "/api/inventory/items/{part_sku}/open-po-exists",
            get(open_po_exists_for_sku::<R>),
        )
        .route(
            "/api/inventory/items/{part_sku}/primary-vendor",
            get(primary_vendor_for_sku::<R>),
        )
        .route(
            "/api/inventory/overhead-absorbed",
            post(overhead_absorbed_handler::<R>),
        )
        .route(
            "/api/inventory/items/{part_sku}/receive",
            post(receive_part_handler::<R>),
        )
        .route("/api/inventory/items/batch", post(batch_upsert_items::<R>))
        .route("/api/inventory/orders/create", post(create_order::<R>))
        .route(
            "/api/inventory/orders/batch",
            post(batch_create_orders::<R>),
        )
        .route(
            "/api/inventory/orders/{id}/status",
            put(update_order_status::<R>),
        )
        .route("/api/inventory/vendors", get(list_vendors::<R>))
        .route("/api/inventory/vendors", post(create_vendor::<R>))
        .route(
            "/api/inventory/vendors/{id}",
            put(update_vendor::<R>).delete(delete_vendor::<R>),
        )
        .route(
            "/api/inventory/vendor-invoices",
            get(list_vendor_invoices::<R>).post(upsert_vendor_invoice::<R>),
        )
        .route(
            "/api/inventory/vendor-invoices/batch-pay",
            post(batch_pay_vendor_invoices::<R>),
        )
        .route(
            "/api/inventory/vendor-invoices/from-po/{po_id}",
            post(create_vendor_invoice_from_po::<R>),
        )
        .route("/api/inventory/ap-aging", get(ap_aging::<R>))
        .route(
            "/api/inventory/warehouse-status",
            get(warehouse_status::<R>),
        )
        .with_state(shared)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-inventory-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::in_memory::InMemoryInventory;
    use crate::types::*;

    fn test_item(sku: &str) -> InventoryItem {
        InventoryItem {
            part_sku: sku.to_string(),
            bin: "A-01".to_string(),
            on_hand: 50,
            allocated: 10,
            reorder_point: 20,
            reorder_qty: 100,
            trailing_90d_usage: 30,
            value_cents: 0,
            avg_cost_cents: 0,
            vendor_price_cents: None,
            vendor_category: None,
        }
    }

    fn test_po(id: &str) -> PurchaseOrder {
        PurchaseOrder {
            id: id.to_string(),
            vendor: Some("Acme Parts Co".to_string()),
            status: PoStatus::new(PoStatus::SUBMITTED),
            placed_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 3, 1).unwrap()),
            expected_on: Some(chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap()),
            received_on: None,
            lines: vec![PurchaseOrderLine {
                part_sku: "PART-001".to_string(),
                qty: 25,
                unit_cost_cents: 15_000,
                currency: "USD".to_string(),
            }],
        }
    }

    fn test_app() -> Router {
        let inventory = Arc::new(InMemoryInventory::new(
            vec![test_item("PART-001"), test_item("PART-002")],
            vec![test_po("PO-001"), test_po("PO-002")],
        ));
        router(InventoryApiState {
            inventory,
            publisher: None,
            clients: None,
            classes_client: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        })
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_items_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let items: Vec<InventoryItem> = serde_json::from_slice(&body).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn get_item_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/items/PART-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_item_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/items/PART-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_orders_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/orders")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let orders: Vec<PurchaseOrder> = serde_json::from_slice(&body).unwrap();
        assert_eq!(orders.len(), 2);
    }

    #[tokio::test]
    async fn get_order_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/orders/PO-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_order_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/orders/PO-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------
    // Vendor invoice three-way match endpoints
    // -----------------------------------------------------------------

    fn test_vendor_invoice(id: &str, po_id: &str) -> VendorInvoice {
        VendorInvoice {
            id: id.to_string(),
            po_id: po_id.to_string(),
            vendor: "Acme Parts Co".to_string(),
            vendor_invoice_no: "INV-EXT-1".to_string(),
            amount_cents: 375_000,
            currency: "USD".to_string(),
            received_on: chrono::NaiveDate::from_ymd_opt(2025, 3, 20).unwrap(),
            matched_on: None,
            approved_on: None,
            paid_on: None,
            status: VendorInvoiceStatus::new(VendorInvoiceStatus::RECEIVED),
            discrepancy_cents: None,
            discrepancy_kind: None,
            lines: Vec::new(),
        }
    }

    #[tokio::test]
    async fn post_vendor_invoice_creates_and_lists() {
        let app = test_app();
        let invoice = test_vendor_invoice("VI-1", "PO-001");
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/inventory/vendor-invoices")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&invoice).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let list_resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/vendor-invoices")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(list_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: Vec<VendorInvoice> = serde_json::from_slice(&body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "VI-1");
        assert_eq!(
            rows[0].status,
            VendorInvoiceStatus::new(VendorInvoiceStatus::RECEIVED)
        );
    }

    #[tokio::test]
    async fn vendor_posts_invoice_from_po_resolves_lines() {
        // The vendor's webhook only names the PO; the endpoint resolves the
        // lines + amount from the PO row and lands the invoice `received`.
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/inventory/vendor-invoices/from-po/PO-001")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let inv: VendorInvoice = serde_json::from_slice(&body).unwrap();
        assert_eq!(inv.id, "vi-PO-001");
        assert_eq!(inv.po_id, "PO-001");
        assert_eq!(inv.vendor, "Acme Parts Co");
        assert_eq!(
            inv.status,
            VendorInvoiceStatus::new(VendorInvoiceStatus::RECEIVED)
        );
        // Resolved from the PO's line (25 × 15_000), not supplied by caller.
        assert_eq!(inv.amount_cents, 25 * 15_000);
        assert_eq!(inv.lines.len(), 1);
        assert_eq!(inv.lines[0].part_sku, "PART-001");
        assert!(inv.approved_on.is_none());
    }

    #[tokio::test]
    async fn from_po_404_for_unknown_po() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/inventory/vendor-invoices/from-po/PO-NOPE")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn from_po_no_op_when_invoice_already_exists() {
        // The guard: once an invoice exists for the PO (e.g. the human
        // bill-approval landed first and advanced it), a late vendor post
        // must NOT overwrite/downgrade it — it no-ops with 200.
        let app = test_app();
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/inventory/vendor-invoices/from-po/PO-001")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::CREATED);
        let second = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/inventory/vendor-invoices/from-po/PO-001")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK); // no-op, not CREATED
    }

    // ----- DiscrepancyKind Class-registry gate -----------------------

    fn app_with_classes_client(classes: Arc<dyn boss_classes_client::ClassesClient>) -> Router {
        let inventory = Arc::new(InMemoryInventory::new(
            vec![test_item("PART-001")],
            vec![test_po("PO-001")],
        ));
        router(InventoryApiState {
            inventory,
            publisher: None,
            clients: None,
            classes_client: Some(classes),
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        })
    }

    fn post_invoice_request(invoice: &VendorInvoice) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/inventory/vendor-invoices")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(invoice).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn upsert_invoice_rejected_when_discrepancy_kind_unknown() {
        use boss_classes_client::FakeClassesClient;
        use boss_core::primitives::ClassRef;
        // Registry only knows `overbilled`; the invoice claims `shorted`
        // → 400 with the actionable error message.
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "vendor-invoice",
            "overbilled",
        )])) as Arc<dyn boss_classes_client::ClassesClient>;
        let app = app_with_classes_client(classes);
        let mut invoice = test_vendor_invoice("VI-D1", "PO-001");
        invoice.discrepancy_cents = Some(500);
        invoice.discrepancy_kind = Some(crate::types::DiscrepancyKind::new("shorted"));
        let resp = app.oneshot(post_invoice_request(&invoice)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(
            body.contains("shorted") && body.contains("subject_kind='vendor-invoice'"),
            "error must name the rejected code and the registry shape, got: {body}"
        );
    }

    #[tokio::test]
    async fn upsert_invoice_accepted_when_discrepancy_kind_registered() {
        use boss_classes_client::FakeClassesClient;
        let classes = Arc::new(FakeClassesClient::permissive())
            as Arc<dyn boss_classes_client::ClassesClient>;
        let app = app_with_classes_client(classes);
        let mut invoice = test_vendor_invoice("VI-D2", "PO-001");
        invoice.discrepancy_cents = Some(500);
        invoice.discrepancy_kind = Some(crate::types::DiscrepancyKind::new("wrong-price"));
        let resp = app.oneshot(post_invoice_request(&invoice)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn upsert_invoice_skips_gate_when_discrepancy_kind_absent() {
        use boss_classes_client::FakeClassesClient;
        use boss_core::primitives::ClassRef;
        // Registry knows no discrepancy code but does know the status
        // (statuses are always seeded); a clean match
        // (discrepancy_kind = None) must pass straight through.
        let classes = Arc::new(FakeClassesClient::with(vec![
            ClassRef::new("vendor-invoice", "overbilled"),
            ClassRef::new("vendor-invoice", "received"),
        ])) as Arc<dyn boss_classes_client::ClassesClient>;
        let app = app_with_classes_client(classes);
        let invoice = test_vendor_invoice("VI-D3", "PO-001"); // discrepancy_kind: None
        let resp = app.oneshot(post_invoice_request(&invoice)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn upsert_invoice_rejected_when_status_unknown() {
        use boss_classes_client::FakeClassesClient;
        use boss_core::primitives::ClassRef;
        // The status vocabulary lives in the Class registry (the
        // VendorInvoiceStatus enum lift); an unregistered status on the
        // client body is a 400, not a silent write.
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "vendor-invoice",
            "received",
        )])) as Arc<dyn boss_classes_client::ClassesClient>;
        let app = app_with_classes_client(classes);
        let mut invoice = test_vendor_invoice("VI-D5", "PO-001");
        invoice.status = crate::types::VendorInvoiceStatus::new("under-review");
        let resp = app.oneshot(post_invoice_request(&invoice)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(
            body.contains("under-review") && body.contains("subject_kind='vendor-invoice'"),
            "error must name the rejected status and the registry shape, got: {body}"
        );
    }

    #[tokio::test]
    async fn upsert_invoice_permissive_when_classes_client_unset() {
        // No Class registry configured → permissive: even a junk
        // discrepancy_kind lands (test_app sets classes_client: None).
        let app = test_app();
        let mut invoice = test_vendor_invoice("VI-D4", "PO-001");
        invoice.discrepancy_cents = Some(500);
        invoice.discrepancy_kind = Some(crate::types::DiscrepancyKind::new("definitely-not-real"));
        let resp = app.oneshot(post_invoice_request(&invoice)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn post_vendor_invoice_upserts_status_transitions() {
        let app = test_app();
        let mut invoice = test_vendor_invoice("VI-2", "PO-002");
        // First POST: received.
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/inventory/vendor-invoices")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&invoice).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Second POST: matched + approved, same id.
        invoice.status = VendorInvoiceStatus::new(VendorInvoiceStatus::APPROVED);
        invoice.matched_on = Some(chrono::NaiveDate::from_ymd_opt(2025, 3, 21).unwrap());
        invoice.approved_on = Some(chrono::NaiveDate::from_ymd_opt(2025, 3, 22).unwrap());
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/inventory/vendor-invoices")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&invoice).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        // List filtered by status=approved.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/inventory/vendor-invoices?status=approved")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: Vec<VendorInvoice> = serde_json::from_slice(&body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "VI-2");
        assert_eq!(
            rows[0].status,
            VendorInvoiceStatus::new(VendorInvoiceStatus::APPROVED)
        );
        assert!(rows[0].approved_on.is_some());
    }

    // ----- warehouse-status: one leg down is not the whole read ------

    /// A shipping client that answers what it was built with — the
    /// in-memory stand-in for boss-shipping, up or down.
    struct StubShipping(Result<boss_shipping_client::OutboundShipmentSummary, &'static str>);

    #[async_trait::async_trait]
    impl ShippingClient for StubShipping {
        async fn outbound_shipment_summary(
            &self,
        ) -> Result<
            boss_shipping_client::OutboundShipmentSummary,
            boss_shipping_client::ShippingClientError,
        > {
            self.0
                .clone()
                .map_err(|e| boss_shipping_client::ShippingClientError::Unreachable(e.to_string()))
        }
    }

    async fn warehouse_status_with(clients: Option<WarehouseClients>) -> (StatusCode, Value) {
        let inventory = Arc::new(InMemoryInventory::new(
            vec![test_item("PART-001"), test_item("PART-002")],
            vec![test_po("PO-001")],
        ));
        let resp = router(InventoryApiState {
            inventory,
            publisher: None,
            clients,
            classes_client: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        })
        .oneshot(
            Request::builder()
                .uri("/api/inventory/warehouse-status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    use serde_json::Value;

    // Backlog 89cf07d8: a shipping outage — distribution's service,
    // off on the live instance — answered 502 for the whole read, so
    // parts stock and inbound POs, both inventory's own, went with it.
    // The leg that failed is named on the wire; the rest still answers.
    #[tokio::test]
    async fn warehouse_status_keeps_inventory_reads_when_shipping_is_down() {
        let (status, body) = warehouse_status_with(Some(WarehouseClients {
            shipping: Arc::new(StubShipping(Err("connection refused"))),
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["parts_stock"]["total_skus"], 2);
        assert_eq!(body["inbound_pos"]["total_open"], 1);
        assert_eq!(body["outbound_shipments"]["kind"], "unavailable");
        assert_eq!(
            body["outbound_shipments"]["reason"],
            "shipping service unreachable: connection refused"
        );
    }

    #[tokio::test]
    async fn warehouse_status_keeps_inventory_reads_when_shipping_is_not_configured() {
        let (status, body) = warehouse_status_with(None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["parts_stock"]["total_skus"], 2);
        assert_eq!(body["inbound_pos"]["total_open"], 1);
        assert_eq!(body["outbound_shipments"]["kind"], "unavailable");
        assert_eq!(
            body["outbound_shipments"]["reason"],
            "shipping client not configured"
        );
    }

    #[tokio::test]
    async fn warehouse_status_carries_the_shipping_summary_when_it_answers() {
        let summary = boss_shipping_client::OutboundShipmentSummary {
            in_transit: 6,
            ..Default::default()
        };
        let (status, body) = warehouse_status_with(Some(WarehouseClients {
            shipping: Arc::new(StubShipping(Ok(summary))),
        }))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["outbound_shipments"]["kind"], "ok");
        assert_eq!(body["outbound_shipments"]["summary"]["in_transit"], 6);
    }
}
