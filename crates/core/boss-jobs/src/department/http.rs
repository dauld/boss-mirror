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
//! are still worth reading. The READINESS read admits
//! `crate::trust::can_read` — the operator tier, a trusted sibling,
//! and the auditor tier the recorded probe reader carries. The bare
//! LIST asks no tier: it is the org chart the chrome bar renders for
//! every signed-in user (backlog dc5788ba), the posture `/api/classes`
//! it took those tabs over from already had.
//!
//! `POST /api/departments/batch[?mode=insert-if-absent|take]` is the
//! registry's one write (backlog 7edf0e97): a bare JSON array of
//! `declare::DepartmentInput`, the agents batch's shape and posture.
//! It admits `crate::trust::is_trusted` — the tier `boss tenant
//! publish` signs with. Every row runs `declare::validate_batch` (a
//! slug, never a reserved root segment, no twin) and every `function`
//! must be an active Class under `(department, function)`; either
//! refusal is a 422 naming the row, and the whole batch lands or none
//! of it does. The answer is the platform's publish grammar —
//! inserted, updated (from → to), kept (the fields named), unchanged.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use boss_classes_client::ClassesClient;
use boss_core::job::JobStatus;
use boss_core::primitives::ClassRef;
use boss_core::publish::ModeQuery;
use boss_policy_client::CurrentUser;
use serde_json::{Value, json};

use super::declare::{DepartmentInput, validate_batch};
use super::readiness::{self, KindReadiness, NewestRetro, NewestTerminal, Part, Readiness};
use super::registry::{Department, DepartmentRegistry};
use super::rules::DispatcherRules;
use crate::port::{JobFilter, JobsRepository};
use crate::registry::WorkflowRegistry;
use crate::sensors::Sensors;
use crate::trust::{can_read, is_trusted};

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
    /// The Class registry a declared `function` is checked against
    /// (`(department, function)`). `None` skips the check — the agents
    /// door's posture for in-memory and test paths; the service binary
    /// wires it whenever it knows the classes URL.
    pub classes: Option<Arc<dyn ClassesClient>>,
}

pub fn router(state: DepartmentsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/departments", get(list))
        .route("/api/departments/batch", post(publish))
        .route("/api/departments/{code}/readiness", get(readiness))
        .with_state(shared)
}

/// The first declared `function` that is not an active Class under
/// `(department, function)`, as the refusal's words. `Err` is the
/// registry not answering — a different fact from "not held".
async fn undeclared_function(
    classes: &dyn ClassesClient,
    rows: &[DepartmentInput],
) -> Result<Option<String>, String> {
    for d in rows {
        let held = classes
            .class_exists_on(
                &ClassRef::new("department", d.function.as_str()),
                "function",
            )
            .await
            .map_err(|e| format!("classes registry: {e}"))?;
        if !held {
            return Ok(Some(format!(
                "department {}: function `{}` is not an active Class in the registry \
                 (subject_kind department, member_attribute function) — declare it in \
                 seeds/classes.json first",
                d.code, d.function
            )));
        }
    }
    Ok(None)
}

