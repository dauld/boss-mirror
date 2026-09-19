//! The converge CHECKS a repo-sourced tenant before it applies the
//! boss-tenant ConfigMap, and a refusal leaves the previous ConfigMap
//! in place (backlog 1af5119d, 2026-09-19).
//!
//! WHY, measured by the builder of 4f1ba1f9: infra/cluster/manifests/
//! boss.yaml's Deployment is strategy Recreate with the gateway and the
//! jobs API in one container, so ANY gateway boot refusal — the
//! unparseable-manifest refusal (car 32bec388, held on the dock for
//! this), an unknown public_reads path (#464), the launcher's 'not a
//! tenant directory' — takes the system of record down until the
//! delivered file is fixed; the cluster watchdog rolls back IMAGES, not
//! the boss-tenant ConfigMap. The 2026-09-07 class (a boot guard that
//! refuses to start takes the SoR down), one layer out.
//!
//! What each case pins, by RUNNING the lib's `tenant_check` and the
//! runner's own `converge_tenant` (its text lifted from the runner,
//! with a stub `boss`, a stub `sudo` standing in for the docker'd
//! kubectl, and the real run-summary.sh and alert-lib.sh):
//!
//!   * a checkout with one bad line: `boss tenant check` refuses, the
//!     ConfigMap is NOT applied, the converge step records
//!     `tenant unchanged: check refused <file>:<line>`, a packet naming
//!     the file and line is filed through the alert door (kept in the
//!     spool here, where the API is a stub that refuses), and the
//!     converge goes on (rc 0 — the train ARRIVES: the software
//!     converged, the tenant did not);
//!   * a clean checkout: checked, applied as before;
//!   * `boss` absent on the host: NOT applied, `could not check` naming
//!     the binary — never 'checked' when nothing ran; and a `boss` that
//!     dies without a verdict is the same 'could not check';
//!   * the stub's rows are the CLI's own render shape, pinned against
//!     tenant.rs so the parser here reads what the real verb prints.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RUN_SUMMARY: &str = "infra/run-summary.sh";
const TENANT_RS: &str = "crates/orchestrators/boss-cli/src/tenant.rs";

/// A tenant checkout in the contract's shape: the root manifest and
/// one seed file. `bad` plants the one bad line the real verb refuses
/// with `TOML parse error at line 2, column 10` (measured 2026-09-19).
fn checkout(root: &Path, bad: bool) -> PathBuf {
    let src = root.join("checkout");
    boss_testing::create_dir(&src.join("seeds"));
    write_file(
        &src.join("tenant.toml"),
        "[meta]\ntenant_id = \"x\"\nname = \"X\"\n",
    );
    let workflows = if bad {
        "[[workflow]]\nkind = \"a\n"
    } else {
        "[[workflow]]\nkind = \"a\"\n"
    };
    write_file(&src.join("seeds/workflows.toml"), workflows);
    src
}

/// The stub `boss`: `tenant check DIR` in the CLI's render shape
/// (tenant.rs `Report::render`, pinned below), judging the fixture the
/// way the real verb does — the planted line is INVALID with its line
/// number, a missing manifest is MISSING with no line. STUB_CHECK=crash
/// dies without a verdict.
fn stub_boss(bin: &Path) {
    write_exec(
        &bin.join("boss"),
        r#"#!/usr/bin/env bash
echo "boss $*" >> "$STUB_LOG"
if [ "${STUB_CHECK:-}" = crash ]; then echo "thread 'main' panicked at tenant.rs" >&2; exit 101; fi
[ "$1 $2" = "tenant check" ] || { echo "stub boss: unexpected verb: $*" >&2; exit 2; }
dir="$3"
echo "boss tenant check $dir"
fail=0
if [ -f "$dir/tenant.toml" ]; then
  echo "  OK       tenant.toml           tenant_id=x display_name=(unset); 0 modules, 0 labels, 0 public reads"
else
  echo "  MISSING  tenant.toml           required"; fail=1
fi
if grep -q 'kind = "a$' "$dir/seeds/workflows.toml"; then
  echo "  INVALID  seeds/workflows.toml  parsing $dir/seeds/workflows.toml: TOML parse error at line 2, column 10"
  echo "                                   |"
  echo "                                 2 | kind = \"a"
  echo "                                   |          ^"
  echo "                                 invalid basic string, expected \`\"\`"
  fail=1
else
  echo "  OK       seeds/workflows.toml  1 workflow"
fi
if [ "$fail" = 1 ]; then echo "1 ok, 0 missing, 1 invalid, 0 unknown — FAIL"; exit 1; fi
echo "2 ok, 0 missing, 0 invalid, 0 unknown — PASS"
"#,
    );
}

