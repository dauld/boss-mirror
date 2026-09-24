//! `boss attach` — an actor attaches a file to a packet, or to one of
//! its steps, and the verb proves it landed by reading it back.
//!
//! WHY THIS IS A VERB (backlog 7610dd2f, David 2026-09-23: "you need to
//! be able to attach files as an actor"). The file store — file_refs,
//! content-addressed, per packet — was switched on by car 1 of backlog
//! 6280be03, and the only way to put a file on a packet was the SPA's
//! upload button: a browser session. An agent, and a human in a shell,
//! had no door. The content-api's `POST /api/files` was not on the LAN
//! machine door either, so even a hand-built multipart curl could only
//! have been aimed at a port nothing routed to.
//!
//! THREE CHOICES, EACH A LESSON THIS CRATE ALREADY PAID FOR.
//!
//! - NO SHELL BETWEEN THE BYTES AND THE RECORD. The file goes from disk
//!   to the socket as a streamed multipart part (`reqwest`'s
//!   `Part::file`): no `cat`, no base64 in a JSON body, no argv. A byte
//!   a shell re-encodes is a byte the record's sha256 no longer vouches
//!   for.
//! - CONFIRMATION OVER STATUS CODES (the rule `boss job patch` states).
//!   A 201 is the store's claim; the verb then GETs the attachment by
//!   its id and hashes what comes back. It reports success only when
//!   the file it read from disk, the row's `sha256` and the bytes read
//!   back are one digest. A store that is switched OFF answers every
//!   `/api/files` path 200 with an `{"kind":"unconfigured"}` envelope
//!   (boss_content_api.rs) — a status code that says yes about nothing
//!   — and that answer is refused here by name.
//! - THE LIMIT IS STATED, AND IT IS THE DOOR'S OWN NUMBER. The store
//!   takes a multipart body of at most
//!   `boss_content::files::http::UPLOAD_BODY_LIMIT_BYTES` — measured
//!   from its source on 2026-09-23 as axum's implicit 2 MiB, and now
//!   declared there and applied to the route. The help and the refusal
//!   read that constant; neither carries a copy of the number.
//!
//! WHO IT SIGNS AS. `BOSS_ACTOR`, like every write verb (backlog
//! 5083d6f5): the row's `uploaded_by` is the actor running the command,
//! and an unnamed write is refused before the socket. The content-api
//! checks the upload as `Update` on the job or step through
//! boss-policy.
//!
//! WHICH SERVICE. The file store is boss-content-api, on its own
//! `boss_ports` port of the machine door (`sor-ports.env`, the door's
//! manifest and `sor-routes.sh`, pinned together by
//! `the_machine_door_carries_every_read_surface.rs`). Its address is
//! `BOSS_JOBS_URL`'s host on that port — the same rule
//! `Bases::on_door` applies to every registry, from the one function
//! both call.

use std::path::Path;

use anyhow::{Context, Result, bail};
use boss_content::files::http::UPLOAD_BODY_LIMIT_BYTES;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::identity;

/// Bytes of multipart framing the verb reserves inside the limit: the
/// boundary lines, three part headers, the two target fields and the
/// file's name. The limit is on the WHOLE body, so a file of exactly
/// the limit could never fit; a file within this allowance of it is
/// refused before it is sent, rather than streamed to a 413. A file
/// name long enough to exceed the allowance still meets the door's own
/// 413, which is refused with the same numbers.
pub(crate) const FRAMING_ALLOWANCE_BYTES: u64 = 1024;

/// The largest FILE this verb sends: the store's body limit, less the
/// framing allowance.
pub(crate) fn largest_file() -> u64 {
    UPLOAD_BODY_LIMIT_BYTES as u64 - FRAMING_ALLOWANCE_BYTES
}

/// `2097152` as `2,097,152` — a limit a reader can check by eye.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The limit, in the words the help and the refusal share.
pub(crate) fn limit_sentence() -> String {
    format!(
        "The file store takes a multipart body of at most {} bytes — the file plus \
         its framing — the limit boss-content-api applies to POST /api/files \
         (boss_content::files::http::UPLOAD_BODY_LIMIT_BYTES). This verb sends a file of at \
         most {} bytes, reserving {} for the framing.",
        grouped(UPLOAD_BODY_LIMIT_BYTES as u64),
        grouped(largest_file()),
        grouped(FRAMING_ALLOWANCE_BYTES),
    )
}

