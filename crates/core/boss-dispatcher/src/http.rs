//! HTTP surface: health + readiness probes, the read-only cascade-viz
//! `rules` feed, and the rule-authoring write endpoints (create-draft /
//! validate / publish / retire) that back the SPA authoring UI. The
//! authoring writes go through `crate::rules::authoring`; the running
//! RulesRunner picks up a published change on its next restart (live
//! hot-reload is a planned follow-up).
//!
//! `/api/dispatcher/health` answers 200 while the PROCESS is up — necessary
//! but NOT sufficient: the consumer loops run detached and can die while the
//! process keeps serving 200 (a NATS blip, JetStream not ready at cold start
//! under a no-restart launcher). `/api/dispatcher/readyz` reports the actual
//! consumer liveness (see [`crate::liveness`]) so operators — and the brewery
//! sim's pre-Go readiness gate — can tell "up" from "actually working."

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::cascade;
use crate::liveness::DispatcherLiveness;
use crate::rules::authoring::{self, AuthoringError};
use crate::rules::registry::{ENFORCED_STATUS, RawRule, authored_why, load_active_rules};

/// HTTP state: the consumer-liveness handle + the Postgres pool, so the
/// read-only `/api/dispatcher/rules` surface can serve the rule registry
/// (the `dispatcher_rules` table) for the cascade visualization, plus
/// the authored registry directory each rule's `why` is read from.
#[derive(Clone)]
pub struct HttpState {
    pub live: Arc<DispatcherLiveness>,
    pub pool: sqlx::PgPool,
    /// `DispatcherConfig::authored_rules_dir` — the authored
    /// `infra/dispatcher/rules` directory, source of each rule's `why`.
    /// `None` is served as `why: null` with the reason stated, never as
    /// "no rule records a why".
    pub authored_rules_dir: Option<PathBuf>,
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/api/dispatcher/health", get(health))
        .route("/api/dispatcher/readyz", get(readyz))
        // GET serves the cascade-viz feed; POST creates a new rule draft.
        .route("/api/dispatcher/rules", get(rules).post(create_rule_draft))
        .route("/api/dispatcher/rules/_validate", post(validate_rule))
        .route(
            "/api/dispatcher/rules/{name}/versions",
            get(list_rule_versions),
        )
        .route(
            "/api/dispatcher/rules/{name}/versions/{version}",
            get(get_rule_version),
        )
        .route("/api/dispatcher/rules/{name}/publish", post(publish_rule))
        .route("/api/dispatcher/rules/{name}/retire", post(retire_rule))
        .with_state(state)
}

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-dispatcher",
        env!("CARGO_PKG_VERSION"),
        "nats-subscriber",
    ))
}

/// Real readiness: are both durable consumers bound + draining? Returns
/// `{ready, assigning, assignment_events, rules_running, rules_events,
/// last_event_unix, dead_letters, dead_letters_unrecorded,
/// last_dead_letter_unix}`. `ready=false` while health is 200 is the exact
/// "process up, but assigning nothing, so Jobs never close" failure.
async fn readyz(State(state): State<HttpState>) -> Json<serde_json::Value> {
    Json(state.live.snapshot())
}

