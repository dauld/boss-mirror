//! Axum HTTP handlers for the commerce API.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_core::publisher::DomainPublisher;
use boss_people_client::PeopleClient;
use boss_policy::{Action, Decision, Resource};
use boss_policy_client::{CurrentUser, PolicyClient};

use crate::port::{CommerceError, CommerceRepository};

fn error_response(err: CommerceError) -> Response {
    match err {
        CommerceError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        CommerceError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        CommerceError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1000;

#[derive(Deserialize)]
struct ListFilter {
    limit: Option<i64>,
    offset: Option<i64>,
    /// Account-scoped filter for the unified account detail view.
    /// Optional — when omitted, returns all rows.
    account_id: Option<String>,
}

impl ListFilter {
    fn limit(&self) -> i64 {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }
    fn offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

#[derive(Serialize)]
struct PaginatedResponse<T: Serialize> {
    data: Vec<T>,
    total: i64,
    limit: i64,
    offset: i64,
}

pub struct CommerceApiState<R: CommerceRepository> {
    pub commerce: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    /// Cross-service guard for validating account_id at write time.
    /// Wrapped in `Arc<dyn>` so the production binary plugs in
    /// `ReqwestPeopleClient` and tests can substitute a fake.
    pub people_client: Arc<dyn PeopleClient>,
    /// Row-level authorization. Null in tests that don't exercise
    /// the policy path — those handlers skip the gate and treat the
    /// request as allowed (preserves existing test surface until the
    /// broader rollout swaps every test's harness over).
    pub policy: Option<Arc<dyn PolicyClient>>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
    /// Class registry for `InvoiceStatus` validation. When configured,
    /// every invoice create checks the incoming status against the
    /// active Class set under `(subject_kind='invoice')`. When `None`,
    /// the API is permissive (test path) — matching the carrier gate
    /// in boss-shipping. The production binary always wires `Some`
    /// from the required `classes_api_url`.
    pub classes_client: Option<Arc<dyn ClassesClient>>,
}

pub fn router<R: CommerceRepository + 'static>(state: CommerceApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/commerce/health", get(health))
        .route("/api/commerce/revenue", get(list_revenue::<R>))
        .route("/api/commerce/summary", get(commerce_summary::<R>))
        .route("/api/commerce/invoices", get(list_invoices::<R>))
        .route("/api/commerce/invoices/{id}", get(get_invoice::<R>))
        .route("/api/commerce/invoices/create", post(create_invoice::<R>))
        .route("/api/commerce/invoices/batch", post(batch_invoices::<R>))
        .route(
            "/api/commerce/invoices/{id}/paid",
            put(mark_invoice_paid::<R>),
        )
        .route(
            "/api/commerce/invoices/{id}/past-due",
            put(mark_invoice_past_due::<R>),
        )
        .route(
            "/api/commerce/invoices/{id}/write-off",
            put(mark_invoice_written_off::<R>),
        )
        .route(
            "/api/commerce/invoices/write-off/from-past-due",
            post(write_off_invoice_from_past_due::<R>),
        )
        .with_state(shared)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-commerce-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

async fn list_revenue<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
) -> Response {
    match state.commerce.all_revenue().await {
        Ok(data) => Json(data).into_response(),
        Err(e) => error_response(e),
    }
}

async fn commerce_summary<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
) -> Response {
    // Source `today` from ClockClient so AR aging buckets +
    // TTM revenue window respect sim-time. Pre-Clock fix: both
    // queries used PostgreSQL's `CURRENT_DATE` (wallclock) and
    // every sim-time invoice landed in "90+" with TTM showing
    // $0 once the sim was more than a year behind wallclock.
    let today = state.clock.now().await.now.date_naive();
    match state.commerce.invoice_summary(today).await {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => error_response(e),
    }
}

async fn list_invoices<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Query(filter): Query<ListFilter>,
) -> Response {
    let limit = filter.limit();
    let offset = filter.offset();
    match state
        .commerce
        .list_invoices(limit, offset, filter.account_id.as_deref())
        .await
    {
        Ok((data, total)) => Json(PaginatedResponse {
            data,
            total,
            limit,
            offset,
        })
        .into_response(),
        Err(e) => error_response(e),
    }
}

async fn get_invoice<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.commerce.invoice_by_id(&id).await {
        Ok(Some(inv)) => Json(inv).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no invoice with ID {id}")).into_response(),
        Err(e) => error_response(e),
    }
}