/// The stub `sudo`: the runner's kubectl is `sudo docker run … kubectl
/// …`; every call is logged whole and stdin drained (the apply reads a
/// dry-run document off a pipe), and a `create configmap` dry run
/// prints a document for the apply to read.
fn stub_sudo(bin: &Path) {
    write_exec(
        &bin.join("sudo"),
        r#"#!/usr/bin/env bash
echo "sudo $*" >> "$STUB_LOG"
cat > /dev/null
case "$*" in
  *"create configmap "*) echo "apiVersion: v1"; echo "kind: ConfigMap" ;;
esac
exit 0
"#,
    );
}

/// The stub `curl`: the alert door's POST, refused — so the packet is
/// KEPT in the spool, which is the door's own word for a filed alert
/// the API could not take yet (alerts-are-packets).
fn stub_curl(bin: &Path) {
    write_exec(
        &bin.join("curl"),
        r#"#!/usr/bin/env bash
echo "curl $*" >> "$STUB_LOG"
cat > /dev/null
echo "curl: (7) Failed to connect to 127.0.0.1 port 9: Connection refused" >&2
exit 7
"#,
    );
}

struct Run {
    rc: i32,
    out: String,
    err: String,
    calls: String,
    recorded: serde_json::Value,
    spooled: Vec<String>,
}

/// The stub bin first, then every PATH entry that holds NO `boss` —
/// the pod and the forge both carry one, and the absent-CLI legs must
/// measure a host without it, not find the operator's.
fn path_with(bin: &Path) -> String {
    let rest: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty() && !Path::new(d).join("boss").exists())
        .map(str::to_string)
        .collect();
    format!("{}:{}", bin.display(), rest.join(":"))
}

/// `converge_tenant` lifted verbatim from the runner and run over the
/// lib, with `tenant_checkout` replaced by a copy of the fixture (the
/// clone is measured elsewhere; a_tenant_source_per_instance.rs).
fn converge_tenant_text() -> String {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let start = src
        .find("converge_tenant() {")
        .expect("converge_tenant is defined in the runner");
    let end = src[start..].find("\n}\n").unwrap();
    src[start..start + end + 3].to_string()
}

