//! `infra/forge/retire-example-reference-rows.sh` is RUN, not read —
//! against a stubbed `kubectl` that records every argv and every byte
//! of stdin it receives, answers `get secret` with a URL held in a
//! file, and answers the psql reads with canned rows — so every verdict
//! below is one the script actually reached. Nothing here touches a
//! cluster or a database; the SQL it streams is exercised against the
//! real schema in example_reference_rows_sql.rs.
//!
//! WHY THE VERB EXISTS (backlog 718ac982; design e2580840 car 3, the
//! eviction shape from the cutover e652c7c6): the migrations seed the
//! example tenants' reference rows on every instance, applied
//! migrations are history, so on an instance that has already booted
//! the residue leaves through a bounded verb with the record on the
//! packet — never a migration, never by hand.
//!
//! What each case pins:
//!
//!   * THE VERDICT LINE IS THE FIRST LINE OF THE COMBINED OUTPUT, then
//!     the one JSON record, then the per-row lines — the probe on the
//!     ops-request reads the verdict at the head of `output`.
//!   * THE PLAN IS READ-ONLY: the psql session opens with
//!     `SET default_transaction_read_only = on`, on stdin (`-i`), and a
//!     dry run streams no delete.
//!   * THE INSTANCE MUST BE A COMPANY'S: an image-sourced example
//!     instance (tenant_dir) is refused by name; an unknown namespace
//!     and boss-dev are refused; a bad mode is usage.
//!   * THE PASSWORD NEVER APPEARS, on any path.
//!   * THE REAL RUN streams the four-transaction eviction, records each
//!     table's deleted keys, reads the plan back, and reports a failed
//!     transaction as FAILED with exit 1.
//!   * THE INSTANCE'S OWN TENANT IS NEVER TOUCHED (backlog 86835bf9;
//!     measured 2026-09-18 on the first --for-real run, ops-request
//!     8522ad76: four departments Algedonic declares under the device
//!     shop's codes — finance, marketing, sales, support — were
//!     unreferenced and deleted with the residue). The verb reads the
//!     tenant checkout the converge stages for the instance
//!     (`<tenants dir>/<instance name>`, cluster-deploy-runner.sh
//!     converge_tenant), hands it to the derivation so every id/code the
//!     tenant declares leaves the candidate set, records them as
//!     `declared_by_tenant` with a `kept … declared by tenant:<id>` line
//!     each, and REFUSES when the checkout cannot be read — never a plan
//!     without it.
//!   * THE VERB FILE serves the forge, is MUTATING, names David, takes
//!     mode/namespace, and declares a timeout.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/forge/retire-example-reference-rows.sh";
/// The fixture tenant's classes: the device shop's `sales` department
/// under the tenant's own name — the measured collision — beside a row
/// no example declares.
const TENANT_CLASSES: &str = r#"[
  {"subject_kind": "employee", "code": "sales", "display_name": "Sales", "member_attribute": "department"},
  {"subject_kind": "employee", "code": "engineering", "display_name": "Engineering", "member_attribute": "department"}
]"#;
const PASSWORD: &str = "s3cretpw0123";
const URL: &str = "postgres://boss:s3cretpw0123@postgres.boss.svc.cluster.local:5432/algedonic";

const PLAN: &str = r#"{"companies":{"candidates":2,"present":2,"deletable":["brewery","used-device-shop"],"kept":[]},"locations":{"candidates":5,"present":2,"deletable":["loc-brewery-brewhouse"],"kept":[{"key":"loc-brewery-taproom","reasons":["employees.location"]}]},"gl_accounts":{"candidates":33,"present":33,"deletable":["1000","1010"],"kept":[{"key":"2300","reasons":["tax_kinds"]}]},"classes":{"candidates":156,"present":58,"deletable":["employee:cto"],"kept":[{"key":"employee:ceo","reasons":["employees.role","policy_rules.role"]}]}}"#;
const PLAN_AFTER: &str = r#"{"companies":{"candidates":2,"present":0,"deletable":[],"kept":[]},"locations":{"candidates":5,"present":1,"deletable":[],"kept":[{"key":"loc-brewery-taproom","reasons":["employees.location"]}]},"gl_accounts":{"candidates":33,"present":31,"deletable":[],"kept":[{"key":"2300","reasons":["tax_kinds"]}]},"classes":{"candidates":156,"present":57,"deletable":[],"kept":[{"key":"employee:ceo","reasons":["employees.role","policy_rules.role"]}]}}"#;
const DELETED: &str = "{\"table\":\"companies\",\"deleted\":[\"brewery\",\"used-device-shop\"],\"subjects_deleted\":2}\n{\"table\":\"locations\",\"deleted\":[\"loc-brewery-brewhouse\"],\"subjects_deleted\":1}\n{\"table\":\"gl_accounts\",\"deleted\":[\"1000\",\"1010\"]}\n{\"table\":\"classes\",\"deleted\":[\"employee:cto\"]}\n";