/// Validate an incoming `InvoiceStatus` against the Class registry.
///
/// `InvoiceStatus` is a free-text wrapper (the closed enum was lifted to
/// a String-newtype in v1.1.0), so the registry is what makes a status
/// string mean something. Status is a non-optional field, so the gate
/// fires on every create. Same contract as `check_carrier` in
/// boss-shipping: permissive when no registry is wired (test path),
/// fail-closed 503 when unreachable, 400 on an unregistered code. Keys
/// on `(subject_kind='invoice', code)`.
async fn check_status(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    status: &str,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("invoice", status);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown invoice status `{status}` — register it as a Class \
                 first (subject_kind='invoice')"
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("classes registry unreachable: {e}"),
        )
            .into_response()),
    }
}

/// Validate a line item's `revenue_category` against the Class registry.
///
/// Revenue categories are a taxonomy the `invoice` subject kind owns
/// (`01-registries.sql`: invoice "owns the status + revenue-category
/// taxonomies"), tenant-curated and seeded per-tenant — the brewery's
/// seven live in classes.json alongside the platform status codes,
/// sharing the `invoice` code namespace without collision
/// (member_attribute is display metadata, not part of the existence
/// key). Same key + same contract as `check_status`: permissive when no
/// registry is wired (test path), 400 on an unregistered code, 503 when
/// the registry is unreachable.
async fn check_revenue_category(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    category: &str,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("invoice", category);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown revenue_category `{category}` — register it as a \
                 Class first (subject_kind='invoice')"
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("classes registry unreachable: {e}"),
        )
            .into_response()),
    }
}

async fn create_invoice<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(invoice): Json<crate::types::Invoice>,
) -> Response {
    // Class-registry gate: the invoice status must be a registered
    // Class under (subject_kind='invoice'). Permissive when no registry
    // is wired (test path). Runs before policy so a malformed status is
    // a clean 400 regardless of the caller's role.
    if let Err(resp) = check_status(state.classes_client.as_ref(), invoice.status.as_str()).await {
        return resp;
    }
    // Every line's revenue_category must likewise be a registered Class
    // under (subject_kind='invoice'). Same fail-loud gate as status.
    for line in &invoice.line_items {
        if let Err(resp) = check_revenue_category(
            state.classes_client.as_ref(),
            line.revenue_category.as_str(),
        )
        .await
        {
            return resp;
        }
    }
    // Policy: creating an invoice requires an active Create rule on
    // Resource::invoice() for the caller's role. If the state doesn't
    // carry a policy client (test path), skip — the existing tests
    // cover the invariants without role gating.
    if let Some(ref policy) = state.policy {
        match policy
            .check(&user, Action::Create, Resource::invoice())
            .await
        {
            Ok(Decision::Allow { .. }) => {}
            Ok(Decision::Deny { reason }) => {
                return (StatusCode::FORBIDDEN, reason).into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("policy check failed: {e}"),
                )
                    .into_response();
            }
        }
    }
    let invoice_id = invoice.id.clone();
    // Outbox phase 2: the adapter records commerce.invoice.created
    // (the ENRICHED invoice — cost_basis populated in-tx) inside the
    // domain transaction; nothing publishes post-commit.
    let stamp = event_stamp(&state, &user).await;
    match state
        .commerce
        .create_invoice_at(&invoice, stamp.timestamp, &stamp)
        .await
    {
        Ok(_enriched) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"ok": true, "id": invoice_id})),
        )
            .into_response(),
        Err(e) => error_response(e),
    }
}

/// Resolve the outbox event stamp for this request: the caller's
/// actor + the authoritative timestamp, with `_simulated` resolved by
/// the publisher's clock probe when one is wired (task-local sim
/// chain only, otherwise — the test paths). The four invoice kinds
/// record their events in the DOMAIN TRANSACTION via this stamp
/// (outbox phase 2); the publisher no longer publishes them
/// post-commit.
async fn event_stamp<R: CommerceRepository>(
    state: &CommerceApiState<R>,
    user: &boss_policy_client::User,
) -> boss_core::publisher::EventStamp {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    match &state.publisher {
        Some(p) => p.stamp_with_actor(actor).await,
        None => boss_core::publisher::EventStamp::new("commerce", actor),
    }
}

