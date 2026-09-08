//! Axum HTTP handlers for the people API.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use boss_core::publisher::DomainPublisher;
use boss_policy::{Action, Decision, Resource};
use boss_policy_client::{CurrentUser, PolicyClient};

use crate::port::{PeopleError, PeopleRepository};
use crate::types::Employee;

pub struct PeopleApiState<R: PeopleRepository> {
    pub people: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    /// Row-level authorization. None in tests that don't exercise
    /// the policy path — those handlers skip the gate and allow the
    /// request, preserving the existing test surface.
    pub policy: Option<Arc<dyn PolicyClient>>,
    /// SubjectKind registry — opt-in validator for tenant-extensible
    /// Subject discriminators. Today the boss-people surface accepts
    /// closed Subject variants (Account, Employee, Vendor, ...) so
    /// the validator never fires; it lands here as scaffolding for
    /// the future boss-accounts carve-out + account_type lift, which
    /// will introduce Subject::Custom into account-shaped writes.
    /// See `boss-jobs::http::check_custom_subject` for the canonical
    /// shape this mirrors.
    pub subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router<R: PeopleRepository + 'static>(state: PeopleApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/people/health", get(health))
        .route(
            "/api/people",
            get(list_employees::<R>).post(create_employee::<R>),
        )
        .route(
            "/api/people/{id}",
            get(get_employee::<R>)
                .put(update_employee::<R>)
                .delete(delete_employee::<R>),
        )
        .route("/api/people/{id}/reports", get(get_reports::<R>))
        .route("/api/people/{id}/exists", get(employee_exists::<R>))
        .with_state(shared)
}

