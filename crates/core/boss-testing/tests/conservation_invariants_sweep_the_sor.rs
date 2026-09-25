//! The conservation-invariant sweep runs IN THE CLUSTER, against the
//! system of record, on every instance — and carries no tenant's chart
//! of accounts (consolidation H12, backlog 236529aa, 2026-09-18).
//!
//! MEASURED. `infra/lint/conservation-invariants.sh` (22 SQL invariants,
//! 703 lines) ran ONLY on boss-gcp, hourly, against `PGHOST=127.0.0.1`
//! — the retired second stack's local postgres, never the system of
//! record. Its journal read `clean (22 invariants checked)` through
//! 2026-09-15 21:00Z and then stopped: the retire-second-stack verb
//! disabled it with the other 51 units. Since then NOTHING has run a
//! conservation invariant against the SoR database, while
//! docs/invariants/correctness-conservation.toml said `run hourly in
//! prod` and the cadence-silence sweep filed CADENCE SILENT daily for
//! a kind that was never once filed on the SoR.
//!
//! And 14 of the 22 were BREWERY assumptions written into a platform
//! sweep: GL account numbers 1300/1320/2300/2150/2200 and cash, batch
//! consume/produce steps, `finished_product_inventory.value_cents`,
//! the `brewery_seed_opening_balance` fact, revenue across >=3
//! accounts, a period close into 3000. On prod (Algedonic LLC, its own
//! chart) those accounts do not exist and the checks are meaningless;
//! on the playground they are the brewery's.
//!
//! WHAT THIS PINS:
//!   * the PLATFORM sweep is the eight tenant-free invariants (A B C D
//!     E F X Y) and names no account code and no brewery table — run
//!     against planted `psql` and `curl` stubs, it reads DATABASE_URL
//!     the way the in-cluster chore hands it over and falls back to the
//!     PGHOST spelling validate-brewery-sim.sh uses;
//!   * the BREWERY sweep (examples/brewery/conservation-invariants.sh)
//!     carries the fourteen, and the brewery sim validation runs BOTH
//!     so the regen gate keeps its 22;
//!   * the CronJob is the audit-integrity chore's shape (same
//!     securityContext, boss-chore.sh around the script), is classified an
//!     `instance` manifest so prod and the playground each sweep their
//!     own database, runs the tenant directory's own sweep when the
//!     image ships one (the playground's brewery), and its hourly
//!     schedule equals the cadence the silence sweep declares — the
//!     interval lives twice, so it gets an equality test (CLAUDE.md
//!     §9a);
//!   * the image COPYs what the manifest runs — the gate never builds
//!     the image, so this is the only reader of that pairing;
//!   * the bare-metal units are gone and roles.toml no longer names a
//!     unit no host installs.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::Path;
use std::process::Command;

const PLATFORM: &str = "infra/lint/conservation-invariants.sh";
const LIB: &str = "infra/lint/lib/conservation.sh";
const BREWERY: &str = "examples/brewery/conservation-invariants.sh";
const VALIDATE: &str = "infra/postgres/validate-brewery-sim.sh";
const MANIFEST: &str = "infra/cluster/manifests/boss-conservation-invariants.yaml";
const SIBLING: &str = "infra/cluster/manifests/boss-audit-integrity.yaml";
const ROSTER: &str = "infra/cluster/instance-manifests.txt";
const RULE: &str = "infra/dispatcher/rules/cadence-silence-sweep-daily.toml";
const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";
const ROLES: &str = "infra/estate/roles.toml";
const KIND: &str = "maintenance-conservation-invariants";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The letters a sweep runs, in file order: the `<L>.` prefix of every
/// `run_invariant "<L>. …"` label.
fn letters(script: &str) -> Vec<String> {
    script
        .lines()
        .filter_map(|l| l.strip_prefix("run_invariant \""))
        .map(|l| l.chars().take_while(|c| *c != '.').collect())
        .collect()
}

