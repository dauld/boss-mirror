//! `infra/postgres/example-reference-rows.sh` is RUN, not read — the one
//! derivation of "which reference rows are the example tenants'" that
//! boss-init (a fresh instance's first start) and the forge verb
//! retire-example-reference-rows both call (backlog 718ac982; design
//! e2580840 car 3). Nothing here needs a database; the SQL it prints
//! is exercised against the real schema in example_reference_rows_sql.rs.
//!
//! What each case pins:
//!
//!   * THE CANDIDATE SET IS READ FROM THE SEEDS: every class row in
//!     examples/*/seeds/classes.{json,toml}, every `[[location]]` id,
//!     every `[[account]]` code, every tenant id — counted here by an
//!     independent read of the same files, so an extraction that
//!     silently dropped a file would show as a count.
//!   * THE PLATFORM'S ROWS ARE IN NO EXAMPLE SEED. The set is what an
//!     instance may lose, so the rows the platform's own baseline and
//!     schema need (the bootstrap admin's role/department/location,
//!     the operator baseline's hire, the account_type column default)
//!     must never be candidates — a seed that carried `platform-admin`
//!     would have a fresh company instance evict the row its first
//!     login needs.
//!   * `boot` DECIDES BY TENANT ID: an example tenant keeps its rows
//!     (exit 3), a company's tenant directory evicts (exit 0), and no
//!     directory / no manifest keeps (exit 3) — never an eviction by
//!     default.
//!   * A SEED ATTRIBUTE THE SCRIPT CANNOT JUDGE IS A REFUSAL (exit 4),
//!     not a delete on a guess.
//!   * THE INSTANCE'S OWN TENANT IS SUBTRACTED BEFORE JUDGING (backlog
//!     86835bf9, measured 2026-09-18 on the first real run, ops-request
//!     8522ad76: four departments Algedonic declares under codes the
//!     device shop also uses — finance, marketing, sales, support — were
//!     unreferenced and went with the residue). `plan-sql` and
//!     `delete-sql` take the tenant directory, every id/code it declares
//!     leaves the candidate set, `seeds <dir>` names what left as
//!     `declared_by_tenant`, and a tenant that cannot be read is a
//!     refusal (exit 4), never a plan without it.
//!   * THE SQL SHAPES: the plan is one read-only SELECT tagged for the
//!     forge verb's stub; the eviction is one transaction per table in
//!     dependency order, each printing one JSON line.
//!   * THE TWO DOORS CALL IT: init.sh runs `boot` and `delete-sql` on
//!     the first start only, and boss.yaml hands the init container
//!     the same BOSS_TENANT_DIR and tenant mount the boss container has.

use boss_testing::{create_dir, repo_root, scratch_dir, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/postgres/example-reference-rows.sh";

fn run(args: &[&str], examples: Option<&Path>) -> (i32, String, String) {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(SCRIPT)).args(args);
    if let Some(e) = examples {
        cmd.env("BOSS_EXAMPLES_DIR", e);
    }
    let out = cmd.output().expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// write_file with the parent directory created — the seeds/ spelling.
fn write(path: &Path, body: &str) {
    create_dir(path.parent().unwrap());
    write_file(path, body);
}

/// A company's tenant directory declaring nothing an example does —
/// the tenant `plan-sql` / `delete-sql` are given when the case is
/// about something else.
fn plain_tenant(name: &str) -> PathBuf {
    let t = scratch_dir(&format!("example-reference-rows-tenant-{name}"));
    write(
        &t.join("tenant.toml"),
        "[meta]\ntenant_id = \"acme\"\ndisplay_name = \"Acme\"\n",
    );
    t
}

fn toml_headers(p: &Path, header: &str) -> usize {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .filter(|l| l.trim() == format!("[[{header}]]"))
        .count()
}

/// (subject_kind, code) of every class row an example seed carries,
/// read with the product's own TOML/JSON parsers rather than the
/// script's awk — the independent read.
fn seeded_classes() -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let examples = repo_root().join("examples");
    for entry in std::fs::read_dir(&examples).unwrap() {
        let d = entry.unwrap().path();
        let json = d.join("seeds/classes.json");
        if json.is_file() {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
            for r in v.as_array().unwrap() {
                rows.push((
                    r["subject_kind"].as_str().unwrap().to_string(),
                    r["code"].as_str().unwrap().to_string(),
                ));
            }
        }
        let t = d.join("seeds/classes.toml");
        if t.is_file() {
            let v: toml::Value = toml::from_str(&std::fs::read_to_string(&t).unwrap()).unwrap();
            for r in v["class"].as_array().unwrap() {
                rows.push((
                    r["subject_kind"].as_str().unwrap().to_string(),
                    r["code"].as_str().unwrap().to_string(),
                ));
            }
        }
    }
    rows.sort();
    rows.dedup();
    rows
}

