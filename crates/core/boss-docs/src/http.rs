//! HTTP handlers for the design decision tracker API — the
//! in-app side of the decision flow described in
//! `docs/architecture-decisions.md` (How decisions evolve).

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::port::{DocsError, DocsRepository};
use crate::reindex::{self, ReindexStats};
use crate::types::{
    DesignDoc, DesignQuestion, DocStatus, FlushJobPayload, JobStatus, JobStatusUpdate,
    PendingDecisionInput,
};

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
        .route("/api/design/rejections", get(list_rejections))
        .route("/api/design/stale-statuses", get(list_stale_statuses))
        .route("/api/design/docs/{*path}", get(get_doc))
        .route("/api/design/reindex", post(post_reindex))
        .route("/api/design/pending-decisions", post(post_pending_decision))
        .route(
            "/api/design/pending-decisions",
            delete(delete_pending_decision),
        )
        .route("/api/design/flush-jobs", post(post_flush_job))
        .route(
            "/api/design/flush-jobs",
            get(list_flush_jobs_by_status).put(put_flush_job_status),
        )
        .route("/api/design/flush-jobs/{id}", get(get_flush_job))
        .route(
            "/api/design/flush-jobs/{id}/retry",
            post(post_retry_flush_job),
        )
        .with_state(shared)
}

/// Hook called by `POST /flush-jobs` after a job is created.
/// No-op in v1; wired to a Claude Agent SDK call in v2. The seam is
/// intentional.
pub fn dispatch_worker(_job_id: &str) {
    // intentionally empty in v1
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
/// The list's job is "what needs review" — `pending_count` (decisions
/// clicked but not yet flushed) can't answer that, and the SPA
/// mislabeling it "Open Qs" showed 0 for a doc with three live
/// questions.
#[derive(Serialize)]
struct DocListRow {
    #[serde(flatten)]
    doc: DesignDoc,
    open_questions: usize,
}

/// `GET /api/design/rejections` — docs on disk that are NOT in the
/// tracker, and why.
///
/// The review panel's algedonic signal. Empty is the healthy state;
/// non-empty means the corpus you are looking at is incomplete, which
/// is otherwise indistinguishable from a doc nobody wrote.
async fn list_rejections(State(state): State<Arc<DocsApiState>>) -> Response {
    match state.repo.all_rejections().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err_to_response(e),
    }
}

/// A doc whose status line no longer describes it.
#[derive(Serialize)]
pub struct StaleStatus {
    pub path: String,
    pub title: String,
    pub status: String,
    pub reason: String,
}

/// Which docs claim live discussion while having nothing open?
///
/// Pure, and separated from the read so the rule can be tested
/// without a database — the rule is the whole content of this
/// feature, and it is one a person will want to argue with.
///
/// DERIVED, NOT STORED, which is the difference between this and
/// `design_doc_rejections`. A rejection is a fact with a history
/// worth keeping ("failing since Tuesday"); a stale status is just
/// what the current rows say when you read them together, so storing
/// it would create a second copy that can disagree with the first.
/// The corpus tells on itself at read time or not at all.
pub(crate) fn stale_statuses(rows: &[(String, String, DocStatus, usize)]) -> Vec<StaleStatus> {
    rows.iter()
        .filter(|(_, _, status, open)| status.claims_live_discussion() && *open == 0)
        .map(|(path, title, status, _)| StaleStatus {
            path: path.clone(),
            title: title.clone(),
            status: status.as_str().to_string(),
            reason: format!(
                "status is `{}`, which asserts live discussion, but the doc has no open questions",
                status.as_str()
            ),
        })
        .collect()
}