struct Case {
    bin: PathBuf,
    answers: PathBuf,
    log: PathBuf,
    stdin_log: PathBuf,
    tree: PathBuf,
    /// The converge's tenant checkouts, beside the tree the way
    /// `$(dirname "$REPO")/tenants` sits beside the forge checkout —
    /// `<tenants>/prod` is the instance's.
    tenants: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("retire-example-reference-rows-{name}"));
        let bin = root.join("bin");
        create_dir(&bin);
        let answers = root.join("answers");
        create_dir(&answers);
        write_file(&answers.join("secret.url"), URL);
        write_file(&answers.join("plan"), PLAN);
        write_file(&answers.join("plan_after"), PLAN_AFTER);
        write_file(&answers.join("delete"), DELETED);
        let log = root.join("argv.log");
        let stdin_log = root.join("stdin.log");
        // The tree: this checkout's examples (the candidate set) beside
        // an instances.toml naming a company instance and an example one.
        let tree = root.join("tree");
        create_dir(&tree.join("infra/cluster"));
        std::os::unix::fs::symlink(repo_root().join("examples"), tree.join("examples")).unwrap();
        write_file(
            &tree.join("infra/cluster/instances.toml"),
            "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant_repo = \"david/algedonic-llc\"\ntenant_ref = \"main\"\nsim = false\nhostname = \"boss.algedonic.dev\"\n\n[playground]\nnamespace = \"boss-playground\"\ntenant_dir = \"examples/brewery\"\nsim = true\nhostname = \"playground.algedonic.dev\"\n",
        );
        // The tenant checkout the converge staged for prod: the repo's
        // shape (tenant.toml at the root, seeds/ beside it).
        let tenants = root.join("tenants");
        create_dir(&tenants.join("prod/seeds"));
        write_file(
            &tenants.join("prod/tenant.toml"),
            "[meta]\ntenant_id = \"algedonic\"\ndisplay_name = \"Algedonic, LLC\"\n",
        );
        write_file(&tenants.join("prod/seeds/classes.json"), TENANT_CLASSES);
        // The stub kubectl: argv appended to $STUB_LOG; `get secret`
        // prints the URL base64-encoded; `exec -i … psql` reads stdin
        // whole, appends it to $STUB_STDIN, and answers by the first
        // tag line: the plan (the after-plan once a delete was served),
        // or the delete lines (four tables in this stub's answer; the
        // real derivation adds the two tax tables, 7f163e58, and the
        // departments, 7edf0e97, and the verb sums whatever tables the
        // answer names).
        write_exec(
            &bin.join("kubectl"),
            r#"#!/usr/bin/env bash
set -u
{ printf '%s\n' "$@"; echo '=== call ==='; } >> "$STUB_LOG"
words=("$@")
if [ "${words[2]:-}" = get ] && [ "${words[3]:-}" = secret ]; then
    [ -n "${STUB_SECRET_UNREADABLE:-}" ] && { echo 'Error from server (Forbidden): secrets "boss-secrets" is forbidden' >&2; exit 1; }
    printf '%s' "$(cat "$STUB_ANSWERS/secret.url")" | base64 -w0
    exit 0
fi
if [ "${words[2]:-}" = exec ]; then
    [ "${words[3]:-}" = -i ] || { echo "stub kubectl: exec without -i (stdin would be lost)" >&2; exit 1; }
    [ "${words[4]:-}" = sts/postgres ] || { echo "stub kubectl: unexpected workload ${words[4]:-}" >&2; exit 1; }
    [ "${words[8]:-}" = psql ] || { echo "stub kubectl: expected psql, got ${words[8]:-}" >&2; exit 1; }
    in=$(cat)
    { printf '%s\n' "$in"; echo '=== stdin ==='; } >> "$STUB_STDIN"
    if printf '%s\n' "$in" | grep -q '^-- retire-example-reference-rows:delete companies$'; then
        [ -n "${STUB_DELETE_FAIL:-}" ] && { head -2 "$STUB_ANSWERS/delete"; echo 'ERROR:  update or delete on table "gl_accounts" violates foreign key constraint' >&2; exit 3; }
        touch "$STUB_ANSWERS/.deleted"
        cat "$STUB_ANSWERS/delete"; exit 0
    fi
    if printf '%s\n' "$in" | grep -q '^-- retire-example-reference-rows:plan$'; then
        [ -n "${STUB_PLAN_FAIL:-}" ] && { echo 'ERROR:  relation "gl_posting_rules" does not exist' >&2; exit 3; }
        if [ -f "$STUB_ANSWERS/.deleted" ]; then cat "$STUB_ANSWERS/plan_after"; else cat "$STUB_ANSWERS/plan"; fi
        exit 0
    fi
    echo "stub kubectl: psql stdin carried no known tag" >&2; exit 1
fi
echo "stub kubectl: unexpected argv: $*" >&2
exit 1
"#,
        );
        Case {
            bin,
            answers,
            log,
            stdin_log,
            tree,
            tenants,
        }
    }

    /// Run with stdout and stderr MERGED in write order (the ops-runner
    /// captures `> raw 2>&1`), so the verdict's position is the packet's.
    fn run_env(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, String) {
        let script = repo_root().join(SCRIPT);
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(format!("exec bash '{}' \"$@\" 2>&1", script.display()))
            .arg("bash")
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_KUBECTL", self.bin.join("kubectl"))
            .env("BOSS_RETIRE_TREE", &self.tree)
            .env("BOSS_FORGE_TENANTS_DIR", &self.tenants)
            .env("STUB_LOG", &self.log)
            .env("STUB_STDIN", &self.stdin_log)
            .env("STUB_ANSWERS", &self.answers);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("bash runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn stdin(&self) -> String {
        std::fs::read_to_string(&self.stdin_log).unwrap_or_default()
    }
}