#[test]
fn seeds_is_the_example_tenants_own_rows_counted_independently() {
    let (rc, out, err) = run(&["seeds"], None);
    assert_eq!(rc, 0, "seeds: {err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one JSON line");
    let root = repo_root();
    let brewery = root.join("examples/brewery/seeds");

    let classes = v["classes"].as_array().unwrap();
    assert_eq!(
        classes.len(),
        seeded_classes().len(),
        "classes = every row of every examples/*/seeds/classes.*, deduplicated on (subject_kind, code)"
    );
    assert!(
        classes.iter().all(|c| c["member_attribute"].is_string()),
        "every candidate class carries its member_attribute — the reference map keys on it"
    );

    assert_eq!(
        v["locations"].as_array().unwrap().len(),
        toml_headers(&brewery.join("locations.toml"), "location"),
        "locations = the brewery's [[location]] ids (the device shop declares none: it sits at the platform's default locations)"
    );
    assert_eq!(
        v["gl_accounts"].as_array().unwrap().len(),
        toml_headers(&brewery.join("chart_of_accounts.toml"), "account"),
        "gl_accounts = the brewery's [[account]] codes"
    );
    let companies: Vec<&str> = v["companies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(
        companies,
        ["brewery", "used-device-shop"],
        "companies = each example manifest's [meta] tenant_id"
    );
    // The tax regime (backlog 7f163e58): the brewery's tax.toml carries
    // the rows 40-ledger.sql seeded — five kinds, 27 states.
    assert_eq!(
        v["tax_kinds"].as_array().unwrap().len(),
        toml_headers(&brewery.join("tax.toml"), "tax_kind"),
        "tax_kinds = the brewery's [[tax_kind]] kinds"
    );
    assert_eq!(v["tax_kinds"].as_array().unwrap().len(), 5);
    assert_eq!(
        v["sales_tax_rates"].as_array().unwrap().len(),
        toml_headers(&brewery.join("tax.toml"), "sales_tax_rate"),
        "sales_tax_rates = the brewery's [[sales_tax_rate]] states"
    );
    assert_eq!(v["sales_tax_rates"].as_array().unwrap().len(), 27);
    let sources: Vec<&str> = v["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    for s in [
        "brewery/seeds/classes.json",
        "brewery/seeds/locations.toml",
        "brewery/seeds/chart_of_accounts.toml",
        "brewery/seeds/tax.toml",
        "used-device-shop/seeds/classes.toml",
    ] {
        assert!(sources.contains(&s), "sources names {s}: {sources:?}");
    }
}

