//! `GET /api/departments` and `GET /api/departments/{code}/readiness`
//! — the department list, and which of the six template parts each
//! department has (design 3613f0af, backlog 1dffde5d).
//!
//! Both are READS over live registries and nothing else: the
//! departments registry for what a department IS, the workflow registry for its
//! protocols, the jobs table for their packets and the newest
//! `department-retro`, the sensor registry for what observes on its
//! behalf, and the dispatcher's read surface for the rules that open
//! its kinds. No seed file is opened — David, 2026-09-18: seeds are
//! for OSS bootstrap and the playground; the instance runs on data.
//!
//! A department the departments registry does not hold is a 404 that
//! names the registry and the row it looked for, so a typo reads as a
//! typo and not as "a department with nothing". A registry that is not
//! wired is a 503, the posture every registry-backed door here takes;
//! a registry that is wired and cannot answer leaves that ONE part
//! `has: null` with the error as its reason, because the other five
//! are still worth reading. Reads admit `crate::trust::can_read` — the
//! operator tier, a trusted sibling, and the auditor tier the recorded
//! probe reader carries.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use boss_core::job::JobStatus;
use boss_policy_client::CurrentUser;
use serde_json::{Value, json};

use super::readiness::{self, KindReadiness, NewestRetro, NewestTerminal, Part, Readiness};
use super::registry::{Department, DepartmentRegistry};
use super::rules::DispatcherRules;
use crate::port::{JobFilter, JobsRepository};
use crate::registry::WorkflowRegistry;
use crate::sensors::Sensors;
use crate::trust::can_read;

/// The workflow kind the retro part reads — the platform bundle's
/// `department-retro`, one packet per department per ISO week, whose
/// `metadata.department` is the code (its admission contract).
pub const RETRO_KIND: &str = "department-retro";

pub struct DepartmentsApiState {
    /// What a department IS: the `departments` table (backlog
    /// 80a77466). `None` → 503 on both routes. It was the classes
    /// registry's employee drawer until that packet, which is a
    /// different question — the values an employee's `department`
    /// column may take, not the departments the company has.
    pub departments: Option<Arc<dyn DepartmentRegistry>>,
    /// The protocols declaring a department. `None` → 503.
    pub kinds: Option<Arc<dyn WorkflowRegistry>>,
    pub jobs: Arc<dyn JobsRepository>,
    /// `None` → the sensors part is undetermined, with the reason.
    pub sensors: Option<Arc<dyn Sensors>>,
    /// `None` → the rules part is undetermined, with the reason.
    pub rules: Option<Arc<dyn DispatcherRules>>,
}

pub fn router(state: DepartmentsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/departments", get(list))
        .route("/api/departments/{code}/readiness", get(readiness))
        .with_state(shared)
}

fn unavailable(what: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!("{what} not configured"),
    )
        .into_response()
}

/// The departments the registry holds, or the reason it could not say.
/// An error is never flattened to an empty roster: the retro rule
/// opens one packet per row this answers, so "no departments" and "the
/// registry did not answer" must not read the same.
async fn departments_or_response(state: &DepartmentsApiState) -> Result<Vec<Department>, Response> {
    let registry = state
        .departments
        .as_ref()
        .ok_or_else(|| unavailable("departments registry"))?;
    registry.list().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("departments registry unreachable: {e}"),
        )
            .into_response()
    })
}

