//! Axum HTTP handlers for the people API.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use boss_core::publisher::DomainPublisher;
use boss_policy::{Action, Resource, Scope, User};
use boss_policy_client::{CurrentUser, PolicyClient};

use crate::port::{PeopleError, PeopleRepository};
use crate::types::Employee;

/// The employee fields `Resource::compensation()` governs: a caller
/// sees them on a row only where `Read` on that resource is granted,
/// in that grant's scope (backlog c7484d0e, 2026-09-23 — every roster
/// read had handed every salary to any signed-in viewer). The grants
/// are policy rows: platform-admin in the core defaults, the tenants'
/// HR and finance roles in their seeds.
const COMPENSATION_FIELDS: &[&str] = &["annual_salary_cents"];

pub struct PeopleApiState<R: PeopleRepository> {
    pub people: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    /// Row-level authorization. None in tests that don't exercise
    /// the policy path — those handlers skip the gate and allow the
    /// request, preserving the existing test surface. The one
    /// exception is pay: None is no compensation grant, so reads
    /// redact it (see `compensation_scope`).
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
    /// Case-insensitive email filter — the question "who already holds
    /// this address?" that `boss-operator-baseline-seed` asks before
    /// injecting the bootstrap admin (backlog 0d2d7daa, 2026-09-16).
    /// Case-insensitive because the login's credential → Employee
    /// match and the schema's unique index are both on LOWER(email).
    email: Option<String>,
}

/// List the roster, optionally filtered by `role`, `status` and/or
/// `email`. `?role=bookkeeper&status=active` powers the
/// role→active-employees lookup the dispatcher's notifier +
/// auto-assign need, and the SPA directory. `role` and `status` are
/// exact-match, `email` is case-insensitive; absent = no constraint.
async fn list_employees<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    CurrentUser(user): CurrentUser,
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
                .filter(|e| {
                    q.email.as_ref().is_none_or(|wanted| {
                        e.email
                            .as_deref()
                            .is_some_and(|have| have.trim().eq_ignore_ascii_case(wanted.trim()))
                    })
                })
                .collect();
            let pay = compensation_scope(&state, &user).await;
            rows_response(&filtered, pay.as_ref(), &user)
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// The scope in which `user` may read compensation, or `None` for
/// nowhere — [`crate::grants::compensation_scope`], which the change
/// log asks too: no policy wired is no grant, and a policy service
/// that cannot answer is no grant either. The roster itself still
/// answers, because the dispatcher's notifier and auto-assign read it
/// by role and never need the pay.
async fn compensation_scope<R: PeopleRepository + 'static>(
    state: &PeopleApiState<R>,
    user: &User,
) -> Option<Scope> {
    crate::grants::compensation_scope(state.policy.as_ref(), user).await
}

/// Whether a compensation grant in `scope` covers `emp`'s row, in the
/// vocabulary every other grant uses ([`crate::grants::covers`]).
fn covers(scope: &Scope, user: &User, emp: &Employee) -> bool {
    crate::grants::covers(scope, user, &emp.id, emp.department.as_deref())
}

/// One employee row as `user` may see it. A row the grant does not
/// cover loses the compensation KEYS — absent, not zero and not null:
/// a zero is a claim about pay, and the tenant publish reads an
/// explicit null as a declaration. A caller that writes such a row
/// back keeps the stored pay (see [`update_employee`]).
fn employee_json(
    emp: &Employee,
    pay: Option<&Scope>,
    user: &User,
) -> Result<serde_json::Value, serde_json::Error> {
    let mut row = serde_json::to_value(emp)?;
    if !pay.is_some_and(|scope| covers(scope, user, emp))
        && let Some(obj) = row.as_object_mut()
    {
        for field in COMPENSATION_FIELDS {
            obj.remove(*field);
        }
    }
    Ok(row)
}