/// The rows the platform itself needs, derived from the files that
/// need them: the bootstrap admin's row (boss-people
/// operator_baseline.rs `bootstrap_admin_row`), the operator
/// baseline's hire (infra/operator-baseline/operator_hires.toml), and
/// the account_type column's default (22-accounts.sql). None may be an
/// example candidate.
fn platform_named_class_rows() -> Vec<(String, String)> {
    let root = repo_root();
    let mut rows = Vec::new();
    let src =
        std::fs::read_to_string(root.join("crates/modules/boss-people/src/operator_baseline.rs"))
            .unwrap();
    let body = src
        .split("pub fn bootstrap_admin_row")
        .nth(1)
        .expect("bootstrap_admin_row exists");
    let body = body.split("\n}").next().unwrap();
    for (field, kind) in [
        ("role", "employee"),
        ("department", "employee"),
        ("employment_type", "employee"),
        ("status", "employee"),
    ] {
        let needle = format!("{field}: Some(\"");
        let v = body
            .split(&needle)
            .nth(1)
            .unwrap_or_else(|| panic!("bootstrap_admin_row sets {field}"));
        rows.push((kind.to_string(), v.split('"').next().unwrap().to_string()));
    }
    let hires: toml::Value = toml::from_str(
        &std::fs::read_to_string(root.join("infra/operator-baseline/operator_hires.toml")).unwrap(),
    )
    .unwrap();
    for h in hires["hire"].as_array().unwrap() {
        for field in ["role", "department", "employment_type", "status"] {
            rows.push(("employee".into(), h[field].as_str().unwrap().to_string()));
        }
    }
    let accounts =
        std::fs::read_to_string(root.join("infra/postgres/schema/22-accounts.sql")).unwrap();
    let dflt = accounts
        .lines()
        .find(|l| l.contains("account_type") && l.contains("DEFAULT"))
        .unwrap();
    let dflt = dflt
        .split("DEFAULT '")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    rows.push(("account".into(), dflt.to_string()));
    rows.sort();
    rows.dedup();
    rows
}

#[test]
fn the_platforms_own_rows_are_in_no_example_seed() {
    let seeded = seeded_classes();
    for (kind, code) in platform_named_class_rows() {
        assert!(
            !seeded.contains(&(kind.clone(), code.clone())),
            "{kind}/{code} is what the platform's baseline or schema needs, and an example seed carries it — a fresh company instance would evict it at boot"
        );
    }
    // The platform's three default locations, where the baseline
    // hires (loc-hq) and the two placeholder buckets 01-registries.sql
    // seeds beside it.
    let (rc, out, _) = run(&["seeds"], None);
    assert_eq!(rc, 0);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    let locations: Vec<&str> = v["locations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    for l in ["loc-hq", "loc-field-default", "loc-remote-default"] {
        assert!(
            !locations.contains(&l),
            "{l} is a platform default location and an example seed declares it"
        );
    }
}

#[test]
fn boot_keeps_for_an_example_and_evicts_for_a_company() {
    let root = repo_root();
    let (rc, out, _) = run(
        &["boot", root.join("examples/brewery").to_str().unwrap()],
        None,
    );
    assert_eq!(rc, 3, "an example tenant keeps its rows: {out}");
    assert!(
        out.starts_with("keep: tenant brewery is an example"),
        "{out}"
    );

    let (rc, out, _) = run(
        &[
            "boot",
            root.join("examples/used-device-shop").to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("used-device-shop is an example"), "{out}");

    let company = scratch_dir("example-reference-rows-company");
    write(
        &company.join("tenant.toml"),
        "[meta]\ntenant_id = \"algedonic\"\ndisplay_name = \"Algedonic, LLC\"\n",
    );
    let (rc, out, _) = run(&["boot", company.to_str().unwrap()], None);
    assert_eq!(rc, 0, "a company's tenant evicts: {out}");
    assert!(
        out.starts_with("evict: tenant algedonic is not an example"),
        "{out}"
    );

    // The delivered spelling: seeds/tenant.toml (a ConfigMap mount).
    let delivered = scratch_dir("example-reference-rows-delivered");
    write(
        &delivered.join("seeds/tenant.toml"),
        "[meta]\ntenant_id = \"acme\"\n",
    );
    let (rc, out, _) = run(&["boot", delivered.to_str().unwrap()], None);
    assert_eq!(rc, 0, "{out}");
    assert!(out.starts_with("evict: tenant acme"), "{out}");

    let (rc, out, _) = run(&["boot", ""], None);
    assert_eq!(rc, 3, "no directory named keeps: {out}");
    assert!(out.starts_with("keep: no tenant directory named"), "{out}");
    let (rc, out, _) = run(&["boot", "/nonexistent/tenant"], None);
    assert_eq!(rc, 3, "{out}");
    assert!(
        out.starts_with("keep: /nonexistent/tenant is not a directory"),
        "{out}"
    );
    let empty = scratch_dir("example-reference-rows-empty");
    let (rc, out, _) = run(&["boot", empty.to_str().unwrap()], None);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("holds no tenant.toml"), "{out}");
}

