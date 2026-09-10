//! HTTP handlers for the design decision tracker API — the
//! in-app side of the decision flow described in
//! `docs/architecture-decisions.md` (How decisions evolve).

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tracing::info;

use crate::port::{DocsError, DocsRepository};
use crate::reindex::{self, ReindexStats};
use crate::types::{DesignDoc, DesignQuestion};

/// Shared state for the HTTP layer.
pub struct DocsApiState {
    pub repo: Arc<dyn DocsRepository>,
    pub repo_root: PathBuf,
}

/// Build the router.
pub fn router(state: DocsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/design/health", get(health))
        // Alias under the canonical /api/<port-name>/health shape
        // (the boss-ports registry calls this service "docs").
        // Without this, the SPA's IT Monitoring panel reports
        // `boss-docs-api` as down even when running.
        .route("/api/docs/health", get(health))
        .route("/api/design/docs", get(list_docs))
        .route("/api/design/docs/{*path}", get(get_doc))
        .route("/api/design/reindex", post(post_reindex))
        .with_state(shared)
}

// ----- Error conversion -----

fn err_to_response(e: DocsError) -> Response {
    match e {
        DocsError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        DocsError::BadRequest(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        DocsError::Conflict(s) => (StatusCode::CONFLICT, s).into_response(),
        DocsError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
    }
}

// ----- Health -----

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-docs-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

// ----- GET /api/design/docs -----

/// A docs-list row: the cached doc plus its live open-question count.
/// The list's job is "what needs review". The row used to carry a
/// second count of recorded-but-unwritten answers, which cannot answer
/// that question, and the SPA mislabeling it "Open Qs" showed 0 for a
/// doc with three live questions; that count went with the flush
/// pipeline (2026-09-10).
#[derive(Serialize)]
struct DocListRow {
    #[serde(flatten)]
    doc: DesignDoc,
    open_questions: usize,
}

async fn list_docs(State(state): State<Arc<DocsApiState>>) -> Response {
    let docs = match state.repo.all_docs().await {
        Ok(docs) => docs,
        Err(e) => return err_to_response(e),
    };
    // One count read per doc — the corpus is a handful of files by
    // design (docs/design/*.md), so a join-shaped repo method isn't
    // worth the port churn yet.
    let mut rows = Vec::with_capacity(docs.len());
    for doc in docs {
        // OPEN questions — not "questions parsed". A question marked
        // `(resolved)`, or one carrying a recorded decision, stays a
        // row so the doc keeps its decision record; counting those
        // here made an answered doc still report its questions
        // outstanding, which read as "answering did nothing".
        let open_questions = match state.repo.questions_for_doc(&doc.path).await {
            Ok(qs) => qs.iter().filter(|q| !q.resolved).count(),
            Err(e) => return err_to_response(e),
        };
        rows.push(DocListRow {
            doc,
            open_questions,
        });
    }
    Json(rows).into_response()
}

// ----- GET /api/design/docs/{path} -----

/// A question as the review surfaces consume it: the parsed fields
/// plus `body_html`, rendered server-side with the same pipeline as
/// the doc body so the two can't drift. The step plugin used to show
/// raw markdown in a <pre>.
#[derive(Serialize)]
struct QuestionDetail {
    #[serde(flatten)]
    question: DesignQuestion,
    body_html: String,
}

#[derive(Serialize)]
struct DocDetail {
    #[serde(flatten)]
    doc: DesignDoc,
    questions: Vec<QuestionDetail>,
}

async fn get_doc(State(state): State<Arc<DocsApiState>>, Path(path): Path<String>) -> Response {
    // Axum captures `{*path}` with the tail without a leading slash.
    let path = path.trim_start_matches('/').to_string();
    match state.repo.doc_by_path(&path).await {
        Ok(Some(doc)) => match state.repo.questions_for_doc(&path).await {
            Ok(questions) => {
                let questions = questions
                    .into_iter()
                    .map(|q| {
                        let body_html = crate::parser::render_html(&q.body_md);
                        QuestionDetail {
                            question: q,
                            body_html,
                        }
                    })
                    .collect();
                Json(DocDetail { doc, questions }).into_response()
            }
            Err(e) => err_to_response(e),
        },
        Ok(None) => (StatusCode::NOT_FOUND, format!("no doc at {path}")).into_response(),
        Err(e) => err_to_response(e),
    }
}

// ----- POST /api/design/reindex -----

#[derive(Serialize)]
struct ReindexResponse {
    docs_indexed: usize,
    docs_deleted: usize,
    duration_ms: u64,
    rejected: Vec<RejectedDocResponse>,
}

#[derive(Serialize)]
struct RejectedDocResponse {
    path: String,
    reason: String,
}

impl From<ReindexStats> for ReindexResponse {
    fn from(s: ReindexStats) -> Self {
        Self {
            docs_indexed: s.docs_indexed,
            docs_deleted: s.docs_deleted,
            duration_ms: s.duration_ms,
            rejected: s
                .rejected
                .into_iter()
                .map(|r| RejectedDocResponse {
                    path: r.path,
                    reason: r.reason,
                })
                .collect(),
        }
    }
}

async fn post_reindex(
    State(state): State<Arc<DocsApiState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    let actor = actor_from_headers(&headers);
    match reindex::reindex(state.repo.as_ref(), &state.repo_root).await {
        Ok(stats) => {
            // Who ran it, and what it refused. The reindex fires at
            // boot AND by hand through `boss docs reindex`, and the
            // two are worth telling apart; the rejected list is the
            // only place a refused doc is now named, so it is logged
            // rather than only returned (CLAUDE.md §Diagnosis — a
            // record reduced before it is stored is the only copy
            // thrown away).
            info!(
                actor = %actor,
                docs_indexed = stats.docs_indexed,
                docs_deleted = stats.docs_deleted,
                rejected = stats.rejected.len(),
                rejections = ?stats
                    .rejected
                    .iter()
                    .map(|r| format!("{}: {}", r.path, r.reason))
                    .collect::<Vec<_>>(),
                "design corpus reindexed"
            );
            Json(ReindexResponse::from(stats)).into_response()
        }
        Err(e) => err_to_response(e),
    }
}

/// The id nothing named. Kept as a constant so the tests and the
/// surfaces that render it agree on one spelling.
pub const UNKNOWN_ACTOR: &str = "unknown";

/// Who a docs write is recorded as. The gateway strips
/// client-supplied `x-boss-*` at the edge and injects the session's
/// own, so the header is the trustworthy channel and a request BODY is
/// not.
///
/// `x-boss-user` FIRST (backlog c3cd3301). It is the one identity
/// header every caller already sends: the gateway builds it from the
/// session, the dispatcher stamps its rule into it, and `boss docs`
/// signs with it through `boss-cli/src/identity.rs`. This handler used
/// to read ONLY `x-boss-employee-id`, which the gateway sets and no
/// service-to-service caller does — so every write the dispatcher
/// queued recorded `unknown`, and the CLI's writes were anonymous by
/// construction: a correct actor on the wire, discarded one layer
/// above it.
///
/// The employee-id fallback stays because it costs nothing and keeps
/// any caller that sends only that one working.
fn actor_from_headers(headers: &axum::http::HeaderMap) -> String {
    let from_user = headers
        .get("x-boss-user")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
        .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(str::to_string))
        .filter(|id| !id.trim().is_empty());
    from_user
        .or_else(|| {
            headers
                .get("x-boss-employee-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .filter(|id| !id.trim().is_empty())
        })
        .unwrap_or_else(|| UNKNOWN_ACTOR.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryDocsRepo;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_state() -> DocsApiState {
        DocsApiState {
            repo: Arc::new(InMemoryDocsRepo::new()),
            repo_root: std::path::PathBuf::from("."),
        }
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
        })
    }

    /// The `x-boss-user` shape every caller sends — the gateway builds
    /// it from the session, `boss-cli/src/identity.rs` builds it for
    /// the CLI, and the dispatcher builds it for a rule.
    fn user_header(id: &str) -> String {
        serde_json::json!({
            "id": id,
            "role": "platform-admin",
            "access_tier": "operator",
        })
        .to_string()
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/design/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_docs_empty() {
        let app = router(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/design/docs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body, serde_json::json!([]));
    }

    #[tokio::test]
    async fn list_counts_open_questions_and_detail_renders_bodies() {
        // The review surfaces need (a) a real open-question count on
        // the list and (b) per-question body_html rendered with the
        // same pipeline as the doc, so the step plugin never shows raw
        // markdown.
        let state = test_state();
        let now = chrono::Utc::now();
        let doc = DesignDoc {
            path: "docs/design/x.md".to_string(),
            title: "X".to_string(),
            status: crate::types::DocStatus::InReview,
            word_count: 10,
            last_modified: now,
            last_author: "alice".to_string(),
            last_indexed_at: now,
            last_commit_sha: "abc".to_string(),
            content_html: "<h1>X</h1>".to_string(),
        };
        let q = crate::types::DesignQuestion {
            id: "docs/design/x.md#Q1".to_string(),
            doc_path: "docs/design/x.md".to_string(),
            anchor: "Q1".to_string(),
            ordinal: 0,
            title: "Which way?".to_string(),
            body_md: "Prefer **value-primary** rows.".to_string(),
            proposal: None,
            context_md: None,
            resolved: false,
        };
        state.repo.upsert_doc(&doc, &[q]).await.unwrap();
        let app = router(state);

        let list = body_json(
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/design/docs")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(list[0]["open_questions"], serde_json::json!(1));

        let detail = body_json(
            app.oneshot(
                Request::builder()
                    .uri("/api/design/docs/docs/design/x.md")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        )
        .await;
        let body_html = detail["questions"][0]["body_html"].as_str().unwrap();
        assert!(
            body_html.contains("<strong>value-primary</strong>"),
            "question body renders as HTML, got: {body_html}"
        );
        // The parsed fields still flatten alongside the rendered body.
        assert_eq!(detail["questions"][0]["anchor"], "Q1");
    }

    /// Backlog c3cd3301, kept after the flush pipeline was deleted.
    /// boss-docs used to read ONLY `x-boss-employee-id`, which the
    /// gateway sets and no service-to-service caller does — so every
    /// write a dispatcher rule made recorded `unknown`: a perfectly
    /// good actor on the wire, discarded one layer above it. The
    /// precedence rule is what this pins.
    #[test]
    fn x_boss_user_wins_and_the_employee_header_still_answers() {
        let headers = |pairs: &[(&str, &str)]| {
            let mut h = axum::http::HeaderMap::new();
            for (k, v) in pairs {
                h.insert(
                    axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    axum::http::HeaderValue::from_str(v).unwrap(),
                );
            }
            h
        };
        assert_eq!(actor_from_headers(&headers(&[])), UNKNOWN_ACTOR);
        assert_eq!(
            actor_from_headers(&headers(&[("x-boss-employee-id", "emp-david")])),
            "emp-david"
        );
        assert_eq!(
            actor_from_headers(&headers(&[(
                "x-boss-user",
                &user_header("rule:design-review-level-sweep")
            )])),
            "rule:design-review-level-sweep"
        );
        // Both present — the gateway sends both — and the id in
        // x-boss-user is the one provenance reads.
        assert_eq!(
            actor_from_headers(&headers(&[
                ("x-boss-employee-id", "emp-david"),
                ("x-boss-user", &user_header("emp-david")),
            ])),
            "emp-david"
        );
        // A malformed or empty header names nobody, and naming nobody
        // is `unknown` — never the empty string.
        assert_eq!(
            actor_from_headers(&headers(&[("x-boss-user", "not json")])),
            UNKNOWN_ACTOR
        );
        assert_eq!(
            actor_from_headers(&headers(&[("x-boss-user", r#"{"id":"  "}"#)])),
            UNKNOWN_ACTOR
        );
    }
}
