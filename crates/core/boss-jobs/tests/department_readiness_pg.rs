//! `GET /api/departments/{code}/readiness` — which of the six template
//! parts a department has, read from live registries on a real
//! database (design 3613f0af, backlog 1dffde5d).
//!
//! Two departments at different completeness, the shape the packet
//! asked for: `sales` has protocols with packets, a sensor, a tenant
//! rule and a retro; `warehouse` is a declared department with nothing
//! else. Then the refusals: a code the departments registry does not
//! hold is a 404 that NAMES the registry, and a registry that is not
//! wired is a 503, not an answer. Every part is computed from a
//! registry row — no seed file is opened.
//!
//! WHICH REGISTRY ANSWERS "what departments are there" is the subject
//! of `the_department_list_is_the_departments_table`: until backlog
//! 80a77466 it was the active `employee` Classes on the `department`
//! attribute — the drawer of values an employee's column may take —
//! and the live endpoint served nine codes against the thirteen the
//! roster holds, an overlap of five. The weekly retro rule reads this
//! endpoint at fire time, so the drawer would have opened retros for
//! four departments that do not exist and none for eight that do.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_jobs::JobsRepository;
use boss_jobs::department::http::{DepartmentsApiState, router};
use boss_jobs::department::registry::{DepartmentRegistry, PgDepartments};
use boss_jobs::department::rules::{DispatcherRules, FakeDispatcherRules};
use boss_jobs::registry::{PgWorkflows, WorkflowRegistry, WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::sensors::types::SensorInput;
use boss_jobs::sensors::{PgSensors, Sensors};
use boss_policy_client::{AccessTier, User};
use boss_testing::TestDb;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

/// A viable workflow row declaring `department`: the platform bundle's
/// department-retro steps under a tenant kind name, because a publish
/// runs the viability gate and an empty step list would not pass it.
fn declaring(kind: &str, department: &str) -> WorkflowSpec {
    let mut spec = load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == "department-retro")
        .expect("department-retro ships in the platform bundle");
    spec.kind = kind.into();
    spec.label = kind.into();
    spec.category = department.into();
    spec.metadata = json!({ "department": department });
    spec.metadata_schema = json!({});
    spec
}

async fn publish(registry: &PgWorkflows, spec: WorkflowSpec) {
    let actor = ActorId::Automation("test".into());
    let now = chrono::Utc::now();
    let kind = spec.kind.clone();
    registry
        .create_draft(spec, &actor, now)
        .await
        .expect("draft");
    registry.publish(&kind, &actor, now).await.expect("publish");
}