async fn batch_invoices<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(invoices): Json<Vec<crate::types::Invoice>>,
) -> Response {
    let mut inserted = 0u64;
    let mut skipped: Vec<(String, String)> = Vec::new();
    let stamp = event_stamp(&state, &user).await;
    for inv in &invoices {
        let now = stamp.timestamp;
        // Status registry gate — reject rows with an unregistered (or
        // unreachable-registry) status, matching this endpoint's
        // skip-and-report batch semantics. Permissive when no registry
        // is wired.
        if let Err(resp) = check_status(state.classes_client.as_ref(), inv.status.as_str()).await {
            let _ = resp;
            tracing::warn!(
                invoice_id = %inv.id,
                status = %inv.status,
                "invoice status not registered as a Class; skipping invoice in batch"
            );
            skipped.push((
                inv.id.clone(),
                format!("unregistered status `{}`", inv.status),
            ));
            continue;
        }
        // Same registry gate for each line's revenue_category — skip
        // the whole invoice and report if any line carries an
        // unregistered (or unreachable-registry) category.
        let bad_category = {
            let mut found = None;
            for line in &inv.line_items {
                if check_revenue_category(
                    state.classes_client.as_ref(),
                    line.revenue_category.as_str(),
                )
                .await
                .is_err()
                {
                    found = Some(line.revenue_category.as_str().to_string());
                    break;
                }
            }
            found
        };
        if let Some(cat) = bad_category {
            tracing::warn!(
                invoice_id = %inv.id,
                revenue_category = %cat,
                "invoice line revenue_category not registered as a Class; skipping invoice in batch"
            );
            skipped.push((
                inv.id.clone(),
                format!("unregistered revenue_category `{cat}`"),
            ));
            continue;
        }
        // Retry a transient Postgres deadlock (40P01): concurrent invoice
        // txs can briefly contend on finished-goods FOR UPDATE row locks
        // under burst load. Deterministic lock ordering in the adapter
        // (sort by SKU) prevents the common case; this rides out any
        // residual so the batch recovers in-request instead of skipping →
        // NAK → dead-letter (which then 404s the downstream collection).
        // create_invoice_at is idempotent (ON CONFLICT + already-issued
        // guard), so re-invoking is safe.
        let mut attempt = 0u32;
        let result = loop {
            match state.commerce.create_invoice_at(inv, now, &stamp).await {
                Err(e) if attempt < 4 && e.to_string().contains("deadlock detected") => {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(15 * attempt as u64)).await;
                }
                other => break other,
            }
        };
        match result {
            Ok(_enriched) => {
                inserted += 1;
            }
            Err(e) => {
                // On a rejected row, log per-row + return the
                // rejected ids so the batch caller can act on the
                // loss rather than silently seeing a lower
                // `inserted` count than it sent.
                tracing::warn!(
                    invoice_id = %inv.id,
                    account_id = %inv.account_id,
                    error = %e,
                    "create_invoice_at failed; skipping invoice in batch"
                );
                skipped.push((inv.id.clone(), e.to_string()));
            }
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "inserted": inserted,
            "skipped": skipped.iter()
                .map(|(id, err)| serde_json::json!({"id": id, "error": err}))
                .collect::<Vec<_>>(),
        })),
    )
        .into_response()
}

#[derive(Debug, Default, serde::Deserialize)]
struct MarkPaidBody {
    /// Sim-day the invoice was paid on. Optional — when absent,
    /// the handler stamps wall-clock NOW() for backwards
    /// compatibility with manual operator clicks. Sim drivers
    /// supply the sim-day so projections age correctly instead
    /// of bunching on whichever wall-clock day the engine
    /// ticked.
    ///
    /// Aliased to `_day` so counterparty triggers in the
    /// shape-driven engine — which auto-inject `_day` into
    /// every event payload (see `engines/batch.rs::inject_day`)
    /// — flow through this path without a per-event payload
    /// rewrite. Sim's manual `end_of_day` flush sends `paid_on`
    /// directly.
    #[serde(alias = "_day")]
    paid_on: Option<chrono::NaiveDate>,
}

async fn mark_invoice_paid<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    body: Option<axum::Json<MarkPaidBody>>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let paid_on = body
        .and_then(|axum::Json(b)| b.paid_on)
        .unwrap_or_else(|| now.date_naive());
    // Outbox phase 2: the adapter records commerce.invoice.paid (full
    // post-update row state, read back inside its own transaction).
    let stamp = event_stamp(&state, &user).await;
    match state
        .commerce
        .mark_invoice_paid_at(&id, paid_on, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error_response(e),
    }
}

async fn mark_invoice_past_due<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    // Outbox phase 2: recorded in the adapter's transaction.
    let stamp = event_stamp(&state, &user).await;
    match state.commerce.mark_invoice_past_due(&id, &stamp).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error_response(e),
    }
}

