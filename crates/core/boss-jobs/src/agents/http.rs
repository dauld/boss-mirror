//! Axum routes for the agents registry: the roster's list and its one
//! write, the tenant batch (backlog f56155f0, 2026-09-17). Until this
//! door the registry's rows arrived by migration only, so the company's
//! own agent was declared nowhere the product read.
//!
//! **Reads admit `crate::trust::can_read`** — operator machinery and
//! the auditor tier, the recorded-probe reader (`boss-sor-read
//! /api/agents` is how the car for this surface is proved). **Writes
//! admit `crate::trust::is_trusted`** — operator tier, the tier
//! `boss tenant publish` signs with. A request with no identity header
//! is refused on both (e84de48e).
//!
//! `POST /api/agents/batch[?mode=insert-if-absent|take]` takes a bare
//! JSON array of `AgentInput` (the classes and locations batch shape),
//! validates every row with the same `validate_agent` the TOML loader
//! ran, refuses the whole batch (422, deterministic) naming the row,
//! and answers what it did — rows inserted, rows the registry held and
//! KEPT with the declared fields they differ on named (the default;
//! design e187198f: the instance is the truth), rows a `take` UPDATED
//! with each change named (field, from, to; the rule of 09887242, now
//! only by decision), rows already as declared — so what a publish
//! moved, and what it did not, is read in the publish line, never in
//! silence. This batch IS the tenant's update door: the declaration is
//! the whole row, so a per-row PUT would only repeat it.
//!
//! A declared `role` or `department` is a Class code under
//! `(employee, role)` / `(employee, department)` — the same registry
//! rows `boss-people` checks an employee's against, through the same
//! `ClassesClient` — and this door checks it BEFORE the batch lands
//! (backlog ab192a9f): an undeclared code refuses the whole batch by
//! agent, attribute and code. The check lives here, not in the
//! adapters, because this is the one write path and the code is data
//! the database cannot vouch for (a CHECK copied from the registry
//! would drift from it). `classes: None` skips it, the people
//! adapter's posture for in-memory and test paths; the service binary
//! always wires it.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_core::publish::ModeQuery;
use boss_policy_client::CurrentUser;

use crate::trust::{can_read, is_trusted};

use super::port::{AgentsError, AgentsRegistry};
use super::types::{AgentInput, validate_agent};

pub struct AgentsApiState {
    pub registry: Arc<dyn AgentsRegistry>,
    /// The Class registry an agent's `role` / `department` are checked
    /// against. `None` skips the check (test and in-memory paths).
    pub classes: Option<Arc<dyn ClassesClient>>,
}

/// Why a declared role or department is refused: the code is not an
/// active Class under `(employee, <attribute>)`. `Ok(None)` when every
/// declared code is held; `Err` when the registry could not answer,
/// which is a different fact from "not held" and is answered as one.
async fn undeclared_class(
    classes: &dyn ClassesClient,
    rows: &[AgentInput],
) -> Result<Option<String>, String> {
    for a in rows {
        for (attribute, code) in [("role", &a.role), ("department", &a.department)] {
            let Some(code) = code else { continue };
            // On its OWN axis (backlog ab1e6ff8): the employee drawer
            // holds role, department, status and employment_type codes
            // side by side, and `class_exists` accepted a department as
            // a role.
            let held = classes
                .class_exists_on(&ClassRef::new("employee", code.as_str()), attribute)
                .await
                .map_err(|e| format!("classes registry: {e}"))?;
            if !held {
                return Ok(Some(format!(
                    "agent {}: {attribute} `{code}` is not an active Class in the registry \
                     (subject_kind employee, member_attribute {attribute}) — declare the Class \
                     in seeds/classes.json first, as for an employee's",
                    a.id
                )));
            }
        }
    }
    Ok(None)
}

pub fn router(state: AgentsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/agents", get(list))
        .route("/api/agents/batch", post(publish))
        .with_state(shared)
}