fn packet(n: u8, kind: &str, status: JobStatus, metadata: Value) -> Job {
    Job {
        id: JobId::from_uuid(
            Uuid::parse_str(&format!("00000000-0000-0000-0000-0000000000{n:02}")).expect("uuid"),
        ),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("custom", "algedonic"),
        title: format!("{kind} #{n}"),
        owner_id: "emp-david".into(),
        status,
        priority: Priority::Standard,
        opened_on: day(2026, 9, 1 + u32::from(n)),
        due_on: None,
        closed_on: (status == JobStatus::Closed).then(|| day(2026, 9, 10 + u32::from(n))),
        metadata,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn sensor(id: &str, opens: &str) -> SensorInput {
    SensorInput {
        id: id.into(),
        source: "site".into(),
        credential: String::new(),
        every_minutes: 0,
        opens: opens.into(),
        subject_kind: "custom".into(),
        enabled: true,
    }
}

/// The rules as `GET /api/dispatcher/rules` serves them: a tenant rule
/// opening a sales kind, a product rule opening something else.
fn rules() -> Arc<dyn DispatcherRules> {
    Arc::new(FakeDispatcherRules(vec![
        json!({ "name": "follow-up-a-prospect", "version": 1, "source": "tenant:algedonic",
                "schedule": { "cadence": "daily" },
                "do": [{ "handler": "jobs.spawn", "args": { "kind": "\"receive-an-inquiry\"" } }] }),
        json!({ "name": "maintenance-sweep-disk-daily", "version": 2, "source": "product",
                "do": [{ "handler": "jobs.spawn", "args": { "kind": "\"maintenance-sweep\"" } }] }),
    ]))
}

struct Fixture {
    app: Router,
    db: TestDb,
}

/// Sales complete but for the probe; warehouse declared and empty.
/// `wired` false leaves the departments registry unconfigured, the
/// 503 case.
async fn fixture(wired: bool, rules: Option<Arc<dyn DispatcherRules>>) -> Fixture {
    let db = TestDb::new().await;
    let jobs = Arc::new(boss_jobs::PgJobs::new(db.pool.clone()));
    let kinds = PgWorkflows::new(db.pool.clone());
    let sensors = PgSensors::new(db.pool.clone());

    publish(&kinds, declaring("receive-an-inquiry", "sales")).await;
    publish(&kinds, declaring("receive-a-sponsorship", "sales")).await;
    // The retro kind itself declares no department: the packet does.
    publish(&kinds, declaring("department-retro", "platform")).await;

    for j in [
        packet(
            1,
            "receive-an-inquiry",
            JobStatus::Closed,
            json!({ "outcome": "answered" }),
        ),
        packet(2, "receive-an-inquiry", JobStatus::Open, json!({})),
        packet(
            3,
            "department-retro",
            JobStatus::Open,
            json!({ "department": "sales", "opened_at": "2026-09-14T00:00:03+00:00" }),
        ),
        // Another department's retro must not read as sales'.
        packet(
            4,
            "department-retro",
            JobStatus::Closed,
            json!({ "department": "marketing" }),
        ),
    ] {
        jobs.create_job(&j).await.expect("seed packet");
    }
    sensors
        .publish("algedonic", &[sensor("inbox", "receive-an-inquiry")])
        .await
        .expect("sensor");

    let state = DepartmentsApiState {
        departments: wired
            .then(|| Arc::new(PgDepartments::new(db.pool.clone())) as Arc<dyn DepartmentRegistry>),
        kinds: Some(Arc::new(kinds) as Arc<dyn WorkflowRegistry>),
        jobs: jobs as Arc<dyn JobsRepository>,
        sensors: Some(Arc::new(sensors) as Arc<dyn Sensors>),
        rules,
    };
    Fixture {
        app: router(state),
        db,
    }
}

fn operator() -> User {
    User {
        id: "automation:test".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

async fn get(app: &Router, path: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header(
                    "x-boss-user",
                    serde_json::to_string(&operator()).expect("user"),
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn readiness(app: &Router, code: &str) -> Value {
    let (status, body) = get(app, &format!("/api/departments/{code}/readiness")).await;
    assert_eq!(status, StatusCode::OK, "{code}: {body}");
    serde_json::from_str(&body).expect("json")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filled_out_department_reads_five_of_six_with_the_probe_undetermined() {
    let f = fixture(true, Some(rules())).await;
    let v = readiness(&f.app, "sales").await;
    assert_eq!(v["department"]["code"], "sales");
    assert_eq!(v["department"]["display_name"], "Sales");
    assert_eq!(
        v["department"]["function"], "revenue",
        "the function is Class data on the department kind"
    );

    assert_eq!(v["surfaces"]["has"], true);
    assert_eq!(v["surfaces"]["jobs_view"], "/api/jobs?department=sales");

    // Protocols: both declaring kinds, sorted, with their packets.
    assert_eq!(v["protocols"]["has"], true);
    let kinds = v["protocols"]["kinds"].as_array().expect("kinds");
    assert_eq!(
        kinds
            .iter()
            .map(|k| k["kind"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["receive-a-sponsorship", "receive-an-inquiry"]
    );
    let inquiry = &kinds[1];
    assert_eq!(inquiry["packets"], 2);
    assert_eq!(inquiry["open"], 1);
    assert_eq!(inquiry["newest_terminal"]["outcome"], "answered");
    assert_eq!(inquiry["newest_terminal"]["closed_on"], "2026-09-11");
    assert_eq!(inquiry["version"], 1);
    let sponsorship = &kinds[0];
    assert_eq!(sponsorship["packets"], 0);
    assert_eq!(
        sponsorship["newest_terminal"],
        Value::Null,
        "no packet has closed"
    );

    // Sensors: the inbox opens an inquiry.
    assert_eq!(v["sensors"]["has"], true);
    assert_eq!(v["sensors"]["sensors"][0]["id"], "inbox");
    assert_eq!(v["sensors"]["sensors"][0]["opens"], "receive-an-inquiry");

    // Rules: the tenant's follow-up, with its source; the product's
    // disk sweep opens no sales kind and is not listed.
    assert_eq!(v["rules"]["has"], true);
    let rules = v["rules"]["rules"].as_array().expect("rules");
    assert_eq!(rules.len(), 1, "{rules:?}");
    assert_eq!(rules[0]["name"], "follow-up-a-prospect");
    assert_eq!(rules[0]["source"], "tenant:algedonic");
    assert_eq!(rules[0]["opens"], json!(["receive-an-inquiry"]));

    // Retro: sales' own, not marketing's.
    assert_eq!(v["retro"]["has"], true);
    assert_eq!(v["retro"]["newest"]["status"], "open");
    assert_eq!(v["retro"]["newest"]["opened_on"], "2026-09-04");
    assert_eq!(
        v["retro"]["newest"]["opened_at"],
        "2026-09-14T00:00:03+00:00"
    );

    // Probes: undetermined, with the reason — never false.
    assert_eq!(v["probes"]["has"], Value::Null);
    assert!(
        v["probes"]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("ship-a-change")),
        "{}",
        v["probes"]
    );

    assert_eq!(v["have"], 5);
    assert_eq!(v["of"], 6);
    assert_eq!(v["undetermined"], json!(["probes"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_but_empty_department_reads_one_of_six() {
    let f = fixture(true, Some(rules())).await;
    // `warehouse` is a department the EMPLOYEE drawer never held, so
    // this read answering at all is the fix: before 80a77466 it was a
    // 404 and the weekly retro rule would never have opened its retro.
    let v = readiness(&f.app, "warehouse").await;
    assert_eq!(
        v["surfaces"]["has"], true,
        "the jobs view exists for every department"
    );
    assert_eq!(v["protocols"]["has"], false);
    assert_eq!(v["protocols"]["kinds"], json!([]));
    assert_eq!(v["sensors"]["has"], false);
    assert_eq!(v["rules"]["has"], false);
    assert_eq!(v["retro"]["has"], false);
    assert_eq!(v["retro"]["newest"], Value::Null);
    assert_eq!(v["probes"]["has"], Value::Null);
    assert_eq!(v["have"], 1);
    assert_eq!(v["undetermined"], json!(["probes"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_code_is_a_404_that_names_the_departments_registry() {
    let f = fixture(true, Some(rules())).await;
    // `engineering` is one of the four codes the employee drawer served
    // that no department, route or packet has (measured live
    // 2026-09-19, backlog 80a77466). It must read as the typo it is.
    let (status, body) = get(&f.app, "/api/departments/engineering/readiness").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("departments registry"), "{body}");
    assert!(body.contains("`departments` table"), "{body}");
    assert!(
        body.contains("`engineering`"),
        "an employee Class is not a department: {body}"
    );
}

/// THE DEFECT THIS CAR FIXED, pinned: the list is the `departments`
/// table and nothing else. The assertions are derived from the table
/// in the same test rather than retyped, so this cannot become a
/// second copy of the roster (CLAUDE.md §9a) — and one retired row
/// proves the filter, since a retired department must not draw a
/// retro.
#[tokio::test(flavor = "multi_thread")]
async fn the_department_list_is_the_departments_table() {
    let f = fixture(true, Some(rules())).await;
    sqlx::query("UPDATE departments SET retired_at = NOW() WHERE id = 'maintenance'")
        .execute(&f.db.pool)
        .await
        .expect("retire a department");
    let expected: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM departments WHERE retired_at IS NULL ORDER BY sort_order, id",
    )
    .fetch_all(&f.db.pool)
    .await
    .expect("the roster");

    let (status, body) = get(&f.app, "/api/departments").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    let served: Vec<String> = v["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|d| d["code"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        served, expected,
        "the table's un-retired rows, in its order"
    );
    assert_eq!(v["total"], served.len());
    assert!(
        !served.iter().any(|c| c == "maintenance"),
        "a retired department draws no retro: {served:?}"
    );
    // The eight the employee drawer was missing on 2026-09-19 are here,
    // and the four it served that are not departments are not.
    for real in [
        "distribution",
        "executive",
        "people",
        "production",
        "qa",
        "service",
        "warehouse",
    ] {
        assert!(served.iter().any(|c| c == real), "{real}: {served:?}");
    }
    for drawer_only in ["engineering", "hosting", "product", "operations"] {
        assert!(
            !served.iter().any(|c| c == drawer_only),
            "{drawer_only} is an employee Class, not a department: {served:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_departments_registry_the_question_is_refused() {
    let f = fixture(false, Some(rules())).await;
    let (status, body) = get(&f.app, "/api/departments/sales/readiness").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("departments registry"), "{body}");
    let (status, _) = get(&f.app, "/api/departments").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_part_whose_registry_is_not_wired_is_undetermined_not_absent() {
    let f = fixture(true, None).await;
    let v = readiness(&f.app, "sales").await;
    assert_eq!(v["rules"]["has"], Value::Null);
    assert!(
        v["rules"]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("dispatcher")),
        "{}",
        v["rules"]
    );
    assert_eq!(v["undetermined"], json!(["rules", "probes"]));
    assert_eq!(v["have"], 4, "the other parts still count");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_tier_caller_is_refused() {
    let f = fixture(true, Some(rules())).await;
    let user = User {
        access_tier: AccessTier::User,
        ..operator()
    };
    let resp = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/departments/sales/readiness")
                .header("x-boss-user", serde_json::to_string(&user).expect("user"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