/// Join the enforced rules with their authored justification — the
/// per-rule view this surface serves. Pure: rows in, JSON out.
///
/// Each object is the stored rule (`name`, `version`, the trigger —
/// `on_event` or `schedule` — `when`, `do`, `delay`) plus three fields
/// the registry row does not itself carry:
///
/// - `status` — which registry status these rows were selected on, so a
///   row SAYS what it is instead of the reader having to know the
///   handler's filter.
/// - `why` — the justification the rule's authored file records, or
///   `null` when no file does.
/// - `authored` — whether the authored registry holds a file for the
///   rule at all. `authored: false` is the §9a drift this surface
///   exists to show: a reaction the system is enforcing that the
///   authored registry does not record, and therefore one the `why`
///   guard (`dispatcher-rules-ratchet.sh`, `parse_raw_dir`) never saw.
fn rule_views(
    rules: &[RawRule],
    status: &str,
    why: &BTreeMap<String, String>,
) -> Vec<serde_json::Value> {
    rules
        .iter()
        .map(|r| {
            // Defensive, not fallible in practice: RawRule serializes to
            // an object. Falling back to an empty map keeps the rule in
            // the list (named below) rather than dropping it silently.
            let mut obj = match serde_json::to_value(r) {
                Ok(serde_json::Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            obj.insert("name".into(), serde_json::Value::String(r.name.clone()));
            obj.insert("status".into(), serde_json::Value::String(status.into()));
            obj.insert(
                "why".into(),
                why.get(&r.name).map_or(serde_json::Value::Null, |w| {
                    serde_json::Value::String(w.clone())
                }),
            );
            obj.insert(
                "authored".into(),
                serde_json::Value::Bool(why.contains_key(&r.name)),
            );
            serde_json::Value::Object(obj)
        })
        .collect()
}

/// Read each rule's `why` from the authored registry directory,
/// alongside a `authored_registry` block describing WHERE the whys came
/// from and what went wrong if they did not.
///
/// The block is the point: without it, an unset or unreadable directory
/// and a registry in which no rule records a why are the same response
/// — well-formed, confident and wrong (CLAUDE.md §Doors). A reader
/// seeing `why: null` everywhere must be able to tell which it is.
fn authored_whys(dir: Option<&std::path::Path>) -> (BTreeMap<String, String>, serde_json::Value) {
    let Some(dir) = dir else {
        return (
            BTreeMap::new(),
            serde_json::json!({
                "dir": null, "rules": 0,
                "error": "BOSS_DISPATCHER_RULES is unset — there is no authored \
                          registry to read `why` from, so every `why` below is \
                          null for want of a source, not for want of a reason",
            }),
        );
    };
    let dir_str = dir.display().to_string();
    match authored_why(dir) {
        Ok(map) => {
            let n = map.len();
            (
                map,
                serde_json::json!({ "dir": dir_str, "rules": n, "error": null }),
            )
        }
        Err(e) => (
            BTreeMap::new(),
            serde_json::json!({ "dir": dir_str, "rules": 0, "error": e.to_string() }),
        ),
    }
}

/// Read-only rule-registry surface: what the dispatcher is enforcing,
/// and why.
///
/// Serves the rows of `dispatcher_rules` at [`ENFORCED_STATUS`] — per
/// rule its `name`, `version`, `status`, trigger (`on_event` or
/// `schedule`), `when`, `do`/args, `delay`, plus the `why` its authored
/// file records and whether it is `authored` at all (see
/// [`rule_views`]) — alongside `authored_registry` (where the whys came
/// from) and the static cascade metadata: per-handler emitted events +
/// the jobs-api/external "system edges" that close the feedback loops.
///
/// Queries the table per request — a low-traffic admin view, and reading
/// live reflects any rule edits without a restart.
async fn rules(State(state): State<HttpState>) -> Json<serde_json::Value> {
    let raw = match load_active_rules(&state.pool).await {
        Ok(raw) => raw,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("load dispatcher_rules: {e}"),
                "rules": [], "authored_registry": null,
                "handler_emits": {}, "system_edges": [],
            }));
        }
    };
    let (why, authored_registry) = authored_whys(state.authored_rules_dir.as_deref());
    let mut out = serde_json::Map::new();
    out.insert(
        "rules".into(),
        serde_json::Value::Array(rule_views(&raw.rules, ENFORCED_STATUS, &why)),
    );
    out.insert("authored_registry".into(), authored_registry);
    out.insert(
        "handler_emits".into(),
        serde_json::to_value(cascade::handler_emits()).unwrap_or_default(),
    );
    out.insert(
        "system_edges".into(),
        serde_json::to_value(cascade::system_edges()).unwrap_or_default(),
    );
    Json(serde_json::Value::Object(out))
}

// ---------------------------------------------------------------------------
// Rule authoring (control-plane writes) — see crate::rules::authoring.
// ---------------------------------------------------------------------------

fn authoring_err(e: AuthoringError) -> Response {
    let code = match &e {
        AuthoringError::NotFound(_) => StatusCode::NOT_FOUND,
        AuthoringError::Invalid(_) => StatusCode::BAD_REQUEST,
        // 422, not 400: the stored row is well-formed — it just would not
        // load as a Rule. Same posture as the Workflow and station
        // registries' publish refusals.
        AuthoringError::Unviable(_) => StatusCode::UNPROCESSABLE_ENTITY,
        AuthoringError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, e.to_string()).into_response()
}

