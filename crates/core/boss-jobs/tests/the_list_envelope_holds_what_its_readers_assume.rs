//! `GET /api/jobs?…` answers `{data, total, limit, offset}` — pinned
//! here in the shape its READERS take it, not the shape the handler
//! happens to write.
//!
//! WHY THIS FILE EXISTS (backlog 10eecbbc). The envelope is read by
//! shell, Rust and the web, and until this file nothing at the HTTP
//! level held it. Every one of its fields fails the same way when it
//! goes missing: as a SMALLER, WELL-FORMED, CONFIDENT answer rather
//! than an error — CLAUDE.md §Doors' "a wrong target answers instead of
//! erroring", living in a response shape rather than a hostname. The
//! readers that depend on it are listed in [`READERS`] with what each
//! one assumes, and every assertion below names them, so a server
//! change that breaks the shape fails HERE and says whom it breaks.
//!
//! `total` is the field that matters most, because it is the only one
//! that makes truncation detectable at all: three of the readers below
//! DEFAULT it when it is absent (the dispatcher's `open_jobs` to 0, the
//! web's `normalise` to the page length, `sweep-archive-branches.sh` to
//! `.data | length`), and each default turns a truncated page into the
//! whole population without a word. The server is the one place all of
//! them can be held at once, so the truncated legs are the heart of
//! this file.
//!
//! PIN, NOT COLLAPSE — DECIDED, NOT DEFAULTED (CLAUDE.md §9a). The
//! packet asked whether the three reader families should share one
//! definition. The Rust half already does, twice over and on purpose:
//! boss-cli reads every listing through `train::jobs_api::rows` (backlog
//! 7b7e0529) and the dispatcher's handlers through
//! `common::rows_or_refuse` (d4698bc2) — two crates that do not depend
//! on each other, each with one reader. The shell and the web re-derive
//! it in jq and TypeScript, and a shared definition would have to cross
//! a language boundary, which is the exact case §9a says to PIN rather
//! than collapse. So the one definition is the server's wire shape, and
//! this file is its pin. `READERS` is held by
//! [`every_named_reader_still_exists`] so the names in the failure
//! messages cannot go quietly dead.
//!
//! The model is `a_listed_packet_carries_its_steps.rs` (d0ee20f8), which
//! pinned the rows' steps the same way — as `ops-runner.sh` reads them.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

/// The readers of the list envelope, as `(file, definition, what it
/// assumes)`. The definition is spelled as it is DECLARED, so a doc
/// comment that merely mentions the name cannot keep a renamed reader
/// looking alive.
const READERS: &[(&str, &str, &str)] = &[
    (
        "crates/orchestrators/boss-cli/src/train/jobs_api.rs",
        "pub(crate) fn rows(",
        "rows under `data`, an array; any other body is refused",
    ),
    (
        "crates/orchestrators/boss-cli/src/train/jobs_api.rs",
        "pub(crate) fn list_total(",
        "`total` an unsigned integer (as_u64); absent is an error",
    ),
    (
        "crates/orchestrators/boss-cli/src/train/jobs_api.rs",
        "pub(crate) async fn list_all_pages",
        "offset = rows gathered, stop once rows >= total or a page is empty",
    ),
    (
        "crates/orchestrators/boss-dispatcher-handlers/src/handlers/common.rs",
        "pub(crate) fn rows_or_refuse",
        "`data` an array; null, an object or absence is refused",
    ),
    (
        "crates/orchestrators/boss-dispatcher-handlers/src/handlers/common.rs",
        "pub(crate) async fn open_jobs(",
        "pages limit=500 on rows gathered; a missing `total` reads as 0 and stops after ONE page",
    ),
    (
        "apps/web/src/data/paginated.ts",
        "export function normalise",
        "a missing `total` reads as data.length, so isCapped cannot see a truncation",
    ),
    (
        "apps/web/src/data/parseResponse.ts",
        "export async function fetchPagedValidated",
        "`total`/`limit`/`offset` optional, defaulted to the page",
    ),
    (
        "infra/ops/ops-runner.sh",
        "QUEUE_PAGE=1000",
        "asks for limit=1000 and trusts a numeric `total` for the queue depth",
    ),
    (
        "infra/cluster/dev-scratch-reclaim.sh",
        "jq '.total // empty'",
        "a missing `total` falls back to the row count, so a truncated page reads as whole",
    ),
    (
        "infra/forge/sweep-archive-branches.sh",
        "(.total // (.data | length)) as $total",
        "a missing `total` falls back to the row count, so a truncated page reads as whole",
    ),
];

/// The kinds the readers above actually list — measured from their
/// `?kind=` call sites on 2026-09-23 (pr-train, gate-run, ship-a-change
/// and backlog-item carry most of the CLI's; ops-request is the
/// runner's and the dispatcher's).
const READ_KINDS: &[&str] = &[
    "pr-train",
    "gate-run",
    "ship-a-change",
    "backlog-item",
    "ops-request",
];

/// Open packets seeded per kind — more than one page at `limit=2`, so
/// the truncated case is reachable for every kind.
const OPEN_PER_KIND: usize = 3;

