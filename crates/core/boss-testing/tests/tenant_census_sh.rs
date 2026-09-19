//! `infra/forge/tenant-census.sh` is RUN, not read — against a stubbed
//! `kubectl` that records every argv it receives and answers canned
//! rows — so every property below is one the script actually has.
//! Nothing here touches a cluster or a database.
//!
//! WHY THE VERB EXISTS (backlog d07dcc2b, 2026-09-16). The cutover
//! (ffc83387) evicts the brewery's reference data from prod and trims
//! the simulated packets, and the eviction must REFUSE if any real
//! packet references a row it would delete. Nothing could measure that:
//! the brewery's footprint spans a dozen seed files and the projections
//! 7,437 closed simulated packets left, the only readers are per-service
//! per-actor APIs, the dev pod cannot exec into the cluster, and a
//! port-forward to the prod database is the trap the
//! never-test-against-prod rule names. So the measurement is an ops verb
//! on the forge, through the same kubectl + kubeconfig resolution the
//! deploy runner and delete-orphan-object use.
//!
//! What each case pins:
//!
//!   * READ-ONLY, by construction. Every psql the script issues opens
//!     with `SET default_transaction_read_only = on`, and every SQL block
//!     it carries begins with SELECT or WITH and contains no mutating
//!     keyword. This is the property the operator relies on when running
//!     a verb against the production database through an audited door.
//!   * THE SEED KEYS COME FROM THE CHECKOUT, never a typed list: each
//!     set `--seeds` prints has the count an independent read of the
//!     same file gives (`jq length`, or the count of `[[table]]` headers
//!     for TOML, which has no jq).
//!   * THE DOCUMENT'S SHAPE, from the stub: one JSON object, the five
//!     sections the packet asked for plus `log`, the kubectl argv the
//!     brief named (namespace, workload, container, user, database),
//!     and the seed keys riding INSIDE the SQL rather than in a second
//!     round trip.
//!   * THE LOG SECTION MEASURES THE MARKER, NOT THE PACKET (backlog
//!     e2604427, 2026-09-16). Before a log-filtered rebuild can trim
//!     the simulated projections (design e652c7c6 option 1), one fact
//!     has to be measured: what marks a simulated EVENT. Every
//!     audit_log payload carries `_simulated`, but that bool resolved
//!     from the CLOCK MODE at publish time, while the packet partition
//!     (`jobs.partition`, 508cc38c) is per packet — so under the sim
//!     clock a real packet's events may read `_simulated: true`. The
//!     `log` query cross-tabs the marker against the partition of the
//!     job the row names, reads `_partition` (the stamp the rebuilder
//!     prefers, boss-core partition.rs) beside it, counts the rows
//!     that name no job at all, and reads event_outbox / event_facts
//!     for the same marker so a reader can see whether they mirror.
//!   * EXIT 4 NAMES THE QUERY and prints NO document: a partial census
//!     that looks whole is the failure mode the conformance-report
//!     shape exists to prevent.
//!   * THE VERB FILE serves the forge only, takes no params, and says
//!     READ-ONLY — the word the bounded-verbs lint derives its roster
//!     from is MUTATING, and this file must not carry it.
//!   * THE TARGET IS THE MANIFEST'S: the namespace, StatefulSet,
//!     container, user and database the script exec's into are the ones
//!     `infra/cluster/manifests/boss.yaml` declares (CLAUDE.md §9a — a
//!     fact that lives twice gets an equality test).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

const CANNOT_ANSWER: i32 = 4;

fn script() -> PathBuf {
    repo_root().join("infra/forge/tenant-census.sh")
}

fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("tenant-census-{case}"))
}

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// The stub kubectl: appends its argv (one word per line, a `=== call ===` line between
/// calls) to `$STUB_LOG`, finds the `-- census:<name>` tag in its last
/// word, and prints `$STUB_ANSWERS/<name>.json`. A tag with no answer
/// file is the "database said no" leg.
fn write_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("kubectl");
    boss_testing::write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
{ printf '%s\n' "$@"; echo '=== call ==='; } >> "$STUB_LOG"
last="${@: -1}"
tag=$(printf '%s' "$last" | sed -n 's/^-- census:\([a-z_]*\).*/\1/p' | head -1)
[ -n "$tag" ] || { echo "stub kubectl: no census tag in the last argv word" >&2; exit 1; }
[ -f "$STUB_ANSWERS/$tag.json" ] || { echo "stub kubectl: ERROR:  relation \"$tag\" does not exist" >&2; exit 1; }
cat "$STUB_ANSWERS/$tag.json"
"#,
    );
    stub
}