fn err_response(e: AgentsError) -> Response {
    match e {
        AgentsError::Unpriced(m) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "default_model `{m}` is not a rate-card model — the registry refuses a default \
                 it cannot price (spell it as the card does, e.g. opus-5[1m], not claude-…)"
            ),
        )
            .into_response(),
        AgentsError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn list(
    State(state): State<Arc<AgentsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.list().await {
        Ok(rows) => Json(serde_json::json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(e) => err_response(e),
    }
}

async fn publish(
    State(state): State<Arc<AgentsApiState>>,
    CurrentUser(user): CurrentUser,
    Query(ModeQuery { mode }): Query<ModeQuery>,
    Json(rows): Json<Vec<AgentInput>>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(why) = rows.iter().find_map(|a| validate_agent(a).err()) {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    if let Some(classes) = &state.classes {
        match undeclared_class(classes.as_ref(), &rows).await {
            Ok(None) => {}
            Ok(Some(why)) => return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response(),
            Err(why) => return (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
        }
    }
    // Same envelope construction as the credentials door: the actor
    // the request signed with rides as `_actor` (and is named again
    // as `declared_by` on each inserted row's fact), the stamp is
    // wall-clock, and the source is `jobs` — this service is the one
    // recording. A trusted sibling with no identity is the platform.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = boss_core::publisher::EventStamp::new("jobs", actor);
    match state.registry.publish(&rows, mode, &stamp).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::agents::InMemoryAgents;

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

    fn probe_reader() -> Option<String> {
        Some(header(
            "audit-readonly",
            "audit-readonly",
            AccessTier::Auditor,
        ))
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

    fn app(registry: &Arc<InMemoryAgents>) -> Router {
        router(AgentsApiState {
            registry: registry.clone() as Arc<dyn AgentsRegistry>,
            classes: None,
        })
    }

    /// The door with the Class registry wired, holding exactly the
    /// tenant's `engineering-agent` role and `engineering` department,
    /// each on its own axis — the shape the live registry serves (every
    /// live employee Class carries a member_attribute, 2026-09-23).
    fn app_with_classes(registry: &Arc<InMemoryAgents>) -> Router {
        use boss_classes_client::FakeClassesClient;
        use boss_core::primitives::Class;
        let on = |code: &str, attribute: &str| Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some(attribute.into()),
            metadata: Value::Null,
            sort_order: 0,
            retired_at: None,
        };
        router(AgentsApiState {
            registry: registry.clone() as Arc<dyn AgentsRegistry>,
            classes: Some(Arc::new(FakeClassesClient::with_classes(vec![
                on("engineering-agent", "role"),
                on("engineering", "department"),
            ]))),
        })
    }

    fn batch() -> Value {
        json!([{
            "id": "agent-claude", "display_name": "Claude (engineering)",
            "default_model": "opus-5[1m]", "aliases": ["claude@algedonic.dev"]
        }])
    }

    /// The tenant batch lands, the roster lists it with its alias, a
    /// declaration that disagrees with a registered row is KEPT by
    /// default with the differing fields named (design e187198f: the
    /// instance is the truth), UPDATES it under `?mode=take` naming
    /// each change (backlog 09887242), a re-run of the same declaration
    /// changes nothing and says so, and an alias the tenant did not
    /// declare is kept.
    #[tokio::test]
    async fn a_tenant_publishes_and_the_roster_lists_the_agent() {
        let registry = Arc::new(InMemoryAgents::new());
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 1);
        assert_eq!(out["updated"], json!([]));
        assert_eq!(out["unchanged"], 0);

        let (status, body) = send(app(&registry), "GET", "/api/agents", None, probe_reader()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let listing: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(listing["total"], 1);
        assert_eq!(listing["data"][0]["id"], "agent-claude");
        assert_eq!(listing["data"][0]["default_model"], "opus-5[1m]");
        assert_eq!(
            listing["data"][0]["aliases"],
            json!(["claude@algedonic.dev"])
        );

        // An operator-added login the tenant's file does not list.
        let registry =
            Arc::new(InMemoryAgents::new().with_agent("agent-claude", ["ops-added@algedonic.dev"]));
        let mut renamed = batch();
        renamed[0]["display_name"] = json!("Claude (renamed)");
        // The default: the held row is kept, the declared login lands
        // (nobody held it), and the answer names what still differs.
        let (_, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(renamed.clone()),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0, "a held row is not inserted twice");
        assert_eq!(out["unchanged"], 0);
        assert_eq!(out["kept"][0]["id"], "agent-claude");
        assert_eq!(
            out["kept"][0]["differs"],
            json!(["display_name", "default_model"]),
            "{out}"
        );
        assert_eq!(
            out["updated"][0]["changes"][0]["field"], "aliases",
            "the declared login landed, and that is the one change: {out}"
        );
        let rows = registry.list().await.unwrap();
        assert_eq!(
            rows[0].display_name, "agent-claude",
            "the instance's row is kept under the default"
        );
        assert_eq!(
            rows[0].aliases,
            ["claude@algedonic.dev", "ops-added@algedonic.dev"],
            "the declared alias landed and the undeclared one is kept"
        );

        // Under take the declaration overwrites, naming each change.
        let (_, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch?mode=take",
            Some(renamed.clone()),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0);
        assert_eq!(out["kept"], json!([]));
        assert_eq!(out["unchanged"], 0);
        assert_eq!(out["updated"][0]["id"], "agent-claude");
        let fields: Vec<&str> = out["updated"][0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["field"].as_str().unwrap())
            .collect();
        assert_eq!(fields, ["display_name", "default_model"]);
        assert_eq!(out["updated"][0]["changes"][0]["from"], "agent-claude");
        assert_eq!(out["updated"][0]["changes"][0]["to"], "Claude (renamed)");
        let rows = registry.list().await.unwrap();
        assert_eq!(
            rows[0].display_name, "Claude (renamed)",
            "the declaration wins under take"
        );

        // The same declaration again: nothing moves, and the answer
        // says so rather than naming a phantom update.
        let (_, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(renamed),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0);
        assert_eq!(out["updated"], json!([]));
        assert_eq!(out["kept"], json!([]));
        assert_eq!(out["unchanged"], 1);
        let events = registry.recorded_events();
        assert_eq!(
            events.len(),
            2,
            "one agent.updated for the alias that landed, one for the take, none for the re-run: {events:?}"
        );
        assert_eq!(events[1].kind, super::super::AGENT_UPDATED);
        assert_eq!(events[1].payload["id"], "agent-claude");
        assert_eq!(events[1].payload["display_name"], "Claude (renamed)");
        assert_eq!(events[1].payload["changes"][0]["field"], "display_name");
        assert_eq!(events[1].payload["updated_by"], "automation:tenant-seed");
        assert_eq!(events[1].payload["_actor"], events[1].payload["updated_by"]);

        // A mode the door does not know is a caller error, not a
        // silent default.
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/agents/batch?mode=overwrite",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// The fact a declaration leaves (backlog d9409039, 2026-09-17):
    /// one `agent.declared` per row INSERTED — the row as inserted plus
    /// `declared_by`, the actor the request signed with — one
    /// `agent.updated` for the row the registry already held and the
    /// declaration moved, nothing for the batch.
    #[tokio::test]
    async fn a_batch_records_one_declared_event_per_inserted_row_and_one_updated_for_a_held_row() {
        let registry = Arc::new(InMemoryAgents::new().with_agent("agent-claude", []));
        let mut rows = batch();
        rows.as_array_mut().unwrap().push(json!({
            "id": "agent-scout", "display_name": "Scout",
            "default_model": "opus-5[1m]", "aliases": ["scout@algedonic.dev"]
        }));
        rows.as_array_mut().unwrap().push(json!({
            "id": "agent-clerk", "display_name": "Clerk", "default_model": "opus-5[1m]",
            "max_concurrent_runs": 2
        }));
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(rows),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 2);
        assert_eq!(out["updated"][0]["id"], "agent-claude");

        let all = registry.recorded_events();
        assert!(all.iter().all(|e| e.source == "jobs"), "{all:?}");
        let kinds: Vec<&str> = all.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                super::super::AGENT_UPDATED,
                super::super::AGENT_DECLARED,
                super::super::AGENT_DECLARED
            ],
            "one agent.updated for the held agent-claude, one agent.declared per inserted row"
        );
        assert_eq!(all[0].payload["id"], "agent-claude");
        let events: Vec<_> = all
            .into_iter()
            .filter(|e| e.kind == super::super::AGENT_DECLARED)
            .collect();
        let ids: Vec<&str> = events
            .iter()
            .map(|e| e.payload["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["agent-scout", "agent-clerk"]);
        assert_eq!(events[0].payload["display_name"], json!("Scout"));
        assert_eq!(events[0].payload["default_model"], json!("opus-5[1m]"));
        assert_eq!(events[0].payload["aliases"], json!(["scout@algedonic.dev"]));
        assert_eq!(events[1].payload["max_concurrent_runs"], json!(2));
        assert_eq!(
            events[0].payload["declared_by"],
            json!("automation:tenant-seed"),
            "the actor the request signed with, named on the fact"
        );
        assert_eq!(
            events[0].payload["_actor"], events[0].payload["declared_by"],
            "declared_by and the stamp's actor are one value"
        );
    }

    /// An agent's role and department are Class codes under
    /// `(employee, role)` / `(employee, department)` — the rows an
    /// employee's are validated against (backlog ab192a9f). A declared
    /// code the registry holds lands and reads back; one it does not
    /// hold refuses the WHOLE batch, 422, naming the agent, the
    /// attribute and the code, and nothing lands. A row that declares
    /// neither reads `role: null` — the KEY is always on the wire, so
    /// a reader tells "holds no role" from "an older registry".
    #[tokio::test]
    async fn a_role_and_a_department_are_class_codes_checked_at_the_door() {
        let registry = Arc::new(InMemoryAgents::new());
        let mut declared = batch();
        declared[0]["role"] = json!("engineering-agent");
        declared[0]["department"] = json!("engineering");
        let (status, body) = send(
            app_with_classes(&registry),
            "POST",
            "/api/agents/batch",
            Some(declared.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = send(
            app_with_classes(&registry),
            "GET",
            "/api/agents",
            None,
            probe_reader(),
        )
        .await;
        let listing: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(listing["data"][0]["role"], "engineering-agent");
        assert_eq!(listing["data"][0]["department"], "engineering");

        // An undeclared code, and a declared code on the OTHER axis: a
        // department is not a role however active its row is (backlog
        // ab1e6ff8 — this door asked the axis-blind question until then).
        for (attribute, code) in [
            ("role", "wizard"),
            ("department", "narnia"),
            ("role", "engineering"),
            ("department", "engineering-agent"),
        ] {
            let mut bad = declared.clone();
            bad[0][attribute] = json!(code);
            let (status, body) = send(
                app_with_classes(&registry),
                "POST",
                "/api/agents/batch",
                Some(bad),
                seed(),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{attribute}: {body}"
            );
            assert!(
                body.contains("agent-claude")
                    && body.contains(attribute)
                    && body.contains(code)
                    && body.contains("Class"),
                "{attribute}: {body}"
            );
        }
        let rows = registry.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].role.as_deref(),
            Some("engineering-agent"),
            "a refused batch lands nothing: the held row keeps its declared role"
        );
        assert_eq!(
            registry.recorded_events().len(),
            1,
            "one agent.declared for the row that landed, nothing for the refusals"
        );

        // A declaration that names neither: the key is there, null.
        let registry = Arc::new(InMemoryAgents::new());
        let (status, body) = send(
            app_with_classes(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = send(
            app_with_classes(&registry),
            "GET",
            "/api/agents",
            None,
            probe_reader(),
        )
        .await;
        let listing: Value = serde_json::from_str(&body).unwrap();
        let row = listing["data"][0].as_object().unwrap();
        assert!(row.contains_key("role") && row["role"].is_null(), "{row:?}");
        assert!(
            row.contains_key("department") && row["department"].is_null(),
            "{row:?}"
        );
    }

    /// A bad declaration refuses the WHOLE batch, 422, naming the row;
    /// nothing lands.
    #[tokio::test]
    async fn a_bad_declaration_refuses_the_batch_by_name() {
        let registry = Arc::new(InMemoryAgents::new());
        let mut b = batch();
        b.as_array_mut().unwrap().push(json!({
            "id": "claude@algedonic.dev", "display_name": "x", "default_model": "opus-5"
        }));
        let (status, body) =
            send(app(&registry), "POST", "/api/agents/batch", Some(b), seed()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("claude@algedonic.dev"), "{body}");
        assert!(registry.list().await.unwrap().is_empty());
    }

    /// The probe reader reads; a request with no identity header does
    /// not (backlog e84de48e, 2026-09-25 — the roster, aliases and all,
    /// was one of the fifteen reads a headerless caller was trusted
    /// with); a user-tier session writes nothing; the auditor reads and
    /// cannot write.
    #[tokio::test]
    async fn reads_admit_the_probe_reader_and_writes_are_operator_machinery() {
        let registry = Arc::new(InMemoryAgents::new());
        for (user, want) in [
            (probe_reader(), StatusCode::OK),
            (None, StatusCode::FORBIDDEN),
        ] {
            let (status, _) = send(app(&registry), "GET", "/api/agents", None, user.clone()).await;
            assert_eq!(status, want, "{user:?}");
        }
        let david = Some(header("emp-david", "platform-admin", AccessTier::User));
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            david,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            probe_reader(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