/// The long help's closing section: the limit, and what "attached"
/// means when the verb says it.
pub(crate) fn limit_help() -> String {
    format!(
        "{}\n\nIt reports success only after reading the attachment back by its id and \
         finding the bytes on disk, the row's sha256 and the bytes read back to be one \
         digest. It signs as BOSS_ACTOR (or the actor file), and an unnamed write is \
         refused before anything is sent.",
        limit_sentence()
    )
}

/// Per-module verb (84f9fbc0): flattened into `Commands` by `main.rs`.
#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Attach a file to a packet, or to one of its steps, and confirm it
    /// by reading it back (backlog 7610dd2f).
    ///
    /// The file is streamed from disk as a multipart upload to the file
    /// store (boss-content-api, POST /api/files) on the machine door —
    /// BOSS_JOBS_URL's host, the content service's port — signed as the
    /// actor running the command.
    #[command(after_long_help = limit_help())]
    Attach {
        /// The packet: its full uuid, 8+ characters of its id, or its
        /// branch.
        packet: String,
        /// The file to attach. Its name on disk is the attachment's
        /// name.
        file: std::path::PathBuf,
        /// Attach to this step of the packet instead of the packet
        /// itself: the step's name in its Workflow (`spec_slug`, e.g.
        /// `build`) or its id.
        #[arg(long)]
        step: Option<String>,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Attach { packet, file, step } => run(&packet, &file, step.as_deref()).await,
    }
}

/// What a file is attached TO: the content-api's `target_kind` and
/// `target_id`, and how to name it to a reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) kind: &'static str,
    pub(crate) id: String,
    pub(crate) label: String,
}

/// The target a packet read resolves to: the packet itself, or the one
/// step `step` names by `spec_slug` or by id. Pure, so the refusals are
/// pinned without a socket.
pub(crate) fn target_for(job: &Value, step: Option<&str>) -> Result<Target> {
    let job_id = crate::envelope::job_id(job)
        .context("the packet read carries no id")?
        .to_string();
    let short = job_id.get(..8).unwrap_or(&job_id).to_string();
    let Some(wanted) = step else {
        return Ok(Target {
            kind: "job",
            id: job_id,
            label: format!("packet {short}"),
        });
    };
    let steps = crate::envelope::steps(job);
    let field = |s: &Value, k: &str| s.get(k).and_then(Value::as_str).map(str::to_string);
    let matches: Vec<&Value> = steps
        .iter()
        .copied()
        .filter(|s| {
            field(s, "spec_slug").as_deref() == Some(wanted)
                || field(s, "id").as_deref() == Some(wanted)
        })
        .collect();
    match matches.as_slice() {
        [one] => {
            let id = field(one, "id").context("the matched step carries no id")?;
            let slug = field(one, "spec_slug").unwrap_or_else(|| id.clone());
            Ok(Target {
                kind: "step",
                id,
                label: format!("step `{slug}` of packet {short}"),
            })
        }
        [] => {
            let names: Vec<String> = steps.iter().filter_map(|s| field(s, "spec_slug")).collect();
            bail!(
                "packet {short} has no step {wanted:?}. Its steps: {}. The file store does not \
                 check that a target exists, so an attachment to a step the packet does not \
                 carry would be taken and never seen — refusing instead.",
                names.join(", ")
            )
        }
        many => bail!(
            "{} steps of packet {short} answer to {wanted:?} — name one by its id",
            many.len()
        ),
    }
}

/// What the store recorded, as the verb confirmed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attached {
    pub(crate) file_id: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) filename: String,
    pub(crate) uploaded_by: String,
}