fn answers(dir: &Path) -> PathBuf {
    let a = dir.join("answers");
    boss_testing::create_dir(&a);
    boss_testing::write_file(
        &a.join("row_counts.json"),
        r#"{"jobs": 7900, "steps": 41000, "employees": 409, "accounts": 81, "audit_log": 500000}"#,
    );
    boss_testing::write_file(
        &a.join("seed_owned.json"),
        r#"{"employees": {"key": "id", "seeded": 409, "present": 409, "sample": ["emp-aa-001"]}, "accounts": {"key": "name", "seeded": 50, "present": 80, "sample": ["acc-bigseed-0000"]}}"#,
    );
    boss_testing::write_file(
        &a.join("partitions.json"),
        r#"{"by_partition_column": {"jobs": {"real": 463, "simulated": 7437}}, "by_job_id": {"steps": {"real": 2000, "simulated": 39000}, "account_facts": {"unlinked": 12}}, "unknown": ["invoices", "shipments"]}"#,
    );
    boss_testing::write_file(
        &a.join("references.json"),
        r#"{"jobs.owner_id": {"count": 2, "ids": ["11111111-1111-1111-1111-111111111111", "22222222-2222-2222-2222-222222222222"]}, "jobs.subject_id": {"count": 0, "ids": []}, "steps.assignee_id": {"count": 0, "ids": []}, "steps.completed_by": {"count": 0, "ids": []}}"#,
    );
    boss_testing::write_file(
        &a.join("workflows.json"),
        r#"[{"owning_team": "brewery", "kinds": 37, "versions": 40, "active_versions": 37, "open_jobs": 12, "open_real_jobs": 0}, {"owning_team": "platform", "kinds": 20, "versions": 60, "active_versions": 20, "open_jobs": 300, "open_real_jobs": 300}]"#,
    );
    boss_testing::write_file(
        &a.join("log.json"),
        r#"{"audit_log": {"rows": 500000, "by_simulated": {"true": 490000, "false": 9000, "absent": 1000}, "by_partition_stamp": {"absent": 499000, "real": 1000}, "by_simulated_and_partition_stamp": {"true": {"absent": 490000}, "false": {"absent": 8000, "real": 1000}, "absent": {"absent": 1000}}, "by_prefix": {"jobs": {"true": 400000, "false": 5000}, "ledger": {"true": 90000, "false": 4000, "absent": 1000}}, "job_linked": {"rows": 405000, "cross_tab": {"true": {"simulated": 399000, "real": 900, "missing": 100}, "false": {"real": 5000}}, "missing_job": 100}, "unlinked": {"rows": 95000, "by_prefix": {"ledger": {"true": 90000, "false": 4000, "absent": 1000}}}}, "event_outbox": {"rows": 12, "pending": 0, "by_simulated": {"true": 12}, "note": "mirrors"}, "event_facts": {"rows": 500000, "by_simulated": {"true": 490000, "false": 9000, "absent": 1000}, "note": "projection"}}"#,
    );
    a
}

struct Run {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    log: String,
}

fn run(case: &str, args: &[&str], drop_answer: Option<&str>) -> Run {
    let dir = scratch(case);
    let stub = write_stub(&dir);
    let a = answers(&dir);
    if let Some(name) = drop_answer {
        std::fs::remove_file(a.join(format!("{name}.json"))).unwrap();
    }
    let log = dir.join("argv.log");
    let out = Command::new("bash")
        .arg(script())
        .args(args)
        .env("BOSS_KUBECTL", stub.to_str().unwrap())
        .env("STUB_LOG", &log)
        .env("STUB_ANSWERS", &a)
        .env_remove("KUBECONFIG")
        .output()
        .expect("bash runs");
    Run {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        log: std::fs::read_to_string(&log).unwrap_or_default(),
    }
}

/// The SQL blocks the script carries, each as the text between a
/// `<<'SQL'` opener and its closing `SQL` line.
fn sql_blocks() -> Vec<String> {
    let src = std::fs::read_to_string(script()).unwrap();
    let mut blocks = Vec::new();
    let mut cur: Option<String> = None;
    for line in src.lines() {
        match &mut cur {
            None if line.contains("<<'SQL'") || line.contains("<<SQL") => cur = Some(String::new()),
            Some(b) if line.trim() == "SQL" => blocks.push(std::mem::take(b)),
            Some(b) => {
                b.push_str(line);
                b.push('\n');
            }
            None => {}
        }
        if line.trim() == "SQL" {
            cur = None;
        }
    }
    blocks
}

