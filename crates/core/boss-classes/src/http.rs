//! HTTP API for the Class registry. Reads are open; the one write —
//! `POST /api/classes/batch` — seeds the registry via the public API
//! (replacing the direct `psql -f classes.sql` end-around) and is
//! gated to operator-tier callers (with the `x-sim-origin` bypass).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use boss_core::primitives::{Class, ClassRef};
use boss_policy_client::{AccessTier, CurrentUser};
use serde::Deserialize;
use serde_json::Value;

use crate::port::ClassRepository;

#[derive(Clone)]
pub struct ClassesApiState {
    pub classes: Arc<dyn ClassRepository>,
}

pub fn router(state: ClassesApiState) -> Router {
    Router::new()
        .route("/api/classes/health", get(health))
        .route("/api/classes", get(list_classes))
        .route("/api/classes/batch", post(batch_upsert))
        .route(
            "/api/classes/{subject_kind}/{code}",
            get(get_class).put(update_class),
        )
        .route(
            "/api/classes/{subject_kind}/{code}/exists",
            get(class_exists),
        )
        .route(
            "/api/classes/{subject_kind}/{code}/retire",
            axum::routing::post(retire_class),
        )
        .with_state(state)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

/// Standard health probe — every boss-*-api binary exposes one
/// at `/api/<service>/health`. The SPA's MonitoringPage polls
/// this on every page load; a missing endpoint surfaces as 404
/// console spam.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "boss-classes-api",
        "storage": STORAGE,
    }))
}

#[derive(Deserialize)]
struct ListQuery {
    subject_kind: String,
}

