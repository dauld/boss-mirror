//! HTTP surface for file references.
//!
//! Lives behind boss-content-api. Routes:
//! - POST   /api/files            — multipart upload
//! - GET    /api/files            — list for a (target_kind, target_id)
//! - GET    /api/files/{id}       — download bytes
//! - DELETE /api/files/{id}       — soft-delete (event + row, bytes
//!   GC'd later by the Session 3 sweep)
//!
//! Per design Q4 (write-then-check): the upload streams + hashes the
//! body first, then policy-checks, then writes the row + emits the
//! event. On policy denial the just-written object is rolled back via
//! `FileStorage::delete`.
//!
//! Per design Q1 the bucket is deployment-scoped and configured at
//! startup; the handlers don't make per-request bucket decisions.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::handler::Handler;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Files larger than this go through a signed-URL redirect on
/// download instead of streaming through the gateway.
pub const LARGE_DOWNLOAD_THRESHOLD_BYTES: i64 = 8 * 1024 * 1024;

/// The largest multipart body `POST /api/files` takes: 2,097,152 bytes
/// (2 MiB), the WHOLE body — the file plus its framing and the two
/// target fields — not the file alone.
///
/// MEASURED, NOT CHOSEN (backlog 7610dd2f, 2026-09-23). Nothing in the
/// content-api set a body limit, so the number in force was axum's
/// implicit default for the `Multipart` extractor: axum-core 0.5.6,
/// `with_limited_body`, `DEFAULT_LIMIT = 2_097_152`. An implicit number
/// can only be restated elsewhere by copying it, and `boss attach` has
/// to state it — in its help and in its refusal — so it is declared
/// here, applied to the route below, and read by the verb from this one
/// constant (CLAUDE.md §9a). Raising it is an edit here; the verb
/// follows.
pub const UPLOAD_BODY_LIMIT_BYTES: usize = 2_097_152;

/// TTL for a presigned GET URL on the large-file download path.
/// Short — the URL leaks scope by construction; the requesting user
/// has already passed policy at request time, so the URL is
/// effectively a 5-minute grant for one user's exact ask.
const SIGNED_GET_TTL: Duration = Duration::from_secs(5 * 60);

/// TTL for a presigned PUT URL on the large-file upload path.
/// Same shape: short window for one specific upload by one client.
const SIGNED_PUT_TTL: Duration = Duration::from_secs(15 * 60);

use boss_policy_client::{Action, Resource};
use boss_policy_client::{CurrentUser, PolicyClient};

use crate::files::error::FileError;
use crate::files::port::{FileRepository, FileStorage};
use crate::files::types::{FileRef, FileRefDraft, ResourceKind, ResourceRef};

/// Per-process state injected into every file-references handler.
pub struct FilesApiState {
    pub repo: Arc<dyn FileRepository>,
    pub storage: Arc<dyn FileStorage>,
    pub publisher: Option<boss_core::publisher::DomainPublisher>,
    pub policy: Arc<dyn PolicyClient>,
    /// Bucket every PUT lands in. Per design Q1 (one bucket per
    /// deployment) this is set once at startup; the handlers don't
    /// take a bucket per request.
    pub bucket: String,
    /// Optional pool for the audit endpoint. None = `/api/files/audit`
    /// returns 503 (lib has no Pg dep without the postgres feature on
    /// the binary). The bin always supplies this.
    #[cfg(feature = "postgres")]
    pub pool: Option<sqlx::PgPool>,
    /// Authoritative clock — every uploaded_at / soft-delete
    /// timestamp + every emitted audit event stamps via clock-api.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router(state: FilesApiState) -> Router {
    let shared = Arc::new(state);
    let r = Router::new()
        .route(
            "/api/files",
            get(list).post(upload.layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT_BYTES))),
        )
        .route("/api/files/{id}", get(download).delete(soft_delete))
        // Session 4 — large-file path. Both routes use `_`-prefixed
        // names so they can never collide with a Uuid that parses to
        // the same literal under /api/files/{id}.
        .route("/api/files/_upload-url", post(request_upload_url))
        .route("/api/files/_finalize", post(finalize_upload));
    // `_audit` underscore-prefix avoids any future "what if a Uuid
    // parses to the literal `audit`" question — the route lives
    // inside the /api/files namespace but can't collide with {id}.
    #[cfg(feature = "postgres")]
    let r = r.route("/api/files/_audit", get(audit));
    r.with_state(shared)
}

