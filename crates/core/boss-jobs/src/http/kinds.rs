//! Workflow registry handlers — author, version, publish, and retire
//! Workflow specs via `/api/workflows`.

use super::*;

use axum::extract::{Path, Query};

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) fn kind_registry_or_503<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<&Arc<dyn WorkflowRegistry>, Response> {
    state.kind_registry.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "job kind registry not configured",
        )
            .into_response()
    })
}

/// The who + when every registry write hands to the adapter, which
/// builds and records the outbox event atomically with the row.
/// Actor per the Level-B stamping invariant: the authenticated
/// session identity, falling back to the platform automation for
/// header-less internal calls. `now` is clock-routed — never
/// wallclock — so sim-mode registry edits stamp sim time.
/// Shared with the step-plugin handlers (`plugins.rs`), whose
/// writes carry the same stamp contract.
pub(super) async fn write_stamp<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
) -> (boss_core::actor::ActorId, chrono::DateTime<chrono::Utc>) {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let now = boss_clock_client::now_from(&state.clock).await;
    (actor, now)
}

pub(super) fn kind_err_response(err: WorkflowError) -> Response {
    match err {
        WorkflowError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        WorkflowError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        WorkflowError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        // 422, not 400: the spec parsed and is well-formed JSON — it
        // just describes work no Job could finish. Body is the same
        // `{ok, problems}` shape `_validate` returns so the editor
        // renders a refused publish exactly like a failed dry run.
        WorkflowError::Unviable(problems) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(lint_result_json(&problems)),
        )
            .into_response(),
        WorkflowError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

/// The lint result body — `{ok, problems}`. One definition shared by
/// the author-time dry run (200) and the publish refusal (422).
pub(super) fn lint_result_json(
    problems: &[crate::workflow_lint::WorkflowLintError],
) -> serde_json::Value {
    serde_json::json!({
        "ok": problems.is_empty(),
        "problems": crate::workflow_lint::problems_json(problems),
    })
}

pub(super) async fn policy_check<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    action: Action,
) -> Result<(), Response> {
    match state.policy.check(user, action, Resource::workflow()).await {
        Ok(Decision::Allow { .. }) => Ok(()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("policy check failed: {e}"),
        )
            .into_response()),
    }
}

#[derive(Deserialize)]
pub(super) struct ListKindsQuery {
    category: Option<String>,
}

pub(super) async fn list_kinds<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListKindsQuery>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.list_active(q.category.as_deref()).await {
        Ok(kinds) => Json(kinds).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn get_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.get_active(&kind).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn get_kind_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((kind, version)): Path<(String, i32)>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.get_version(&kind, version).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn list_kind_versions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.list_versions(&kind).await {
        Ok(versions) => Json(versions).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn create_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<WorkflowSpec>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Create).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => kind_err_response(e),
    }
}

/// Body for the author-time dry run. Only the kind slug (for error
/// labels) and the step list are needed — the lint validates the graph,
/// not the heavyweight registry-row fields — so the editor doesn't have
/// to assemble a full `WorkflowSpec` on every keystroke.
#[derive(serde::Deserialize)]
pub(super) struct DraftLintRequest {
    #[serde(default)]
    pub kind: String,
    pub steps: Vec<crate::registry::StepSpec>,
}

/// Author-time dry run — lint a draft's steps WITHOUT persisting.
/// Calls `workflow_lint::gate_active`, the same function the publish
/// path enforces, so an editor showing "no problems" will publish
/// cleanly and a refused publish shows the same problem list.
/// Always returns 200 with a structured result; lint failures are
/// data, not an HTTP error — the editor renders them on the graph.
/// See architecture-decisions.md §Jobs, Workflows, Steps.
pub(super) async fn validate_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<DraftLintRequest>,
) -> Response {
    // Gated like create — the dry run is an authoring affordance.
    if let Err(r) = policy_check(&state, &user, Action::Create).await {
        return r;
    }
    let kind = if req.kind.is_empty() {
        "draft"
    } else {
        req.kind.as_str()
    };
    let spec = WorkflowSpec::platform_seed(kind, "draft", "draft", Vec::new(), req.steps);
    let errs = match crate::workflow_lint::gate_active(&spec) {
        Ok(()) => Vec::new(),
        Err(problems) => problems,
    };
    (StatusCode::OK, Json(lint_result_json(&errs))).into_response()
}