#[test]
fn an_attribute_the_script_cannot_judge_is_a_refusal() {
    let examples = scratch_dir("example-reference-rows-unmapped");
    let t = examples.join("acme");
    write(
        &t.join("seeds/tenant.toml"),
        "[meta]\ntenant_id = \"acme\"\n",
    );
    write(
        &t.join("seeds/classes.toml"),
        "[[class]]\nsubject_kind = \"employee\"\nmember_attribute = \"role\"\ncode = \"ceo\"\ndisplay_name = \"CEO\"\n\n[[class]]\nsubject_kind = \"widget\"\nmember_attribute = \"colour\"\ncode = \"red\"\ndisplay_name = \"Red\"\n",
    );
    write(
        &t.join("seeds/locations.toml"),
        "[[location]]\nid = \"loc-x\"\nname = \"X\"\nkind = \"remote\"\ntimezone = \"UTC\"\n",
    );
    write(
        &t.join("seeds/chart_of_accounts.toml"),
        "[[account]]\ncode = \"1000\"\nname = \"Bank\"\nkind = \"asset\"\nnormal_balance = \"debit\"\n",
    );
    write(
        &t.join("seeds/tax.toml"),
        "[[tax_kind]]\nkind = \"sales\"\nliability_account = \"1000\"\n\n[[sales_tax_rate]]\nstate = \"CA\"\njurisdiction = \"US-CA\"\nrate_bps = 725\n",
    );
    let (rc, out, _) = run(&["seeds"], Some(&examples));
    assert_eq!(rc, 0, "seeds reads the set: {out}");
    let tenant = plain_tenant("unmapped");
    let tenant = tenant.to_str().unwrap();
    let (rc, _, err) = run(&["plan-sql", tenant], Some(&examples));
    assert_eq!(rc, 4, "an unmapped attribute cannot be judged: {err}");
    assert!(err.contains("widget|colour"), "names the attribute: {err}");
    let (rc, _, err) = run(&["delete-sql", tenant], Some(&examples));
    assert_eq!(rc, 4, "{err}");
}

#[test]
fn the_sql_shapes_are_a_read_only_plan_and_one_transaction_per_table() {
    let tenant = plain_tenant("shapes");
    let tenant = tenant.to_str().unwrap();
    let (rc, plan, err) = run(&["plan-sql", tenant], None);
    assert_eq!(rc, 0, "{err}");
    assert!(
        plan.starts_with("-- retire-example-reference-rows:plan\n"),
        "the plan is tagged for the verb's stub"
    );
    assert!(!plan.contains("DELETE"), "the plan deletes nothing");
    assert_eq!(
        plan.matches("SELECT json_build_object(\n    'companies'")
            .count(),
        1,
        "one SELECT printing one document"
    );

    let (rc, del, err) = run(&["delete-sql", tenant], None);
    assert_eq!(rc, 0, "{err}");
    let tags: Vec<&str> = del
        .lines()
        .filter(|l| l.starts_with("-- retire-example-reference-rows:delete "))
        .collect();
    assert_eq!(
        tags,
        [
            "-- retire-example-reference-rows:delete companies",
            "-- retire-example-reference-rows:delete locations",
            "-- retire-example-reference-rows:delete tax_kinds",
            "-- retire-example-reference-rows:delete sales_tax_rates",
            "-- retire-example-reference-rows:delete gl_accounts",
            "-- retire-example-reference-rows:delete classes"
        ],
        "dependency order: a tax kind FKs its accounts, so the tax tables go before gl_accounts; a location's kind is a class, so classes go last"
    );
    assert_eq!(
        del.matches("\nBEGIN;\n").count(),
        6,
        "one transaction per table"
    );
    assert_eq!(del.matches("\nCOMMIT;\n").count(), 6);
    for t in [
        "companies",
        "locations",
        "tax_kinds",
        "sales_tax_rate_by_state",
        "gl_accounts",
        "classes",
    ] {
        assert!(
            del.contains(&format!("DELETE FROM {t} ")),
            "deletes from {t}"
        );
    }
    // Every reference the header promises is a reason the SQL can name.
    for reason in [
        "employees.role",
        "employees.department",
        "policy_rules.role",
        "locations.kind",
        "accounts.account_type",
        "asset_models.category",
        "classes.parent_code",
        "employees.location",
        "requisitions.location",
        "locations.parent_id",
        "jobs.subject_id",
        "gl_journal_lines.account_id",
        "tax_kinds",
        "tax_filings.kind",
        "gl_posting_rules.lines",
    ] {
        assert!(
            plan.contains(&format!("'{reason}'")),
            "the plan can name {reason}"
        );
    }
}

