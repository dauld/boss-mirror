//! The eviction's SQL, RUN against the real schema — the half
//! example_reference_rows_sh.rs cannot pin without a database
//! (backlog 718ac982; design e2580840 car 3).
//!
//! A TestDb IS a fresh instance the moment migrate.sh has run: every
//! migration applied, no tenant published — so it holds exactly the
//! reference rows 01-registries.sql, 40-ledger.sql and the keg
//! migration seed, and nothing referencing them. That is the state
//! boss-init's first-start step sees. Two things are pinned here:
//!
//!   * WHAT THE PLATFORM KEEPS IS EXACTLY WHAT THE PLATFORM NAMES. After
//!     an eviction on the bare schema, the rows left in the taxonomies
//!     the migrations seeded for the examples (employee roles and
//!     departments, location kinds, account types, asset categories,
//!     locations, accounts, companies) are compared to a set DERIVED
//!     from the platform's own files: the system roles the migration
//!     flags as such, the roles the platform Workflow bundle names, the
//!     bootstrap admin's and the operator baseline's row, the kinds the
//!     platform's default locations wear, the account_type column
//!     default. One generic row (`owner`, the sole-proprietor role) is
//!     named by hand with its reason. A migration that seeds a new
//!     example row, or an example seed that stops carrying one, moves
//!     this line. Until backlog 7f163e58 (2026-09-18) the five accounts
//!     the migration's tax kinds name (2150 / 2300 / 2310 / 2320 / 6500)
//!     were kept here too, under the demo's names, on every instance;
//!     the kinds and the sales-tax rates are the brewery's seed now, so
//!     on a bare schema every account goes and both tax tables empty.
//!   * DELETABLE ONLY WHEN UNREFERENCED. A fixture wearing the rows — an
//!     employee with the role, department and location; an account of
//!     the type; a policy grant naming a role; a job about the company
//!     and one about a location; a child class; a tax filing naming a
//!     tax kind, which keeps the kind AND the accounts the kind names —
//!     keeps each of them, named with the column that points at it, and
//!     the run deletes the rest.
//!   * A ROW THE INSTANCE'S OWN TENANT DECLARES IS NOT A CANDIDATE
//!     (backlog 86835bf9; measured 2026-09-18, ops-request 8522ad76:
//!     four departments Algedonic declares under the device shop's
//!     codes were unreferenced and deleted). With the tenant directory
//!     given, a re-declared department, location and account survive
//!     the plan and the run — and because the subtraction happens
//!     BEFORE the judgement, a re-declared child keeps its example
//!     parent (`locations.parent_id`), which a reason added after the
//!     fact could not.
//!   * A RETIRED EXAMPLE'S MIGRATION ROWS STILL LEAVE (backlog a8991c86,
//!     car 6). On a tree without examples/used-device-shop — the tree
//!     car 7 leaves — the same fresh-schema line holds, because the rows
//!     01-registries.sql seeds for it are declared under
//!     infra/postgres/retired-examples/; and without that list they
//!     would not (the control).
//!
//! Never against production: TestDb refuses a server hosting a database
//! named `boss` (test_db.rs).

use boss_testing::{TestDb, create_dir, repo_root, scratch_dir, write_file};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A company's tenant declaring nothing an example does — what every
/// case not about the subtraction hands the derivation.
fn plain_tenant() -> PathBuf {
    let t = scratch_dir("example-reference-rows-sql-tenant");
    write_file(
        &t.join("tenant.toml"),
        "[meta]\ntenant_id = \"acme\"\ndisplay_name = \"Acme\"\n",
    );
    t
}

/// The derivation's environment: empty for this tree as it stands, or
/// `BOSS_EXAMPLES_DIR` / `BOSS_RETIRED_EXAMPLES_DIR` for a tree shaped
/// like a later car's.
type Env<'a> = &'a [(&'a str, &'a Path)];