/// Attach `path` to `target` on the store at `content_base`, and read it
/// back. The signing decision is made by the caller (the seam the wire
/// tests go through, as `gate::api_at_signed` is): a refused signature
/// never reaches the socket.
pub(crate) async fn attach_at(
    http: &reqwest::Client,
    content_base: &str,
    target: &Target,
    path: &Path,
    signature: identity::Signature,
) -> Result<Attached> {
    let signer = identity::apply(signature)?;
    let size = file_that_fits(path)?;
    let on_disk = sha256_of_file(path).await?;
    let base = content_base.trim_end_matches('/');

    // THE UPLOAD. `Part::file` opens the file and streams it into the
    // body with its length declared; its name on disk is the part's
    // filename and the row's.
    let part = reqwest::multipart::Part::file(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let form = reqwest::multipart::Form::new()
        .text("target_kind", target.kind)
        .text("target_id", target.id.clone())
        .part("file", part);
    let url = format!("{base}/api/files");
    let resp = http
        .post(&url)
        .header("x-boss-user", identity::header(&signer))
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        bail!(
            "POST {url} -> 413: the file store refused {} as too large ({}). {}",
            path.display(),
            body.trim(),
            limit_sentence()
        );
    }
    if !status.is_success() {
        bail!("POST {url} -> {status}: {}", body.trim());
    }
    let row: Value = serde_json::from_str(&body)
        .with_context(|| format!("POST {url} -> {status}, and the body is not JSON: {body}"))?;
    if row.get("kind").and_then(Value::as_str) == Some("unconfigured") {
        bail!(
            "the file store at {base} is not switched on: it answered {status} with {}. \
             Nothing was attached. boss-content-api mounts that envelope when its config has \
             no [files] table (a BOSS_FILES_ROOT naming the mounted store).",
            body.trim()
        );
    }
    if status != reqwest::StatusCode::CREATED {
        bail!("POST {url} -> {status}, not 201 Created: {}", body.trim());
    }
    let text = |k: &str| {
        row.get(k)
            .and_then(Value::as_str)
            .map(str::to_string)
            .with_context(|| format!("the store's row has no `{k}`: {body}"))
    };
    let attached = Attached {
        file_id: text("id")?,
        sha256: text("sha256")?,
        size_bytes: row
            .get("size_bytes")
            .and_then(Value::as_u64)
            .with_context(|| format!("the store's row has no `size_bytes`: {body}"))?,
        filename: text("filename")?,
        uploaded_by: text("uploaded_by")?,
    };
    if attached.sha256 != on_disk || attached.size_bytes != size {
        bail!(
            "the store recorded file {} as sha256 {} ({} bytes), but {} on disk is sha256 {} \
             ({} bytes) — the file changed while it was sent, or the store kept other bytes",
            attached.file_id,
            attached.sha256,
            attached.size_bytes,
            path.display(),
            on_disk,
            size
        );
    }

    // THE CONFIRMATION. The 201 and the row are the store's claim; the
    // bytes it serves back by id are the fact.
    let back_url = format!("{base}/api/files/{}", attached.file_id);
    let back = http
        .get(&back_url)
        .header("x-boss-user", identity::header(&signer))
        .send()
        .await
        .with_context(|| format!("GET {back_url}"))?;
    let back_status = back.status();
    let bytes = back
        .bytes()
        .await
        .with_context(|| format!("GET {back_url}: reading the body"))?;
    if !back_status.is_success() {
        bail!(
            "file {} was recorded but does not read back: GET {back_url} -> {back_status}: {}",
            attached.file_id,
            String::from_utf8_lossy(&bytes).trim()
        );
    }
    let read_back = hex::encode(Sha256::digest(&bytes));
    if read_back != on_disk {
        bail!(
            "file {} does not read back byte-identical: GET {back_url} served {} bytes with \
             sha256 {read_back}, and the file sent is sha256 {on_disk}",
            attached.file_id,
            bytes.len()
        );
    }
    Ok(attached)
}

/// The file's size, when it is a regular file the store can take —
/// else the refusal, before anything is read or sent.
fn file_that_fits(path: &Path) -> Result<u64> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    let size = meta.len();
    if size > largest_file() {
        bail!(
            "refusing to send {}: it is {} bytes. {} Nothing was sent.",
            path.display(),
            grouped(size),
            limit_sentence()
        );
    }
    Ok(size)
}