#[test]
fn the_two_doors_call_the_one_derivation() {
    let root = repo_root();
    let init = std::fs::read_to_string(root.join("infra/oss-quickstart/init.sh")).unwrap();
    let first_start = init
        .split("everything below is FIRST START ONLY")
        .nth(1)
        .expect("init.sh keeps its first-start gate");
    assert!(
        first_start.contains("example-reference-rows.sh"),
        "init.sh runs the derivation AFTER the first-start gate, never on a restart"
    );
    assert!(
        first_start.contains("\" boot \"${BOSS_TENANT_DIR:-}\""),
        "init.sh asks `boot` about BOSS_TENANT_DIR — the launcher's tenant directory"
    );
    assert!(
        first_start.contains("delete-sql \"$BOSS_TENANT_DIR\" | psql"),
        "init.sh streams delete-sql into psql WITH the tenant directory, so the tenant's own declarations are subtracted (86835bf9)"
    );
    assert!(
        !init
            .split("everything below is FIRST START ONLY")
            .next()
            .unwrap()
            .contains("delete-sql"),
        "no eviction before the first-start gate"
    );

    let manifest = std::fs::read_to_string(root.join("infra/cluster/manifests/boss.yaml")).unwrap();
    let init_block = manifest
        .split("- name: boss-init")
        .nth(1)
        .unwrap()
        .split("containers:")
        .next()
        .unwrap();
    assert!(
        init_block.contains("- {name: BOSS_TENANT_DIR, value: /opt/boss/tenant}"),
        "the init container reads the same BOSS_TENANT_DIR as the boss container"
    );
    assert!(
        init_block
            .contains("- {name: boss-tenant, mountPath: /opt/boss/tenant/seeds, readOnly: true}"),
        "and mounts the delivered tenant to read its manifest"
    );

    let verb =
        std::fs::read_to_string(root.join("infra/forge/retire-example-reference-rows.sh")).unwrap();
    assert!(
        verb.contains("infra/postgres/example-reference-rows.sh"),
        "the forge verb reads the same derivation"
    );
    assert!(
        verb.contains("\"$DERIVE\" plan-sql \"$TENANT_CHECKOUT\"")
            && verb.contains("\"$DERIVE\" delete-sql \"$TENANT_CHECKOUT\""),
        "plan and delete both come from it, with the instance's tenant checkout (86835bf9)"
    );
    let dockerfile = std::fs::read_to_string(root.join("infra/oss-quickstart/Dockerfile")).unwrap();
    assert!(
        dockerfile.contains("COPY infra/postgres /opt/boss/infra/postgres"),
        "the image ships infra/postgres whole, so boss-init finds the derivation at /opt/boss/infra/postgres"
    );
    assert!(
        dockerfile.contains("COPY examples /opt/boss/examples"),
        "and the examples it reads"
    );
    let derive: PathBuf = root.join(SCRIPT);
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&derive).unwrap().permissions());
    assert!(
        mode & 0o111 != 0,
        "the derivation is executable (init.sh and the verb exec it)"
    );
}

