//! The census's SQL, RUN against the real schema — the half
//! `tenant_census_sh.rs` cannot pin with a stub that answers canned rows.
//!
//! A stub that echoes JSON proves the script's shape; it proves nothing
//! about the five queries, and those are the part the eviction verb
//! will take its bounds from (backlog d07dcc2b). So here the stub
//! `kubectl` does one translation: it drops the `-n boss exec
//! sts/postgres -c postgres -- psql -U boss -d boss` head the script
//! sends to the cluster and runs the SAME psql arguments — the read-only
//! SET, then the query — against a `TestDb` with the schema loaded. Every
//! column the queries name, every catalog trick (`query_to_xml` per
//! table), and the read-only session are exercised by Postgres itself.
//!
//! The scratch schema is empty, so every count is 0 and the seed's
//! "present" is 0 against a "seeded" of hundreds — which is exactly the
//! answer a fresh instance should give, and the document's shape does not
//! depend on the data. A fixture inserting a brewery employee and one
//! real job that names it makes the references section non-trivial: the
//! job's id must come back under `jobs.owner_id`.
//!
//! Never against production: `TestDb` refuses a server hosting a
//! database named `boss` (test_db.rs), which is what a port-forward to
//! the cluster looks like from here.

use boss_testing::{TestDb, repo_root};
use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    repo_root().join("infra/forge/tenant-census.sh")
}

/// The translating stub: strip the exec head, run psql against `$DB_URL`
/// with the arguments the script chose. The head is asserted, not
/// skipped blind — a script that stopped sending it would fail here.
fn write_stub(dir: &std::path::Path) -> PathBuf {
    let stub = dir.join("kubectl");
    boss_testing::write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
[ "$1 $2 $3 $4 $5 $6 $7 $8" = "-n boss exec sts/postgres -c postgres -- psql" ] \
    || { echo "stub kubectl: unexpected argv head: $*" >&2; exit 1; }
shift 8
# drop `-U boss -d boss` — the URL names the scratch database
args=()
while [ $# -gt 0 ]; do
    case "$1" in
        -U|-d) shift 2 ;;
        *) args+=("$1"); shift ;;
    esac
done
exec psql "$DB_URL" "${args[@]}"
"#,
    );
    stub
}