#[test]
fn every_sql_block_is_a_select_and_the_session_is_read_only() {
    let blocks = sql_blocks();
    assert!(
        blocks.len() >= 5,
        "expected the five census queries as <<'SQL' blocks, found {}",
        blocks.len()
    );
    let mutating = [
        "INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "TRUNCATE", "CREATE", "GRANT", "REVOKE",
        "COPY", "VACUUM", "REFRESH", "LOCK", "CALL", "DO ",
    ];
    for b in &blocks {
        let first = b
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("--"))
            .unwrap_or("");
        assert!(
            first.starts_with("SELECT") || first.starts_with("WITH"),
            "a census query must open with SELECT or WITH, this one opens with: {first}"
        );
        let upper = b.to_uppercase();
        for kw in mutating {
            // whole words only: `updated_at` is a column, `UPDATE ` is a verb
            let hit = upper
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|w| w == kw.trim());
            assert!(!hit, "census SQL carries the mutating word {kw}:\n{b}");
        }
    }
    let src = std::fs::read_to_string(script()).unwrap();
    assert!(
        src.contains("-c 'SET default_transaction_read_only = on'"),
        "every psql the census issues must open the session read-only"
    );
}

#[test]
fn the_seed_keys_are_read_from_the_checkout_not_typed() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let out = Command::new("bash")
        .arg(script())
        .arg("--seeds")
        .output()
        .expect("bash runs");
    assert!(
        out.status.success(),
        "--seeds failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seeds: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--seeds prints JSON");
    let root = repo_root();
    let seeds_dir = root.join("examples/brewery/seeds");
    let data_dir = root.join("examples/brewery/data");
    let json_len = |p: PathBuf| -> usize {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        v.as_array().unwrap().len()
    };
    let headers = |file: &str, header: &str| -> usize {
        std::fs::read_to_string(seeds_dir.join(file))
            .unwrap()
            .lines()
            .filter(|l| l.trim() == format!("[[{header}]]"))
            .count()
    };
    let len = |k: &str| {
        seeds[k]
            .as_array()
            .unwrap_or_else(|| panic!("--seeds has no array {k}"))
            .len()
    };

    // JSON seeds: the count jq gives.
    assert_eq!(
        len("employees"),
        json_len(seeds_dir.join("employees.json")) + headers("operator_hires.toml", "hire"),
        "employees = employees.json ids + operator_hires.toml hires"
    );
    assert_eq!(len("classes"), json_len(seeds_dir.join("classes.json")));
    assert_eq!(
        len("business_calendars"),
        json_len(seeds_dir.join("business_calendars.json"))
    );
    assert_eq!(
        len("marketing_assets"),
        json_len(data_dir.join("marketing-assets.json"))
    );
    assert_eq!(len("assets"), json_len(data_dir.join("assets.json")));
    assert_eq!(len("asset_models"), json_len(data_dir.join("catalog.json")));
    // TOML seeds: one key per [[table]] header.
    assert_eq!(len("vendors"), headers("vendors.toml", "vendor"));
    assert_eq!(len("locations"), headers("locations.toml", "location"));
    assert_eq!(len("products"), headers("products.toml", "products"));
    assert_eq!(len("parts"), headers("parts.toml", "parts"));
    assert_eq!(len("bulletins"), headers("bulletins.toml", "bulletin"));
    assert_eq!(len("messages"), headers("messages.toml", "thread"));
    assert_eq!(
        len("excise_jurisdictions"),
        headers("excise_rates.toml", "schedule")
    );
    assert_eq!(
        len("subject_kinds"),
        headers("subject_kinds.toml", "subject_kind")
    );
    assert_eq!(len("workflows"), headers("workflows.toml", "workflow"));
    // The tax regime, keyed from tax.toml the way the chart's neighbours
    // are (backlog fc27a0ce): one kind per [[tax_kind]], one state per
    // [[sales_tax_rate]].
    assert_eq!(len("tax_kinds"), headers("tax.toml", "tax_kind"));
    assert_eq!(
        len("sales_tax_states"),
        headers("tax.toml", "sales_tax_rate")
    );
    assert!(
        seeds["sales_tax_states"]
            .as_array()
            .unwrap()
            .contains(&"CA".into())
    );
    // accounts.toml is arrays, not tables: the `names = [ ... ]` block.
    let accounts = std::fs::read_to_string(seeds_dir.join("accounts.toml")).unwrap();
    let names_block = accounts
        .split("names = [")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .unwrap();
    assert_eq!(len("accounts"), names_block.matches('"').count() / 2);
    // policy roles: the distinct words in every `roles = [...]` array.
    let roles = seeds["policy_roles"].as_array().unwrap();
    assert!(
        roles.len() >= 10,
        "brewery policy seed names at least ten roles, got {}",
        roles.len()
    );
    assert!(roles.iter().any(|r| r == "ceo"));
    let mut sorted: Vec<_> = roles.iter().map(|r| r.as_str().unwrap()).collect();
    sorted.dedup();
    assert_eq!(sorted.len(), roles.len(), "policy roles are distinct");
    assert_eq!(
        seeds["tenant_id"], "brewery",
        "tenant id is read from tenant.toml [meta]"
    );
    // Spot checks that the extraction read VALUES, not lines.
    assert!(
        seeds["employees"]
            .as_array()
            .unwrap()
            .contains(&"emp-aa-001".into())
    );
    assert!(
        seeds["locations"]
            .as_array()
            .unwrap()
            .contains(&"loc-brewery-taproom".into())
    );
    assert!(
        seeds["parts"]
            .as_array()
            .unwrap()
            .contains(&"ING-MALT-2ROW-50".into())
    );
    assert!(
        seeds["accounts"]
            .as_array()
            .unwrap()
            .contains(&"Cascade Hop Distributors".into())
    );
    assert_eq!(seeds["classes"][0]["subject_kind"], "employee");
}