/// The server's page ceiling, written here as the number a reader
/// meets: `ops-runner.sh` asks for exactly this many. If the handler's
/// `MAX_LIMIT` moves, this leg says so and names the runner.
const SERVER_MAX_LIMIT: u64 = 1000;

/// The default page when a reader sends no `limit`.
const SERVER_DEFAULT_LIMIT: u64 = 100;

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, d).expect("valid date")
}

fn ceo() -> User {
    User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn packet(kind: &str, n: usize, status: JobStatus) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("branch", format!("fix/{kind}-{n}")),
        title: format!("{kind} #{n}"),
        owner_id: "emp-david".into(),
        status,
        priority: Priority::Standard,
        // Distinct days so the list order is total, not a tie.
        opened_on: day(1 + n as u32),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// Every read kind with `OPEN_PER_KIND` open packets and one closed
/// one, so a `status=open` read has something to leave out of `total`.
async fn seed() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: day(23).and_hms_opt(12, 0, 0).expect("noon").and_utc(),
        simulated: true,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    for kind in READ_KINDS {
        for n in 0..OPEN_PER_KIND {
            jobs.create_job(&packet(kind, n, JobStatus::Open))
                .await
                .expect("open packet");
        }
        jobs.create_job(&packet(kind, OPEN_PER_KIND, JobStatus::Closed))
            .await
            .expect("closed packet");
    }
    router(JobsApiState::minimal(jobs, bus, publisher, policy, clock))
}

async fn list(app: &Router, query: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs?{query}"))
                .header("x-boss-user", serde_json::to_string(&ceo()).expect("user"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let body = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(status, StatusCode::OK, "GET /api/jobs?{query}: {body}");
    serde_json::from_str(&body).expect("json")
}

/// Who breaks if `field` is wrong — the failure text every assertion
/// carries, so a red names its readers rather than just its field.
fn readers_of(field: &str) -> String {
    let who: Vec<String> = READERS
        .iter()
        .filter(|(_, _, assumes)| assumes.contains(field))
        .map(|(file, def, assumes)| format!("  {file} [{def}]: {assumes}"))
        .collect();
    format!("readers of {field} this breaks:\n{}", who.join("\n"))
}

/// The four fields as every reader above takes them, or a panic naming
/// the field and its readers. `data` an array, and the three numbers
/// UNSIGNED INTEGERS — boss-cli's `list_total` is `as_u64`, and the
/// shell readers guard with `*[!0-9]*`, so a float or a string is as
/// absent to them as a missing key.
fn envelope(body: &Value, query: &str) -> (Vec<Value>, u64, u64, u64) {
    let data = body
        .get("data")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "?{query}: no `data` array in {body}\n{}",
                readers_of("`data`")
            )
        })
        .clone();
    let num = |field: &str| {
        body.get(field).and_then(Value::as_u64).unwrap_or_else(|| {
            panic!(
                "?{query}: `{field}` is not an unsigned integer in {body}\n{}",
                readers_of(&format!("`{field}`"))
            )
        })
    };
    (data, num("total"), num("limit"), num("offset"))
}

fn ids(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|r| r["id"].as_str().expect("row id").to_string())
        .collect()
}

#[tokio::test]
async fn a_whole_page_carries_the_full_count_and_echoes_its_window() {
    let app = seed().await;
    for kind in READ_KINDS {
        let q = format!("kind={kind}&status=open&limit=100");
        let (data, total, limit, offset) = envelope(&list(&app, &q).await, &q);
        assert_eq!(
            data.len(),
            OPEN_PER_KIND,
            "?{q}: every open {kind} on one page"
        );
        assert_eq!(
            total,
            data.len() as u64,
            "?{q}: a page holding everything must say `total` = its length — \
             the only way a reader can tell a whole list from a page\n{}",
            readers_of("`total`")
        );
        assert_eq!(limit, 100, "?{q}: `limit` echoes the page size asked for");
        assert_eq!(offset, 0, "?{q}: `offset` echoes where the page starts");
        assert!(
            data.iter()
                .all(|r| r["kind"] == *kind && r["status"] == "open"),
            "?{q}: the page holds only what was asked for"
        );
    }
}