/// `GET /api/design/stale-statuses` — docs whose status line has
/// drifted from what the doc actually is.
///
/// A REPORT, never a rejection, and the distinction is load-bearing.
/// `crates-and-layers.md` reads `in-review (agent draft; the
/// re-tiering call is david's)` — a doc legitimately waiting on a
/// person with no questions registered. Refusing to index that would
/// be wrong, and a rule that has to be right about intent is a rule
/// that will be wrong about it. Saying "these eleven claim to be
/// live and are not" needs no such judgement.
async fn list_stale_statuses(State(state): State<Arc<DocsApiState>>) -> Response {
    let docs = match state.repo.all_docs().await {
        Ok(docs) => docs,
        Err(e) => return err_to_response(e),
    };
    let mut rows = Vec::with_capacity(docs.len());
    for doc in docs {
        let open = match state.repo.questions_for_doc(&doc.path).await {
            Ok(qs) => qs.iter().filter(|q| !q.resolved).count(),
            Err(e) => return err_to_response(e),
        };
        rows.push((doc.path, doc.title, doc.status, open));
    }
    Json(stale_statuses(&rows)).into_response()
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
        // `(resolved)` stays a row so the doc keeps its decision
        // record, but counting it here made a doc whose review had
        // just been flushed still report its questions outstanding,
        // which read as "the flush did nothing".
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

async fn post_reindex(State(state): State<Arc<DocsApiState>>) -> Response {
    match reindex::reindex(state.repo.as_ref(), &state.repo_root).await {
        Ok(stats) => Json(ReindexResponse::from(stats)).into_response(),
        Err(e) => err_to_response(e),
    }
}

// ----- POST /api/design/pending-decisions -----

/// The id nothing named. Kept as a constant so the tests and the
/// surfaces that render it agree on one spelling.
pub const UNKNOWN_ACTOR: &str = "unknown";

/// Who a write is recorded as. For v1 we trust the header — the
/// gateway strips client-supplied `x-boss-*` at the edge and injects
/// the session's own, so the header is the trustworthy channel and the
/// BODY is not; FIDO gating lands later.
///
/// `x-boss-user` FIRST (backlog c3cd3301). It is the one identity
/// header every caller already sends: the gateway builds it from the
/// session, the dispatcher stamps its rule into it, and `boss docs`
/// now signs with it through `boss-cli/src/identity.rs`. This handler
/// used to read ONLY `x-boss-employee-id`, which the gateway sets and
/// no service-to-service caller does — so every flush job the
/// `design-decision-flush-queue` rule queued recorded `unknown`, and
/// the CLI's writes were anonymous by construction.
///
/// The employee-id fallback stays because it costs nothing and keeps
/// any caller that sends only that one working.
fn decided_by_from_headers(headers: &axum::http::HeaderMap) -> String {
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

async fn post_pending_decision(
    State(state): State<Arc<DocsApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<PendingDecisionInput>,
) -> Response {
    let decided_by = decided_by_from_headers(&headers);
    match state
        .repo
        .upsert_pending_decision(&input, &decided_by)
        .await
    {
        Ok(pending) => Json(pending).into_response(),
        Err(e) => err_to_response(e),
    }
}

// ----- DELETE /api/design/pending-decisions?doc_path=...&anchor=... -----

#[derive(Deserialize)]
struct DeletePendingQuery {
    doc_path: String,
    anchor: String,
}

async fn delete_pending_decision(
    State(state): State<Arc<DocsApiState>>,
    Query(q): Query<DeletePendingQuery>,
) -> Response {
    match state
        .repo
        .delete_pending_decision(&q.doc_path, &q.anchor)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_to_response(e),
    }
}

// ----- POST /api/design/flush-jobs -----

#[derive(Deserialize)]
struct CreateFlushJobRequest {
    doc_path: String,
}

async fn post_flush_job(
    State(state): State<Arc<DocsApiState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateFlushJobRequest>,
) -> Response {
    // Pull the current pending decisions for the doc and snapshot
    // them into a FlushJobPayload. The repository's create_flush_job
    // still validates that the rows exist — we're not relying on this
    // read being atomic with the insert, the transaction inside the
    // repository is what makes it safe.
    let pending = match state.repo.pending_decisions_for_doc(&req.doc_path).await {
        Ok(p) => p,
        Err(e) => return err_to_response(e),
    };
    if pending.is_empty() {
        // D8 — defensive BadRequest even though the UI should prevent it.
        return (
            StatusCode::BAD_REQUEST,
            format!("no pending decisions for {}", req.doc_path),
        )
            .into_response();
    }

    let decisions: Vec<_> = pending
        .iter()
        .map(|p| crate::types::FlushDecision {
            anchor: p.anchor.clone(),
            kind: p.kind,
            resolution: p.resolution.clone(),
            rationale: p.rationale.clone(),
        })
        .collect();

    let base_commit_sha = crate::reindex::current_head_sha(&state.repo_root);
    let payload = FlushJobPayload {
        doc_path: req.doc_path.clone(),
        base_commit_sha,
        decisions,
    };

    let requested_by = decided_by_from_headers(&headers);
    match state.repo.create_flush_job(&payload, &requested_by).await {
        Ok(job) => {
            // D14 — call the dispatch_worker hook. v1: no-op. v2:
            // wired to a Claude Agent SDK call.
            dispatch_worker(&job.id);
            (StatusCode::CREATED, Json(job)).into_response()
        }
        Err(e) => err_to_response(e),
    }
}

// ----- GET /api/design/flush-jobs?status=queued -----

#[derive(Deserialize)]
struct JobListQuery {
    status: Option<String>,
    limit: Option<i64>,
}

async fn list_flush_jobs_by_status(
    State(state): State<Arc<DocsApiState>>,
    Query(q): Query<JobListQuery>,
) -> Response {
    if let Some(status_str) = q.status {
        let status = match status_str.as_str() {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "succeeded" => JobStatus::Succeeded,
            "failed" => JobStatus::Failed,
            other => {
                return (StatusCode::BAD_REQUEST, format!("invalid status `{other}`"))
                    .into_response();
            }
        };
        match state.repo.flush_jobs_by_status(status).await {
            Ok(jobs) => Json(jobs).into_response(),
            Err(e) => err_to_response(e),
        }
    } else {
        let limit = q.limit.unwrap_or(20).min(100);
        match state.repo.recent_flush_jobs(limit).await {
            Ok(jobs) => Json(jobs).into_response(),
            Err(e) => err_to_response(e),
        }
    }
}

// ----- GET /api/design/flush-jobs/{id} -----

async fn get_flush_job(State(state): State<Arc<DocsApiState>>, Path(id): Path<String>) -> Response {
    match state.repo.flush_job_by_id(&id).await {
        Ok(Some(job)) => Json(job).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no job {id}")).into_response(),
        Err(e) => err_to_response(e),
    }
}

// ----- PUT /api/design/flush-jobs?id=X -----
//
// Used by the worker (v1: Claude in chat) to transition a job
// through the lifecycle. Takes the id via query param because axum
// 0.8 has a quirk with colliding GET+PUT on the same path pattern
// when one uses a captured segment.

#[derive(Deserialize)]
struct PutJobQuery {
    id: String,
}

async fn put_flush_job_status(
    State(state): State<Arc<DocsApiState>>,
    Query(q): Query<PutJobQuery>,
    headers: axum::http::HeaderMap,
    Json(update): Json<JobStatusUpdate>,
) -> Response {
    // Who moved it. Read off the request, never the body: the body is
    // the caller's to write, the identity header is the gateway's
    // (backlog c3cd3301). An unnamed caller records as `unknown` here
    // rather than being refused — the CLI already refuses an unnamed
    // WRITE before it reaches the network, and a service that dropped
    // the flusher's terminal PUT would strand the job it just
    // committed.
    let worked_by = decided_by_from_headers(&headers);
    match state
        .repo
        .update_flush_job_status(&q.id, &update, Some(worked_by.as_str()))
        .await
    {
        Ok(job) => Json(job).into_response(),
        Err(e) => err_to_response(e),
    }
}

// ----- POST /api/design/flush-jobs/{id}/retry -----

async fn post_retry_flush_job(
    State(state): State<Arc<DocsApiState>>,
    Path(id): Path<String>,
) -> Response {
    match state.repo.retry_flush_job(&id).await {
        Ok(job) => {
            if job.status == JobStatus::Queued {
                dispatch_worker(&job.id);
            }
            Json(job).into_response()
        }
        Err(e) => err_to_response(e),
    }
}

// ---------------------------------------------------------------------------
// Tests — TestApp pattern using tower::ServiceExt to exercise the router.
// ---------------------------------------------------------------------------

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

    async fn post_pending(
        app: &axum::Router,
        doc_path: &str,
        anchor: &str,
        resolution: &str,
    ) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/design/pending-decisions")
                    .header("content-type", "application/json")
                    .header("x-boss-employee-id", "alice")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "doc_path": doc_path,
                            "anchor": anchor,
                            "kind": "accept",
                            "resolution": resolution,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn seed_doc(state: &DocsApiState, path: &str) {
        let now = chrono::Utc::now();
        let doc = DesignDoc {
            path: path.to_string(),
            title: "Test".to_string(),
            status: crate::types::DocStatus::InReview,
            pending_count: 0,
            word_count: 100,
            last_modified: now,
            last_author: "alice".to_string(),
            last_indexed_at: now,
            last_commit_sha: "abc".to_string(),
            content_html: "<h1>Test</h1>".to_string(),
        };
        state.repo.upsert_doc(&doc, &[]).await.unwrap();
    }

    #[tokio::test]
    async fn list_counts_open_questions_and_detail_renders_bodies() {
        // The review surfaces need (a) a real open-question count on the
        // list — pending_count is unflushed decisions, not questions —
        // and (b) per-question body_html rendered with the same pipeline
        // as the doc, so the step plugin never shows raw markdown.
        let state = test_state();
        let now = chrono::Utc::now();
        let doc = DesignDoc {
            path: "docs/design/x.md".to_string(),
            title: "X".to_string(),
            status: crate::types::DocStatus::InReview,
            pending_count: 0,
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
        assert_eq!(list[0]["pending_count"], serde_json::json!(0));

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

    #[tokio::test]
    async fn full_flush_flow_happy_path() {
        let state = test_state();
        seed_doc(&state, "docs/design/test.md").await;
        let repo = state.repo.clone();
        let app = router(state);

        // Click through 2 decisions.
        for i in 0..2 {
            let resp = post_pending(
                &app,
                "docs/design/test.md",
                &format!("Q{i}"),
                &format!("answer {i}"),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
        // Pending count is 2.
        let doc = repo
            .doc_by_path("docs/design/test.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(doc.pending_count, 2);

        // POST flush job.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/design/flush-jobs")
                    .header("content-type", "application/json")
                    .header("x-boss-employee-id", "alice")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "doc_path": "docs/design/test.md",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let job = body_json(resp).await;
        let job_id = job.get("id").and_then(|v| v.as_str()).unwrap().to_string();
        assert_eq!(job.get("status").and_then(|v| v.as_str()), Some("queued"));
        let decisions = job
            .get("payload")
            .and_then(|p| p.get("decisions"))
            .and_then(|d| d.as_array())
            .unwrap();
        assert_eq!(decisions.len(), 2);

        // Pending rows are gone.
        let pending = repo
            .pending_decisions_for_doc("docs/design/test.md")
            .await
            .unwrap();
        assert!(pending.is_empty());

        // GET the job by id.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/design/flush-jobs/{job_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // PUT status → running.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/design/flush-jobs?id={job_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "status": "running",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // PUT status → succeeded with commit_sha.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/design/flush-jobs?id={job_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "status": "succeeded",
                            "commit_sha": "abcd1234",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(
            body.get("commit_sha").and_then(|v| v.as_str()),
            Some("abcd1234")
        );
    }

    /// Backlog c3cd3301, at the receiving end. `boss docs
    /// flush-pending` now signs every call with `x-boss-user`; a
    /// service that dropped the header would leave the fix decorative,
    /// so the status PUT records who moved the job.
    #[tokio::test]
    async fn a_flush_job_records_the_actor_that_moved_it() {
        let state = test_state();
        seed_doc(&state, "docs/design/test.md").await;
        let repo = state.repo.clone();
        let app = router(state);
        post_pending(&app, "docs/design/test.md", "Q0", "yes").await;
        let job_id = queue_flush(&app, "docs/design/test.md").await;

        // The flusher claims it, signed as the operator running it.
        let resp = put_status(
            &app,
            &job_id,
            serde_json::json!({"status": "running"}),
            Some(&user_header("claude@algedonic.dev")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let job = repo.flush_job_by_id(&job_id).await.unwrap().unwrap();
        assert_eq!(
            job.worked_by.as_deref(),
            Some("claude@algedonic.dev"),
            "the status PUT must record the actor that made it"
        );

        // ...and a requeue clears it: a job waiting to run has no
        // worker, and the last one is not the current one.
        let resp = put_status(
            &app,
            &job_id,
            serde_json::json!({"status": "queued"}),
            Some(&user_header("claude@algedonic.dev")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let job = repo.flush_job_by_id(&job_id).await.unwrap().unwrap();
        assert_eq!(job.worked_by, None, "a requeued job has no worker");
    }

    /// The dispatcher's flush-queue rule has always sent `x-boss-user`
    /// and this handler read only `x-boss-employee-id`, so every job
    /// the `design-decision-flush-queue` rule queued recorded
    /// `unknown` — a write with a perfectly good actor on the wire,
    /// discarded one layer above it.
    #[tokio::test]
    async fn a_service_caller_is_recorded_from_x_boss_user() {
        let state = test_state();
        seed_doc(&state, "docs/design/test.md").await;
        let repo = state.repo.clone();
        let app = router(state);
        post_pending(&app, "docs/design/test.md", "Q0", "yes").await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/design/flush-jobs")
                    .header("content-type", "application/json")
                    .header(
                        "x-boss-user",
                        user_header("rule:design-decision-flush-queue"),
                    )
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"doc_path": "docs/design/test.md"}))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let job_id = body_json(resp).await["id"].as_str().unwrap().to_string();
        let job = repo.flush_job_by_id(&job_id).await.unwrap().unwrap();
        assert_eq!(
            job.requested_by, "rule:design-decision-flush-queue",
            "a caller that names itself in x-boss-user must not record as `{UNKNOWN_ACTOR}`"
        );
    }

    /// The gateway's own header still answers, and a call that names
    /// nobody still records something rather than failing — the CLI
    /// refuses an unnamed WRITE before it reaches the network, and a
    /// service that dropped the flusher's terminal PUT would strand a
    /// job whose commit is already in git.
    #[tokio::test]
    async fn an_unnamed_caller_records_as_unknown_and_the_employee_header_still_answers() {
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
        assert_eq!(decided_by_from_headers(&headers(&[])), UNKNOWN_ACTOR);
        assert_eq!(
            decided_by_from_headers(&headers(&[("x-boss-employee-id", "emp-david")])),
            "emp-david"
        );
        // Both present — the gateway sends both — and the id in
        // x-boss-user is the one provenance reads.
        assert_eq!(
            decided_by_from_headers(&headers(&[
                ("x-boss-employee-id", "emp-david"),
                ("x-boss-user", &user_header("emp-david")),
            ])),
            "emp-david"
        );
        // A malformed or empty header names nobody, and naming nobody
        // is `unknown` — never the empty string.
        assert_eq!(
            decided_by_from_headers(&headers(&[("x-boss-user", "not json")])),
            UNKNOWN_ACTOR
        );
        assert_eq!(
            decided_by_from_headers(&headers(&[("x-boss-user", r#"{"id":"  "}"#)])),
            UNKNOWN_ACTOR
        );
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

    async fn queue_flush(app: &axum::Router, doc_path: &str) -> String {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/design/flush-jobs")
                    .header("content-type", "application/json")
                    .header("x-boss-employee-id", "alice")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"doc_path": doc_path})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        body_json(resp).await["id"].as_str().unwrap().to_string()
    }

    async fn put_status(
        app: &axum::Router,
        job_id: &str,
        body: serde_json::Value,
        user: Option<&str>,
    ) -> axum::response::Response {
        let mut req = Request::builder()
            .method("PUT")
            .uri(format!("/api/design/flush-jobs?id={job_id}"))
            .header("content-type", "application/json");
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        app.clone()
            .oneshot(
                req.body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn flush_with_zero_pending_returns_400() {
        let state = test_state();
        seed_doc(&state, "docs/design/test.md").await;
        let app = router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/design/flush-jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "doc_path": "docs/design/test.md",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_jobs_by_status() {
        let state = test_state();
        seed_doc(&state, "docs/design/test.md").await;
        let app = router(state);

        // Create a pending decision then flush.
        let resp = post_pending(&app, "docs/design/test.md", "Q1", "answer").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/design/flush-jobs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "doc_path": "docs/design/test.md",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // List queued jobs.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/design/flush-jobs?status=queued")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body.as_array().unwrap().len(), 1);

        // List by recent.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/design/flush-jobs?limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_pending_decision_flow() {
        let state = test_state();
        seed_doc(&state, "docs/design/test.md").await;
        let app = router(state);

        post_pending(&app, "docs/design/test.md", "Q1", "answer").await;

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/design/pending-decisions?doc_path=docs/design/test.md&anchor=Q1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }
}

#[cfg(test)]
mod stale_status_tests {
    use super::stale_statuses;
    use crate::types::DocStatus;

    fn row(path: &str, status: DocStatus, open: usize) -> (String, String, DocStatus, usize) {
        (path.to_string(), format!("Title of {path}"), status, open)
    }

    // The measured case. Eleven docs read like this on 2026-08-15 and
    // nothing in the system said so.
    #[test]
    fn a_doc_claiming_review_with_nothing_open_is_reported() {
        let got = stale_statuses(&[row("docs/design/a.md", DocStatus::InReview, 0)]);
        assert_eq!(got.len(), 1);
        assert!(got[0].reason.contains("in-review"), "{}", got[0].reason);
        assert!(
            got[0].reason.contains("no open questions"),
            "{}",
            got[0].reason
        );
    }

    #[test]
    fn draft_and_reopened_claim_discussion_too() {
        let got = stale_statuses(&[
            row("a.md", DocStatus::Draft, 0),
            row("b.md", DocStatus::Reopened, 0),
        ]);
        assert_eq!(got.len(), 2);
    }

    // The half that keeps this a report worth reading. A doc with
    // live questions is doing exactly what its status says.
    #[test]
    fn a_doc_with_open_questions_is_not_stale() {
        assert!(stale_statuses(&[row("a.md", DocStatus::InReview, 3)]).is_empty());
    }

    // Settled statuses are the OTHER predicate's business — the
    // reindex already rejects a shipped doc carrying questions, and
    // reporting them here as well would double-count the same drift
    // under two names.
    #[test]
    fn settled_statuses_are_not_this_predicates_business() {
        let got = stale_statuses(&[
            row("a.md", DocStatus::Shipped, 0),
            row("b.md", DocStatus::Approved, 0),
            row("c.md", DocStatus::Living, 0),
            row("d.md", DocStatus::Superseded, 0),
        ]);
        assert!(got.is_empty(), "{got:?}", got = got.len());
    }

    // The two predicates must not overlap: a status is either
    // asserting discussion or forbidding it, never both, or a doc
    // could be rejected AND reported for opposite reasons.
    #[test]
    fn the_two_predicates_are_disjoint() {
        for s in [
            DocStatus::Draft,
            DocStatus::InReview,
            DocStatus::Approved,
            DocStatus::Shipped,
            DocStatus::Reopened,
            DocStatus::Superseded,
            DocStatus::Living,
        ] {
            assert!(
                !(s.claims_live_discussion() && s.forbids_open_questions()),
                "{} both claims and forbids discussion",
                s.as_str()
            );
        }
    }
}
