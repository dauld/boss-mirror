//! THE FRESH INSTANCE'S SEED PATH, RUN ON AN EMPTY DATABASE (backlog
//! 0d2d7daa, 2026-09-16; design e652c7c6 Option 3 — David: "make sure
//! we have our seeding setup properly for our new, real instance").
//!
//! What runs here is what the pod runs: the schema on an empty TestDb,
//! the REAL people router (`PgPeople`), the REAL locations router
//! (`PgLocations`), the REAL agents router (`PgAgents`, over the
//! migration-registered `agent-claude`) and the REAL policy router
//! (`PgPolicy`, after the same default-rule reconcile boss-policy-api
//! runs at boot) on one ephemeral port, then the shipped `boss`
//! binary's `tenant publish
//! --gateway` against a verbatim copy of the real company's tenant
//! directory (tests/fixtures/tenant-algedonic, its HEAD 20f3a9e — no
//! secrets: the manifest says so and a grep agrees), then the
//! operator-baseline seed's library entry
//! (`boss_people::operator_baseline::seed`, the binary's whole body)
//! with BOSS_BOOTSTRAP_ADMIN_EMAIL set to the founder's address, in
//! the order tenant-launch.sh now runs them for a tenant with no
//! engine.
//!
//! Then the database is read back: ONE row holds the founder's email
//! and it is `emp-david`, not `emp-bootstrap-admin`; `emp-david` is
//! the deployment's platform-admin (the tenant's decision, 2026-09-16:
//! "only David Auld as the Platform Admin" — the role core's default
//! rules already grant everything to, so the file grants nothing and
//! the policy engine still allows him); `emp-audit` landed.
//!
//! WHAT THIS PROOF STOPS SHORT OF, BY NAME. The four other doors —
//! classes, calendars, the company Subject, and the Workflows — answer
//! from a stub here: the jobs API's design-Job walk needs the whole
//! platform bundle and is proven by boss-jobs' own tests and the
//! stub-level tests in src/tenant_publish.rs; the stub answers the
//! workflow kind as already operator-published so the door SKIPS, the
//! way a second publish does. `receive-a-sponsorship active` is
//! therefore NOT asserted here (follow-up: a jobs-router harness).
//!
//! WHAT IT FOUND, AND WHAT CLOSED IT. On 2026-09-16 the real tenant's
//! founder declared `location = "loc-algedonic-hq"` and no door seeded
//! a tenant's locations — `employees.location` is a foreign key into
//! a registry only the schema filled — so run verbatim the publish
//! was REFUSED at employees.json naming the location (the first case
//! pinned that refusal loud; it used to be a 409 swallowed as "already
//! there"), and the tenant worked around it by sitting the founder at
//! the platform's `loc-hq`. Backlog 1ec8312a (2026-09-17) added the
//! door: `POST /api/locations/batch`, sent by `boss tenant publish`
//! BEFORE the roster. The first case now proves the verbatim tenant
//! lands and that the location is there before the employee — the
//! foreign key is the machine's own proof of the order. Backlog
//! f56155f0 (same day) added the agents seed: the same case proves the
//! tenant's `agent-claude` meets the migration's row, is KEPT as the
//! platform registered it, and the publish line names the field the
//! declaration differs on (`display_name`) instead of a silent "already
//! there" — the fixture's agents.toml is the contract's shape, which is
//! the real tenant's @ 20f3a9e minus the `role` and `department` the
//! registry cannot hold (check refuses those by name).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use boss_policy_client::{PermissivePolicyClient, PolicyClient};
use boss_testing::{TestDb, repo_root, scratch_dir};
use serde_json::{Value, json};
use sqlx::PgPool;

const FIXTURE: &str = "crates/orchestrators/boss-cli/tests/fixtures/tenant-algedonic";
const OPERATOR_HIRES: &str = "infra/operator-baseline/operator_hires.toml";
const FOUNDER_EMAIL: &str = "david@algedonic.dev";
const FOUNDER_ID: &str = "emp-david";