// ---- Policy mapping -------------------------------------------------------
//
// Files inherit from their target — read a Job, read its files; update
// a Job, attach files to it. Mapping ResourceKind to the policy
// Resource enum:
//
//   Job   → Resource::job()
//   Step  → Resource::step()
//   Subject → Resource::account() (v1 simplification — Subjects are
//             generic; using the most-common kind unblocks the slice.
//             Session 3 can introduce a Subject-kind-aware mapping
//             once we wire the SPA onto Subject pages.)
//   Event → no policy check; gateway cookie auth gates access. Events
//           are append-only audit rows; their evidence is read-gated
//           by whoever already saw the event in the audit page.

fn target_to_policy_resource(kind: ResourceKind) -> Option<Resource> {
    match kind {
        ResourceKind::Job => Some(Resource::job()),
        ResourceKind::Step => Some(Resource::step()),
        ResourceKind::Subject => Some(Resource::account()),
        ResourceKind::Event => None,
    }
}

async fn check_policy(
    state: &FilesApiState,
    user: &boss_policy_client::User,
    action: Action,
    target: &ResourceRef,
) -> Result<(), Response> {
    let Some(resource) = target_to_policy_resource(target.kind) else {
        return Ok(()); // Event: no policy check
    };
    match state.policy.check(user, action, resource).await {
        Ok(boss_policy_client::Decision::Allow { .. }) => Ok(()),
        Ok(boss_policy_client::Decision::Deny { reason }) => {
            Err((StatusCode::FORBIDDEN, format!("policy denied: {reason}")).into_response())
        }
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("policy unreachable: {e}"),
        )
            .into_response()),
    }
}

fn err(e: FileError) -> Response {
    match e {
        FileError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        FileError::Validation(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        FileError::DuplicateObject(s) => (StatusCode::CONFLICT, s).into_response(),
        FileError::Repository(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
        FileError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
        FileError::Unsupported(s) => (StatusCode::NOT_IMPLEMENTED, s).into_response(),
    }
}

// ---- Handlers -------------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    target_kind: String,
    target_id: String,
}

async fn list(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListQuery>,
) -> Response {
    let Some(kind) = ResourceKind::parse(&q.target_kind) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown target_kind: {}", q.target_kind),
        )
            .into_response();
    };
    let target = ResourceRef {
        kind,
        id: q.target_id,
    };
    if let Err(resp) = check_policy(&state, &user, Action::Read, &target).await {
        return resp;
    }
    match state.repo.list_for(&target).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

/// Multipart upload. Required fields: `target_kind`, `target_id`,
/// and one file part (any name; first non-text part wins). The file
/// part's `filename` + `content_type` headers populate the row.
///
/// Streaming + hashing in one pass: each chunk feeds both the SHA-256
/// hasher and an in-memory buffer. Object key is `sha256/<hash>`,
/// computed after the body is fully read; the bytes are PUT to that
/// key in one shot (v1 doesn't multipart-upload; that's Session 4).
async fn upload(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    mut multipart: Multipart,
) -> Response {
    let mut target_kind: Option<String> = None;
    let mut target_id: Option<String> = None;
    let mut buffer: Option<Bytes> = None;
    let mut filename: Option<String> = None;
    let mut mime: Option<String> = None;
    let mut hasher = Sha256::new();

    // `e.status()`, not a flat 400: a body past UPLOAD_BODY_LIMIT_BYTES
    // is 413, and that status is how a caller tells "too large" from
    // "malformed" without parsing prose (backlog 7610dd2f).
    while let Some(field) = match multipart.next_field().await {
        Ok(f) => f,
        Err(e) => return (e.status(), format!("multipart: {e}")).into_response(),
    } {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "target_kind" => {
                target_kind = field.text().await.ok();
            }
            "target_id" => {
                target_id = field.text().await.ok();
            }
            // Anything else is the file part.
            _ => {
                if buffer.is_some() {
                    return (StatusCode::BAD_REQUEST, "only one file part allowed").into_response();
                }
                filename = field.file_name().map(str::to_string);
                mime = field
                    .content_type()
                    .map(str::to_string)
                    .or(Some("application/octet-stream".to_string()));
                let bytes = match field.bytes().await {
                    Ok(b) => b,
                    Err(e) => {
                        return (e.status(), format!("body read: {e}")).into_response();
                    }
                };
                hasher.update(&bytes);
                buffer = Some(bytes);
            }
        }
    }

    let (Some(kind_str), Some(tid), Some(bytes), Some(fname), Some(mt)) =
        (target_kind, target_id, buffer, filename, mime)
    else {
        return (
            StatusCode::BAD_REQUEST,
            "missing target_kind, target_id, or file part".to_string(),
        )
            .into_response();
    };
    let Some(kind) = ResourceKind::parse(&kind_str) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown target_kind: {kind_str}"),
        )
            .into_response();
    };
    let target = ResourceRef { kind, id: tid };

    if let Err(resp) = check_policy(&state, &user, Action::Update, &target).await {
        return resp;
    }

    let sha256_bytes = hasher.finalize();
    let sha = hex::encode(sha256_bytes);
    let object_key = format!("sha256/{sha}");
    let size_bytes = bytes.len() as i64;

    if let Err(e) = state.storage.put(&object_key, bytes, &mt).await {
        return err(e);
    }

    // OUTBOX (phase 2) below records content.file.attached in the
    // same transaction as the row; `uploaded_at` takes the stamp's
    // wall instant so row, payload, and record stamp agree.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let now = stamp.timestamp;
    let id = Uuid::new_v4();
    let draft = FileRefDraft {
        id,
        target: target.clone(),
        bucket: state.bucket.clone(),
        object_key: object_key.clone(),
        sha256: sha,
        size_bytes,
        mime: mt,
        filename: fname,
        uploaded_by: user.id.clone(),
        uploaded_at: now,
    };

    match state.repo.insert(draft.clone(), &stamp).await {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(e) => {
            // Row insert failed after bytes wrote — this is the
            // duplicate-object-key case (concurrent upload of the
            // same sha) or a transient repo error. Either way the
            // bytes are dedup-safe (same content => same key) so we
            // do NOT delete the object on this failure path; another
            // live ref might be using it. Just surface the error.
            err(e)
        }
    }
}