/// The file's sha256, read in chunks — never whole into memory.
async fn sha256_of_file(path: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

async fn run(packet: &str, file: &Path, step: Option<&str>) -> Result<()> {
    // Both refusals that need no network come first: an unnamed write,
    // and a file the store cannot take.
    let signature =
        identity::signature_for(&reqwest::Method::POST, "/api/files", identity::caller());
    if let identity::Signature::Refused(msg) = &signature {
        bail!("{msg}");
    }
    file_that_fits(file)?;

    let http = reqwest::Client::new();
    let jobs_base = crate::gate::resolve_jobs_base(None)?;
    let id = crate::job::fetch_and_resolve(&http, packet).await?;
    let job = crate::gate::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .with_context(|| format!("the read of packet {id} returned no body"))?;
    let target = target_for(&job, step)?;
    let content = crate::tenant_publish::service_on_door(&jobs_base, "content")?;
    let a = attach_at(&http, &content, &target, file, signature).await?;
    println!(
        "boss attach: {} ({} bytes, sha256 {}) attached to {} as file {}, uploaded by {}\n\
         confirmed: read back from {content}/api/files/{} byte-identical",
        a.filename,
        grouped(a.size_bytes),
        a.sha256,
        target.label,
        a.file_id,
        a.uploaded_by,
        a.file_id
    );
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use boss_content::files::http::{FilesApiState, router};
    use boss_content::files::{InMemoryFileRepository, InMemoryFileStorage};
    use serde_json::json;
    use std::sync::Arc;

    /// The REAL files router over in-memory adapters, on a loopback
    /// socket: the same handler, limit and multipart parser production
    /// runs, with a store the test can read.
    /// Shared with `design`'s exhibit tests, which attach through this
    /// same path (design 26a89f11's file_refs arm).
    pub(crate) async fn store() -> String {
        serve(router(FilesApiState {
            repo: Arc::new(InMemoryFileRepository::new()),
            storage: Arc::new(InMemoryFileStorage::new()),
            publisher: None,
            policy: Arc::new(boss_policy_client::PermissivePolicyClient),
            bucket: "test".into(),
            pool: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        }))
        .await
    }

    /// What a stub handler takes, even when it ignores it: axum's
    /// `Bytes` extractor reads the WHOLE request body before the handler
    /// runs, so the stub answers a request it has finished reading.
    ///
    /// WHY (backlog cef615f6, 2026-09-23). A handler that answers
    /// without reading lets the server write its response and close
    /// while `Part::file` is still streaming the file part; the client's
    /// next write meets a closed socket. Measured on this module's tests,
    /// 400 runs 12 at a time, twice: 23 and 27 failed, every one `error
    /// writing a body to connection: Broken pipe`, and only in the two
    /// stub tests — the real files router, which parses the multipart
    /// body, never lost. It was never a bind race: `serve` binds before
    /// it returns, and the kernel queues a connect on a bound socket
    /// before anything calls accept.
    type WholeRequest = axum::body::Bytes;

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn file_of(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = boss_testing::scratch_dir("boss-attach").join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn job_target() -> Target {
        Target {
            kind: "job",
            id: "7610dd2f-c626-41a5-8b21-f28b9bbc5135".into(),
            label: "packet 7610dd2f".into(),
        }
    }

    fn as_agent() -> identity::Signature {
        identity::Signature::As("agent-test".into())
    }

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    /// A port nothing listens on: a refusal that names the limit or the
    /// actor, rather than a connection error, proves nothing was sent.
    const NOWHERE: &str = "http://127.0.0.1:9";

    #[tokio::test]
    async fn a_file_attached_is_read_back_byte_identical_and_credited_to_the_actor() {
        // Not UTF-8, with a NUL and a CR LF — bytes a shell or a JSON
        // body would have re-encoded.
        let bytes: Vec<u8> = (0u8..=255).chain(b"\r\n\0end".iter().copied()).collect();
        let path = file_of("proof.bin", &bytes);
        let base = store().await;
        let a = attach_at(
            &reqwest::Client::new(),
            &base,
            &job_target(),
            &path,
            as_agent(),
        )
        .await
        .expect("attached");
        assert_eq!(a.sha256, sha(&bytes));
        assert_eq!(a.size_bytes, bytes.len() as u64);
        assert_eq!(a.filename, "proof.bin");
        assert_eq!(a.uploaded_by, "agent-test", "the row names who attached it");

        // The attachment is listed on the target it was sent to.
        let listed: Value = reqwest::Client::new()
            .get(format!(
                "{base}/api/files?target_kind=job&target_id={}",
                job_target().id
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed[0]["id"].as_str(), Some(a.file_id.as_str()));
    }

    #[tokio::test]
    async fn an_unnamed_write_is_refused_before_the_socket() {
        let path = file_of("a.txt", b"hello");
        let e = attach_at(
            &reqwest::Client::new(),
            NOWHERE,
            &job_target(),
            &path,
            identity::Signature::Refused(identity::refusal("POST", "/api/files")),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(e.contains("refusing to sign POST /api/files"), "{e}");
    }

    #[tokio::test]
    async fn a_file_past_the_limit_is_refused_before_it_is_sent_naming_the_limit() {
        let path = file_of("big.bin", &vec![b'x'; largest_file() as usize + 1]);
        let e = attach_at(
            &reqwest::Client::new(),
            NOWHERE,
            &job_target(),
            &path,
            as_agent(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(e.contains("2,097,152"), "the refusal states the limit: {e}");
        assert!(
            e.contains(&grouped(largest_file() + 1)),
            "and the size: {e}"
        );
    }

    #[tokio::test]
    async fn the_largest_file_the_verb_sends_is_taken_by_the_real_door() {
        let path = file_of("edge.bin", &vec![b'y'; largest_file() as usize]);
        let a = attach_at(
            &reqwest::Client::new(),
            &store().await,
            &job_target(),
            &path,
            as_agent(),
        )
        .await
        .expect("the allowance leaves room for the framing");
        assert_eq!(a.size_bytes, largest_file());
    }

    #[tokio::test]
    async fn a_store_that_is_switched_off_is_refused_not_reported() {
        // What boss-content-api mounts when its config has no [files]
        // table: 200 on every /api/files path, with an envelope. This
        // stub reads the request before it answers (`WholeRequest`);
        // boss_content_api.rs's own handler does not, and that race is
        // the store's to fix, not this test's to reproduce.
        let off = axum::Router::new().route(
            "/api/files",
            axum::routing::any(|_: WholeRequest| async {
                axum::Json(json!({"kind": "unconfigured", "reason": "no [files] block"}))
            }),
        );
        let path = file_of("a.txt", b"hello");
        let e = attach_at(
            &reqwest::Client::new(),
            &serve(off).await,
            &job_target(),
            &path,
            as_agent(),
        )
        .await
        .unwrap_err();
        let e = format!("{e:#}");
        assert!(e.contains("not switched on"), "{e}");
    }

    #[tokio::test]
    async fn a_201_whose_bytes_do_not_read_back_is_a_failure() {
        // A store that records the right digest and serves other bytes:
        // the status code and the row both say yes, the bytes say no.
        let bytes = b"the real bytes".to_vec();
        let digest = sha(&bytes);
        let row = json!({
            "id": "0b8f6c1e-0000-4000-8000-000000000001",
            "target": {"kind": "job", "id": job_target().id},
            "bucket": "b", "object_key": format!("sha256/{digest}"),
            "sha256": digest, "size_bytes": bytes.len(), "mime": "text/plain",
            "filename": "a.txt", "uploaded_by": "agent-test",
            "uploaded_at": "2026-09-23T00:00:00Z", "deleted_at": null
        });
        let liar = axum::Router::new()
            .route(
                "/api/files",
                axum::routing::post(move |_: WholeRequest| {
                    let row = row.clone();
                    async move { (axum::http::StatusCode::CREATED, axum::Json(row)) }
                }),
            )
            .route(
                "/api/files/{id}",
                axum::routing::get(|| async { "other bytes" }),
            );
        let path = file_of("a.txt", &bytes);
        let e = attach_at(
            &reqwest::Client::new(),
            &serve(liar).await,
            &job_target(),
            &path,
            as_agent(),
        )
        .await
        .unwrap_err();
        let e = format!("{e:#}");
        assert!(e.contains("does not read back"), "{e}");
    }

    fn packet() -> Value {
        json!({
            "id": "7610dd2f-c626-41a5-8b21-f28b9bbc5135",
            "kind": "backlog-item",
            "title": "An actor attaches a file to a packet",
            "steps": [
                {"id": "d59b30bf-6482-43a1-a62f-7d77ef947b69", "spec_slug": "triage",
                 "kind": "task", "status": "completed"},
                {"id": "f39c2f4b-96a3-4d74-853f-0c5c50e2802e", "spec_slug": "build",
                 "kind": "task", "status": "active"}
            ]
        })
    }

    #[test]
    fn no_step_named_is_the_packet_itself() {
        let t = target_for(&packet(), None).unwrap();
        assert_eq!(t.kind, "job");
        assert_eq!(t.id, "7610dd2f-c626-41a5-8b21-f28b9bbc5135");
    }

    #[test]
    fn a_step_is_named_by_its_workflow_name_or_its_id() {
        let by_name = target_for(&packet(), Some("build")).unwrap();
        assert_eq!(by_name.kind, "step");
        assert_eq!(by_name.id, "f39c2f4b-96a3-4d74-853f-0c5c50e2802e");
        assert!(by_name.label.contains("build"), "{}", by_name.label);
        let by_id = target_for(&packet(), Some("f39c2f4b-96a3-4d74-853f-0c5c50e2802e")).unwrap();
        assert_eq!(by_id, by_name);
    }

    /// The store does not check that a target exists — a well-formed
    /// upload to a step id nobody has would be taken. So a step the
    /// packet does not carry is refused here, naming the ones it does.
    #[test]
    fn a_step_the_packet_does_not_carry_is_refused_naming_the_ones_it_does() {
        let e = target_for(&packet(), Some("deploy"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("triage") && e.contains("build"), "{e}");
    }

    #[test]
    fn the_help_states_the_doors_limit_from_its_constant() {
        let h = limit_help();
        assert!(h.contains("2,097,152 bytes"), "{h}");
        assert!(h.contains("UPLOAD_BODY_LIMIT_BYTES"), "{h}");
        assert!(h.contains(&grouped(largest_file())), "{h}");
    }

    #[test]
    fn grouping_puts_a_comma_every_three_digits() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(2_097_152), "2,097,152");
    }
}