fn converge(name: &str, bad: bool, env: &[(&str, &str)], with_boss: bool) -> Run {
    let dir = scratch_dir(&format!("converge-checks-tenant-{name}"));
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    if with_boss {
        stub_boss(&bin);
    }
    stub_sudo(&bin);
    stub_curl(&bin);
    let src = checkout(&dir, bad);
    let tenants = dir.join("tenants");
    let summary = dir.join("summary.json");
    let log = dir.join("calls");
    let spool = dir.join("spool");
    let script = format!(
        "set -euo pipefail\n. '{summary}'\n. '{lib}'\n\
         REPO='{repo}'\nTENANTS_DIR='{tenants}'\nKUBECONFIG_PATH='{kc}'\n\
         KAPPLY='sudo docker run --rm -i kubectl'\nSITE_SOURCE=''\n\
         tenant_checkout() {{ rm -rf \"$4\"; cp -R '{src}' \"$4\"; }}\n\
         {fun}\nconverge_tenant boss-x boss-x david/tenant-x main\n",
        summary = repo_root().join(RUN_SUMMARY).display(),
        lib = repo_root().join(LIB).display(),
        repo = dir.display(),
        tenants = tenants.display(),
        kc = dir.join("kc.yaml").display(),
        src = src.display(),
        fun = converge_tenant_text(),
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .env("PATH", path_with(&bin))
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("STUB_LOG", &log)
        .env("ALERT_SPOOL", &spool)
        .env("JOBS_API", "http://127.0.0.1:9")
        .env_remove("BOSS_CLI")
        .current_dir(&dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let spooled = std::fs::read_dir(&spool)
        .map(|rd| {
            rd.map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
                .collect()
        })
        .unwrap_or_default();
    Run {
        rc: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).to_string(),
        err: String::from_utf8_lossy(&out.stderr).to_string(),
        calls: std::fs::read_to_string(&log).unwrap_or_default(),
        recorded: std::fs::read_to_string(&summary)
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap_or(serde_json::Value::Null),
        spooled,
    }
}

fn applied_boss_tenant(calls: &str) -> bool {
    calls
        .lines()
        .any(|l| l.starts_with("sudo ") && l.contains("create configmap boss-tenant "))
}

fn tenant_check_field(run: &Run) -> String {
    run.recorded["tenant_check"]
        .as_str()
        .unwrap_or_else(|| panic!("tenant_check rides the packet; recorded: {}", run.recorded))
        .to_string()
}

// ---------------------------------------------------------------------
// tenant_check, the lib's measure
// ---------------------------------------------------------------------

fn tenant_check(
    name: &str,
    bad: bool,
    env: &[(&str, &str)],
    with_boss: bool,
) -> (i32, String, String) {
    let dir = scratch_dir(&format!("tenant-check-{name}"));
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    if with_boss {
        stub_boss(&bin);
    }
    let src = checkout(&dir, bad);
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(format!(
            "set -euo pipefail\n. '{}'\ntenant_check '{}'\n",
            repo_root().join(LIB).display(),
            src.display()
        ))
        .env("PATH", path_with(&bin))
        .env("STUB_LOG", dir.join("calls"))
        .env_remove("BOSS_CLI")
        .current_dir(&dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn a_bad_line_is_refused_by_file_and_line_and_a_clean_checkout_passes() {
    let (rc, out, err) = tenant_check("bad", true, &[], true);
    assert_eq!(rc, 1, "one bad line is a refusal (rc 1): {out}\n{err}");
    assert_eq!(
        out.trim(),
        "seeds/workflows.toml:2",
        "the refusal names the file and the line the loader named"
    );
    assert!(
        err.contains("TOML parse error at line 2"),
        "the verb's whole report goes to the journal: {err}"
    );
    let (rc, out, err) = tenant_check("clean", false, &[], true);
    assert_eq!(rc, 0, "a clean checkout passes: {out}\n{err}");
    assert_eq!(out.trim(), "", "nothing to name on a pass");
}

#[test]
fn a_missing_manifest_is_refused_by_file_alone() {
    let dir = scratch_dir("tenant-check-missing");
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    stub_boss(&bin);
    let src = checkout(&dir, false);
    std::fs::remove_file(src.join("tenant.toml")).unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -euo pipefail\n. '{}'\ntenant_check '{}'\n",
            repo_root().join(LIB).display(),
            src.display()
        ))
        .env("PATH", path_with(&bin))
        .env("STUB_LOG", dir.join("calls"))
        .env_remove("BOSS_CLI")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "tenant.toml",
        "a MISSING row has no line to name"
    );
}

#[test]
fn an_absent_or_dying_cli_is_could_not_check_never_a_verdict() {
    let (rc, out, err) = tenant_check("absent", true, &[], false);
    assert_eq!(
        rc, 3,
        "no boss on the host is rc 3, could not check: {out}\n{err}"
    );
    assert!(
        err.contains("boss is not on this host") && err.contains("NOT checked"),
        "the refusal names the binary and says nothing was checked: {err}"
    );
    assert_eq!(out.trim(), "", "{out}");
    let (rc, out, err) = tenant_check("crash", true, &[("STUB_CHECK", "crash")], true);
    assert_eq!(
        rc, 3,
        "a boss that dies without a verdict is could not check: {out}\n{err}"
    );
    assert!(
        err.contains("exited 101") && err.contains("NOT checked"),
        "the exit is named: {err}"
    );
    // BOSS_CLI names the binary, so a test or a host can point at one.
    let (rc, _, err) = tenant_check("named", true, &[("BOSS_CLI", "/nonexistent/boss")], true);
    assert_eq!(rc, 3, "{err}");
    assert!(err.contains("/nonexistent/boss"), "{err}");
}

// ---------------------------------------------------------------------
// converge_tenant, the runner's own function, gated on the check
// ---------------------------------------------------------------------