async fn download(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Response {
    let row = match state.repo.get(id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(e) => return err(e),
    };
    if row.deleted_at.is_some() {
        return (StatusCode::GONE, "file detached").into_response();
    }
    if let Err(resp) = check_policy(&state, &user, Action::Read, &row.target).await {
        return resp;
    }

    // Large files: if the storage backend can mint a presigned URL,
    // 302 to it so the bytes flow client → store directly. The
    // local-disk backend can't presign (returns Unsupported), so we
    // fall through to streaming the bytes through the content-api —
    // fine for a single-VM deployment with no separate object store.
    if row.size_bytes > LARGE_DOWNLOAD_THRESHOLD_BYTES {
        match state
            .storage
            .sign_get_url(&row.object_key, SIGNED_GET_TTL)
            .await
        {
            Ok(url) => return Redirect::temporary(&url).into_response(),
            // Backends without presigned URLs (local disk) stream the
            // bytes through the content-api instead — fall through to
            // the streaming path below.
            Err(FileError::Unsupported(_)) => {}
            Err(FileError::NotFound(_)) => {
                return (
                    StatusCode::GONE,
                    "bytes_missing — row exists but bytes were GC'd",
                )
                    .into_response();
            }
            Err(e) => return err(e),
        }
    }

    match state.storage.get(&row.object_key).await {
        Ok(bytes) => download_response(&row, bytes),
        Err(FileError::NotFound(_)) => (
            StatusCode::GONE,
            "bytes_missing — row exists but bytes were GC'd",
        )
            .into_response(),
        Err(e) => err(e),
    }
}

fn download_response(row: &FileRef, bytes: Bytes) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        row.mime
            .parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );
    // RFC-5987 filename* would be nicer for non-ASCII but `filename=`
    // is universal; stick to ASCII for v1 and let later sessions add
    // the * variant if a non-ASCII upload shows up.
    let disp = format!("attachment; filename=\"{}\"", row.filename.replace('"', ""));
    if let Ok(v) = disp.parse() {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    (StatusCode::OK, headers, bytes).into_response()
}

// ---- Large-file upload path (presigned, optional backend) ----------------
//
// Two-phase: client asks for a presigned PUT URL, uploads bytes
// directly to the object store, then asks the service to finalize
// (insert row + emit event). This requires a storage backend that can
// mint presigned URLs. The default `LocalDiskStorage` cannot, so
// `sign_put_url` returns `Unsupported` and these endpoints are unused —
// the SPA uploads every size via the multipart `upload` handler. The
// endpoints are retained for a future object-store backend.
//
// State between phases is carried by the client — every metadata field
// the row needs gets sent on _both_ requests, and the finalize handler
// HEADs the bucket to verify the bytes actually landed before inserting.
// No server-side pending-upload table; that would require a new schema
// + GC sweep for abandoned uploads, which is overkill for v1.
//
// Trust model: the client-supplied sha256 is taken at face value
// (re-hashing >50 MiB server-side defeats the streaming-upload point).
// A wrong sha just means the bytes land at the wrong key; downloads
// 410 and the audit-sample sweep flags the divergence on the next run.