/// `POST /api/dispatcher/rules` — append a new draft version of a rule.
/// Body is the rule spec (name, on_event, when?, do[], delay?). The draft is
/// validated (must load via `Rule::from_raw`) before it persists; `201` on
/// success returns the stored draft.
async fn create_rule_draft(State(state): State<HttpState>, Json(raw): Json<RawRule>) -> Response {
    match authoring::create_draft(&state.pool, &raw).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/_validate` — dry-run a draft without
/// persisting. Returns `{ ok, error }` so the authoring UI can surface
/// topic/predicate/arg parse errors live, before publish.
async fn validate_rule(Json(raw): Json<RawRule>) -> Json<serde_json::Value> {
    match authoring::validate(&raw) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "error": null })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// `GET /api/dispatcher/rules/{name}/versions` — all versions, oldest first
/// (draft + active + retired).
async fn list_rule_versions(State(state): State<HttpState>, Path(name): Path<String>) -> Response {
    match authoring::list_versions(&state.pool, &name).await {
        Ok(vs) => Json(vs).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `GET /api/dispatcher/rules/{name}/versions/{version}` — one version.
async fn get_rule_version(
    State(state): State<HttpState>,
    Path((name, version)): Path<(String, i32)>,
) -> Response {
    match authoring::get_version(&state.pool, &name, version).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/{name}/publish` — activate the latest draft,
/// retiring the prior active version.
async fn publish_rule(State(state): State<HttpState>, Path(name): Path<String>) -> Response {
    match authoring::publish(&state.pool, &name).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/{name}/retire` — retire the active version.
async fn retire_rule(State(state): State<HttpState>, Path(name): Path<String>) -> Response {
    match authoring::retire(&state.pool, &name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => authoring_err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::registry::RawDoStep;

    fn rule(name: &str) -> RawRule {
        RawRule {
            name: name.into(),
            on_event: Some("step.done.task".into()),
            schedule: None,
            when: Some("spec_slug = \"merged\"".into()),
            do_steps: vec![RawDoStep {
                handler: "jobs.spawn".into(),
                args: Default::default(),
            }],
            delay: None,
            version: 3,
        }
    }

    /// The seven fields an operator asking "what are you enforcing, and
    /// why" needs, in one row: name, version, status, the trigger, the
    /// predicate, the side effects, and the justification.
    #[test]
    fn a_rule_view_carries_what_the_system_enforces_and_why() {
        let why = BTreeMap::from([(
            "converge-on-merge".to_string(),
            "a cross-protocol reactor".to_string(),
        )]);
        let views = rule_views(&[rule("converge-on-merge")], ENFORCED_STATUS, &why);

        let v = &views[0];
        assert_eq!(v["name"], "converge-on-merge");
        assert_eq!(v["version"], 3);
        assert_eq!(v["status"], ENFORCED_STATUS);
        assert_eq!(v["on_event"], "step.done.task");
        assert_eq!(v["when"], "spec_slug = \"merged\"");
        assert_eq!(v["do"][0]["handler"], "jobs.spawn");
        assert_eq!(v["why"], "a cross-protocol reactor");
        assert_eq!(v["authored"], serde_json::Value::Bool(true));
    }

    /// A rule the system enforces that NO authored file records reads as
    /// `authored: false, why: null` — the §9a drift, visible in the
    /// response itself rather than only to whoever runs the check.
    /// Measured 2026-09-10 against the live registry: four of sixty-four
    /// enforced rules had no file, so the `why` guard covered sixty.
    #[test]
    fn an_unauthored_rule_says_so_instead_of_going_quiet() {
        let views = rule_views(
            &[rule("auto-park-on-gate-green")],
            ENFORCED_STATUS,
            &BTreeMap::new(),
        );

        assert_eq!(views[0]["name"], "auto-park-on-gate-green");
        assert_eq!(views[0]["authored"], serde_json::Value::Bool(false));
        assert!(views[0]["why"].is_null(), "{}", views[0]);
    }

    /// An unset or unreadable authored registry must be DISTINGUISHABLE
    /// from one in which no rule records a why. Both serve `why: null`;
    /// only the `authored_registry` block says which, and without it the
    /// response is the confident wrong answer CLAUDE.md §Doors names.
    #[test]
    fn the_response_says_why_the_whys_are_missing() {
        let (map, block) = authored_whys(None);
        assert!(map.is_empty());
        assert!(block["dir"].is_null(), "{block}");
        assert!(
            block["error"]
                .as_str()
                .unwrap_or_default()
                .contains("BOSS_DISPATCHER_RULES"),
            "an unset directory must name the knob that sets it: {block}"
        );

        let missing = std::path::Path::new("/nonexistent/dispatcher/rules");
        let (map, block) = authored_whys(Some(missing));
        assert!(map.is_empty());
        assert_eq!(block["dir"], missing.display().to_string());
        assert!(
            !block["error"].is_null(),
            "an unreadable directory must report its error, not read as \
             an empty registry: {block}"
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sweep.toml"),
            "[[rule]]\nname = \"sweep\"\nwhy = \"\"\"\na timer\n\"\"\"\n\
             on_event = \"x.y\"\n[[rule.do]]\nhandler = \"noop\"\n",
        )
        .unwrap();
        let (map, block) = authored_whys(Some(dir.path()));
        assert_eq!(map.len(), 1);
        assert_eq!(block["rules"], 1);
        assert!(block["error"].is_null(), "{block}");
    }
}