/// Lightweight existence check used by cross-service write guards
/// (boss-assets's actor_id validation, etc). Returns `{"exists": bool}`
/// instead of the full employee record so the caller doesn't pay for
/// data it isn't going to use.
async fn employee_exists<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.people.employee_by_id(&id).await {
        Ok(opt) => Json(serde_json::json!({ "exists": opt.is_some() })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-people-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

#[derive(Deserialize)]
struct ListEmployeesQuery {
    /// Exact role-slug filter (e.g. `bookkeeper`, `head-brewer`).
    role: Option<String>,
    /// Exact status filter (e.g. `active`). Omit for all statuses.
    status: Option<String>,
}

/// List the roster, optionally filtered by `role` and/or `status`.
/// `?role=bookkeeper&status=active` powers the role→active-employees
/// lookup the dispatcher's notifier + auto-assign need, and the SPA
/// directory. Both filters are exact-match; absent = no constraint.
async fn list_employees<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Query(q): Query<ListEmployeesQuery>,
) -> Response {
    match state.people.all_employees().await {
        Ok(employees) => {
            let filtered: Vec<Employee> = employees
                .into_iter()
                .filter(|e| q.role.as_ref().is_none_or(|r| e.role.as_ref() == Some(r)))
                .filter(|e| {
                    q.status
                        .as_ref()
                        .is_none_or(|s| e.status.as_ref() == Some(s))
                })
                .collect();
            Json(filtered).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.people.employee_by_id(&id).await {
        Ok(Some(emp)) => Json(emp).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no employee with ID {id}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_reports<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.people.direct_reports(&id).await {
        Ok(reports) => Json(reports).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The transitional identity the deployment bootstraps itself with
/// (`boss-operator-baseline-seed` injects it from
/// `BOSS_BOOTSTRAP_ADMIN_EMAIL`). It exists so the system can write
/// before anyone is hired — the F2 keystone — and it has nothing left
/// to do the moment a named person holds the same authority.
pub const BOOTSTRAP_IDENTITY: &str = "emp-bootstrap-admin";

/// The role that makes it redundant. A brewer being hired does not
/// retire the deployment's only administrative identity.
const BOOTSTRAP_ROLE: &str = "platform-admin";

async fn create_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Json(emp): Json<Employee>,
) -> Response {
    if let Err(msg) = validate_email(emp.email.as_deref()) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    // OUTBOX (phase 2): the adapter records people.employee.created
    // (full row state — what the rebuilder consumes) inside the
    // domain transaction; nothing publishes post-commit. Row-touch
    // columns bind the stamp's wall time so a rebuild reproduces
    // them from audit_log.timestamp.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let id = match state
        .people
        .create_employee_at(&emp, stamp.timestamp, &stamp)
        .await
    {
        Ok(id) => id,
        Err(e) => return people_error_response(e),
    };
    retire_bootstrap_identity_if_superseded(&state, &emp).await;
    (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response()
}

/// Hand over from the bootstrap identity to the person who supersedes
/// it (David, 2026-08-15: "maybe we have it so emp-bootstrap-admin
/// auto-retires on provisioning of the first real employee").
///
/// HIRING is the trigger because hiring is the event that makes the
/// bootstrap row redundant. Putting the check in the seed instead
/// would only fire at bootstrap — precisely when no real employee
/// exists yet — which is how a transitional identity becomes a
/// permanent one.
///
/// Deliberately BEST-EFFORT and after the create. A hire is the
/// caller's business and it has already succeeded; a deployment with
/// no bootstrap row, or a retirement that fails, must not turn a good
/// hire into an error the caller has to interpret. Idempotent: a row
/// already terminated is left untouched, so re-running a seed does not
/// emit a second retirement.
async fn retire_bootstrap_identity_if_superseded<R: PeopleRepository + 'static>(
    state: &Arc<PeopleApiState<R>>,
    hired: &Employee,
) {
    if hired.id == BOOTSTRAP_IDENTITY
        || hired.role.as_deref() != Some(BOOTSTRAP_ROLE)
        || hired.status.as_deref() != Some("active")
    {
        return;
    }
    let Ok(Some(mut boot)) = state.people.employee_by_id(BOOTSTRAP_IDENTITY).await else {
        return; // no bootstrap row — nothing to hand over.
    };
    if boot.status.as_deref() == Some("terminated") {
        return;
    }
    boot.status = Some("terminated".to_string());
    let stamp = crate::events::event_stamp(&state.publisher).await;
    match state
        .people
        .update_employee_at(BOOTSTRAP_IDENTITY, &boot, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => tracing::info!(
            superseded_by = %hired.id,
            "retired {BOOTSTRAP_IDENTITY}: a named {BOOTSTRAP_ROLE} now holds the authority it \
             was bootstrapping"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "could not retire {BOOTSTRAP_IDENTITY} after hiring {}; the hire stands",
            hired.id
        ),
    }
}

async fn update_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(emp): Json<Employee>,
) -> Response {
    // Policy: editing an employee record requires Action::Update on
    // Resource::employee(). Test path (policy: None) bypasses the gate.
    if let Some(ref policy) = state.policy {
        match policy
            .check(&user, Action::Update, Resource::employee())
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
    if let Err(msg) = validate_email(emp.email.as_deref()) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    let stamp = crate::events::event_stamp(&state.publisher).await;
    match state
        .people
        .update_employee_at(&id, &emp, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => people_error_response(e),
    }
}

async fn delete_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    let stamp = crate::events::event_stamp(&state.publisher).await;
    match state
        .people
        .delete_employee_at(&id, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => people_error_response(e),
    }
}

/// Cheap email validation. The OSS quickstart auth keys
/// credentials by email; the future Authelia / OIDC migration
/// will too. We don't try to be RFC 5322 — just non-empty + has
/// the shape `local@domain.tld`. Callers that want stricter
/// validation can layer it on top; this rejects the obvious
/// "bookkeeper" / "" / "no-email" footguns the existing seeds
/// surfaced.
fn validate_email(email: Option<&str>) -> Result<(), String> {
    // Identity-first: no email yet is fine (an id-only employee record).
    // Validate only what's provided.
    let Some(email) = email else {
        return Ok(());
    };
    let trimmed = email.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let (local, domain) = match trimmed.rsplit_once('@') {
        Some(parts) => parts,
        None => return Err("email must contain '@'".into()),
    };
    if local.is_empty() {
        return Err("email must have a local-part before '@'".into());
    }
    if !domain.contains('.') {
        return Err("email domain must contain '.'".into());
    }
    Ok(())
}

fn people_error_response(e: PeopleError) -> Response {
    match e {
        PeopleError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        PeopleError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        PeopleError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryPeople;
    use crate::types::*;

    fn test_emp(id: &str, manager: Option<&str>) -> Employee {
        Employee {
            id: id.to_string(),
            name: Some(format!("Test {id}")),
            email: Some(format!("{id}@boss.io")),
            role: Some("service-tech".to_string()),
            department: Some("service".to_string()),
            skill_level: Some(3),
            skills: vec![],
            hire_date: Some(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            location: Some("loc-hq".to_string()),
            manager_id: manager.map(String::from),
            employment_type: Some("full-time".to_string()),
            status: Some("active".to_string()),
            certifications: vec![],
            annual_salary_cents: None,
        }
    }

    fn app_with(people: Arc<InMemoryPeople>) -> Router {
        let policy: Arc<dyn PolicyClient> = Arc::new(boss_policy_client::PermissivePolicyClient);
        router(PeopleApiState {
            people,
            publisher: None,
            policy: Some(policy),
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    fn test_app() -> Router {
        let people = Arc::new(InMemoryPeople::new(vec![
            test_emp("emp-001", None),
            test_emp("emp-002", Some("emp-001")),
            test_emp("emp-003", Some("emp-001")),
        ]));
        let policy: Arc<dyn PolicyClient> = Arc::new(boss_policy_client::PermissivePolicyClient);
        router(PeopleApiState {
            people,
            publisher: None,
            policy: Some(policy),
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    /// The bootstrap identity is TRANSITIONAL: it exists so the system
    /// can write before anyone is hired, and it should stop existing the
    /// moment a named person can do the writing.
    ///
    /// David, 2026-08-15: "We really need to retire emp-bootstrap-admin
    /// as an actor ... now that my david@algedonic.dev identity is
    /// established that it give the impression I am still bootstrapping
    /// things while I am working. Maybe we have it so emp-bootstrap-admin
    /// auto-retires on provisioning of the first real employee."
    ///
    /// Hiring is the trigger because hiring is the event that makes the
    /// bootstrap row redundant — putting it in the seed instead would only
    /// fire at bootstrap, which is exactly when the real employee does not
    /// exist yet.
    #[tokio::test]
    async fn hiring_a_real_platform_admin_retires_the_bootstrap_identity() {
        let mut boot = test_emp(BOOTSTRAP_IDENTITY, None);
        boot.role = Some("platform-admin".to_string());
        let people = Arc::new(InMemoryPeople::new(vec![boot]));
        let app = app_with(people.clone());

        let mut hire = test_emp("emp-david", None);
        hire.role = Some("platform-admin".to_string());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&hire).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let after = people
            .employee_by_id(BOOTSTRAP_IDENTITY)
            .await
            .unwrap()
            .expect("the bootstrap row is retired, never deleted — the audit log names it");
        assert_eq!(
            after.status.as_deref(),
            Some("terminated"),
            "a named platform-admin exists, so the bootstrap identity has nothing left to do"
        );
    }

    /// Hiring anyone else leaves it alone. The bootstrap identity holds
    /// platform-admin; retiring it because a brewer was hired would take
    /// the deployment's only administrative identity away.
    #[tokio::test]
    async fn hiring_an_ordinary_employee_leaves_the_bootstrap_identity_alone() {
        let mut boot = test_emp(BOOTSTRAP_IDENTITY, None);
        boot.role = Some("platform-admin".to_string());
        let people = Arc::new(InMemoryPeople::new(vec![boot]));
        let app = app_with(people.clone());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&test_emp("emp-brewer", None)).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let after = people
            .employee_by_id(BOOTSTRAP_IDENTITY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.status.as_deref(), Some("active"));
    }

    /// Re-running the hire is a no-op rather than a second retirement
    /// event, and a deployment that never had a bootstrap row does not
    /// fail a hire over its absence.
    #[tokio::test]
    async fn retiring_the_bootstrap_identity_is_idempotent_and_optional() {
        let people = Arc::new(InMemoryPeople::new(vec![]));
        let app = app_with(people.clone());
        let mut hire = test_emp("emp-david", None);
        hire.role = Some("platform-admin".to_string());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/people")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&hire).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::CREATED,
            "no bootstrap row to retire is not a reason to refuse a hire"
        );
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_all() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let emps: Vec<Employee> = serde_json::from_slice(&body).unwrap();
        assert_eq!(emps.len(), 3);
    }

    #[tokio::test]
    async fn list_filtered_by_role_and_status() {
        let mut brewer = test_emp("emp-brewer-1", None);
        brewer.role = Some("brewer".to_string());
        let mut bookkeeper = test_emp("emp-bk-1", None);
        bookkeeper.role = Some("bookkeeper".to_string());
        let mut ex_brewer = test_emp("emp-brewer-2", None);
        ex_brewer.role = Some("brewer".to_string());
        ex_brewer.status = Some("terminated".to_string());
        let people = Arc::new(InMemoryPeople::new(vec![brewer, bookkeeper, ex_brewer]));
        let policy: Arc<dyn PolicyClient> = Arc::new(boss_policy_client::PermissivePolicyClient);
        let app = router(PeopleApiState {
            people,
            publisher: None,
            policy: Some(policy),
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/people?role=brewer&status=active")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let emps: Vec<Employee> = serde_json::from_slice(&body).unwrap();
        // Only the active brewer — bookkeeper (wrong role) and the
        // terminated brewer (wrong status) are filtered out.
        assert_eq!(emps.len(), 1);
        assert_eq!(emps[0].id, "emp-brewer-1");
    }

    #[tokio::test]
    async fn get_by_id_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people/emp-001")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_by_id_not_found() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people/emp-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_reports() {
        let resp = test_app()
            .oneshot(
                Request::builder()
                    .uri("/api/people/emp-001/reports")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let reports: Vec<Employee> = serde_json::from_slice(&body).unwrap();
        assert_eq!(reports.len(), 2);
    }
}