#[derive(Deserialize)]
struct UploadUrlRequest {
    target_kind: String,
    target_id: String,
    sha256: String,
    size_bytes: i64,
    mime: String,
    /// Carried for symmetry with the finalize body — the server does
    /// not key off of it during URL generation, but accepting it now
    /// means the SPA can send one canonical metadata object in both
    /// requests rather than a slim/full split. Deleting the field would
    /// silently relax the request contract (a body without `filename`
    /// is refused today); `expect` fails the build the day a handler
    /// starts reading it, which is the cue to drop this marker.
    #[expect(
        dead_code,
        reason = "required by the upload request contract, not read during URL generation"
    )]
    filename: String,
}

#[derive(Serialize)]
struct UploadUrlResponse {
    file_id: Uuid,
    upload_url: String,
    object_key: String,
    /// Seconds until `upload_url` expires. Client must complete the
    /// PUT before this elapses; a missed window is recoverable by
    /// re-requesting the URL with the same sha256.
    expires_in_secs: u64,
}

async fn request_upload_url(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<UploadUrlRequest>,
) -> Response {
    let Some(kind) = ResourceKind::parse(&req.target_kind) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown target_kind: {}", req.target_kind),
        )
            .into_response();
    };
    if req.sha256.is_empty() || req.sha256.len() != 64 {
        return (StatusCode::BAD_REQUEST, "sha256 must be 64 hex chars").into_response();
    }
    if req.size_bytes <= 0 {
        return (StatusCode::BAD_REQUEST, "size_bytes must be positive").into_response();
    }
    let target = ResourceRef {
        kind,
        id: req.target_id,
    };
    if let Err(resp) = check_policy(&state, &user, Action::Update, &target).await {
        return resp;
    }
    let object_key = format!("sha256/{}", req.sha256);
    let upload_url = match state
        .storage
        .sign_put_url(&object_key, &req.mime, SIGNED_PUT_TTL)
        .await
    {
        Ok(u) => u,
        Err(e) => return err(e),
    };
    let _ = user; // policy passed; user id lands on the row at finalize
    let resp = UploadUrlResponse {
        file_id: Uuid::new_v4(),
        upload_url,
        object_key,
        expires_in_secs: SIGNED_PUT_TTL.as_secs(),
    };
    Json(resp).into_response()
}

#[derive(Deserialize)]
struct FinalizeRequest {
    file_id: Uuid,
    target_kind: String,
    target_id: String,
    sha256: String,
    size_bytes: i64,
    mime: String,
    filename: String,
}

async fn finalize_upload(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<FinalizeRequest>,
) -> Response {
    let Some(kind) = ResourceKind::parse(&req.target_kind) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown target_kind: {}", req.target_kind),
        )
            .into_response();
    };
    let target = ResourceRef {
        kind,
        id: req.target_id,
    };
    if let Err(resp) = check_policy(&state, &user, Action::Update, &target).await {
        return resp;
    }
    let object_key = format!("sha256/{}", req.sha256);

    // Verify the bytes actually landed. HEAD is cheap; if it 404s
    // the client's PUT either failed or hasn't completed yet — let
    // them retry rather than insert a row that points at nothing.
    match state.storage.head(&object_key).await {
        Ok(actual_size) => {
            // Tolerance: S3's HEAD returns the stored size which
            // should match exactly. Reject mismatches so a
            // misbehaving client can't claim a different size than
            // the bytes it uploaded.
            if (actual_size as i64) != req.size_bytes {
                return (
                    StatusCode::CONFLICT,
                    format!(
                        "uploaded size {} does not match claimed size {}",
                        actual_size, req.size_bytes
                    ),
                )
                    .into_response();
            }
        }
        Err(FileError::NotFound(_)) => {
            return (
                StatusCode::CONFLICT,
                "bytes_not_found — finalize before completing the PUT",
            )
                .into_response();
        }
        Err(e) => return err(e),
    }

    // Same shape as the upload path: the stamp is the one instant.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    let now = stamp.timestamp;
    let draft = FileRefDraft {
        id: req.file_id,
        target: target.clone(),
        bucket: state.bucket.clone(),
        object_key,
        sha256: req.sha256,
        size_bytes: req.size_bytes,
        mime: req.mime,
        filename: req.filename,
        uploaded_by: user.id.clone(),
        uploaded_at: now,
    };
    match state.repo.insert(draft, &stamp).await {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(e) => err(e),
    }
}