fn rows_response(rows: &[Employee], pay: Option<&Scope>, user: &User) -> Response {
    match rows
        .iter()
        .map(|e| employee_json(e, pay, user))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_employee<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    match state.people.employee_by_id(&id).await {
        Ok(Some(emp)) => {
            let pay = compensation_scope(&state, &user).await;
            match employee_json(&emp, pay.as_ref(), &user) {
                Ok(row) => Json(row).into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }
        Ok(None) => (StatusCode::NOT_FOUND, format!("no employee with ID {id}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_reports<R: PeopleRepository + 'static>(
    State(state): State<Arc<PeopleApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    match state.people.direct_reports(&id).await {
        Ok(reports) => {
            let pay = compensation_scope(&state, &user).await;
            rows_response(&reports, pay.as_ref(), &user)
        }
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
    // Resource::employee(). Test path (policy: None) bypasses the gate;
    // the binary wires a client — until 2026-09-23 it did not, and this
    // gate never ran in production (backlog 8cdad84c).
    if let Err(refused) = crate::grants::require(
        state.policy.as_ref(),
        &user,
        Action::Update,
        Resource::employee(),
    )
    .await
    {
        return refused;
    }
    if let Err(msg) = validate_email(emp.email.as_deref()) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    // A caller that could not see the stored pay cannot be clearing
    // it: the tenant publish and both engines' prepare GET a row, set
    // one field and PUT it back, and without a compensation grant that
    // GET came back with no salary (backlog c7484d0e). So a PUT that
    // carries none keeps the stored one unless the caller's grant
    // covers the row — the only caller for whom an omitted salary can
    // mean "clear it".
    let mut emp = emp;
    if emp.annual_salary_cents.is_none() {
        let stored = match state.people.employee_by_id(&id).await {
            Ok(stored) => stored,
            Err(e) => return people_error_response(e),
        };
        if let Some(stored) = stored {
            let pay = compensation_scope(&state, &user).await;
            if !pay.is_some_and(|scope| covers(&scope, &user, &stored)) {
                emp.annual_salary_cents = stored.annual_salary_cents;
            }
        }
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

    /// `?email=` is the question the bootstrap-admin injection asks
    /// before it injects (backlog 0d2d7daa, 2026-09-16): "does anyone
    /// already hold this address?" The match is case-insensitive
    /// because the login resolves credential → Employee on
    /// lower(email), and the schema's unique index is on LOWER(email)
    /// — a filter that answered differently from either would let the
    /// injection add a second row the index then refuses.
    #[tokio::test]
    async fn list_filtered_by_email_is_case_insensitive() {
        let mut david = test_emp("emp-david", None);
        david.email = Some("David@Example.com".to_string());
        let people = Arc::new(InMemoryPeople::new(vec![
            david,
            test_emp("emp-other", None),
        ]));
        let app = app_with(people);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/people?email=david%40example.com")
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
        assert_eq!(emps.len(), 1, "{emps:?}");
        assert_eq!(emps[0].id, "emp-david");
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

    // ---- Compensation is read by grant, not by sign-in -------------
    //
    // Backlog c7484d0e (2026-09-23, the /ux/people page audit): every
    // read of the roster returned `annual_salary_cents` to any
    // signed-in viewer. The roster itself stays open — the dispatcher's
    // notifier and auto-assign read it by role — so the fix is the
    // field, not the list.

    const SALARY: i64 = 8_500_000;

    fn paid_roster() -> Arc<InMemoryPeople> {
        let mut boss = test_emp("emp-001", None);
        boss.department = Some("finance".to_string());
        let rows = [boss, test_emp("emp-002", Some("emp-001"))]
            .into_iter()
            .map(|mut e| {
                e.annual_salary_cents = Some(SALARY);
                e
            })
            .collect();
        Arc::new(InMemoryPeople::new(rows))
    }

    fn app_with_policy(
        people: Arc<InMemoryPeople>,
        policy: Option<Arc<dyn PolicyClient>>,
    ) -> Router {
        router(PeopleApiState {
            people,
            publisher: None,
            policy,
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    fn caller(id: &str, role: &str, reports: &[&str]) -> String {
        serde_json::json!({
            "id": id,
            "role": role,
            "direct_report_ids": reports,
        })
        .to_string()
    }

    async fn read_json(app: Router, uri: &str, user: &str) -> serde_json::Value {
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-boss-user", user)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "GET {uri}");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// Every row a caller can be handed: the list, the single row and
    /// the reports list, flattened.
    async fn every_read(app: &Router, user: &str) -> Vec<serde_json::Value> {
        let mut rows = Vec::new();
        for uri in ["/api/people", "/api/people/emp-001/reports"] {
            let v = read_json(app.clone(), uri, user).await;
            rows.extend(v.as_array().cloned().unwrap_or_default());
        }
        rows.push(read_json(app.clone(), "/api/people/emp-001", user).await);
        rows.push(read_json(app.clone(), "/api/people/emp-002", user).await);
        rows
    }

    fn salary_of(row: &serde_json::Value) -> Option<&serde_json::Value> {
        row.as_object()
            .expect("an employee row is an object")
            .get("annual_salary_cents")
    }

    /// A caller with no compensation grant still reads the whole
    /// roster — role, status, email, the fields the dispatcher routes
    /// on — and the salary KEY is gone, not zeroed and not null: a zero
    /// is a claim about pay, and a null reads as "no salary" to a
    /// caller that writes the row back.
    #[tokio::test]
    async fn a_caller_without_a_compensation_grant_reads_the_roster_without_salary() {
        let deny: Arc<dyn PolicyClient> =
            Arc::new(boss_policy_client::FakePolicyClient::deny_all());
        let app = app_with_policy(paid_roster(), Some(deny));
        let rows = every_read(&app, &caller("emp-002", "service-tech", &[])).await;
        assert_eq!(rows.len(), 5, "list 2 + reports 1 + two single rows");
        for row in &rows {
            assert_eq!(salary_of(row), None, "salary leaked: {row}");
            assert_eq!(
                row["role"], "service-tech",
                "the roster itself stays readable"
            );
        }
    }

    /// Unwired policy is not a grant. The write gate's `None` = allow is
    /// a test convenience, and the live people-api runs with `None`
    /// (2026-09-23) — a private read must not fail open on that.
    #[tokio::test]
    async fn no_policy_wired_is_no_compensation_grant() {
        let app = app_with_policy(paid_roster(), None);
        for row in every_read(&app, &caller("emp-001", "platform-admin", &[])).await {
            assert_eq!(salary_of(&row), None, "salary leaked: {row}");
        }
    }

    /// Read on `compensation` with scope `all` shows every salary.
    #[tokio::test]
    async fn a_compensation_grant_shows_the_salary() {
        let grant: Arc<dyn PolicyClient> = Arc::new(
            boss_policy_client::FakePolicyClient::builder()
                .allow(
                    "head-of-people",
                    Action::Read,
                    Resource::compensation(),
                    boss_policy::Scope::All,
                )
                .build(),
        );
        let app = app_with_policy(paid_roster(), Some(grant));
        for row in every_read(&app, &caller("emp-hr", "head-of-people", &[])).await {
            assert_eq!(salary_of(&row), Some(&serde_json::json!(SALARY)), "{row}");
        }
    }

    /// The grant's scope is honoured row by row, in the vocabulary the
    /// rest of policy uses: `self` is your own pay, `team` adds your
    /// direct reports, `department:<d>` is that department's rows.
    #[tokio::test]
    async fn a_scoped_compensation_grant_shows_only_the_rows_it_covers() {
        let seen = |scope: boss_policy::Scope, user: String| async move {
            let grant: Arc<dyn PolicyClient> = Arc::new(
                boss_policy_client::FakePolicyClient::builder()
                    .allow("staff", Action::Read, Resource::compensation(), scope)
                    .build(),
            );
            let app = app_with_policy(paid_roster(), Some(grant));
            let list = read_json(app, "/api/people", &user).await;
            let mut ids: Vec<String> = list
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| salary_of(r).is_some())
                .map(|r| r["id"].as_str().unwrap().to_string())
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(
            seen(boss_policy::Scope::Self_, caller("emp-002", "staff", &[])).await,
            vec!["emp-002"]
        );
        assert_eq!(
            seen(
                boss_policy::Scope::Team,
                caller("emp-001", "staff", &["emp-002"])
            )
            .await,
            vec!["emp-001", "emp-002"]
        );
        assert_eq!(
            seen(
                boss_policy::Scope::Department("finance".into()),
                caller("emp-x", "staff", &[])
            )
            .await,
            vec!["emp-001"]
        );
    }

    /// Four callers read a row and PUT it back to change one field —
    /// the tenant publish's manager links and `--take`, both engines'
    /// prepare. A redacted read written back must not wipe the pay it
    /// never saw: without a grant, a PUT that carries no salary keeps
    /// the stored one. Measured with no policy wired, which is how the
    /// live people-api runs.
    #[tokio::test]
    async fn a_redacted_row_written_back_keeps_the_stored_salary() {
        let people = paid_roster();
        let app = app_with_policy(people.clone(), None);
        let user = caller("emp-001", "platform-admin", &[]);
        let mut row = read_json(app.clone(), "/api/people/emp-002", &user).await;
        assert_eq!(salary_of(&row), None);
        row["manager_id"] = serde_json::json!(null);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/people/emp-002")
                    .header("content-type", "application/json")
                    .header("x-boss-user", &user)
                    .body(Body::from(row.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let stored = people.employee_by_id("emp-002").await.unwrap().unwrap();
        assert_eq!(stored.manager_id, None, "the edit landed");
        assert_eq!(stored.annual_salary_cents, Some(SALARY), "the pay survived");
    }

    // ---- The Update gate, once policy is wired ----------------------
    //
    // Backlog 8cdad84c (2026-09-23): the binary built this state with
    // `policy: None`, so the gate below never ran in production. It is
    // wired now; these pin what it asks and what it refuses.

    async fn put_status(app: Router, user: &str) -> StatusCode {
        let body = serde_json::to_string(&test_emp("emp-002", None)).unwrap();
        app.oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/people/emp-002")
                .header("content-type", "application/json")
                .header("x-boss-user", user)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    /// A caller whose role holds no Update on `employee` is refused,
    /// and the row is untouched.
    #[tokio::test]
    async fn a_put_without_an_employee_update_grant_is_refused() {
        let people = paid_roster();
        let read_only: Arc<dyn PolicyClient> = Arc::new(
            boss_policy_client::FakePolicyClient::builder()
                .allow(
                    "service-tech",
                    Action::Read,
                    Resource::employee(),
                    boss_policy::Scope::All,
                )
                .build(),
        );
        let app = app_with_policy(people.clone(), Some(read_only));
        let status = put_status(app, &caller("emp-002", "service-tech", &[])).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let stored = people.employee_by_id("emp-002").await.unwrap().unwrap();
        assert_eq!(stored.manager_id.as_deref(), Some("emp-001"), "unchanged");
    }

    /// The identity every seed writer signs as — platform-admin, the
    /// tenant publish's and both engines' prepare — passes the gate on
    /// the core default rules alone, so wiring policy breaks none of
    /// them (the audit on 8cdad84c).
    #[tokio::test]
    async fn the_seed_writers_identity_passes_the_update_gate_on_the_default_rules() {
        let defaults: Arc<dyn PolicyClient> = Arc::new(
            boss_policy_client::defaults::default_rules()
                .into_iter()
                .fold(boss_policy_client::FakePolicyClient::builder(), |b, r| {
                    b.allow(r.role, r.action, r.resource, r.scope)
                })
                .build(),
        );
        let app = app_with_policy(paid_roster(), Some(defaults));
        let status = put_status(
            app,
            &caller("automation:tenant-seed", "platform-admin", &[]),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
}
