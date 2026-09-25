//! Step UX plugin registry handlers — author, version, publish, and
//! retire StepPlugins via `/api/jobs/step-plugins`.

use super::*;

use axum::extract::{Path, Query};

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
fn plugin_registry_or_503<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<&Arc<dyn StepPluginRegistry>, Response> {
    state.plugin_registry.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "step plugin registry not configured",
        )
            .into_response()
    })
}

fn plugin_err_response(err: StepPluginError) -> Response {
    match err {
        StepPluginError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        StepPluginError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        StepPluginError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        StepPluginError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

async fn plugin_policy_check<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    action: Action,
) -> Result<(), Response> {
    match state
        .policy
        .check(user, action, Resource::step_plugin())
        .await
    {
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
pub(super) struct ListPluginsQuery {
    category: Option<String>,
}

pub(super) async fn list_plugins<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListPluginsQuery>,
) -> Response {
    let reg = match plugin_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = plugin_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.list_active(q.category.as_deref()).await {
        Ok(plugins) => Json(plugins).into_response(),
        Err(e) => plugin_err_response(e),
    }
}

pub(super) async fn get_plugin<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match plugin_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = plugin_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.get_active(&kind).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => plugin_err_response(e),
    }
}

pub(super) async fn get_plugin_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((kind, version)): Path<(String, i32)>,
) -> Response {
    let reg = match plugin_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = plugin_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.get_version(&kind, version).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => plugin_err_response(e),
    }
}

pub(super) async fn list_plugin_versions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match plugin_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = plugin_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.list_versions(&kind).await {
        Ok(versions) => Json(versions).into_response(),
        Err(e) => plugin_err_response(e),
    }
}

pub(super) async fn create_plugin<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<StepPluginSpec>,
) -> Response {
    let reg = match plugin_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = plugin_policy_check(&state, &user, Action::Create).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => plugin_err_response(e),
    }
}

pub(super) async fn update_plugin<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
    Json(mut spec): Json<StepPluginSpec>,
) -> Response {
    let reg = match plugin_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = plugin_policy_check(&state, &user, Action::Update).await {
        return r;
    }
    spec.kind = kind;
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => plugin_err_response(e),
    }
}

pub(super) async fn publish_plugin<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match plugin_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = plugin_policy_check(&state, &user, Action::Publish).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.publish(&kind, &actor, now).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => plugin_err_response(e),
    }
}

pub(super) async fn retire_plugin<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match plugin_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = plugin_policy_check(&state, &user, Action::Retire).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.retire(&kind, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => plugin_err_response(e),
    }
}

/// Count non-terminal Steps whose `kind` equals the plugin kind. The
/// admin UI calls this before a retire confirm so the operator sees
/// the blast radius — in-flight Steps keep rendering their current
/// bundle; only brand-new Steps of this kind are blocked by retire.
pub(super) async fn in_flight_plugin_count<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    if let Err(r) = plugin_policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match state.jobs.count_in_flight_steps_by_kind(&kind).await {
        Ok(n) => Json(serde_json::json!({ "kind": kind, "in_flight": n })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /api/jobs/repairs/step-plugin-version` — the dry run of the
/// one-time repair (backlog 5a670a71): every step whose log-derived
/// plugin version differs from its row, and what the write would do
/// with each, with nothing written. `POST` below is the write.
///
/// Both halves take `publish` on `step_plugin`, the authority that
/// decides which plugin version a step is stamped with
/// (`platform-admin` in the core defaults): the dry run lists every
/// packet's divergent steps, so it is the repair's reader, not a
/// general one. The contract is `crate::plugin_version_repair`.
pub(super) async fn preview_plugin_version_repair<
    R: JobsRepository + 'static,
    B: EventBus + 'static,
>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    plugin_version_repair(&state, &user, false).await
}

/// `POST /api/jobs/repairs/step-plugin-version` — append ONE correcting
/// STEP_UPDATED, built from the stored row and signed as the caller,
/// for each step whose divergence is exactly the version-0 STEP_CREATED
/// defect; refuse and list the rest. A second call writes nothing.
pub(super) async fn run_plugin_version_repair<
    R: JobsRepository + 'static,
    B: EventBus + 'static,
>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    plugin_version_repair(&state, &user, true).await
}

async fn plugin_version_repair<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    write: bool,
) -> Response {
    if let Err(r) = plugin_policy_check(state, user, Action::Publish).await {
        return r;
    }
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = state.publisher.stamp_with_actor(actor).await;
    match state.jobs.repair_step_plugin_versions(write, &stamp).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