/// The real people, locations and policy routers over `pool`, plus a
/// stub for the four doors outside this proof, on one ephemeral port
/// — the shape `--gateway` expects (every /api prefix through one
/// base).
async fn serve(pool: PgPool) -> String {
    let people = Arc::new(boss_people::PgPeople::new(pool.clone()));
    let policy: Arc<dyn PolicyClient> = Arc::new(PermissivePolicyClient);
    let people_router = boss_people::http::router(boss_people::http::PeopleApiState {
        people,
        publisher: None,
        policy: Some(policy),
        subject_kinds: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    });
    // The locations door (backlog 1ec8312a): the real router over the
    // same pool, so `employees.location` FKs into rows the tenant
    // itself declared.
    let locations_router = boss_locations::http::router(boss_locations::http::LocationsApiState {
        locations: Arc::new(boss_locations::PgLocations::new(pool.clone())),
    });
    // The agents door (backlog f56155f0): the real registry over the
    // same pool — the schema's migration already registered
    // agent-claude, so the tenant's declaration meets a kept row.
    let agents_router = boss_jobs::agents::http::router(boss_jobs::agents::http::AgentsApiState {
        registry: Arc::new(boss_jobs::agents::PgAgents::new(pool.clone())),
    });
    // What boss-policy-api does before it binds: reconcile the code
    // defaults (platform-admin / audit-readonly / smoke-tester / guest)
    // into policy_rules. On a fresh database this is where every
    // platform-admin grant comes from.
    let repo = Arc::new(boss_policy::PgPolicy::new(pool));
    boss_policy::PolicyRepository::bootstrap_reconcile(&*repo, &boss_policy::default_rules())
        .await
        .expect("the default rules reconcile into an empty policy_rules");
    let engine = Arc::new(boss_policy::PolicyEngine::new(repo.clone()));
    let policy_router =
        boss_policy::http::router(boss_policy::http::PolicyApiState { repo, engine });
    let outside_this_proof = Router::new()
        .route(
            "/api/classes/batch",
            post(|Json(rows): Json<Vec<Value>>| async move {
                Json(json!({"received": rows.len(), "inserted": rows.len()}))
            }),
        )
        .route(
            "/api/calendar/business-calendars/batch",
            post(|| async { StatusCode::OK }),
        )
        .route(
            "/api/subjects/company",
            post(|| async { StatusCode::CREATED }),
        )
        // The sensors batch (14c9b2ad) — the fixture declares one.
        .route(
            "/api/sensors/batch",
            post(|Json(b): Json<Value>| async move {
                let n = b["sensors"].as_array().map_or(0, Vec::len);
                Json(json!({"received": n, "inserted": n}))
            }),
        )
        // Already operator-published → the workflow door skips (its own
        // idempotence rule), so no design-Job walk is attempted here.
        .route(
            "/api/workflows/{kind}",
            get(|| async { Json(json!({"authoring_job_id": "outside-this-proof"})) }),
        );
    let app = people_router
        .merge(locations_router)
        .merge(agents_router)
        .merge(policy_router)
        .merge(outside_this_proof);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// A scratch copy of the fixture, so a case can perturb one field.
fn tenant_copy(case: &str) -> PathBuf {
    let dst = scratch_dir(&format!("fresh-instance-seed-{case}")).join("tenant");
    copy_tree(&repo_root().join(FIXTURE), &dst);
    dst
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The shipped binary: `boss tenant publish <dir> --gateway <base>`.
fn boss_tenant_publish(dir: &Path, base: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_boss"))
        .args(["tenant", "publish"])
        .arg(dir)
        .args(["--gateway", base])
        .output()
        .expect("boss runs");
    (
        out.status.success(),
        format!(
            "--- stdout\n{}--- stderr\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The operator baseline, as the launcher runs it after the tenant:
/// BOSS_BOOTSTRAP_ADMIN_EMAIL names the founder's address.
async fn operator_baseline(
    base: String,
) -> anyhow::Result<boss_people::operator_baseline::Summary> {
    // SAFETY: a process-wide env var, set to the one value every case
    // in this file wants, before the only reader (the seed) runs.
    unsafe { std::env::set_var("BOSS_BOOTSTRAP_ADMIN_EMAIL", FOUNDER_EMAIL) };
    let seeds = repo_root().join(OPERATOR_HIRES);
    tokio::task::spawn_blocking(move || boss_people::operator_baseline::seed(&base, &seeds))
        .await
        .unwrap()
}

/// `(id, role)` of every employee holding `email`, case-insensitively
/// — the schema's own uniqueness key.
async fn holders_of(pool: &PgPool, email: &str) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT id, role FROM employees WHERE LOWER(email) = LOWER($1) ORDER BY id")
        .bind(email)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The tenant's own site lands BEFORE the employee who sits at it
/// (backlog 1ec8312a). The order is proved two ways: the publish
/// prints the locations line above the employees line, and the
/// database holds the employee's FK into a row the schema never
/// seeded — Postgres would have refused the row otherwise, which is
/// exactly what it did on 2026-09-16.
#[tokio::test(flavor = "multi_thread")]
async fn the_real_tenant_verbatim_lands_its_location_before_its_founder() {
    let db = TestDb::new().await;
    let base = serve(db.pool.clone()).await;
    let dir = tenant_copy("verbatim");
    let before: Option<String> =
        sqlx::query_scalar("SELECT id FROM locations WHERE id = 'loc-algedonic-hq'")
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    assert_eq!(before, None, "the schema does not seed the tenant's site");

    let (ok, out) = boss_tenant_publish(&dir, &base);
    assert!(ok, "the verbatim tenant publishes:\n{out}");
    let line_of = |needle: &str| {
        out.lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} has a line:\n{out}"))
    };
    assert!(
        line_of("seeds/locations.toml") < line_of("seeds/employees.json"),
        "the sites are sent before the roster:\n{out}"
    );
    let locations_line = out
        .lines()
        .find(|l| l.contains("seeds/locations.toml"))
        .unwrap();
    assert!(
        locations_line.contains("POST /api/locations/batch")
            && locations_line.contains("received 1, inserted 1"),
        "{locations_line}"
    );

    let site: Option<(String, String, String)> = sqlx::query_as(
        "SELECT name, kind, timezone FROM locations WHERE id = 'loc-algedonic-hq' AND retired_at IS NULL",
    )
    .fetch_optional(&db.pool)
    .await
    .unwrap();
    let (name, kind, timezone) = site.expect("the tenant's HQ landed");
    assert_eq!(name, "Algedonic, LLC — HQ (remote)");
    assert_eq!(kind, "office");
    assert_eq!(timezone, "America/Los_Angeles");
    let at: Option<String> = sqlx::query_scalar("SELECT location FROM employees WHERE id = $1")
        .bind(FOUNDER_ID)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        at.as_deref(),
        Some("loc-algedonic-hq"),
        "the founder sits at the tenant's own site, not the platform's loc-hq"
    );

    // The agent (backlog f56155f0): the migration registered
    // agent-claude under the platform's own display name; the tenant
    // declares it under its own. Insert-if-absent keeps the migration's
    // row, the publish line NAMES the field that differs, the declared
    // alias resolves, and the agents door went before the Workflows.
    assert!(
        line_of("seeds/agents.toml") < line_of("seeds/workflows.toml"),
        "the agents are sent before the Workflows:\n{out}"
    );
    let agents_line = out
        .lines()
        .find(|l| l.contains("seeds/agents.toml"))
        .unwrap();
    assert!(
        agents_line.contains("POST /api/agents/batch")
            && agents_line.contains("received 1, inserted 0")
            && agents_line.contains("agent-claude (display_name differs)"),
        "{agents_line}"
    );
    let agent: Option<(String, String)> =
        sqlx::query_as("SELECT display_name, default_model FROM agents WHERE id = 'agent-claude'")
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    let (display_name, default_model) = agent.expect("the agent row is there");
    assert_eq!(
        display_name, "Claude (Claude Code sessions on the dev pod)",
        "kept as the migration registered it, never overwritten"
    );
    assert_eq!(
        default_model, "opus-5[1m]",
        "and the tenant's model matches it"
    );
    let alias: Option<String> = sqlx::query_scalar(
        "SELECT actor_id FROM actor_aliases WHERE alias = 'claude@algedonic.dev'",
    )
    .fetch_optional(&db.pool)
    .await
    .unwrap();
    assert_eq!(alias.as_deref(), Some("agent-claude"));

    // A second publish inserts nothing and changes nothing.
    let (ok, out) = boss_tenant_publish(&dir, &base);
    assert!(ok, "the second publish:\n{out}");
    let locations_line = out
        .lines()
        .find(|l| l.contains("seeds/locations.toml"))
        .unwrap();
    assert!(
        locations_line.contains("received 1, inserted 0"),
        "{locations_line}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tenant_then_baseline_leaves_one_founder_row_and_no_bootstrap_admin() {
    let db = TestDb::new().await;
    let base = serve(db.pool.clone()).await;
    let dir = tenant_copy("one-founder");

    // 1. The tenant, verbatim, through the shipped verb.
    let (ok, out) = boss_tenant_publish(&dir, &base);
    assert!(ok, "boss tenant publish:\n{out}");
    assert!(
        out.contains("1 posted, 0 already there"),
        "the founder was POSTed, not skipped:\n{out}"
    );
    let after_tenant = holders_of(&db.pool, FOUNDER_EMAIL).await;
    assert_eq!(after_tenant.len(), 1, "one row after the tenant");
    assert_eq!(after_tenant[0].0, FOUNDER_ID);

    // 2. The operator baseline, after — the launcher's order for a
    //    tenant with no engine.
    let summary = operator_baseline(base.clone())
        .await
        .expect("baseline seeds");
    match &summary.injection {
        boss_people::operator_baseline::Injection::HeldBy { id, role, .. } => {
            assert_eq!(id, FOUNDER_ID, "the roster named the founder");
            assert_eq!(
                role.as_deref(),
                Some("platform-admin"),
                "and the founder IS the platform-admin (the tenant's decision, 2026-09-16)"
            );
        }
        other => panic!(
            "the injection asked the roster and was told the founder holds the email; got {other:?}"
        ),
    }
    assert_eq!(summary.inserted, 1, "emp-audit landed");

    // 3. Read the database back: the claim, not the log.
    let holders = holders_of(&db.pool, FOUNDER_EMAIL).await;
    assert_eq!(
        holders.len(),
        1,
        "exactly ONE employee holds {FOUNDER_EMAIL}: {holders:?}"
    );
    assert_eq!(holders[0].0, FOUNDER_ID, "and it is the tenant's founder");
    let bootstrap: Option<String> =
        sqlx::query_scalar("SELECT id FROM employees WHERE id = 'emp-bootstrap-admin'")
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    assert_eq!(bootstrap, None, "no second bootstrap admin was injected");
    let audit: Option<String> =
        sqlx::query_scalar("SELECT id FROM employees WHERE id = 'emp-audit'")
            .fetch_optional(&db.pool)
            .await
            .unwrap();
    assert_eq!(audit.as_deref(), Some("emp-audit"));
    // Q7 owner resolution keys on the platform-admin ROLE for
    // automation-owned platform Jobs (41 platform workflows say
    // owner_role = platform-admin): the fresh instance has exactly one
    // active holder, the founder.
    let admins: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM employees WHERE role = 'platform-admin' AND status = 'active'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(admins, vec![FOUNDER_ID.to_string()]);
    // The founder's authority comes from core's default rules, not
    // the tenant file (which grants nothing, by decision): the policy
    // engine, over the reconciled table, allows him to create a Job.
    let decision: Value = reqwest::Client::new()
        .post(format!("{base}/api/policy/check"))
        .json(&json!({
            "user": {"id": FOUNDER_ID, "role": "platform-admin", "access_tier": "operator"},
            "action": "create",
            "resource": "job",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        decision["decision"], "allow",
        "platform-admin creates a Job on the fresh instance: {decision}"
    );
    let tenant_rules: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM policy_rules WHERE updated_by <> 'bootstrap'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        tenant_rules, 0,
        "the tenant's policy file grants nothing (its decision); every rule is core's"
    );
}