async fn list(
    State(state): State<Arc<DepartmentsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match departments_or_response(&state).await {
        Ok(rows) => Json(json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(r) => r,
    }
}

async fn readiness(
    State(state): State<Arc<DepartmentsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(code): Path<String>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let departments = match departments_or_response(&state).await {
        Ok(rows) => rows,
        Err(r) => return r,
    };
    let Some(department) = departments.into_iter().find(|d| d.code == code) else {
        return (
            StatusCode::NOT_FOUND,
            format!(
                "no department `{code}` in the departments registry (an un-retired row of the \
                 `departments` table, whose id IS the code a packet carries) — declare it there \
                 first. An employee Class on the `department` attribute is the employee drawer, \
                 which is a different roster and is not read here (backlog 80a77466)"
            ),
        )
            .into_response();
    };
    let Some(kinds) = state.kinds.as_ref() else {
        return unavailable("workflow registry");
    };
    let specs = match kinds.list_active(None).await {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("workflow registry read failed: {e}"),
            )
                .into_response();
        }
    };

    // PROTOCOLS: the kinds whose active row declares this department,
    // each with its packets and newest terminal.
    let mut protocol_rows: Vec<KindReadiness> = Vec::new();
    for spec in readiness::protocols_of(&specs, &code) {
        let (packets, open, newest) = match count_and_newest(state.jobs.as_ref(), &spec.kind).await
        {
            Ok(t) => t,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("jobs read for kind {} failed: {e}", spec.kind),
                )
                    .into_response();
            }
        };
        protocol_rows.push(KindReadiness {
            kind: spec.kind.clone(),
            version: spec.version,
            label: spec.label.clone(),
            packets,
            open,
            newest_terminal: newest,
        });
    }
    let kind_names: Vec<String> = protocol_rows.iter().map(|k| k.kind.clone()).collect();
    let protocols = Part::judged(!protocol_rows.is_empty(), json!({ "kinds": protocol_rows }));

    // SENSORS: rows whose readings open one of those kinds.
    let sensors = match state.sensors.as_ref() {
        None => Part::undetermined(
            "no sensor registry is wired on this deployment",
            json!({ "sensors": [] }),
        ),
        Some(repo) => match repo.list().await {
            Ok(rows) => {
                let opening = readiness::sensors_opening(&rows, &kind_names);
                Part::judged(!opening.is_empty(), json!({ "sensors": opening }))
            }
            Err(e) => Part::undetermined(
                format!("sensor registry read failed: {e}"),
                json!({ "sensors": [] }),
            ),
        },
    };

    // RULES: enforced dispatcher rules that open one of those kinds,
    // each with its source (product / tenant:<id>).
    let rules = match state.rules.as_ref() {
        None => Part::undetermined(
            "no dispatcher rules reader is wired on this deployment",
            json!({ "rules": [] }),
        ),
        Some(reader) => match reader.enforced_rules().await {
            Ok(rows) => {
                let opening = readiness::rules_opening(&rows, &kind_names);
                Part::judged(!opening.is_empty(), json!({ "rules": opening }))
            }
            Err(e) => Part::undetermined(
                format!("dispatcher rules read failed: {e}"),
                json!({ "rules": [] }),
            ),
        },
    };

    // RETRO: the newest department-retro carrying this code.
    let retro = match newest_retro(state.jobs.as_ref(), &code).await {
        Ok(newest) => Part::judged(newest.is_some(), json!({ "newest": newest })),
        Err(e) => Part::undetermined(
            format!("jobs read for {RETRO_KIND} failed: {e}"),
            json!({ "newest": Value::Null }),
        ),
    };

    let answer = Readiness::assemble(
        department,
        readiness::surfaces_part(&code),
        sensors,
        rules,
        protocols,
        readiness::probes_part(),
        retro,
    );
    Json(answer).into_response()
}

/// `(packets, open, newest terminal)` for one kind — two counted
/// listings (a `limit=1` page carries the total) and the port's
/// newest-closed read.
async fn count_and_newest(
    jobs: &dyn JobsRepository,
    kind: &str,
) -> Result<(i64, i64, Option<NewestTerminal>), crate::port::JobsError> {
    let all = JobFilter {
        kind: Some(kind.to_string()),
        ..Default::default()
    };
    let (_, packets) = jobs.list_jobs(&all, 1, 0).await?;
    let open_filter = JobFilter {
        kind: Some(kind.to_string()),
        status: Some(JobStatus::Open),
        ..Default::default()
    };
    let (_, open) = jobs.list_jobs(&open_filter, 1, 0).await?;
    let newest = jobs
        .newest_closed_job(kind)
        .await?
        .as_ref()
        .map(NewestTerminal::of);
    Ok((packets, open, newest))
}

/// The newest `department-retro` whose `metadata.department` is
/// `code` — the listing sorts newest-opened first, so one row answers.
async fn newest_retro(
    jobs: &dyn JobsRepository,
    code: &str,
) -> Result<Option<NewestRetro>, crate::port::JobsError> {
    let filter = JobFilter {
        kind: Some(RETRO_KIND.to_string()),
        metadata_contains: Some(json!({ "department": code })),
        ..Default::default()
    };
    let (rows, _) = jobs.list_jobs(&filter, 1, 0).await?;
    Ok(rows.first().map(NewestRetro::of))
}