async fn mark_invoice_written_off<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    // Outbox phase 2: the adapter records the event in the flip's own
    // transaction, structurally gated on the flip winning — the
    // emit-once-on-`newly` dance this handler used to do is gone.
    let stamp = event_stamp(&state, &user).await;
    match state.commerce.mark_invoice_written_off(&id, &stamp).await {
        Ok(_newly) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error_response(e),
    }
}

/// Adapter for the `[counterparty.bad-debt-writeoff]` drive. The
/// counterparty fires once per `commerce.invoice.past_due` copy it
/// receives, and post-#100 there are TWO per invoice: ar-aging's
/// sim-internal emission (trigger = the `step.done.billing` payload,
/// so step_id sits at `trigger.trigger.step_id`) and the system's
/// webhook copy (the enriched invoice row — its own `id`, no trigger
/// lineage). A path template can resolve only one of those, which is
/// how the first post-#102 year run hard-failed at sim-day ~119.
/// Mirrors from-paid-invoice (#102): resolve either shape to the
/// invoice id, then let the transition-gated flip converge the
/// double delivery — the event emits only on the actual flip.
#[derive(Deserialize)]
struct FromPastDueBody {
    trigger: serde_json::Value,
}

async fn write_off_invoice_from_past_due<R: CommerceRepository + 'static>(
    State(state): State<Arc<CommerceApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<FromPastDueBody>,
) -> Response {
    let invoice_id = if let Some(step_id) = body
        .trigger
        .get("trigger")
        .and_then(|t| t.get("step_id"))
        .and_then(|v| v.as_str())
    {
        format!("inv-step-{step_id}")
    } else if let Some(id) = body
        .trigger
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|id| id.starts_with("inv-"))
    {
        id.to_string()
    } else {
        return (
            StatusCode::BAD_REQUEST,
            "trigger carries neither trigger.step_id (counterparty chain) \
             nor an inv-* id (invoice-past-due webhook copy)"
                .to_string(),
        )
            .into_response();
    };

    // Outbox phase 2: emit-once is structural in the adapter's
    // transaction; `newly` stays in the response for the caller.
    let stamp = event_stamp(&state, &user).await;
    match state
        .commerce
        .mark_invoice_written_off(&invoice_id, &stamp)
        .await
    {
        Ok(newly) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "invoice_id": invoice_id,
                "written_off": newly,
            })),
        )
            .into_response(),
        Err(e) => error_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_people_client::PeopleClientError;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryCommerce;
    use crate::types::*;

    /// Test stub: every account and employee "exists." Lets the
    /// inline tests in this module exercise the existing happy
    /// paths without spinning up an HTTP server.
    struct AlwaysExistsPeople;

    #[async_trait::async_trait]
    impl PeopleClient for AlwaysExistsPeople {
        async fn employee_exists(&self, _id: &str) -> Result<bool, PeopleClientError> {
            Ok(true)
        }
        async fn account_exists(&self, _id: &str) -> Result<bool, PeopleClientError> {
            Ok(true)
        }
    }

    fn test_invoice(id: &str) -> Invoice {
        Invoice {
            id: id.to_string(),
            account_id: "account-001".to_string(),
            issued_on: chrono::NaiveDate::from_ymd_opt(2025, 3, 15).unwrap(),
            due_on: chrono::NaiveDate::from_ymd_opt(2025, 4, 15).unwrap(),
            paid_on: None,
            status: InvoiceStatus::OUTSTANDING.into(),
            amount_cents: 1_200_000,
            currency: "USD".to_string(),
            tax_cents: 0,
            tax_jurisdiction: None,
            payment_method: None,
            line_items: vec![InvoiceLineItem {
                id: format!("{id}-l1"),
                invoice_id: id.to_string(),
                revenue_category: RevenueCategory::from("wholesale"),
                amount_cents: 1_200_000,
                currency: "USD".to_string(),
                description: "Test device sale".to_string(),
                ref_id: None,
                sku: None,
                qty: None,
                cost_basis_cents: None,
                cost_total_cents: None,
            }],
        }
    }

    #[test]
    fn invoice_created_payload_omits_tax_lines_when_untaxed() {
        let payload = crate::events::invoice_created_payload(&test_invoice("inv-notax"));
        assert!(
            payload.get("tax_lines").is_none(),
            "zero-tax invoice must not carry tax_lines in the audit payload"
        );
        // Header + line_items still present.
        assert_eq!(payload["id"], "inv-notax");
        assert!(payload["line_items"].is_array());
    }

    #[test]
    fn invoice_created_payload_injects_tax_lines_when_taxed() {
        // Mirror the live fact: a taxed invoice's audit event MUST carry
        // tax_lines so the rebuild reconstructs the 2300 credit.
        let mut inv = test_invoice("inv-taxed");
        inv.amount_cents = 1_300_000;
        inv.tax_cents = 100_000;
        inv.tax_jurisdiction = Some("US-CA".to_string());

        let payload = crate::events::invoice_created_payload(&inv);
        let tax_lines = payload
            .get("tax_lines")
            .and_then(|v| v.as_array())
            .expect("taxed invoice carries tax_lines");
        assert_eq!(tax_lines.len(), 1);
        assert_eq!(tax_lines[0]["account"], "2300");
        assert_eq!(tax_lines[0]["amount_cents"], 100_000);
        assert_eq!(tax_lines[0]["jurisdiction"], "US-CA");
    }

    fn test_app() -> Router {
        let commerce = Arc::new(InMemoryCommerce::new(vec![
            test_invoice("inv-001"),
            test_invoice("inv-002"),
        ]));
        let policy: Arc<dyn PolicyClient> = Arc::new(boss_policy_client::PermissivePolicyClient);
        router(CommerceApiState {
            commerce,
            publisher: None,
            people_client: Arc::new(AlwaysExistsPeople),
            policy: Some(policy),
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: None,
        })
    }

    fn test_app_with_classes(classes: Arc<dyn ClassesClient>) -> Router {
        let commerce = Arc::new(InMemoryCommerce::new(vec![]));
        let policy: Arc<dyn PolicyClient> = Arc::new(boss_policy_client::PermissivePolicyClient);
        router(CommerceApiState {
            commerce,
            publisher: None,
            people_client: Arc::new(AlwaysExistsPeople),
            policy: Some(policy),
            clock: Arc::new(boss_clock_client::WallClockClient),
            classes_client: Some(classes),
        })
    }

    async fn post_invoice(app: &Router, invoice: &Invoice) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/commerce/invoices/create")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(invoice).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// With a Class registry wired, every invoice line's
    /// `revenue_category` is validated against (subject_kind='invoice',
    /// code): a registered category creates (201), an unregistered one is
    /// rejected (400). The status gate must also see a registered code,
    /// so `outstanding` is registered alongside.
    #[tokio::test]
    async fn create_invoice_gates_revenue_category_against_the_class_registry() {
        use boss_classes_client::FakeClassesClient;

        let classes = Arc::new(FakeClassesClient::with(vec![
            ClassRef::new("invoice", "outstanding"),
            ClassRef::new("invoice", "wholesale"),
        ])) as Arc<dyn ClassesClient>;
        let app = test_app_with_classes(classes);

        // Registered revenue_category → created.
        let mut ok = test_invoice("inv-ok");
        ok.line_items[0].revenue_category = RevenueCategory::from("wholesale");
        assert_eq!(post_invoice(&app, &ok).await.status(), StatusCode::CREATED);

        // Unregistered revenue_category → rejected before the write.
        let mut bad = test_invoice("inv-bad");
        bad.line_items[0].revenue_category = RevenueCategory::from("unicorn-sales");
        assert_eq!(
            post_invoice(&app, &bad).await.status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/commerce/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_invoice_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/commerce/invoices/inv-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn mark_paid_body_accepts_paid_on_and_day_alias() {
        // Sim's end_of_day flush sends `paid_on` directly.
        let direct: MarkPaidBody = serde_json::from_value(serde_json::json!({
            "paid_on": "2026-04-15"
        }))
        .unwrap();
        assert_eq!(
            direct.paid_on,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
        );

        // Counterparty triggers go through `engines/batch.rs::inject_day`,
        // which adds `_day` to every payload. Alias picks it up so the
        // PUT-to-route path stamps sim-day instead of falling through
        // to the wall-clock NOW() default.
        let aliased: MarkPaidBody = serde_json::from_value(serde_json::json!({
            "_day": "2026-04-16",
            "trigger": { "step_id": "step-123" }
        }))
        .unwrap();
        assert_eq!(
            aliased.paid_on,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 16).unwrap()),
        );

        // Empty body: SPA-driven operator click. Handler falls back to
        // wall-clock NOW() (verified at the call site, not here).
        let empty: MarkPaidBody = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(empty.paid_on.is_none());
    }
}