async fn list_classes(
    State(state): State<ClassesApiState>,
    Query(q): Query<ListQuery>,
) -> Response {
    match state.classes.list_for_subject_kind(&q.subject_kind).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_class(
    State(state): State<ClassesApiState>,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    let class_ref = ClassRef::new(subject_kind, code);
    match state.classes.get(&class_ref).await {
        Ok(Some(c)) => Json(c).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such class").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `PUT /api/classes/{subject_kind}/{code}` — edit an existing Class.
///
/// `batch_upsert` is insert-if-absent by design, so that a re-run of a
/// seed cannot clobber operator edits. The consequence nobody noticed
/// was that once a Class was seeded, NOTHING could change it: the
/// registry CLAUDE.md §9 calls tenant-editable data was write-once.
/// This is the edit path.
///
/// The composite key is taken from the URL and never from the body, so
/// a rename is impossible here — a code is an identity other rows point
/// at (`employees.role`), and rewriting it in place would orphan them
/// silently. Renaming is a retire-and-create, deliberately louder.
async fn update_class(
    State(state): State<ClassesApiState>,
    CurrentUser(user): CurrentUser,
    Path((subject_kind, code)): Path<(String, String)>,
    Json(body): Json<ClassInput>,
) -> Response {
    let sim = boss_core::sim_origin::is_in_sim_chain();
    let tier_ok = matches!(user.access_tier, AccessTier::Operator);
    if !(sim || tier_ok) {
        return (StatusCode::FORBIDDEN, "operator tier required").into_response();
    }

    let mut class: Class = body.into();
    // URL wins. A body that disagrees is a caller error, not an
    // instruction to move the row.
    class.subject_kind = subject_kind;
    class.code = code;

    match state.classes.update(&class).await {
        Ok(true) => Json(class).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such class").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Withdraw a Class from active use. POST, not DELETE: the row stays
/// (other rows point at the code), and what happens is a state
/// transition — `retired_at` gets stamped, `list` and `exists_active`
/// stop offering the code. Idempotent; 404 only when the composite
/// key names nothing.
async fn retire_class(
    State(state): State<ClassesApiState>,
    CurrentUser(user): CurrentUser,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    let sim = boss_core::sim_origin::is_in_sim_chain();
    let tier_ok = matches!(user.access_tier, AccessTier::Operator);
    if !(sim || tier_ok) {
        return (StatusCode::FORBIDDEN, "operator tier required").into_response();
    }
    let class_ref = ClassRef::new(subject_kind, code);
    match state.classes.retire(&class_ref).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such class").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn class_exists(
    State(state): State<ClassesApiState>,
    Path((subject_kind, code)): Path<(String, String)>,
) -> Response {
    let class_ref = ClassRef::new(subject_kind, code);
    match state.classes.exists_active(&class_ref).await {
        Ok(b) => Json(serde_json::json!({ "exists": b })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// One row in a `POST /api/classes/batch` body. Mirrors the `classes`
/// table's authorable columns; `retired_at` / `created_at` /
/// `updated_at` are owned by the table and not accepted here (seeded
/// rows arrive active; withdrawing one is its own action — POST
/// `{subject_kind}/{code}/retire`). Optional fields default so a
/// minimal seed row is `{"subject_kind","code","display_name"}`.
///
/// Public since fcc1d57b: `boss tenant check` parses a tenant's
/// `seeds/classes.json` / `classes.toml` rows with THIS type, so the
/// check and the batch endpoint cannot disagree about a row.
#[derive(Debug, Deserialize)]
pub struct ClassInput {
    pub subject_kind: String,
    pub code: String,
    pub display_name: String,
    #[serde(default)]
    pub parent_code: Option<String>,
    #[serde(default)]
    pub member_attribute: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
    #[serde(default)]
    pub sort_order: i32,
}

fn empty_object() -> Value {
    serde_json::json!({})
}

impl From<ClassInput> for Class {
    fn from(i: ClassInput) -> Self {
        Class {
            subject_kind: i.subject_kind,
            code: i.code,
            display_name: i.display_name,
            parent_code: i.parent_code,
            member_attribute: i.member_attribute,
            metadata: i.metadata,
            sort_order: i.sort_order,
            retired_at: None,
        }
    }
}

/// Batch-upsert Class rows — the single write surface, used to seed
/// the registry from JSON instead of `psql -f classes.sql`. Each row
/// inserts `ON CONFLICT (subject_kind, code) DO NOTHING`, so the call
/// is idempotent.
///
/// Gated to operator-tier callers, with the `x-sim-origin` bypass that
/// every seed path honors (the trusted simulator/seeder masquerades as
/// operators; its requests carry `x-sim-origin: true`, which the
/// request-context middleware scopes into `is_in_sim_chain`). Reads
/// stay open; only this write is privileged.
async fn batch_upsert(
    State(state): State<ClassesApiState>,
    CurrentUser(user): CurrentUser,
    Json(rows): Json<Vec<ClassInput>>,
) -> Response {
    let sim = boss_core::sim_origin::is_in_sim_chain();
    let tier_ok = matches!(user.access_tier, AccessTier::Operator);
    if !(sim || tier_ok) {
        return (StatusCode::FORBIDDEN, "operator tier required").into_response();
    }

    let classes: Vec<Class> = rows.into_iter().map(Into::into).collect();
    match state.classes.batch_upsert(&classes).await {
        Ok(inserted) => Json(serde_json::json!({
            "received": classes.len(),
            "inserted": inserted,
        }))
        .into_response(),
        // A class for an unregistered kind is a caller error, not a
        // storage failure — 422 with the offending kind named.
        Err(e @ crate::port::ClassError::UnregisteredKind(_)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryClasses;
    use axum::body::to_bytes;
    use axum::http::Request;
    use boss_core::primitives::Class;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn employee(code: &str, sort: i32) -> Class {
        Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.to_uppercase(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: json!({}),
            sort_order: sort,
            retired_at: None,
        }
    }

    fn build_app(rows: Vec<Class>) -> Router {
        let state = ClassesApiState {
            classes: Arc::new(InMemoryClasses::new(rows)),
        };
        router(state)
    }

    #[tokio::test]
    async fn list_returns_classes_for_subject_kind() {
        let app = build_app(vec![employee("ceo", 10), employee("cto", 11)]);
        let req = Request::builder()
            .uri("/api/classes?subject_kind=employee")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_returns_404_for_missing_class() {
        let app = build_app(vec![employee("ceo", 10)]);
        let req = Request::builder()
            .uri("/api/classes/employee/no-such-role")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn exists_returns_boolean_envelope() {
        let app = build_app(vec![employee("ceo", 10)]);
        let req = Request::builder()
            .uri("/api/classes/employee/ceo/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    /// A Class code containing a slash (`1/2-bbl-keg`, a product
    /// package_unit) must round-trip through the `/exists` path: the
    /// client percent-encodes the slash to `%2F`, matchit keeps it in a
    /// single `{code}` segment, and the `Path` extractor decodes it back
    /// before the lookup. Without encoding the raw slash splits the path
    /// and 404s — the bug that broke the products taxonomy gate.
    #[tokio::test]
    async fn exists_resolves_a_slash_in_the_code_when_percent_encoded() {
        let class = Class {
            subject_kind: "product".into(),
            code: "1/2-bbl-keg".into(),
            display_name: "1/2 BBL Keg".into(),
            parent_code: None,
            member_attribute: Some("package_unit".into()),
            metadata: json!({}),
            sort_order: 10,
            retired_at: None,
        };
        let app = build_app(vec![class]);
        let req = Request::builder()
            .uri("/api/classes/product/1%2F2-bbl-keg/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    /// `x-boss-user` JSON for an operator-tier caller. Mirrors the
    /// header the gateway injects + the seed binaries send.
    fn operator_header() -> String {
        json!({
            "id": "automation:test-seed",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    fn batch_request(user_header: Option<&str>, body: Value) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/classes/batch")
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    fn put_request(
        user_header: Option<&str>,
        subject_kind: &str,
        code: &str,
        body: Value,
    ) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("PUT")
            .uri(format!("/api/classes/{subject_kind}/{code}"))
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    fn seeded() -> Arc<InMemoryClasses> {
        Arc::new(InMemoryClasses::new(vec![Class {
            subject_kind: "employee".into(),
            code: "platform-admin".into(),
            display_name: "Platform admin".into(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: json!({"is_executive": true, "is_system_role": true}),
            sort_order: 3,
            retired_at: None,
        }]))
    }

    /// The gap this closes: `batch_upsert` is insert-if-absent, so a
    /// seeded Class could never be edited by anything. A taxonomy the
    /// tenant cannot change is not data.
    #[tokio::test]
    async fn put_edits_an_existing_class() {
        let repo = seeded();
        let app = router(ClassesApiState {
            classes: repo.clone(),
        });
        let resp = app
            .oneshot(put_request(
                Some(&operator_header()),
                "employee",
                "platform-admin",
                json!({
                    "subject_kind": "employee",
                    "code": "platform-admin",
                    "display_name": "Platform admin",
                    "member_attribute": "role",
                    "sort_order": 3,
                    "metadata": {"is_system_role": true}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let stored = repo
            .get(&ClassRef::new("employee", "platform-admin"))
            .await
            .unwrap()
            .expect("row still present");
        assert_eq!(stored.metadata, json!({"is_system_role": true}));
    }

    /// The key comes from the URL, never the body. A code is an
    /// identity other rows point at, so honouring a body that
    /// disagreed would move the row and orphan them silently.
    #[tokio::test]
    async fn put_ignores_a_key_in_the_body() {
        let repo = seeded();
        let app = router(ClassesApiState {
            classes: repo.clone(),
        });
        let resp = app
            .oneshot(put_request(
                Some(&operator_header()),
                "employee",
                "platform-admin",
                json!({
                    "subject_kind": "vendor",
                    "code": "somebody-else",
                    "display_name": "Renamed",
                    "member_attribute": "role",
                    "sort_order": 3,
                    "metadata": {}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        assert!(
            repo.get(&ClassRef::new("vendor", "somebody-else"))
                .await
                .unwrap()
                .is_none(),
            "a body key must not create or move a row"
        );
        let stored = repo
            .get(&ClassRef::new("employee", "platform-admin"))
            .await
            .unwrap()
            .expect("original row edited in place");
        assert_eq!(stored.display_name, "Renamed");
    }

    #[tokio::test]
    async fn put_on_a_missing_class_is_not_found() {
        let app = router(ClassesApiState {
            classes: Arc::new(InMemoryClasses::new(vec![])),
        });
        let resp = app
            .oneshot(put_request(
                Some(&operator_header()),
                "employee",
                "ghost",
                json!({"subject_kind": "employee", "code": "ghost", "display_name": "Ghost", "member_attribute": "role", "sort_order": 1, "metadata": {}}),
            ))
            .await
            .unwrap();
        // Not an implicit create: PUT here edits, and a silent insert
        // would let a typo'd code become a real Class.
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn put_requires_operator_tier() {
        let app = router(ClassesApiState { classes: seeded() });
        let resp = app
            .oneshot(put_request(
                None,
                "employee",
                "platform-admin",
                json!({"subject_kind": "employee", "code": "platform-admin", "display_name": "x", "member_attribute": "role", "sort_order": 3, "metadata": {}}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn batch_upsert_inserts_rows_for_operator() {
        let repo = Arc::new(InMemoryClasses::new(vec![]));
        let app = router(ClassesApiState {
            classes: repo.clone(),
        });
        let body = json!([
            {"subject_kind": "employee", "code": "head-brewer", "display_name": "Head Brewer", "member_attribute": "role", "sort_order": 30},
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer", "member_attribute": "role", "sort_order": 32},
        ]);
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["received"], json!(2));
        assert_eq!(v["inserted"], json!(2));

        let stored = repo.list_for_subject_kind("employee").await.unwrap();
        assert_eq!(stored.len(), 2);
    }

    #[tokio::test]
    async fn batch_upsert_is_idempotent_on_conflict() {
        let repo = Arc::new(InMemoryClasses::new(vec![employee("brewer", 32)]));
        let app = router(ClassesApiState {
            classes: repo.clone(),
        });
        // `brewer` already present → DO NOTHING; only `cellar-tech` is new.
        let body = json!([
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer", "member_attribute": "role", "sort_order": 32},
            {"subject_kind": "employee", "code": "cellar-tech", "display_name": "Cellar Tech", "member_attribute": "role", "sort_order": 33},
        ]);
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["received"], json!(2));
        assert_eq!(v["inserted"], json!(1), "conflicting row is left untouched");
        assert_eq!(
            repo.list_for_subject_kind("employee").await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn batch_upsert_forbidden_for_non_operator() {
        // Default (no header) → anonymous user, AccessTier::User.
        let app = build_app(vec![]);
        let body = json!([
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer"},
        ]);
        let resp = app.oneshot(batch_request(None, body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn batch_upsert_bypassed_by_sim_origin() {
        // Sim traffic carries `x-sim-origin: true`, which the request-
        // context middleware scopes into `is_in_sim_chain`. The router
        // under test omits that middleware, so we set the task-local
        // directly to exercise the bypass branch with a non-operator
        // (anonymous) caller.
        let repo = Arc::new(InMemoryClasses::new(vec![]));
        let app = router(ClassesApiState {
            classes: repo.clone(),
        });
        let body = json!([
            {"subject_kind": "employee", "code": "brewer", "display_name": "Brewer", "member_attribute": "role", "sort_order": 32},
        ]);
        let resp =
            boss_core::sim_origin::with_sim_chain(true, app.oneshot(batch_request(None, body)))
                .await
                .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            repo.list_for_subject_kind("employee").await.unwrap().len(),
            1
        );
    }
    fn retire_request(user_header: Option<&str>, code: &str) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(format!("/api/classes/employee/{code}/retire"));
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn retire_withdraws_a_code_from_active_use_but_keeps_the_row() {
        let app = build_app(vec![employee("ceo", 10), employee("cto", 11)]);
        let h = operator_header();
        let resp = app
            .clone()
            .oneshot(retire_request(Some(&h), "cto"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Withdrawn: the validation primitive refuses it…
        let req = Request::builder()
            .uri("/api/classes/employee/cto/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["exists"],
            json!(false),
            "exists_active must refuse a retired code"
        );

        // …the list stops offering it…
        let req = Request::builder()
            .uri("/api/classes?subject_kind=employee")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v.as_array().unwrap().len(),
            1,
            "retired codes leave the list"
        );

        // …but the row stays readable: existing rows point at it.
        let req = Request::builder()
            .uri("/api/classes/employee/cto")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a retired Class is history, not gone"
        );
    }

    #[tokio::test]
    async fn retire_is_idempotent() {
        let app = build_app(vec![employee("ceo", 10)]);
        let h = operator_header();
        for _ in 0..2 {
            let resp = app
                .clone()
                .oneshot(retire_request(Some(&h), "ceo"))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::NO_CONTENT,
                "a repeat retire is a no-op"
            );
        }
    }

    #[tokio::test]
    async fn retire_of_a_missing_code_is_404() {
        let app = build_app(vec![employee("ceo", 10)]);
        let h = operator_header();
        let resp = app
            .clone()
            .oneshot(retire_request(Some(&h), "no-such"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn retire_requires_operator_tier() {
        let app = build_app(vec![employee("ceo", 10)]);
        let resp = app
            .clone()
            .oneshot(retire_request(None, "ceo"))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "withdrawing taxonomy is operator work"
        );
    }
}