fn derive_in(mode: &str, tenant: &Path, env: Env) -> String {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/postgres/example-reference-rows.sh"))
        .arg(mode)
        .arg(tenant)
        .envs(env.iter().copied())
        .output()
        .expect("bash runs");
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// psql with the SQL on stdin, the way boss-init and the verb send it.
fn psql(url: &str, sql: &str, read_only: bool) -> String {
    let mut child = Command::new("psql")
        .args([url, "-X", "-q", "-At", "-v", "ON_ERROR_STOP=1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("psql on PATH");
    {
        let mut stdin = child.stdin.take().unwrap();
        if read_only {
            stdin
                .write_all(b"SET default_transaction_read_only = on;\n")
                .unwrap();
        }
        stdin.write_all(sql.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "psql: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn plan_for(url: &str, tenant: &Path) -> serde_json::Value {
    plan_in(url, tenant, &[])
}

fn plan_in(url: &str, tenant: &Path, env: Env) -> serde_json::Value {
    let out = psql(url, &derive_in("plan-sql", tenant, env), true);
    serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("plan is one JSON line ({e}): {out}"))
}

fn plan(url: &str) -> serde_json::Value {
    plan_for(url, &plain_tenant())
}

fn evict_for(url: &str, tenant: &Path) -> Vec<serde_json::Value> {
    evict_in(url, tenant, &[])
}

fn evict_in(url: &str, tenant: &Path, env: Env) -> Vec<serde_json::Value> {
    psql(url, &derive_in("delete-sql", tenant, env), false)
        .lines()
        .map(|l| {
            serde_json::from_str(l).unwrap_or_else(|e| panic!("one JSON line per table ({e}): {l}"))
        })
        .collect()
}

fn evict(url: &str) -> Vec<serde_json::Value> {
    evict_for(url, &plain_tenant())
}

/// An examples directory shaped like the tree after the used-device
/// shop's deletion (backlog a8991c86, car 7): the brewery alone.
fn examples_without_the_device_shop() -> PathBuf {
    let e = scratch_dir("example-reference-rows-sql-car7");
    std::os::unix::fs::symlink(repo_root().join("examples/brewery"), e.join("brewery")).unwrap();
    e
}

async fn set(db: &TestDb, sql: &str) -> BTreeSet<String> {
    sqlx::query_scalar::<_, String>(sql)
        .fetch_all(&db.pool)
        .await
        .unwrap()
        .into_iter()
        .collect()
}

fn s(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn keys(v: &serde_json::Value) -> BTreeSet<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect()
}

fn kept(v: &serde_json::Value) -> Vec<(String, Vec<String>)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|k| {
            (
                k["key"].as_str().unwrap().to_string(),
                k["reasons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect()
}

/// Every role the platform Workflow bundle names: any `role`,
/// `owner_role` or `authority_role` string and any `sign_offs_required`
/// entry, wherever it sits in infra/platform/workflows/*.toml.
fn platform_workflow_roles() -> BTreeSet<String> {
    fn walk(v: &toml::Value, out: &mut BTreeSet<String>) {
        match v {
            toml::Value::Table(t) => {
                for (k, v) in t {
                    match (k.as_str(), v) {
                        ("role" | "owner_role" | "authority_role", toml::Value::String(s)) => {
                            out.insert(s.clone());
                        }
                        ("sign_offs_required", toml::Value::Array(a)) => {
                            for s in a.iter().filter_map(|s| s.as_str()) {
                                out.insert(s.to_string());
                            }
                        }
                        _ => walk(v, out),
                    }
                }
            }
            toml::Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    for e in std::fs::read_dir(repo_root().join("infra/platform/workflows")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "toml") {
            let v: toml::Value = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            walk(&v, &mut out);
        }
    }
    out
}

/// The operator baseline's rows: the bootstrap admin (boss-people
/// operator_baseline.rs) and every hire in operator_hires.toml, as
/// (field, value).
fn operator_baseline() -> Vec<(String, String)> {
    let root = repo_root();
    let mut rows = Vec::new();
    let src =
        std::fs::read_to_string(root.join("crates/modules/boss-people/src/operator_baseline.rs"))
            .unwrap();
    let body = src
        .split("pub fn bootstrap_admin_row")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    for field in [
        "role",
        "department",
        "location",
        "employment_type",
        "status",
    ] {
        let v = body.split(&format!("{field}: Some(\"")).nth(1).unwrap();
        rows.push((field.to_string(), v.split('"').next().unwrap().to_string()));
    }
    let hires: toml::Value = toml::from_str(
        &std::fs::read_to_string(root.join("infra/operator-baseline/operator_hires.toml")).unwrap(),
    )
    .unwrap();
    for h in hires["hire"].as_array().unwrap() {
        for field in [
            "role",
            "department",
            "location",
            "employment_type",
            "status",
        ] {
            rows.push((field.to_string(), h[field].as_str().unwrap().to_string()));
        }
    }
    rows
}

fn baseline(field: &str) -> BTreeSet<String> {
    operator_baseline()
        .into_iter()
        .filter(|(f, _)| f == field)
        .map(|(_, v)| v)
        .collect()
}

const CLASSES: &str =
    "SELECT code FROM classes WHERE subject_kind = $1 AND member_attribute = $2 ORDER BY code";

async fn classes(db: &TestDb, kind: &str, attr: &str) -> BTreeSet<String> {
    sqlx::query_scalar::<_, String>(CLASSES)
        .bind(kind)
        .bind(attr)
        .fetch_all(&db.pool)
        .await
        .unwrap()
        .into_iter()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn on_a_fresh_schema_what_remains_is_exactly_what_the_platform_names() {
    let db = TestDb::new().await;
    what_remains_is_exactly_what_the_platform_names(&db, &[]).await;
}

/// The tree after car 7 of backlog a8991c86, which deletes
/// examples/used-device-shop. 01-registries.sql still seeds its 26
/// roles, ten departments, three account types, `warehouse-zone` and
/// its companies row on every fresh instance, so they must still leave.
///
/// The control first: with the brewery alone and NO retired list, the
/// rows only the device shop carried are no candidate at all — the
/// hazard the car-0 measure recorded, reproduced. Then the same tree
/// with the retired list (the default, beside the script): what remains
/// is exactly what the platform names, the same line the current tree
/// is held to, and every row the list names was on the fresh schema and
/// left. That, with the DB-free pin in example_reference_rows_sh.rs
/// (the list names only rows 01-registries.sql seeds), is the equality:
/// the list holds every device-shop row the platform does not keep and
/// the brewery does not carry, and nothing the migration did not seed.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_device_shop_example_its_migration_rows_still_leave() {
    let db = TestDb::new().await;
    let url = db.url();
    let examples = examples_without_the_device_shop();
    let no_list = scratch_dir("example-reference-rows-sql-no-retired");

    let p = plan_in(
        &url,
        &plain_tenant(),
        &[
            ("BOSS_EXAMPLES_DIR", &examples),
            ("BOSS_RETIRED_EXAMPLES_DIR", &no_list),
        ],
    );
    let deletable = keys(&p["classes"]["deletable"]);
    for stranded in [
        "employee:refurb-tech",
        "employee:service",
        "account:clinic",
        "location:warehouse-zone",
    ] {
        assert!(
            !deletable.contains(stranded),
            "the control: without the list, {stranded} is no candidate — it would stay on every fresh instance"
        );
    }
    assert!(!keys(&p["companies"]["deletable"]).contains("used-device-shop"));

    let deleted =
        what_remains_is_exactly_what_the_platform_names(&db, &[("BOSS_EXAMPLES_DIR", &examples)])
            .await;
    let listed: toml::Value = toml::from_str(
        &std::fs::read_to_string(
            repo_root().join("infra/postgres/retired-examples/used-device-shop/seeds/classes.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    for r in listed["class"].as_array().unwrap() {
        let k = format!(
            "{}:{}",
            r["subject_kind"].as_str().unwrap(),
            r["code"].as_str().unwrap()
        );
        assert!(
            deleted.contains(&k),
            "{k} is on the retired list, so a fresh schema held it and the run evicted it"
        );
    }
}

/// Plan, run and read back the eviction on a fresh schema, with the
/// derivation reading `env`, and hold what remains to what the platform
/// names. Returns the class keys the run deleted.
async fn what_remains_is_exactly_what_the_platform_names(
    db: &TestDb,
    env: Env<'_>,
) -> BTreeSet<String> {
    let url = db.url();
    let plan = |url: &str| plan_in(url, &plain_tenant(), env);
    let evict = |url: &str| evict_in(url, &plain_tenant(), env);

    let roles_before = classes(db, "employee", "role").await;
    let employment_before = classes(db, "employee", "employment_type").await;
    let status_before = classes(db, "employee", "status").await;
    let phases_before = classes(db, "asset", "phase").await;

    // The plan on the bare schema: every present candidate is deletable.
    // The migration's tax kinds name six accounts, and until 7f163e58
    // that kept them; a kind that is itself leaving keeps nothing.
    // 6550 joined the five when 20260922052339 gave the per-production
    // kind the expense account its accruals debit (backlog c0b83e13):
    // the row is the one definition of both of a kind's accounts.
    let p = plan(&url);
    let tax_accounts = set(db, "SELECT liability_account FROM tax_kinds UNION SELECT expense_account FROM tax_kinds WHERE expense_account IS NOT NULL").await;
    assert_eq!(
        tax_accounts,
        s(&["2150", "2300", "2310", "2320", "6500", "6550"]),
        "the schema's tax kinds name these accounts"
    );
    assert_eq!(
        p["tax_kinds"]["present"].as_u64().unwrap(),
        5,
        "the migration's five kinds are candidates: {p}"
    );
    assert_eq!(p["sales_tax_rates"]["present"].as_u64().unwrap(), 27);
    for t in [
        "companies",
        "locations",
        "tax_kinds",
        "sales_tax_rates",
        "gl_accounts",
        "classes",
    ] {
        assert_eq!(
            p[t]["kept"].as_array().unwrap().len(),
            0,
            "{t}: nothing references a row on a bare schema"
        );
        assert!(
            p[t]["present"].as_u64().unwrap() > 0,
            "{t}: the migrations seeded candidates"
        );
        assert_eq!(
            p[t]["present"].as_u64().unwrap(),
            p[t]["deletable"].as_array().unwrap().len() as u64
        );
    }
    assert!(
        p["classes"]["present"].as_u64().unwrap() < p["classes"]["candidates"].as_u64().unwrap(),
        "the seeds carry more than the migrations did (the tenants' own rows)"
    );

    // The run.
    let runs = evict(&url);
    let tables: Vec<&str> = runs.iter().map(|r| r["table"].as_str().unwrap()).collect();
    assert_eq!(
        tables,
        [
            "companies",
            "locations",
            "tax_kinds",
            "sales_tax_rates",
            "gl_accounts",
            "classes"
        ]
    );
    for r in &runs {
        let t = r["table"].as_str().unwrap();
        assert_eq!(
            keys(&r["deleted"]),
            keys(&p[t]["deletable"]),
            "{t}: the run deleted what the plan said"
        );
    }
    let after = plan(&url);
    for t in [
        "companies",
        "locations",
        "tax_kinds",
        "sales_tax_rates",
        "gl_accounts",
        "classes",
    ] {
        assert_eq!(
            after[t]["deletable"].as_array().unwrap().len(),
            0,
            "{t}: nothing left to delete"
        );
    }
    // A second run is a no-op.
    for r in evict(&url) {
        assert_eq!(r["deleted"].as_array().unwrap().len(), 0);
    }

    // --- what remains, against what the platform names -------------------
    // employee roles: the rows the migration flags as the system's, the
    // roles the platform bundle names that the migrations seed, the
    // baseline's roles, and `owner` (01-registries.sql: the generic
    // sole-proprietor / founder role, cross-tenant by design).
    let system = set(db, "SELECT code FROM classes WHERE subject_kind = 'employee' AND member_attribute = 'role' AND (metadata->>'is_system_role' = 'true' OR metadata->>'is_test_fixture' = 'true')").await;
    let named: BTreeSet<String> = platform_workflow_roles()
        .intersection(&roles_before)
        .cloned()
        .collect();
    let mut expected_roles: BTreeSet<String> = system.union(&named).cloned().collect();
    expected_roles.extend(baseline("role"));
    expected_roles.insert("owner".into());
    assert_eq!(
        classes(db, "employee", "role").await,
        expected_roles,
        "employee roles kept = system rows + platform-named + baseline + owner"
    );
    assert!(
        named.contains("platform-admin"),
        "the bundle names platform-admin and the migration seeds it"
    );
    // Every role the platform names is either kept here or carried by an
    // example seed (publish-request's owner_role = shift-lead is the
    // brewery's, in its classes.json — a leak this line makes visible).
    let seeds: serde_json::Value =
        serde_json::from_str(derive_in("seeds", &plain_tenant(), env).trim()).unwrap();
    let seeded_roles: BTreeSet<String> = seeds["classes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["subject_kind"] == "employee" && c["member_attribute"] == "role")
        .map(|c| c["code"].as_str().unwrap().to_string())
        .collect();
    let policy_roles = s(&["workflow-approver"]); // a policy grant role, never an employee class
    for r in platform_workflow_roles() {
        assert!(
            expected_roles.contains(&r) || seeded_roles.contains(&r) || policy_roles.contains(&r),
            "the platform bundle names role {r}, which no instance carries"
        );
    }

    assert_eq!(
        classes(db, "employee", "department").await,
        baseline("department"),
        "departments kept = the operator baseline's (it)"
    );
    assert_eq!(
        classes(db, "employee", "employment_type").await,
        employment_before,
        "employment types are the platform's vocabulary, untouched"
    );
    assert_eq!(classes(db, "employee", "status").await, status_before);
    assert_eq!(
        classes(db, "asset", "phase").await,
        phases_before,
        "module-tier vocabularies are untouched"
    );

    let locations = set(db, "SELECT id FROM locations ORDER BY id").await;
    assert!(
        locations.is_superset(&baseline("location")),
        "the baseline hires at loc-hq, which stays"
    );
    assert_eq!(
        locations,
        s(&["loc-field-default", "loc-hq", "loc-remote-default"]),
        "locations kept = the platform's three defaults (01-registries.sql: HQ, the field bucket, the remote bucket)"
    );
    let worn = set(db, "SELECT DISTINCT kind FROM locations").await;
    assert_eq!(
        classes(db, "location", "kind").await,
        worn,
        "location kinds kept = exactly the kinds the platform's default locations wear"
    );

    let dflt = set(db, "SELECT trim(both '''' from split_part(column_default, '::', 1)) FROM information_schema.columns WHERE table_name = 'accounts' AND column_name = 'account_type'").await;
    assert_eq!(
        classes(db, "account", "type").await,
        dflt,
        "account types kept = the column's default (unspecified)"
    );
    assert_eq!(
        classes(db, "asset", "category").await,
        BTreeSet::new(),
        "every asset category was an example's"
    );
    assert_eq!(
        set(db, "SELECT id FROM companies").await,
        BTreeSet::new(),
        "both companies rows were examples'"
    );
    assert_eq!(
        set(db, "SELECT code FROM gl_accounts").await,
        BTreeSet::new(),
        "the whole starter chart was the brewery's, the five tax accounts included (7f163e58)"
    );
    assert_eq!(
        set(db, "SELECT kind FROM tax_kinds").await,
        BTreeSet::new(),
        "every tax kind was the brewery's"
    );
    assert_eq!(
        set(db, "SELECT state FROM sales_tax_rate_by_state").await,
        BTreeSet::new(),
        "every sales-tax rate was the brewery's"
    );
    runs.iter()
        .filter(|r| r["table"] == "classes")
        .flat_map(|r| keys(&r["deleted"]))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_referenced_row_is_kept_and_named_and_the_rest_go() {
    let db = TestDb::new().await;
    let url = db.url();
    sqlx::raw_sql(
        "INSERT INTO employees (id, name, role, department, location) VALUES ('emp-f', 'F', 'ceo', 'sales', 'loc-brewery-taproom');
         INSERT INTO accounts (id, name, account_type) VALUES ('acc-f', 'Clinic F', 'clinic');
         INSERT INTO policy_rules (id, role, resource, action, scope) VALUES ('service-tech:job:read', 'service-tech', 'job', 'read', 'self');
         INSERT INTO classes (subject_kind, code, display_name, member_attribute, parent_code) VALUES ('account', 'wholesale-west', 'Wholesale West', 'type', 'wholesale');
         INSERT INTO workflows (kind, version, status, label, category, subject_kinds, steps, owning_team)
             VALUES ('org-thing', 1, 'active', 'Org thing', 'ops', '[\"company\", \"location\"]', '[]', 'acme');
         INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, status, priority, opened_on, partition)
             VALUES ('11111111-1111-1111-1111-111111111111', 'org-thing', 'company', 'brewery', 'about the company', 'emp-f', 'open', 'standard', '2026-09-17', 'real'),
                    ('22222222-2222-2222-2222-222222222222', 'org-thing', 'location', 'loc-brewery-brewhouse', 'about the site', 'emp-f', 'closed', 'standard', '2026-09-17', 'simulated');
         INSERT INTO tax_filings (id, kind, jurisdiction, period_start, period_end, due_on, amount_cents, liability_account, status)
             VALUES ('tf-f', 'sales', 'US-CA', '2026-07-01', '2026-09-30', '2026-10-31', 12500, '2300', 'accrued');",
    )
    .execute(&db.pool)
    .await
    .expect("fixture rows");

    let p = plan(&url);
    let want = |p: &serde_json::Value, t: &str, key: &str, reasons: &[&str]| {
        let k = kept(&p[t]["kept"]);
        let (_, got) = k
            .iter()
            .find(|(kk, _)| kk == key)
            .unwrap_or_else(|| panic!("{t}: {key} should be kept; kept = {k:?}"));
        assert_eq!(got, reasons, "{t}: {key}");
    };
    want(&p, "classes", "employee:ceo", &["employees.role"]);
    want(&p, "classes", "employee:sales", &["employees.department"]);
    want(
        &p,
        "classes",
        "employee:service-tech",
        &["policy_rules.role"],
    );
    want(&p, "classes", "account:clinic", &["accounts.account_type"]);
    want(&p, "classes", "account:wholesale", &["classes.parent_code"]);
    want(
        &p,
        "locations",
        "loc-brewery-taproom",
        &["employees.location"],
    );
    want(
        &p,
        "locations",
        "loc-brewery-brewhouse",
        &["jobs.subject_id"],
    );
    want(&p, "companies", "brewery", &["jobs.subject_id"]);
    // A filing names the kind, so the kind stays — and a kind that
    // stays keeps the accounts it names (7f163e58); the filing names
    // its liability account too. The other four kinds go, and with
    // them their accounts.
    want(&p, "tax_kinds", "sales", &["tax_filings.kind"]);
    want(
        &p,
        "gl_accounts",
        "2300",
        &["tax_kinds", "tax_filings.liability_account"],
    );
    assert_eq!(kept(&p["tax_kinds"]["kept"]).len(), 1);
    assert_eq!(kept(&p["gl_accounts"]["kept"]).len(), 1);
    assert_eq!(kept(&p["sales_tax_rates"]["kept"]).len(), 0);
    assert_eq!(
        kept(&p["classes"]["kept"]).len(),
        5,
        "and nothing else is kept among the classes"
    );
    assert_eq!(kept(&p["locations"]["kept"]).len(), 2);
    assert_eq!(kept(&p["companies"]["kept"]).len(), 1);
    // The brewery's taproom kind is not kept for the kept location: the
    // migration's row wears 'retail', which no class names. But a
    // location the run would keep DOES keep its kind — pin that with
    // the tenant's own kind on the kept row.
    sqlx::query("UPDATE locations SET kind = 'taproom' WHERE id = 'loc-brewery-taproom'")
        .execute(&db.pool)
        .await
        .unwrap();
    let p = plan(&url);
    want(&p, "classes", "location:taproom", &["locations.kind"]);
    assert!(
        !kept(&p["classes"]["kept"])
            .iter()
            .any(|(k, _)| k == "location:brewhouse"),
        "a kind worn only by a location the run evicts is itself deletable — the plan predicts the run"
    );

    let runs = evict(&url);
    let deleted: BTreeSet<String> = runs.iter().flat_map(|r| keys(&r["deleted"])).collect();
    for k in [
        "employee:ceo",
        "employee:sales",
        "employee:service-tech",
        "account:clinic",
        "account:wholesale",
        "location:taproom",
        "loc-brewery-taproom",
        "loc-brewery-brewhouse",
        "brewery",
        "sales",
        "2300",
    ] {
        assert!(!deleted.contains(k), "{k} was referenced and must survive");
    }
    for k in [
        "employee:cto",
        "employee:refurb",
        "account:lab",
        "asset:barrel",
        "location:brewhouse",
        "used-device-shop",
        "1000",
        "income",
        "6500",
        "CA",
    ] {
        assert!(
            deleted.contains(k),
            "{k} was unreferenced and must go: {deleted:?}"
        );
    }
    assert_eq!(set(&db, "SELECT id FROM companies").await, s(&["brewery"]));
    assert_eq!(set(&db, "SELECT kind FROM tax_kinds").await, s(&["sales"]));
    assert_eq!(set(&db, "SELECT code FROM gl_accounts").await, s(&["2300"]));
    assert_eq!(
        set(&db, "SELECT state FROM sales_tax_rate_by_state").await,
        BTreeSet::new()
    );
    assert_eq!(
        set(
            &db,
            "SELECT id FROM locations WHERE id LIKE 'loc-brewery-%'"
        )
        .await,
        s(&["loc-brewery-brewhouse", "loc-brewery-taproom"])
    );
    // The subjects projection rows of evicted locations/companies go with them.
    let subjects_deleted: i64 = runs
        .iter()
        .filter_map(|r| r["subjects_deleted"].as_i64())
        .sum();
    assert_eq!(
        subjects_deleted, 0,
        "no subjects rows existed on the bare schema"
    );
}

/// The measured hole (backlog 86835bf9, ops-request 8522ad76): the
/// tenant's own declarations under example codes. Fresh schema, nothing
/// referencing anything — exactly the state in which the four
/// departments were deleted on prod — and a tenant re-declaring the
/// device shop's `sales` department, the brewery's taproom and its
/// `1100` account, plus a route location under the brewhouse.
#[tokio::test(flavor = "multi_thread")]
async fn a_row_the_tenant_declares_survives_the_plan_and_the_run() {
    let db = TestDb::new().await;
    let url = db.url();
    let tenant = scratch_dir("example-reference-rows-sql-redeclares");
    write_file(
        &tenant.join("tenant.toml"),
        "[meta]\ntenant_id = \"algedonic\"\ndisplay_name = \"Algedonic, LLC\"\n",
    );
    create_dir(&tenant.join("seeds"));
    write_file(
        &tenant.join("seeds/classes.json"),
        r#"[{"subject_kind": "employee", "code": "sales", "display_name": "Sales", "member_attribute": "department"}]"#,
    );
    write_file(
        &tenant.join("seeds/locations.toml"),
        "[[location]]\nid = \"loc-brewery-taproom\"\nname = \"Taproom\"\nkind = \"hq\"\ntimezone = \"UTC\"\n\n[[location]]\nid = \"loc-brewery-route-mission\"\nname = \"Mission route\"\nkind = \"hq\"\nparent_id = \"loc-brewery-brewhouse\"\ntimezone = \"UTC\"\n",
    );
    write_file(
        &tenant.join("seeds/chart_of_accounts.toml"),
        "[[account]]\ncode = \"1100\"\nname = \"AR\"\nkind = \"asset\"\nnormal_balance = \"debit\"\n",
    );
    // The route the tenant re-declares is on the instance already
    // (published by an earlier tenant publish), under the brewery's
    // brewhouse — a candidate whose parent is a candidate.
    sqlx::raw_sql(
        "INSERT INTO locations (id, name, kind, parent_id, timezone) VALUES ('loc-brewery-route-mission', 'Mission route', 'distribution-route', 'loc-brewery-brewhouse', 'UTC')",
    )
    .execute(&db.pool)
    .await
    .expect("fixture row");

    let without = plan(&url);
    let p = plan_for(&url, &tenant);
    assert_eq!(
        p["classes"]["candidates"].as_u64().unwrap(),
        without["classes"]["candidates"].as_u64().unwrap() - 1,
        "the re-declared department is not a candidate"
    );
    assert!(
        !keys(&p["classes"]["deletable"]).contains("employee:sales"),
        "{p}"
    );
    assert!(
        !kept(&p["classes"]["kept"])
            .iter()
            .any(|(k, _)| k == "employee:sales"),
        "not kept either — it was never judged"
    );
    assert!(keys(&without["classes"]["deletable"]).contains("employee:sales"));
    assert!(!keys(&p["gl_accounts"]["deletable"]).contains("1100"));
    assert!(keys(&without["gl_accounts"]["deletable"]).contains("1100"));
    assert!(!keys(&p["locations"]["deletable"]).contains("loc-brewery-taproom"));
    // Subtracted BEFORE judging: the brewhouse keeps its example row
    // because a location the tenant declares sits under it.
    let k = kept(&p["locations"]["kept"]);
    let (_, reasons) = k
        .iter()
        .find(|(kk, _)| kk == "loc-brewery-brewhouse")
        .unwrap_or_else(|| panic!("the brewhouse is kept for the tenant's route under it: {k:?}"));
    assert_eq!(reasons, &["locations.parent_id"]);
    assert!(
        keys(&without["locations"]["deletable"]).contains("loc-brewery-brewhouse"),
        "without the tenant, the route is a candidate too and the brewhouse goes with it"
    );

    let runs = evict_for(&url, &tenant);
    let deleted: BTreeSet<String> = runs.iter().flat_map(|r| keys(&r["deleted"])).collect();
    for k in [
        "employee:sales",
        "1100",
        "loc-brewery-taproom",
        "loc-brewery-route-mission",
        "loc-brewery-brewhouse",
    ] {
        assert!(!deleted.contains(k), "{k} is the tenant's and must survive");
    }
    for k in ["employee:finance", "employee:cto", "1000", "brewery"] {
        assert!(
            deleted.contains(k),
            "{k} is residue and must go: {deleted:?}"
        );
    }
    assert!(
        classes(&db, "employee", "department")
            .await
            .contains("sales"),
        "the department the tenant declares is still on the instance"
    );
    assert_eq!(
        set(&db, "SELECT code FROM gl_accounts WHERE code = '1100'").await,
        s(&["1100"])
    );
    assert_eq!(
        set(
            &db,
            "SELECT id FROM locations WHERE id LIKE 'loc-brewery-%'"
        )
        .await,
        s(&[
            "loc-brewery-brewhouse",
            "loc-brewery-route-mission",
            "loc-brewery-taproom"
        ])
    );
}