/// The measured hole (86835bf9): a tenant declaring a code an example
/// also declares. The fixture re-declares the device shop's `sales`
/// department, the brewery's taproom location and its `1100` account
/// — beside its own rows, which are no example's and change nothing.
fn redeclaring_tenant(name: &str) -> PathBuf {
    let t = scratch_dir(&format!("example-reference-rows-redeclares-{name}"));
    write(
        &t.join("tenant.toml"),
        "[meta]\ntenant_id = \"algedonic\"\ndisplay_name = \"Algedonic, LLC\"\n",
    );
    write(
        &t.join("seeds/classes.json"),
        r#"[
  {"subject_kind": "employee", "code": "sales", "display_name": "Sales", "member_attribute": "department"},
  {"subject_kind": "employee", "code": "engineering", "display_name": "Engineering", "member_attribute": "department"}
]"#,
    );
    write(
        &t.join("seeds/locations.toml"),
        "[[location]]\nid = \"loc-brewery-taproom\"\nname = \"The taproom we bought\"\nkind = \"hq\"\ntimezone = \"UTC\"\n\n[[location]]\nid = \"loc-algedonic-hq\"\nname = \"HQ\"\nkind = \"hq\"\ntimezone = \"UTC\"\n",
    );
    write(
        &t.join("seeds/chart_of_accounts.toml"),
        "[[account]]\ncode = \"1100\"\nname = \"Accounts receivable\"\nkind = \"asset\"\nnormal_balance = \"debit\"\n\n[[account]]\ncode = \"7100\"\nname = \"Hosting\"\nkind = \"expense\"\nnormal_balance = \"debit\"\n",
    );
    // The brewery's `sales` kind and its CA rate, re-declared (backlog
    // 7f163e58), beside a kind and a state that are no example's.
    write(
        &t.join("seeds/tax.toml"),
        "[[tax_kind]]\nkind = \"sales\"\nliability_account = \"1100\"\n\n[[tax_kind]]\nkind = \"gross-receipts\"\nliability_account = \"1100\"\n\n[[sales_tax_rate]]\nstate = \"CA\"\njurisdiction = \"US-CA\"\nrate_bps = 725\n\n[[sales_tax_rate]]\nstate = \"HI\"\njurisdiction = \"US-HI\"\nrate_bps = 400\n",
    );
    t
}