#[test]
fn the_census_is_one_document_read_through_the_postgres_container() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let r = run("document", &[], None);
    assert!(
        r.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        r.stdout,
        r.stderr
    );
    let doc: serde_json::Value = serde_json::from_str(r.stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}):\n{}", r.stdout));
    for key in [
        "row_counts",
        "seed_owned",
        "partitions",
        "references",
        "workflows",
        "log",
    ] {
        assert!(
            doc.get(key).is_some(),
            "document lacks {key}:\n{}",
            r.stdout
        );
    }
    assert_eq!(doc["verb"], "tenant-census");
    assert_eq!(doc["tenant_id"], "brewery");
    assert_eq!(doc["row_counts"]["jobs"], 7900);
    assert_eq!(doc["references"]["jobs.owner_id"]["count"], 2);
    assert_eq!(doc["workflows"][0]["owning_team"], "brewery");
    assert_eq!(doc["log"]["audit_log"]["by_simulated"]["true"], 490000);
    assert_eq!(
        doc["log"]["audit_log"]["job_linked"]["cross_tab"]["true"]["real"],
        900
    );
    // The log section's runtime rides beside it: 1.4M rows, and the
    // brief asked for the measured cost, not a guess.
    assert!(
        doc["log"]["elapsed_ms"].as_u64().is_some(),
        "log carries elapsed_ms:\n{}",
        r.stdout
    );
    // The seed keys ride in the document too, so a reader of the packet
    // sees what the queries matched against.
    assert!(doc["seed_keys"]["employees"].as_array().unwrap().len() > 400);

    // Every kubectl call is the read the brief named, and nothing else.
    let calls: Vec<Vec<&str>> = r
        .log
        .split("=== call ===\n")
        .filter(|c| !c.trim().is_empty())
        .map(|c| c.lines().collect())
        .collect();
    assert_eq!(calls.len(), 6, "six census queries, six reads:\n{}", r.log);
    let mut sqls: Vec<String> = Vec::new();
    for call in &calls {
        let head: Vec<&str> = call.iter().take_while(|w| **w != "--").copied().collect();
        assert_eq!(
            head,
            ["-n", "boss", "exec", "sts/postgres", "-c", "postgres"],
            "kubectl argv head: {call:?}"
        );
        let tail: Vec<&str> = call
            .iter()
            .skip_while(|w| **w != "--")
            .skip(1)
            .copied()
            .collect();
        assert_eq!(tail[0], "psql");
        assert!(tail.contains(&"-U") && tail.contains(&"boss"));
        assert!(tail.contains(&"-d"));
        assert!(tail.contains(&"ON_ERROR_STOP=1"));
        let set_at = tail
            .iter()
            .position(|w| *w == "SET default_transaction_read_only = on")
            .unwrap_or_else(|| panic!("no read-only SET in {tail:?}"));
        // The log is one line per word, and the SQL word is many lines:
        // everything after the SET's `-c` and its own `-c` is the query.
        assert_eq!(
            tail[set_at + 1],
            "-c",
            "the query follows the SET in its own -c"
        );
        let sql = tail[set_at + 2..].join("\n");
        assert!(
            sql.starts_with("-- census:"),
            "the query names itself: {sql}"
        );
        sqls.push(sql);
    }
    // The seed keys ride inside the SQL as a dollar-quoted literal — one
    // round trip per section, no temp table, nothing written.
    let seed_sql = sqls
        .iter()
        .find(|s| s.starts_with("-- census:seed_owned"))
        .expect("a seed_owned query");
    assert!(
        seed_sql.contains("$seed$"),
        "seed keys are dollar-quoted into the query"
    );
    assert!(seed_sql.contains("emp-aa-001"));
    assert!(seed_sql.contains("Cascade Hop Distributors"));

    // The log query reads the marker AND the partition, from every
    // table that carries the envelope — never one without the other,
    // which is the gap the packet names.
    let log_sql = sqls
        .iter()
        .find(|s| s.starts_with("-- census:log"))
        .expect("a log query");
    for want in [
        "FROM audit_log",
        "'_simulated'",
        "'_partition'",
        "'job_id'",
        "LIKE 'jobs.job.%'",
        "LEFT JOIN jobs",
        "partition",
        "FROM event_outbox",
        "delivered_at IS NULL",
        "FROM event_facts",
    ] {
        assert!(log_sql.contains(want), "log query lacks {want}:\n{log_sql}");
    }
}