/// Optional audit endpoint — re-fetches a sample of live refs and
/// re-hashes them to verify the sha256 chain. Operator-tier only;
/// boss-policy gates with PolicyRule:Read.
#[cfg(feature = "postgres")]
async fn audit(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<AuditQuery>,
) -> Response {
    // Reuse the policy matrix: the audit view is platform-admin-shaped,
    // so check Action::Read on Resource::policy_rule() (the audit log
    // surface uses the same gate today).
    match state
        .policy
        .check(&user, Action::Read, Resource::policy_rule())
        .await
    {
        Ok(boss_policy_client::Decision::Allow { .. }) => {}
        Ok(boss_policy_client::Decision::Deny { reason }) => {
            return (StatusCode::FORBIDDEN, format!("policy denied: {reason}")).into_response();
        }
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("policy unreachable: {e}"),
            )
                .into_response();
        }
    }
    let Some(pool) = state.pool.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "audit endpoint requires postgres pool",
        )
            .into_response();
    };
    let sample_size = q.sample.unwrap_or(50).clamp(1, 500);
    match crate::files::rebuild::audit_sample(&pool, state.storage.clone(), sample_size).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => err(e),
    }
}

#[cfg(feature = "postgres")]
#[derive(Deserialize)]
struct AuditQuery {
    sample: Option<i64>,
}

async fn soft_delete(
    State(state): State<Arc<FilesApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<Uuid>,
) -> Response {
    let row = match state.repo.get(id).await {
        Ok(Some(r)) => r,
        Ok(None) => return (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(e) => return err(e),
    };
    if let Err(resp) = check_policy(&state, &user, Action::Update, &row.target).await {
        return resp;
    }
    // OUTBOX (phase 2): the adapter records content.file.detached in
    // the same transaction as the flip — and only on an actual flip,
    // so a repeat delete no longer publishes a duplicate event.
    let stamp = crate::events::event_stamp(&state.publisher).await;
    if let Err(e) = state
        .repo
        .soft_delete(id, stamp.timestamp, &user.id, &stamp)
        .await
    {
        return err(e);
    }
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    //! The upload door's body limit, measured against the router itself.
    //!
    //! Backlog 7610dd2f (2026-09-23): an actor attaching a file through
    //! `boss attach` is told the largest body this door takes BEFORE it
    //! sends one, and the number it is told must be the number the door
    //! enforces. So the limit is one constant, applied to the route and
    //! read by the verb, and these tests hold the route to it byte-exactly:
    //! a multipart body of exactly the limit is taken, and one byte more
    //! is refused with 413 — the status that names the fact — rather than
    //! the 400 the handler used to fold every multipart error into.
    use super::*;
    use crate::files::in_memory::{InMemoryFileRepository, InMemoryFileStorage};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const BOUNDARY: &str = "boss-attach-limit-test";

    fn app() -> Router {
        router(FilesApiState {
            repo: Arc::new(InMemoryFileRepository::new()),
            storage: Arc::new(InMemoryFileStorage::new()),
            publisher: None,
            policy: Arc::new(boss_policy_client::PermissivePolicyClient),
            bucket: "test".into(),
            #[cfg(feature = "postgres")]
            pool: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    /// A multipart body of EXACTLY `total` bytes: the two text fields,
    /// then one file part padded so the whole body lands on `total`.
    fn body_of_length(total: usize) -> Vec<u8> {
        let head = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"target_kind\"\r\n\r\njob\r\n\
             --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"target_id\"\r\n\r\njob-1\r\n\
             --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.bin\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        );
        let tail = format!("\r\n--{BOUNDARY}--\r\n");
        let fill = total - head.len() - tail.len();
        let mut out = head.into_bytes();
        out.extend(std::iter::repeat_n(b'x', fill));
        out.extend(tail.into_bytes());
        assert_eq!(out.len(), total);
        out
    }

    async fn post(total: usize) -> StatusCode {
        let req = Request::post("/api/files")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .header(
                "x-boss-user",
                r#"{"id":"agent-test","role":"platform-admin","access_tier":"operator"}"#,
            )
            .body(Body::from(body_of_length(total)))
            .unwrap();
        app().oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn a_body_of_exactly_the_limit_is_taken() {
        assert_eq!(post(UPLOAD_BODY_LIMIT_BYTES).await, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn one_byte_over_the_limit_is_refused_as_too_large() {
        assert_eq!(
            post(UPLOAD_BODY_LIMIT_BYTES + 1).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    /// The number is the one axum applied implicitly before it was
    /// declared (axum-core 0.5.6 `with_limited_body`, `DEFAULT_LIMIT`),
    /// so declaring it refused no upload that used to be taken.
    #[test]
    fn the_declared_limit_is_the_two_mebibytes_the_door_already_enforced() {
        assert_eq!(UPLOAD_BODY_LIMIT_BYTES, 2_097_152);
    }
}