fn strs(v: &serde_json::Value, k: &str) -> Vec<String> {
    v[k].as_array()
        .unwrap_or_else(|| panic!("{k} is a list in {v}"))
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn class_keys(v: &serde_json::Value) -> Vec<String> {
    v["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            format!(
                "{}:{}",
                c["subject_kind"].as_str().unwrap(),
                c["code"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
fn a_row_the_tenant_declares_leaves_the_candidate_set_and_is_named() {
    let tenant = redeclaring_tenant("seeds");
    let tenant = tenant.to_str().unwrap();
    let (rc, plain, err) = run(&["seeds"], None);
    assert_eq!(rc, 0, "{err}");
    let plain: serde_json::Value = serde_json::from_str(plain.trim()).unwrap();
    let (rc, out, err) = run(&["seeds", tenant], None);
    assert_eq!(rc, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one JSON line");

    assert!(class_keys(&plain).contains(&"employee:sales".to_string()));
    assert!(
        !class_keys(&v).contains(&"employee:sales".to_string()),
        "the department the tenant declares is not a candidate: {out}"
    );
    assert_eq!(
        class_keys(&v).len(),
        class_keys(&plain).len() - 1,
        "and only that row left the classes"
    );
    assert!(!strs(&v, "locations").contains(&"loc-brewery-taproom".to_string()));
    assert!(strs(&v, "locations").contains(&"loc-brewery-brewhouse".to_string()));
    assert!(!strs(&v, "gl_accounts").contains(&"1100".to_string()));
    assert!(strs(&v, "gl_accounts").contains(&"1000".to_string()));
    assert_eq!(strs(&v, "companies"), strs(&plain, "companies"));
    assert!(!strs(&v, "tax_kinds").contains(&"sales".to_string()));
    assert!(strs(&v, "tax_kinds").contains(&"income".to_string()));
    assert!(!strs(&v, "sales_tax_rates").contains(&"CA".to_string()));
    assert!(strs(&v, "sales_tax_rates").contains(&"TX".to_string()));

    let d = &v["declared_by_tenant"];
    assert_eq!(
        d["tenant"], "algedonic",
        "the record names the tenant: {out}"
    );
    assert_eq!(
        strs(d, "classes"),
        ["employee:sales"],
        "what was subtracted, and only that — the tenant's own rows are no example's"
    );
    assert_eq!(strs(d, "locations"), ["loc-brewery-taproom"]);
    assert_eq!(strs(d, "gl_accounts"), ["1100"]);
    assert_eq!(strs(d, "companies"), Vec::<String>::new());
    assert_eq!(strs(d, "tax_kinds"), ["sales"]);
    assert_eq!(strs(d, "sales_tax_rates"), ["CA"]);
    assert_eq!(d["directory"], tenant);
    assert!(
        plain["declared_by_tenant"].is_null(),
        "without a tenant, seeds is the raw example set"
    );

    // The SQL embeds the subtracted set: the row is in no candidate
    // list the judgement reads, so nothing can delete it.
    let (rc, plan, err) = run(&["plan-sql", tenant], None);
    assert_eq!(rc, 0, "{err}");
    let (rc, del, err) = run(&["delete-sql", tenant], None);
    assert_eq!(rc, 0, "{err}");
    for sql in [&plan, &del] {
        assert!(
            !sql.contains(r#"{"subject_kind":"employee","code":"sales""#),
            "employee:sales is not a candidate in the SQL"
        );
        assert!(sql.contains(r#"{"subject_kind":"employee","code":"cto""#));
        assert!(!sql.contains(r#""loc-brewery-taproom""#));
        assert!(!sql.contains(r#""1100""#));
        assert!(
            !sql.contains(r#""CA""#),
            "the re-declared state is not a candidate"
        );
        assert!(sql.contains(r#""TX""#));
        let kinds = sql
            .split(r#""tax_kinds":["#)
            .nth(1)
            .and_then(|rest| rest.split(']').next())
            .expect("the SQL embeds the tax_kinds candidates");
        assert!(
            !kinds.contains(r#""sales""#) && kinds.contains(r#""income""#),
            "the re-declared kind is not a candidate: {kinds}"
        );
    }
    // A tenant declaring nothing an example does leaves the set whole.
    let plain_t = plain_tenant("whole");
    let (rc, out, _) = run(&["seeds", plain_t.to_str().unwrap()], None);
    assert_eq!(rc, 0);
    let w: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(w["classes"], plain["classes"]);
    assert_eq!(
        strs(&w["declared_by_tenant"], "classes"),
        Vec::<String>::new()
    );
    assert_eq!(w["declared_by_tenant"]["tenant"], "acme");
}

#[test]
fn a_tenant_that_cannot_be_read_is_a_refusal_not_a_plan_without_it() {
    for mode in ["plan-sql", "delete-sql"] {
        let (rc, out, err) = run(&[mode], None);
        assert_eq!(
            rc, 2,
            "{mode} without a tenant directory is usage, never a plan without one: {err}"
        );
        assert!(out.is_empty(), "no SQL printed: {out}");
        let (rc, out, err) = run(&[mode, "/nonexistent/tenant"], None);
        assert_eq!(rc, 4, "{mode}: {err}");
        assert!(
            err.contains("CANNOT ANSWER") && err.contains("/nonexistent/tenant"),
            "names the directory: {err}"
        );
        assert!(out.is_empty(), "{out}");
        let empty = scratch_dir(&format!("example-reference-rows-no-manifest-{mode}"));
        let (rc, _, err) = run(&[mode, empty.to_str().unwrap()], None);
        assert_eq!(rc, 4, "{mode}: {err}");
        assert!(err.contains("tenant.toml"), "{err}");
    }
    let (rc, _, err) = run(&["seeds", "/nonexistent/tenant"], None);
    assert_eq!(rc, 4, "{err}");
    // A seed the tenant carries that cannot be parsed is the same refusal.
    let broken = scratch_dir("example-reference-rows-broken-tenant");
    write(
        &broken.join("tenant.toml"),
        "[meta]\ntenant_id = \"acme\"\n",
    );
    write(
        &broken.join("seeds/classes.json"),
        "[{\"subject_kind\": \"employee\"",
    );
    let (rc, _, err) = run(&["plan-sql", broken.to_str().unwrap()], None);
    assert_eq!(rc, 4, "{err}");
    assert!(err.contains("CANNOT ANSWER"), "{err}");
}