/// The SQL a sweep sends — every line between a `<<'SQL'` and its
/// closing `SQL`, so the assertions read what postgres would, not the
/// comments that explain it.
fn sql_of(script: &str) -> String {
    let mut inside = false;
    let mut out = String::new();
    for line in script.lines() {
        if line.contains("<<'SQL'") {
            inside = true;
            continue;
        }
        if line == "SQL" {
            inside = false;
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// A scratch bin dir carrying a `psql` that records its argv and
/// answers no rows, and a `curl` that answers a balanced balance sheet.
/// Every query returning nothing IS a clean sweep — each invariant
/// selects its violators.
fn stubs(name: &str) -> std::path::PathBuf {
    let bin = scratch_dir(name);
    write_exec(
        &bin.join("psql"),
        // One line per call: the SQL argument spans lines, so argv is
        // flattened before it is logged.
        "#!/usr/bin/env bash\nprintf '%s' \"$*\" | tr '\\n' ' ' >> \"$STUB_LOG\"\necho >> \"$STUB_LOG\"\nexit 0\n",
    );
    write_exec(
        &bin.join("curl"),
        "#!/usr/bin/env bash\nprintf '%s\\n' \"curl $*\" >> \"$STUB_LOG\"\n\
         echo '{\"total_assets_cents\":100,\"total_liabilities_cents\":40,\"total_equity_cents\":60}'\n",
    );
    bin
}

fn run_sweep(script: &str, bin: &Path, env: &[(&str, &str)]) -> (i32, String, String) {
    let log = bin.join("argv.log");
    let _ = std::fs::remove_file(&log);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(script))
        .env_remove("DATABASE_URL")
        .env_remove("PGHOST")
        .env_remove("PGUSER")
        .env_remove("PGDATABASE")
        .env("PATH", path)
        .env("STUB_LOG", &log);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("bash runs the sweep");
    let argv = std::fs::read_to_string(&log).unwrap_or_default();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        argv,
    )
}

// --- the platform sweep ----------------------------------------------------

#[test]
fn the_platform_sweep_is_the_eight_tenant_free_invariants() {
    let script = read(PLATFORM);
    assert_eq!(
        letters(&script),
        ["A", "B", "C", "D", "E", "F", "X", "Y"],
        "{PLATFORM} runs exactly the platform-shaped invariants, in that order"
    );
    let sql = sql_of(&script);
    // The boundary: an account CODE is a tenant's chart of accounts,
    // a batch step or a brewery table is a tenant's process. A query
    // naming either belongs to the tenant's own sweep.
    for tenant_shaped in [
        "a.code = '",
        "code = '1",
        "'1000'",
        "'1300'",
        "'1310'",
        "'1320'",
        "'2150'",
        "'2200'",
        "'2300'",
        "'3000'",
        "finished_product_inventory",
        "brewery_seed_opening_balance",
        "production-produce",
        "production-consume",
        "batch_id",
        "value_cents",
    ] {
        assert!(
            !sql.contains(tenant_shaped),
            "{PLATFORM} sends SQL naming `{tenant_shaped}` — a tenant assumption in the platform sweep"
        );
    }
    assert!(
        script.contains(". \"$(dirname \"${BASH_SOURCE[0]}\")/lib/conservation.sh\""),
        "{PLATFORM} sources the one helper both sweeps share ({LIB})"
    );
}

#[test]
fn the_platform_sweep_reads_database_url_and_falls_back_to_the_pg_spelling() {
    let bin = stubs("conservation-platform-stubs");
    // The in-cluster chore's spelling: DATABASE_URL from the instance
    // Secret, which psql accepts as the dbname argument.
    let (rc, out, argv) = run_sweep(
        PLATFORM,
        &bin,
        &[
            ("DATABASE_URL", "postgres://u:p@db.example:5432/sor"),
            ("LEDGER_BASE", "http://ledger.example:7080"),
        ],
    );
    assert_eq!(
        rc, 0,
        "a sweep whose every query returns no rows is clean:\n{out}"
    );
    assert!(
        out.contains("clean (8 invariants checked)"),
        "the verdict counts the eight:\n{out}"
    );
    let psql_calls: Vec<&str> = argv.lines().filter(|l| !l.starts_with("curl ")).collect();
    assert_eq!(psql_calls.len(), 8, "one psql call per invariant:\n{argv}");
    for call in &psql_calls {
        assert!(
            call.starts_with("postgres://u:p@db.example:5432/sor "),
            "psql is handed DATABASE_URL, not a host triple: {call}"
        );
        assert!(
            !call.contains("-h "),
            "no PGHOST fallback rides beside the URL: {call}"
        );
    }
    assert!(
        argv.lines().any(|l| l.starts_with("curl ")
            && l.contains("http://ledger.example:7080/api/ledger/balance-sheet")),
        "invariant S reads the balance sheet from LEDGER_BASE:\n{argv}"
    );
    // The bare-metal / validate-brewery-sim spelling still works.
    let (rc, out, argv) = run_sweep(
        PLATFORM,
        &bin,
        &[
            ("PGHOST", "pg.example"),
            ("PGUSER", "someone"),
            ("PGDATABASE", "somedb"),
            ("LEDGER_BASE", "http://ledger.example:7080"),
        ],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(
        argv.lines()
            .filter(|l| !l.starts_with("curl "))
            .all(|l| l.starts_with("-h pg.example -U someone -d somedb ")),
        "without DATABASE_URL the PG* triple is what psql gets:\n{argv}"
    );
}

// --- the brewery sweep -----------------------------------------------------

#[test]
fn the_brewery_sweep_carries_the_fourteen_tenant_invariants() {
    let script = read(BREWERY);
    let mut got = letters(&script);
    got.sort();
    assert_eq!(
        got,
        [
            "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "V", "W"
        ],
        "{BREWERY} carries every invariant that names the brewery's chart or process"
    );
    let sql = sql_of(&script);
    for brewery_shaped in [
        "'1300'",
        "'1320'",
        "finished_product_inventory",
        "brewery_seed_opening_balance",
    ] {
        assert!(
            sql.contains(brewery_shaped),
            "{BREWERY} still asserts over `{brewery_shaped}`"
        );
    }
    assert!(
        script.contains("lib/conservation.sh"),
        "{BREWERY} sources the shared helper rather than carrying a second run_invariant"
    );
    let bin = stubs("conservation-brewery-stubs");
    let (rc, out, argv) = run_sweep(
        BREWERY,
        &bin,
        &[("DATABASE_URL", "postgres://u:p@db.example:5432/play")],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("clean (14 invariants checked)"), "{out}");
    assert_eq!(
        argv.lines().count(),
        14,
        "one psql call per invariant, no HTTP leg:\n{argv}"
    );
}

#[test]
fn the_brewery_sim_validation_runs_both_sweeps() {
    let validate = read(VALIDATE);
    let platform = validate
        .find("infra/lint/conservation-invariants.sh")
        .expect("validate-brewery-sim.sh runs the platform sweep");
    let brewery = validate
        .find("examples/brewery/conservation-invariants.sh")
        .expect("validate-brewery-sim.sh runs the brewery sweep too — the regen gate keeps its 22");
    assert!(platform < brewery, "platform first, then the tenant's own");
    assert!(
        !validate.contains("boss-conservation-invariants.timer"),
        "no bare-metal timer to quiesce any more"
    );
}

// --- the chore -------------------------------------------------------------

/// A cron schedule's period in minutes, for the shapes a chore here
/// uses: `M * * * *` is hourly, `*/N * * * *` every N minutes.
fn cron_minutes(schedule: &str) -> u64 {
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    assert_eq!(fields.len(), 5, "a five-field cron schedule: {schedule}");
    match fields.as_slice() {
        [m, "*", "*", "*", "*"] if m.parse::<u64>().is_ok() => 60,
        [m, "*", "*", "*", "*"] if m.starts_with("*/") => m[2..].parse().unwrap(),
        _ => panic!("a schedule shape this test cannot read: {schedule}"),
    }
}

fn block(yaml: &str, from: &str, to: &str) -> String {
    let start = yaml
        .find(from)
        .unwrap_or_else(|| panic!("`{from}` in the manifest"));
    let end = yaml[start..]
        .find(to)
        .map(|i| start + i)
        .unwrap_or(yaml.len());
    yaml[start..end].to_string()
}

#[test]
fn the_cronjob_is_the_audit_integrity_chores_shape_on_every_instance() {
    let yaml = read(MANIFEST);
    let sibling = read(SIBLING);
    assert!(yaml.contains("kind: CronJob\n"), "{MANIFEST} is a CronJob");
    assert!(yaml.contains("  name: boss-conservation-invariants\n"));
    assert!(
        yaml.contains("  namespace: boss\n"),
        "written for the source instance"
    );
    assert!(yaml.contains("    boss-chore: \"true\"\n"));
    assert_eq!(
        block(
            &yaml,
            "          securityContext:",
            "          restartPolicy:"
        ),
        block(
            &sibling,
            "          securityContext:",
            "          restartPolicy:"
        ),
        "the pod securityContext is the sibling's, byte for byte"
    );
    assert_eq!(
        block(
            &yaml,
            "              securityContext:",
            "              command:"
        ),
        block(
            &sibling,
            "              securityContext:",
            "              command:"
        ),
        "so is the container's"
    );
    // One wrapper call opens the packet, runs the check, and records
    // ok OR failed (480e183c) — the check is both sweeps, platform then
    // tenant, as one `bash -c` after the `--` so one verdict covers
    // both. a_chore_records_ok_and_failed.rs pins the wrapper itself.
    let chore = yaml
        .find(&format!(
            "/usr/local/bin/boss-chore.sh {KIND} \"Conservation-invariant sweep\" -- bash -euo pipefail -c '"
        ))
        .expect("opens, runs and records through boss-chore.sh, the check under bash -e");
    let run = yaml
        .find("/opt/boss/infra/lint/conservation-invariants.sh")
        .expect("runs the platform sweep from where the image carries it");
    let tenant = yaml
        .find("$BOSS_TENANT_DIR/conservation-invariants.sh")
        .expect("runs the tenant directory's own sweep when it ships one");
    assert!(
        chore < run && run < tenant,
        "wrapper, platform, tenant — in that order"
    );
    for env in [
        "- name: DATABASE_URL\n                  valueFrom:\n                    secretKeyRef:\n                      name: boss-secrets\n                      key: database-url",
        "- name: BOSS_JOBS_URL\n                  value: http://boss-jobs-internal.boss.svc.cluster.local:7900",
        "- name: LEDGER_BASE\n                  value: http://boss-jobs-internal.boss.svc.cluster.local:7080",
        "- name: BOSS_TENANT_DIR\n                  value: /opt/boss/tenant",
        "- name: BOSS_MACHINE_TOKEN\n                  valueFrom:\n                    secretKeyRef:\n                      name: boss-secrets\n                      key: machine-token",
    ] {
        assert!(yaml.contains(env), "{MANIFEST} carries:\n{env}");
    }
    // Rendered per instance: prod and the playground each sweep their
    // own database and file on their own jobs door.
    let roster = read(ROSTER);
    assert!(
        roster
            .lines()
            .any(|l| l.trim() == "boss-conservation-invariants.yaml instance"),
        "{ROSTER} classifies the chore as an instance manifest"
    );
    // And the playground's render points the tenant sweep at the
    // brewery directory the image ships — the renderer's tenant
    // substitution, landing on this manifest's BOSS_TENANT_DIR.
    let out = Command::new("bash")
        .arg(repo_root().join("infra/cluster/render-instance.sh"))
        .args([
            "boss-playground",
            "examples/brewery",
            "true",
            "playground.example",
            "audit",
        ])
        .current_dir(repo_root())
        .output()
        .expect("the renderer runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rendered = String::from_utf8_lossy(&out.stdout);
    let chore = block(
        &rendered,
        "  name: boss-conservation-invariants\n",
        "\n---\n",
    );
    assert!(
        chore.contains(
            "- name: BOSS_TENANT_DIR\n                  value: /opt/boss/examples/brewery\n"
        ),
        "the playground chore runs the brewery's sweep:\n{chore}"
    );
    assert!(chore.contains("http://boss-jobs-internal.boss-playground.svc.cluster.local:7900"));
}

#[test]
fn the_chores_schedule_is_the_cadence_the_silence_sweep_declares() {
    let yaml = read(MANIFEST);
    let schedule = yaml
        .lines()
        .find_map(|l| l.trim().strip_prefix("schedule: "))
        .expect("the CronJob declares a schedule")
        .trim_matches('"');
    let declared: u64 = read(RULE)
        .split(&format!("\"interval_minutes.{KIND}\" = \""))
        .nth(1)
        .expect("the silence sweep declares the kind")
        .split('"')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        cron_minutes(schedule),
        declared,
        "the CronJob runs `{schedule}` but {RULE} declares {declared} minutes for {KIND} — \
         the sweep would alarm on a healthy chore or stay quiet on a dead one"
    );
}

#[test]
fn the_image_carries_what_the_chore_runs() {
    let dockerfile = read(DOCKERFILE);
    assert!(
        dockerfile.contains(&format!("COPY {PLATFORM} /opt/boss/{PLATFORM}\n")),
        "{DOCKERFILE} COPYs the platform sweep to the path the manifest runs"
    );
    assert!(
        dockerfile.contains(&format!("COPY {LIB} /opt/boss/{LIB}\n")),
        "{DOCKERFILE} COPYs the helper the sweep sources"
    );
    assert!(
        dockerfile.contains("COPY examples /opt/boss/examples\n"),
        "the brewery's own sweep rides in with the examples directory"
    );
    assert!(repo_root().join(LIB).is_file(), "{LIB} exists");
}

// --- the bare-metal residue ------------------------------------------------

#[test]
fn the_bare_metal_units_are_gone() {
    for unit in [
        "infra/lint/boss-conservation-invariants.service",
        "infra/lint/boss-conservation-invariants.timer",
    ] {
        assert!(
            !repo_root().join(unit).exists(),
            "{unit} is bare-metal residue: no host installs it since 2026-09-15"
        );
    }
    let roles = read(ROLES);
    assert!(
        !roles.contains("boss-conservation-invariants"),
        "{ROLES} names a unit no host installs — the cluster CronJob is the one that runs"
    );
}