pub(super) async fn update_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
    Json(mut spec): Json<WorkflowSpec>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Update).await {
        return r;
    }
    // Force kind match — a PUT for /kinds/foo always edits foo.
    spec.kind = kind;
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => kind_err_response(e),
    }
}

/// Promote the latest draft of `kind` to ACTIVE.
///
/// The viability gate runs inside `WorkflowRegistry::publish`,
/// against the draft row the transaction actually promotes — not
/// against a copy re-read here, which could race a concurrent
/// author. An unviable draft comes back as `WorkflowError::Unviable`
/// and leaves as 422 + the problem list (2026-08-13: publish used to
/// accept anything, and the bad row surfaced as a dead API on the
/// next pod roll).
pub(super) async fn publish_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Publish).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.publish(&kind, &actor, now).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => kind_err_response(e),
    }
}

/// `DELETE /api/workflows/{kind}/versions/{version}` — discard a DRAFT
/// (ebd7bb70). The publish guard refuses a dirty registry and used to
/// demand "resolve that draft first" while no verb or route could;
/// this is that route. Draft-only: 409 for active/retired (history),
/// 404 for a version that never existed (a typo must not read as
/// success). Same policy gate as every registry write.
pub(super) async fn discard_kind_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((kind, version)): Path<(String, i32)>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Update).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.discard_draft(&kind, version, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(crate::registry::WorkflowError::NotFound(m)) => {
            (StatusCode::NOT_FOUND, m).into_response()
        }
        Err(crate::registry::WorkflowError::Conflict(m)) => {
            (StatusCode::CONFLICT, m).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn retire_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Retire).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.retire(&kind, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => kind_err_response(e),
    }
}

#[cfg(test)]
mod subject_kind_validator_tests {
    use super::*;
    use boss_core::job::Subject;
    use boss_subject_kinds_client::{FakeSubjectKindsClient, SubjectKindsClient};

    fn registry(client: FakeSubjectKindsClient) -> Option<Arc<dyn SubjectKindsClient>> {
        Some(Arc::new(client) as Arc<dyn SubjectKindsClient>)
    }

    #[tokio::test]
    async fn no_hardcoded_bypass_even_for_platform_named_kinds() {
        // The registry is the single source of truth — core no longer
        // fast-paths a baked-in vocabulary. A platform-named kind like
        // `account` absent from the registry is rejected like any other
        // unknown kind...
        let s = Subject::new("account", "acc-1");
        let reg = registry(FakeSubjectKindsClient::with(vec![]));
        assert!(
            check_custom_subject(reg.as_ref(), &s).await.is_err(),
            "an unknown-to-registry kind must 400, even a platform name"
        );
        // ...and passes once the registry lists it.
        let reg = registry(FakeSubjectKindsClient::with(vec!["account".into()]));
        check_custom_subject(reg.as_ref(), &s).await.unwrap();
    }

    #[tokio::test]
    async fn missing_registry_skips_check() {
        let s = Subject::new("anything", "x");
        check_custom_subject(None, &s).await.unwrap();
    }

    #[tokio::test]
    async fn known_custom_kind_passes() {
        let s = Subject::new("asset", "A-1");
        let reg = registry(FakeSubjectKindsClient::with(vec!["asset".into()]));
        check_custom_subject(reg.as_ref(), &s).await.unwrap();
    }

    #[tokio::test]
    async fn unknown_custom_kind_returns_400_with_actionable_message() {
        let s = Subject::new("made-up-kind", "x");
        let reg = registry(FakeSubjectKindsClient::with(vec!["asset".into()]));
        let resp = check_custom_subject(reg.as_ref(), &s).await.unwrap_err();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