/// THE CASE WHERE A WRONG READING IS SILENT. A page smaller than the
/// population must say so through `total`, on the first page, on a
/// middle page, and past the end — where `data` must still be an
/// ARRAY (empty), because both Rust paginators stop on an empty page
/// and both Rust readers refuse a missing one.
#[tokio::test]
async fn a_truncated_page_says_how_much_it_left_out() {
    let app = seed().await;
    for kind in READ_KINDS {
        let q = format!("kind={kind}&status=open&limit=2");
        let (data, total, limit, offset) = envelope(&list(&app, &q).await, &q);
        assert_eq!(data.len(), 2, "?{q}: the page is capped at its limit");
        assert_eq!(
            total,
            OPEN_PER_KIND as u64,
            "?{q}: `total` counts the whole open population, not the page — \
             every reader that compares rows to it learns it read {} of {}\n{}",
            data.len(),
            OPEN_PER_KIND,
            readers_of("`total`")
        );
        assert!(
            total > data.len() as u64,
            "?{q}: a truncated page must be detectable as one"
        );
        assert_eq!((limit, offset), (2, 0), "?{q}: the window echoed");

        let q = format!("kind={kind}&status=open&limit=2&offset=2");
        let (tail, total, limit, offset) = envelope(&list(&app, &q).await, &q);
        assert_eq!(tail.len(), OPEN_PER_KIND - 2, "?{q}: the rest");
        assert_eq!(
            total, OPEN_PER_KIND as u64,
            "?{q}: `total` does not shrink with the offset"
        );
        assert_eq!((limit, offset), (2, 2), "?{q}: the window echoed");

        let q = format!("kind={kind}&status=open&limit=2&offset=50");
        let (past, total, _, offset) = envelope(&list(&app, &q).await, &q);
        assert!(past.is_empty(), "?{q}: nothing past the end");
        assert_eq!(
            total, OPEN_PER_KIND as u64,
            "?{q}: past the end still counts"
        );
        assert_eq!(offset, 50, "?{q}: the offset echoed as asked");
    }
}

/// The readers' own walk — `list_all_pages` and `open_jobs` both
/// request `offset = rows gathered` and stop once the rows reach
/// `total` or a page comes back empty. Driven at the smallest page, it
/// must reach every open packet exactly once: `total` authoritative,
/// the order stable across pages, no row skipped or repeated.
#[tokio::test]
async fn paging_on_total_reaches_every_row_exactly_once() {
    let app = seed().await;
    let mut queries: Vec<String> = READ_KINDS
        .iter()
        .map(|k| format!("kind={k}&status=open"))
        .collect();
    // The dispatcher's `open_jobs(None)` — the whole open board.
    queries.push("status=open".into());

    for base in queries {
        let mut gathered: Vec<Value> = Vec::new();
        let last_total = loop {
            let q = format!("{base}&limit=1&offset={}", gathered.len());
            let (page, total, _, _) = envelope(&list(&app, &q).await, &q);
            if page.is_empty() {
                break total;
            }
            gathered.extend(page);
            if gathered.len() as u64 >= total {
                break total;
            }
            assert!(gathered.len() < 100, "?{base}: the walk terminates");
        };
        let want = if base == "status=open" {
            READ_KINDS.len() * OPEN_PER_KIND
        } else {
            OPEN_PER_KIND
        };
        assert_eq!(
            last_total,
            want as u64,
            "?{base}: `total` is the population the walk must reach\n{}",
            readers_of("rows gathered")
        );
        let mut seen = ids(&gathered);
        assert_eq!(seen.len(), want, "?{base}: the walk read every row");
        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            want,
            "?{base}: no row read twice — the order must hold across pages"
        );
    }
}

/// `limit` echoes the page the server APPLIED, not the one asked for:
/// a reader that sized its walk from its own request would otherwise
/// believe a 1000-row page held 5000. `ops-runner.sh` asks for exactly
/// the ceiling; a server that lowered it would leave the runner reading
/// a partial queue as its depth.
#[tokio::test]
async fn limit_echoes_the_page_the_server_applied() {
    let app = seed().await;

    let q = "kind=ops-request&status=open";
    let (_, _, limit, offset) = envelope(&list(&app, q).await, q);
    assert_eq!(
        (limit, offset),
        (SERVER_DEFAULT_LIMIT, 0),
        "?{q}: no limit asked, the default applied"
    );

    let q = format!("kind=ops-request&status=open&limit={SERVER_MAX_LIMIT}");
    let (_, _, limit, _) = envelope(&list(&app, &q).await, &q);
    assert_eq!(
        limit,
        SERVER_MAX_LIMIT,
        "?{q}: the ceiling a reader asks for is served whole\n{}",
        readers_of("limit=1000")
    );

    let q = "kind=ops-request&status=open&limit=5000";
    let (_, _, limit, _) = envelope(&list(&app, q).await, q);
    assert_eq!(
        limit, SERVER_MAX_LIMIT,
        "?{q}: an over-cap limit echoes the cap applied, not the ask"
    );
}

/// The failure messages above name readers by file and definition;
/// this is what stops those names going stale. A reader renamed or
/// moved fails HERE, and the fix is to update `READERS` — deliberately,
/// by someone who has just read what the new reader assumes. The
/// handler names this file too, so an author changing the envelope is
/// pointed at it before the test is.
#[test]
fn every_named_reader_still_exists() {
    let root = boss_testing::repo_root();
    for (file, def, _) in READERS {
        let text =
            std::fs::read_to_string(root.join(file)).unwrap_or_else(|e| panic!("read {file}: {e}"));
        assert!(
            text.contains(def),
            "{file} no longer holds `{def}` — update READERS in this file to the reader \
             that replaced it, and what it assumes"
        );
    }
    let handler = "crates/core/boss-jobs/src/http/jobs.rs";
    let text = std::fs::read_to_string(root.join(handler)).expect("handler");
    assert!(
        text.contains("the_list_envelope_holds_what_its_readers_assume"),
        "{handler} must name the test that holds the list envelope"
    );
}