#[test]
fn a_refused_tenant_keeps_the_previous_configmap_records_the_line_and_files_a_packet() {
    let run = converge("refused", true, &[], true);
    assert_eq!(
        run.rc, 0,
        "the converge goes on — the train arrives: {}\n{}",
        run.out, run.err
    );
    assert!(
        !applied_boss_tenant(&run.calls),
        "the boss-tenant ConfigMap is NOT applied on a refusal; calls:\n{}",
        run.calls
    );
    assert!(
        !run.calls.contains("apply -f -"),
        "nothing reached kubectl apply; calls:\n{}",
        run.calls
    );
    assert_eq!(
        tenant_check_field(&run),
        "boss-x: tenant unchanged: check refused seeds/workflows.toml:2 (david/tenant-x@main)",
        "the converge step says the tenant did not change and why"
    );
    assert!(
        run.err.contains("UNCHANGED") && run.err.contains("seeds/workflows.toml:2"),
        "the journal names the file and line: {}",
        run.err
    );
    assert_eq!(
        run.spooled.len(),
        1,
        "one packet filed through the alert door (kept: the API refused); spool: {:?}\ncalls:\n{}",
        run.spooled,
        run.calls
    );
    let packet: serde_json::Value = serde_json::from_str(&run.spooled[0]).unwrap();
    assert_eq!(packet["kind"], "backlog-item");
    assert_eq!(packet["priority"], "urgent");
    let title = packet["title"].as_str().unwrap();
    assert!(
        title.contains("seeds/workflows.toml:2")
            && title.contains("david/tenant-x@main")
            && title.contains("boss-x"),
        "the packet's title names the file, line, source and instance: {title}"
    );
    assert!(
        packet["metadata"]["detail"]
            .as_str()
            .unwrap()
            .contains("TOML parse error at line 2"),
        "the packet carries the verb's own report: {}",
        packet["metadata"]["detail"]
    );
    assert_eq!(
        packet["metadata"]["filed_by"], "automation:cluster-deploy-runner",
        "the converge files as itself, not as the watchdog"
    );
    assert!(
        run.calls
            .lines()
            .any(|l| l.starts_with("curl ") && l.contains("/api/jobs")),
        "the door was tried before the packet was kept; calls:\n{}",
        run.calls
    );
}

#[test]
fn a_clean_tenant_is_checked_and_applied_as_before() {
    let run = converge("clean", false, &[], true);
    assert_eq!(run.rc, 0, "{}\n{}", run.out, run.err);
    assert!(
        applied_boss_tenant(&run.calls) && run.calls.contains("apply -f -"),
        "a clean checkout is applied; calls:\n{}",
        run.calls
    );
    assert!(
        run.calls
            .lines()
            .any(|l| l.starts_with("boss tenant check ")),
        "and it was checked first; calls:\n{}",
        run.calls
    );
    let check = run
        .calls
        .lines()
        .position(|l| l.starts_with("boss tenant check "))
        .unwrap();
    let apply = run
        .calls
        .lines()
        .position(|l| l.contains("create configmap boss-tenant "))
        .unwrap();
    assert!(check < apply, "check BEFORE apply; calls:\n{}", run.calls);
    assert_eq!(
        tenant_check_field(&run),
        "boss-x: checked (david/tenant-x@main)"
    );
    assert!(
        run.spooled.is_empty(),
        "no packet on a pass: {:?}",
        run.spooled
    );
}

#[test]
fn an_absent_cli_applies_nothing_and_says_it_could_not_check() {
    let run = converge("absent", false, &[], false);
    assert_eq!(run.rc, 0, "the converge goes on: {}\n{}", run.out, run.err);
    assert!(
        !applied_boss_tenant(&run.calls),
        "an unchecked tenant is not applied; calls:\n{}",
        run.calls
    );
    let field = tenant_check_field(&run);
    assert!(
        field.starts_with("boss-x: tenant unchanged: could not check")
            && field.contains("boss is not on this host"),
        "never 'checked' when nothing ran: {field}"
    );
    assert_eq!(
        run.spooled.len(),
        1,
        "could-not-check is a packet too: {:?}",
        run.spooled
    );
    let packet: serde_json::Value = serde_json::from_str(&run.spooled[0]).unwrap();
    assert!(
        packet["title"]
            .as_str()
            .unwrap()
            .contains("could not check"),
        "{}",
        packet["title"]
    );
}

// ---------------------------------------------------------------------
// The stub's shape is the verb's shape
// ---------------------------------------------------------------------

/// The parser in `tenant_check` reads `<STATUS> <path> <detail>` rows
/// and `line N` off the detail, so the stub above prints the CLI's
/// render. Pinned to tenant.rs's format string and labels: a render
/// change that moves the path column shows up here, not on the forge.
#[test]
fn the_stub_prints_the_render_the_lib_parses() {
    let rs = std::fs::read_to_string(repo_root().join(TENANT_RS)).unwrap();
    assert!(
        rs.contains(r#""  {:<8} {:<width$}  {}\n""#),
        "Report::render's row is status, path, detail"
    );
    for label in ["\"INVALID\"", "\"MISSING\"", "\"OK\"", "\"UNKNOWN\""] {
        assert!(rs.contains(label), "the {label} label");
    }
    assert!(
        rs.contains("std::process::exit(1)"),
        "a failed check exits 1 (the rc tenant_check reads as a refusal)"
    );
}