fn no_password(out: &str) {
    assert!(!out.contains(PASSWORD), "the password leaked:\n{out}");
}

#[test]
fn a_dry_run_prints_the_verdict_first_and_deletes_nothing() {
    let c = Case::new("dry-run");
    let (rc, out) = c.run(&["--dry-run", "boss"]);
    assert_eq!(rc, 0, "{out}");
    no_password(&out);
    let mut lines = out.lines();
    assert_eq!(
        lines.next().unwrap(),
        "retire-example-reference-rows: dry-run namespace=boss db=algedonic tenant=algedonic declared=1 candidates=196 present=95 deletable=6 kept=3 deleted=0",
        "the verdict is the first line:\n{out}"
    );
    let record: serde_json::Value =
        serde_json::from_str(lines.next().unwrap()).expect("the record is the second line");
    assert_eq!(record["mode"], "dry-run");
    assert_eq!(record["database"], "algedonic");
    assert_eq!(record["tenant_repo"], "david/algedonic-llc");
    assert_eq!(record["deleted"], 0);
    assert_eq!(record["plan"]["classes"]["kept"][0]["key"], "employee:ceo");
    assert!(record["read_back"].is_null());
    // The tenant's own declarations (86835bf9): named, and out of the
    // candidate set the SQL judges.
    assert_eq!(record["tenant"], "algedonic");
    assert_eq!(
        record["tenant_checkout"],
        c.tenants.join("prod").to_str().unwrap()
    );
    assert_eq!(
        record["declared_by_tenant"],
        serde_json::json!({"classes": ["employee:sales"], "locations": [], "gl_accounts": [], "companies": [], "tax_kinds": [], "sales_tax_rates": [], "departments": []}),
        "what the tenant declares under an example's key, and only that:\n{out}"
    );
    assert!(
        out.contains("kept classes employee:sales: declared by tenant:algedonic"),
        "the subtracted row is named like a kept one:\n{out}"
    );
    assert!(
        out.contains("kept classes employee:ceo: employees.role, policy_rules.role"),
        "kept rows are named with their reasons:\n{out}"
    );
    assert!(out.contains("kept locations loc-brewery-taproom: employees.location"));
    assert!(out.contains("classes: 58 of 156 candidates present, 1 deletable, 1 kept"));
    // One read, read-only, on stdin; no delete.
    let stdin = c.stdin();
    assert_eq!(
        stdin.matches("=== stdin ===").count(),
        1,
        "one psql session"
    );
    assert!(
        stdin.starts_with(
            "SET default_transaction_read_only = on;\n-- retire-example-reference-rows:plan\n"
        ),
        "{stdin}"
    );
    assert!(!stdin.contains(":delete "), "a dry run streams no delete");
    assert!(
        !stdin.contains(r#"{"subject_kind":"employee","code":"sales""#),
        "the tenant's department is in no candidate list the SQL judges:\n{stdin}"
    );
    assert!(
        stdin.contains(r#"{"subject_kind":"employee","code":"cto""#),
        "the residue still is"
    );
    let log = std::fs::read_to_string(&c.log).unwrap();
    assert!(
        log.contains("-n\nboss\nget\nsecret\nboss-secrets\n"),
        "the Secret is read in the packet's namespace:\n{log}"
    );
    assert!(
        log.contains("-U\nboss\n-d\nalgedonic\n"),
        "psql targets the Secret's database:\n{log}"
    );
}

#[test]
fn an_example_instance_an_unknown_namespace_and_the_pipeline_are_refused() {
    let c = Case::new("refusals");
    let (rc, out) = c.run(&["--dry-run", "boss-playground"]);
    assert_eq!(rc, 2, "{out}");
    assert!(
        out.contains("REFUSED")
            && out.contains("image-sourced")
            && out.contains("examples/brewery"),
        "{out}"
    );
    assert!(out.contains("Nothing was changed."));
    assert!(c.stdin().is_empty(), "nothing was read");

    let (rc, out) = c.run(&["--for-real", "boss-elsewhere"]);
    assert_eq!(rc, 2, "{out}");
    assert!(
        out.contains("no section of infra/cluster/instances.toml names namespace boss-elsewhere"),
        "{out}"
    );

    let (rc, out) = c.run(&["--for-real", "boss-dev"]);
    assert_eq!(rc, 2, "{out}");
    assert!(out.contains("pipeline's namespace"), "{out}");

    let (rc, out) = c.run(&["--for-real", "Boss Prod"]);
    assert_eq!(rc, 2, "{out}");
    let (rc, out) = c.run(&["--now", "boss"]);
    assert_eq!(rc, 2, "{out}");
    assert!(
        out.contains("the only modes are --dry-run and --for-real"),
        "{out}"
    );
    let (rc, out) = c.run(&["boss"]);
    assert_eq!(rc, 2, "{out}");
    assert!(c.stdin().is_empty());
}

#[test]
fn the_secret_stays_secret_and_an_unreadable_one_refuses() {
    let c = Case::new("secret");
    let (rc, out) = c.run_env(&["--dry-run", "boss"], &[("STUB_SECRET_UNREADABLE", "1")]);
    assert_eq!(rc, 2, "{out}");
    assert!(
        out.contains("cannot read Secret boss-secrets key database-url in boss"),
        "{out}"
    );
    no_password(&out);
    // A URL of another shape, or another host, is refused — and never echoed with its password.
    write_file(
        &c.answers.join("secret.url"),
        "postgres://boss:s3cretpw0123@db.example.net:5432/algedonic",
    );
    let (rc, out) = c.run(&["--dry-run", "boss"]);
    assert_eq!(rc, 2, "{out}");
    assert!(
        out.contains("db.example.net, not this instance's postgres Service"),
        "{out}"
    );
    no_password(&out);
    write_file(&c.answers.join("secret.url"), "not-a-url");
    let (rc, out) = c.run(&["--dry-run", "boss"]);
    assert_eq!(rc, 2, "{out}");
    assert!(out.contains("not of the shape"), "{out}");
}

#[test]
fn a_plan_that_cannot_be_read_is_not_a_verdict() {
    let c = Case::new("plan-fail");
    let (rc, out) = c.run_env(&["--for-real", "boss"], &[("STUB_PLAN_FAIL", "1")]);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("cannot judge the candidates") && out.contains("gl_posting_rules"),
        "psql's words are on the packet:\n{out}"
    );
    assert!(!c.stdin().contains(":delete "), "nothing was deleted");
    no_password(&out);
}

#[test]
fn a_real_run_evicts_per_table_and_reads_the_plan_back() {
    let c = Case::new("for-real");
    let (rc, out) = c.run(&["--for-real", "boss"]);
    assert_eq!(rc, 0, "{out}");
    no_password(&out);
    let mut lines = out.lines();
    assert_eq!(
        lines.next().unwrap(),
        "retire-example-reference-rows: for-real namespace=boss db=algedonic tenant=algedonic declared=1 candidates=196 present=95 deletable=6 kept=3 deleted=6",
        "{out}"
    );
    let record: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(record["mode"], "for-real");
    assert_eq!(record["deleted"], 6);
    assert_eq!(
        record["declared_by_tenant"]["classes"],
        serde_json::json!(["employee:sales"])
    );
    let tables: Vec<&str> = record["deleted_by_table"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["table"].as_str().unwrap())
        .collect();
    assert_eq!(tables, ["companies", "locations", "gl_accounts", "classes"]);
    assert_eq!(
        record["read_back"]["classes"]["present"], 57,
        "the plan after the run is the read-back"
    );
    assert!(out.contains("deleted classes: employee:cto"), "{out}");
    assert!(
        out.contains("deleted companies: brewery used-device-shop"),
        "{out}"
    );
    // Three psql sessions: the plan, the eviction, the read-back — the
    // eviction stream carries the four tagged transactions in order.
    let stdin = c.stdin();
    assert_eq!(stdin.matches("=== stdin ===").count(), 3, "{stdin}");
    let evict = stdin.split("=== stdin ===").nth(1).unwrap();
    assert!(
        !evict.contains("default_transaction_read_only"),
        "the eviction session is not read-only"
    );
    assert!(
        !evict.contains(r#"{"subject_kind":"employee","code":"sales""#),
        "the eviction judges the same subtracted set as the plan"
    );
    let tags: Vec<&str> = evict
        .lines()
        .filter(|l| l.starts_with("-- retire-example-reference-rows:delete "))
        .collect();
    assert_eq!(
        tags.len(),
        7,
        "companies, locations, departments, tax_kinds, sales_tax_rates, gl_accounts, classes"
    );
    assert_eq!(
        evict.matches("\nBEGIN;\n").count(),
        7,
        "one transaction per table"
    );
    let readback = stdin.split("=== stdin ===").nth(2).unwrap();
    assert!(
        readback.contains("SET default_transaction_read_only = on;"),
        "the read-back is read-only"
    );
}

#[test]
fn a_failed_transaction_is_failed_with_the_completed_tables_recorded() {
    let c = Case::new("delete-fail");
    let (rc, out) = c.run_env(&["--for-real", "boss"], &[("STUB_DELETE_FAIL", "1")]);
    assert_eq!(rc, 1, "{out}");
    let first = out.lines().next().unwrap();
    assert!(
        first.starts_with("retire-example-reference-rows: for-real namespace=boss db=algedonic "),
        "{out}"
    );
    assert!(
        first.ends_with("deleted=3 FAILED"),
        "the verdict counts what completed and says FAILED: {first}"
    );
    assert!(
        out.contains("violates foreign key constraint"),
        "psql's words are on the packet:\n{out}"
    );
    assert!(
        out.contains("transaction FAILED and rolled back whole"),
        "{out}"
    );
    let record: serde_json::Value = serde_json::from_str(out.lines().nth(1).unwrap()).unwrap();
    let tables: Vec<&str> = record["deleted_by_table"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["table"].as_str().unwrap())
        .collect();
    assert_eq!(
        tables,
        ["companies", "locations"],
        "the tables that completed before the failure"
    );
    no_password(&out);
}

#[test]
fn an_instance_whose_tenant_checkout_cannot_be_read_is_refused() {
    // No checkout staged for the instance: the converge has not run
    // since the flip, or the tenants directory is elsewhere.
    let c = Case::new("no-checkout");
    std::fs::remove_dir_all(c.tenants.join("prod")).unwrap();
    let (rc, out) = c.run(&["--for-real", "boss"]);
    assert_eq!(rc, 2, "{out}");
    assert!(
        out.contains("REFUSED")
            && out.contains("tenant checkout")
            && out.contains(c.tenants.join("prod").to_str().unwrap()),
        "names the checkout it could not read:\n{out}"
    );
    assert!(out.contains("Nothing was changed."));
    assert!(c.stdin().is_empty(), "nothing was read, nothing deleted");
    no_password(&out);

    // A directory that is not a tenant (no manifest) is the same refusal.
    let c = Case::new("no-manifest");
    std::fs::remove_file(c.tenants.join("prod/tenant.toml")).unwrap();
    let (rc, out) = c.run(&["--dry-run", "boss"]);
    assert_eq!(rc, 2, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains("tenant.toml"),
        "{out}"
    );
    assert!(c.stdin().is_empty());

    // A seed that cannot be parsed: the derivation cannot answer, and
    // the verb does not plan without it.
    let c = Case::new("broken-seed");
    write_file(
        &c.tenants.join("prod/seeds/classes.json"),
        "[{\"subject_kind\": \"employee\"",
    );
    let (rc, out) = c.run(&["--dry-run", "boss"]);
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("Nothing was changed."), "{out}");
    assert!(c.stdin().is_empty());

    // The delivered spelling (a ConfigMap-shaped checkout: tenant.toml
    // under seeds/) reads the same.
    let c = Case::new("delivered-spelling");
    std::fs::rename(
        c.tenants.join("prod/tenant.toml"),
        c.tenants.join("prod/seeds/tenant.toml"),
    )
    .unwrap();
    let (rc, out) = c.run(&["--dry-run", "boss"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("tenant=algedonic declared=1"), "{out}");
}

#[test]
fn the_verb_file_is_bounded_and_authorized() {
    let root = repo_root();
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("infra/ops/verbs/retire-example-reference-rows.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(verb["hosts"], serde_json::json!(["forge"]));
    assert_eq!(verb["argv"], serde_json::json!([SCRIPT, "{1}", "{2}"]));
    let about = verb["about"].as_str().unwrap();
    assert!(
        about.starts_with("MUTATING"),
        "the roster lint derives the mutating set from this word"
    );
    assert!(
        about.contains("David") && about.contains("718ac982"),
        "names the authorization and the packet"
    );
    assert!(
        about.contains("PASSWORD IS NEVER PRINTED")
            && about.contains("DELETABLE ONLY WHEN UNREFERENCED")
    );
    assert!(
        about.contains("86835bf9") && about.contains("declared_by_tenant"),
        "the verb says the instance's own tenant is subtracted first, and names the record key"
    );
    let params = verb["params"].as_array().unwrap();
    assert_eq!(params[0]["name"], "mode");
    assert_eq!(
        params[0]["one_of"],
        serde_json::json!(["--dry-run", "--for-real"])
    );
    assert_eq!(params[1]["name"], "namespace");
    assert_eq!(params[1]["pattern"], "^boss(-[a-z0-9]+)*$");
    assert!(
        verb["timeout"].as_u64().unwrap() >= 120,
        "two plans and four transactions through kubectl exec exceed the runner's 30 s"
    );
    let script: &Path = &root.join(SCRIPT);
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(script).unwrap().permissions());
    assert!(mode & 0o111 != 0, "the script is executable");
}