#[test]
fn a_failed_read_exits_4_names_the_query_and_prints_no_document() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let r = run("failed-read", &[], Some("partitions"));
    assert_eq!(
        r.status.code(),
        Some(CANNOT_ANSWER),
        "stdout:\n{}\nstderr:\n{}",
        r.stdout,
        r.stderr
    );
    assert!(
        r.stdout.trim().is_empty(),
        "no partial document on a failed read:\n{}",
        r.stdout
    );
    assert!(
        r.stderr.contains("partitions"),
        "stderr names the failed query:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("CANNOT ANSWER"),
        "stderr says it cannot answer:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("does not exist"),
        "stderr carries the database's own words:\n{}",
        r.stderr
    );
}

#[test]
fn the_verb_file_is_a_read_only_forge_verb_with_no_params() {
    let path = repo_root().join("infra/ops/verbs/tenant-census.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(
        v["argv"],
        serde_json::json!(["infra/forge/tenant-census.sh"])
    );
    assert_eq!(v["params"], serde_json::json!([]));
    let about = v["about"].as_str().unwrap();
    assert!(
        about.starts_with("READ-ONLY"),
        "about opens with READ-ONLY: {about}"
    );
    assert!(
        !about.contains("MUTATING"),
        "a read carries no MUTATING word"
    );
    assert!(
        about.contains("d07dcc2b"),
        "about names the packet that asked for it"
    );
    // ~130 tables counted exactly plus five exec round trips through
    // docker+kubectl: the runner's 30 s default is not enough.
    assert!(v["timeout"].as_u64().unwrap() >= 120);
    let script = script();
    assert!(script.exists());
    use std::os::unix::fs::PermissionsExt;
    assert!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o111 != 0);
}

/// §9a: the script exec's into a namespace / StatefulSet / container /
/// user / database that the manifest declares. If the manifest moves
/// postgres, this names the drift.
#[test]
fn the_target_is_the_one_the_manifest_declares() {
    let manifest =
        std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss.yaml")).unwrap();
    let src = std::fs::read_to_string(script()).unwrap();
    let want = |s: &str| assert!(src.contains(s), "script lacks `{s}`");
    want("CENSUS_NS=\"boss\"");
    // The postgres target words live once, in forge-defaults.sh
    // (backlog 5222163e); the census reads them from there.
    want("CENSUS_WORKLOAD=\"$PG_WORKLOAD\"");
    let defaults =
        std::fs::read_to_string(repo_root().join("infra/forge/forge-defaults.sh")).unwrap();
    assert!(defaults.contains("PG_WORKLOAD=\"sts/postgres\""));
    want("CENSUS_CONTAINER=\"$PG_CONTAINER\"");
    assert!(defaults.contains("PG_CONTAINER=\"postgres\""));
    want("CENSUS_DB_USER=\"$PG_USER\"");
    assert!(defaults.contains("PG_USER=\"boss\""));
    want("CENSUS_DB_NAME=\"boss\"");
    // The StatefulSet named postgres, in namespace boss, with a container
    // named postgres, POSTGRES_USER=boss and POSTGRES_DB=boss.
    let sts = manifest
        .split("kind: StatefulSet\n")
        .find(|s| s.starts_with("metadata:\n  name: postgres\n  namespace: boss"))
        .expect("boss.yaml declares StatefulSet postgres in namespace boss");
    let sts = sts.split("kind: ").next().unwrap();
    assert!(
        sts.contains("- name: postgres\n"),
        "the postgres container is named postgres"
    );
    assert!(sts.contains("{name: POSTGRES_USER, value: boss}"));
    assert!(sts.contains("{name: POSTGRES_DB, value: boss}"));
}