/// The tenant batch — see the module doc.
async fn publish(
    State(state): State<Arc<DepartmentsApiState>>,
    CurrentUser(user): CurrentUser,
    Query(ModeQuery { mode }): Query<ModeQuery>,
    Json(rows): Json<Vec<DepartmentInput>>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(registry) = state.departments.as_ref() else {
        return unavailable("departments registry");
    };
    if let Err(why) = validate_batch(&rows) {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    if let Some(classes) = &state.classes {
        match undeclared_function(classes.as_ref(), &rows).await {
            Ok(None) => {}
            Ok(Some(why)) => return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response(),
            Err(why) => return (StatusCode::BAD_GATEWAY, why).into_response(),
        }
    }
    // The agents door's stamp: the actor the request signed with rides
    // as `_actor` and again as `declared_by` / `updated_by`; the source
    // is `jobs`, the service recording. A trusted sibling with no
    // identity is the platform.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = boss_core::publisher::EventStamp::new("jobs", actor);
    match registry.publish(&rows, mode, &stamp).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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

/// The org chart, and it is not operator machinery — so unlike the
/// readiness read beside it, this one asks no tier (backlog dc5788ba).
/// The chrome bar builds its tabs from this on every page for every
/// signed-in user; until that car it derived them from
/// `GET /api/classes`, which gates nothing, so asking `can_read` here
/// would take every department tab away from everyone below operator.
/// The gateway's session cookie is still the door — this is a service
/// behind it, the posture `/api/classes` and `/api/subject-kinds` take.
async fn list(State(state): State<Arc<DepartmentsApiState>>) -> Response {
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_classes_client::FakeClassesClient;
    use boss_core::primitives::Class;
    use boss_policy_client::{AccessTier, User};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::department::registry::{
        DEPARTMENT_DECLARED, DEPARTMENT_UPDATED, InMemoryDepartments,
    };

    fn header(id: &str, role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: id.into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
        .expect("the user header serializes")
    }

    fn seed() -> Option<String> {
        Some(header(
            "automation:tenant-seed",
            "platform-admin",
            AccessTier::Operator,
        ))
    }

    /// The four function Classes the department migration seeds, on
    /// their own axis — the shape the live registry serves.
    fn functions() -> Arc<dyn ClassesClient> {
        let on = |code: &str| Class {
            subject_kind: "department".into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some("function".into()),
            metadata: Value::Null,
            sort_order: 0,
            retired_at: None,
        };
        Arc::new(FakeClassesClient::with_classes(vec![
            on("operations"),
            on("revenue"),
            on("support"),
            on("governance"),
        ]))
    }

    fn row(code: &str, function: &str, sort_order: i32) -> DepartmentInput {
        DepartmentInput {
            code: code.into(),
            display_name: code.to_uppercase(),
            function: function.into(),
            sort_order,
            retired: false,
        }
    }

    /// The registry as migration 20260919181324 leaves an instance:
    /// three of the thirteen, one of them a demo-tenant row.
    fn seeded() -> Arc<InMemoryDepartments> {
        Arc::new(
            InMemoryDepartments::new()
                .with(&row("it", "operations", 1))
                .with(&row("sales", "revenue", 40))
                .with(&row("warehouse", "operations", 100)),
        )
    }

    fn app(registry: &Arc<InMemoryDepartments>) -> Router {
        router(DepartmentsApiState {
            departments: Some(registry.clone() as Arc<dyn DepartmentRegistry>),
            kinds: None,
            jobs: Arc::new(crate::in_memory::InMemoryJobs::new()) as Arc<dyn JobsRepository>,
            sensors: None,
            rules: None,
            classes: Some(functions()),
        })
    }

    async fn send(
        app: Router,
        method: &str,
        path: &str,
        body: Option<Value>,
        user: Option<String>,
    ) -> (StatusCode, String) {
        let mut req = Request::builder().method(method).uri(path);
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        let body = body.map(|b| Body::from(b.to_string())).unwrap_or_default();
        let resp = app
            .oneshot(req.body(body).expect("request builds"))
            .await
            .expect("the router answers");
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn codes(registry: &Arc<InMemoryDepartments>) -> Vec<String> {
        let (status, body) = send(app(registry), "GET", "/api/departments", None, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v: Value = serde_json::from_str(&body).expect("json");
        v["data"]
            .as_array()
            .expect("data")
            .iter()
            .map(|d| d["code"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// THE DEFECT (backlog 7edf0e97): the seeded roster could not be
    /// changed. A tenant's declaration now lands its new rows, keeps a
    /// held row that differs under the default (named, the instance is
    /// the truth), and a take retires a row — which then leaves the
    /// list the retro rule and the chrome bar read — each change on the
    /// log as a fact.
    #[tokio::test]
    async fn a_tenant_declares_its_roster_and_a_take_retires_a_row() {
        let registry = seeded();
        let mut retired = row("warehouse", "operations", 100);
        retired.retired = true;
        let declared = json!([
            row("it", "operations", 1),
            row("product", "operations", 5),
            row("design", "operations", 6),
            retired,
        ]);

        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/departments/batch",
            Some(declared.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(out["received"], 4);
        assert_eq!(out["inserted"], 2, "product and design are new: {out}");
        assert_eq!(out["unchanged"], 1, "it is as declared: {out}");
        assert_eq!(
            out["kept"],
            json!([{ "id": "warehouse", "differs": ["retired"] }]),
            "a plain publish never retires a live row: {out}"
        );
        assert_eq!(
            codes(&registry).await,
            ["it", "product", "design", "sales", "warehouse"]
        );

        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/departments/batch?mode=take",
            Some(declared.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(out["inserted"], 0);
        assert_eq!(
            out["updated"],
            json!([{ "id": "warehouse",
                     "changes": [{ "field": "retired", "from": false, "to": true }] }]),
            "{out}"
        );
        assert_eq!(
            codes(&registry).await,
            ["it", "product", "design", "sales"],
            "a retired department leaves the list; one the tenant did not declare stays"
        );

        // A second take is a no-op: nothing updated, nothing recorded.
        let before = registry.recorded_events().len();
        let (_, body) = send(
            app(&registry),
            "POST",
            "/api/departments/batch?mode=take",
            Some(declared),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(
            (out["inserted"].clone(), out["unchanged"].clone()),
            (json!(0), json!(4))
        );
        assert_eq!(registry.recorded_events().len(), before);

        let events = registry.recorded_events();
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [DEPARTMENT_DECLARED, DEPARTMENT_DECLARED, DEPARTMENT_UPDATED],
            "one fact per inserted row and one per row a take changed"
        );
        assert_eq!(events[0].payload["code"], "product");
        assert_eq!(events[0].payload["declared_by"], "automation:tenant-seed");
        assert_eq!(events[2].payload["code"], "warehouse");
        assert_eq!(events[2].payload["retired"], true);
        assert_eq!(events[2].payload["updated_by"], "automation:tenant-seed");
    }

    /// Every refusal is the whole batch, a 422 naming the row: a
    /// reserved root segment, a code that is no slug, a twin, and a
    /// function the Class registry does not hold on its axis. Nothing
    /// lands from a refused batch, and only a trusted writer may send
    /// one.
    #[tokio::test]
    async fn a_bad_row_refuses_the_whole_batch_by_name() {
        for (bad, says) in [
            (row("api", "operations", 1), "reserved root segment"),
            (row("jobs", "operations", 1), "reserved root segment"),
            (row("Product", "operations", 1), "lowercase slug"),
            (row("product", "engineering", 1), "function `engineering`"),
        ] {
            let registry = seeded();
            let (status, body) = send(
                app(&registry),
                "POST",
                "/api/departments/batch",
                Some(json!([row("design", "operations", 6), bad])),
                seed(),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert!(body.contains(says), "{says}: {body}");
            assert_eq!(codes(&registry).await, ["it", "sales", "warehouse"]);
            assert!(registry.recorded_events().is_empty());
        }
        let registry = seeded();
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/departments/batch",
            Some(json!([
                row("design", "operations", 6),
                row("design", "support", 7)
            ])),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("declared twice"), "{body}");

        let reader = Some(header("emp-someone", "employee", AccessTier::User));
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/departments/batch",
            Some(json!([row("design", "operations", 6)])),
            reader,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(codes(&registry).await, ["it", "sales", "warehouse"]);
    }
}