#[tokio::test(flavor = "multi_thread")]
async fn the_five_queries_run_read_only_against_the_real_schema() {
    let db = TestDb::new().await;
    // A brewery employee (the seed's first id) owning one REAL job, one
    // simulated job under a brewery workflow kind, and a step completed
    // by that employee — so every section has something to find.
    sqlx::raw_sql(
        "INSERT INTO employees (id, name) VALUES ('emp-aa-001', 'ceo');
         INSERT INTO workflows (kind, version, status, label, category, subject_kinds, steps, owning_team)
             VALUES ('morning-brew', 1, 'active', 'Morning brew', 'ops', '[\"employee\"]', '[]', 'brewery');
         INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, status, priority, opened_on, partition)
             VALUES ('11111111-1111-1111-1111-111111111111', 'morning-brew', 'employee', 'emp-aa-001', 'real one', 'emp-aa-001', 'open', 'standard', '2026-09-16', 'real'),
                    ('22222222-2222-2222-2222-222222222222', 'morning-brew', 'employee', 'nobody', 'sim one', 'nobody', 'closed', 'standard', '2026-09-16', 'simulated');
         INSERT INTO steps (id, job_id, kind, title, status, completed_by)
             VALUES ('33333333-3333-3333-3333-333333333333', '11111111-1111-1111-1111-111111111111', 'generic', 'do it', 'completed', 'emp-aa-001');",
    )
    .execute(&db.pool)
    .await
    .expect("fixture rows");

    let dir = boss_testing::scratch_dir("tenant-census-sql");
    let stub = write_stub(&dir);
    let out = Command::new("bash")
        .arg(script())
        .env("BOSS_KUBECTL", stub.to_str().unwrap())
        .env("DB_URL", db.url())
        .env_remove("KUBECONFIG")
        .output()
        .expect("bash runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("not one JSON document ({e}):\n{stdout}"));

    // (a) every public table, exact counts
    let counts = doc["row_counts"]
        .as_object()
        .expect("row_counts is an object");
    assert!(
        counts.len() > 100,
        "row_counts covers the schema: {} tables",
        counts.len()
    );
    assert_eq!(counts["jobs"], 2);
    assert_eq!(counts["employees"], 1);
    assert_eq!(counts["invoices"], 0);

    // (b) the seed's own keys, matched
    let emp = &doc["seed_owned"]["employees"];
    assert_eq!(emp["key"], "id");
    assert_eq!(emp["present"], 1);
    assert_eq!(emp["unmatched"], 0);
    assert!(emp["seeded"].as_i64().unwrap() > 400);
    assert_eq!(emp["sample"], serde_json::json!(["emp-aa-001"]));
    assert_eq!(doc["seed_owned"]["accounts"]["key"], "name");
    assert_eq!(doc["seed_owned"]["accounts"]["present"], 0);
    assert_eq!(doc["seed_owned"]["workflows"]["present"], 1);
    assert_eq!(
        doc["seed_owned"]["workflows"]["sample"],
        serde_json::json!(["morning-brew@v1"])
    );
    assert_eq!(doc["seed_owned"]["classes"]["key"], "(subject_kind, code)");
    assert_eq!(
        doc["seed_owned"]["not_keyed"],
        serde_json::json!([
            "subject_edges",
            "companies",
            "sales_tax_rate_by_state",
            "tax_kinds"
        ])
    );

    // (c) partitions: jobs by its own column, steps through job_id, the
    // rest unknown — derived from the catalog, so a projection that
    // grows a partition column moves lists without a script change.
    let p = &doc["partitions"];
    assert_eq!(p["by_partition_column"]["jobs"]["real"], 1);
    assert_eq!(p["by_partition_column"]["jobs"]["simulated"], 1);
    assert_eq!(p["by_job_id"]["steps"]["real"], 1);
    let unknown = p["unknown"].as_array().unwrap();
    assert!(
        unknown.iter().any(|t| t == "invoices"),
        "invoices carries neither column"
    );
    assert!(!unknown.iter().any(|t| t == "jobs" || t == "steps"));

    // (d) references: the real job and its step name the seed employee
    assert_eq!(doc["references"]["jobs.owner_id"]["count"], 1);
    assert_eq!(
        doc["references"]["jobs.owner_id"]["ids"],
        serde_json::json!(["11111111-1111-1111-1111-111111111111"])
    );
    assert_eq!(doc["references"]["jobs.subject_id"]["count"], 1);
    assert_eq!(
        doc["references"]["jobs.subject_id"]["by_subject_kind"]["employee"],
        1
    );
    assert_eq!(doc["references"]["steps.completed_by"]["count"], 1);
    assert_eq!(doc["references"]["steps.assignee_id"]["count"], 0);

    // (e) workflows by owning team, open jobs split by partition
    let wf = doc["workflows"].as_array().unwrap();
    let brewery = wf
        .iter()
        .find(|w| w["owning_team"] == "brewery")
        .expect("brewery row");
    assert_eq!(brewery["kinds"], 1);
    assert_eq!(brewery["open_jobs"], 1);
    assert_eq!(brewery["open_real_jobs"], 1);
}

/// The read-only SET is not decoration: a statement that writes, sent
/// the way the script sends its queries, is refused by Postgres. This is
/// the property the operator relies on when the verb runs against
/// production, pinned at the layer that enforces it.
#[tokio::test(flavor = "multi_thread")]
async fn the_session_the_script_opens_refuses_a_write() {
    let db = TestDb::new().await;
    let out = Command::new("psql")
        .arg(db.url())
        .args(["-X", "-q", "-At", "-v", "ON_ERROR_STOP=1"])
        .args(["-c", "SET default_transaction_read_only = on"])
        .args(["-c", "INSERT INTO companies (id, name) VALUES ('x', 'x')"])
        .output()
        .expect("psql runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a write went through a read-only session"
    );
    assert!(
        stderr.contains("read-only"),
        "Postgres names the refusal: {stderr}"
    );
}
