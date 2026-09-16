//! An instance the converge SKIPPED (its Secrets absent) is not an
//! instance whose objects are MISSING. `infra/cluster/check-manifests-
//! applied.sh` is RUN against a fixture tree and a stub `kubectl`, so
//! every verdict below is one the script actually reached; and the
//! runner's verify block is RUN against a stub check, so the summary it
//! records on the packet is one it actually captured.
//!
//! WHY (backlog 07d7549c; measured on the forge journal, 2026-09-16
//! 06:06Z, main b4d7a0fd). The instance renderer landed with a secret
//! gate that skips an instance whose Secrets are not minted — the
//! converge printed `instances skipped: boss-playground (secrets absent:
//! boss-oidc, boss-secrets, boss-session-key, boss-tls, forgejo-registry,
//! resend)` — and then FAILED its own verification: the check walks the
//! RENDERED set (81 objects) and reported the skipped instance's 21 as
//! MISSING, `60 present, 21 missing`, exit 1, `result: exit-code` on the
//! converge packet (b7026689). The skip was right; the check had not been
//! told. And the packet carried NO output at all — the cause was
//! readable only in the forge journal (CLAUDE.md §Diagnosis: a verdict
//! must name what failed).
//!
//! What each case pins: the runner hands the check its `instances_skipped`
//! field (one definition — the same string the packet carries) and the
//! check counts those instances' objects SKIPPED, by instance and reason,
//! exiting 0 when the only absences are theirs; a genuinely missing
//! object in an APPLIED instance still fails by name beside the skip; a
//! skip the converge did not declare is still a miss; a declared skip
//! whose objects turn out present counts them present; the check's
//! one-line summary reaches the packet as `manifests_check` whatever its
//! exit; and a run that dies names its stage as `failed_stage`.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const CHECK: &str = "infra/cluster/check-manifests-applied.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RUN_SUMMARY: &str = "infra/run-summary.sh";

/// The runner's `instances_skipped` field as the loop builds it — the
/// one string the packet carries and the check is handed.
const SKIPPED: &str = "boss-playground (secrets absent: boss-secrets, boss-tls)";

/// A fixture tree the renderer accepts (source prod + a playground, two
/// instance manifests, one pipeline manifest), plus a stub kubectl that
/// parses these flat manifests for a dry run and answers `get` from a
/// `Kind/name/namespace` list.
struct Case {
    root: PathBuf,
    tree: PathBuf,
    present: PathBuf,
    log: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("skipped-instance-{name}"));
        let tree = root.join("tree");
        let dir = tree.join("infra/cluster/manifests");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(tree.join("examples/fixture/seeds")).unwrap();
        write_file(&tree.join("examples/fixture/seeds/tenant.toml"), "");
        // The source values the renderer checks the files for: the
        // namespace, the tenant path, the sim flag and the hostname.
        write_file(
            &dir.join("boss.yaml"),
            "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: boss\n---\n\
             apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: boss\n  namespace: boss\n\
             spec:\n  template:\n    spec:\n      containers:\n      - name: boss\n        env:\n\
             \x20       - {name: BOSS_SIM_ENABLED, value: \"false\"}\n\
             \x20       - {name: BOSS_TENANT_MANIFEST_TOML, value: /opt/boss/examples/fixture/seeds/tenant.toml}\n\
             ---\napiVersion: v1\nkind: Service\nmetadata:\n  name: boss-gateway\n  namespace: boss\n",
        );
        write_file(
            &dir.join("boss-tls-front.yaml"),
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: boss-tls-front\n  namespace: boss\n\
             spec:\n  template:\n    spec:\n      containers:\n      - name: caddy\n        args: [boss.algedonic.dev]\n",
        );
        write_file(
            &dir.join("boss-conductor.yaml"),
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: boss-conductor\n  namespace: boss-dev\n",
        );
        write_file(
            &tree.join("infra/cluster/instance-manifests.txt"),
            "boss.yaml instance\nboss-tls-front.yaml instance\nboss-conductor.yaml pipeline\n",
        );
        write_file(
            &tree.join("infra/cluster/instances.toml"),
            "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant = \"examples/fixture/seeds/tenant.toml\"\n\
             sim = false\nhostname = \"boss.algedonic.dev\"\n\n[playground]\nnamespace = \"boss-playground\"\n\
             tenant = \"examples/fixture/seeds/tenant.toml\"\nsim = true\nhostname = \"playground.algedonic.dev\"\n",
        );
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(
            &bin.join("kubectl"),
            r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
case "$1" in
  version) exit 0 ;;
  create)
    # --dry-run=client -o jsonpath={.kind}{"\t"}{.metadata.name}{"\t"}{.metadata.namespace}{"\n"} -f FILE
    awk 'function flush() { if (kind != "") printf "%s\t%s\t%s\n", kind, name, ns; kind = ""; name = ""; ns = ""; meta = 0 }
         /^---/ { flush(); next }
         /^kind: / { kind = $2 }
         /^metadata:$/ { meta = 1; next }
         /^[^ ]/ { meta = 0 }
         meta && /^  name: / { name = $2 }
         meta && /^  namespace: / { ns = $2 }
         END { flush() }' "${@: -1}"
    exit 0 ;;
  get)
    kind="$2"; name="$3"; ns=""
    shift 3
    while [ $# -gt 0 ]; do [ "$1" = -n ] && ns="$2"; shift; done
    if grep -qx -- "$kind/$name/$ns" "$STUB_PRESENT"; then echo "NAME READY"; exit 0; fi
    echo "Error from server (NotFound): $kind \"$name\" not found" >&2; exit 1 ;;
esac
exit 0
"#,
        );
        let present = root.join("present");
        write_file(&present, "");
        let log = root.join("calls");
        Self {
            root,
            tree,
            present,
            log,
        }
    }

    /// Everything the fixture declares, in both namespaces.
    fn all_objects() -> Vec<String> {
        let mut v = vec!["Deployment/boss-conductor/boss-dev".to_string()];
        for ns in ["boss", "boss-playground"] {
            v.push(format!("Namespace/{ns}/"));
            v.push(format!("Deployment/boss/{ns}"));
            v.push(format!("Service/boss-gateway/{ns}"));
            v.push(format!("Deployment/boss-tls-front/{ns}"));
        }
        v
    }

    fn present(&self, objects: &[String]) {
        write_file(&self.present, &format!("{}\n", objects.join("\n")));
    }

    fn run_check(&self, env: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(CHECK))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("BOSS_CLUSTER_TREE", &self.tree)
            .env("STUB_LOG", &self.log)
            .env("STUB_PRESENT", &self.present)
            .env_remove("BOSS_INSTANCES_SKIPPED");
        for (k, v) in env {
            cmd.env(k, v);
        }
        output(cmd)
    }
}

fn output(mut cmd: Command) -> (i32, String, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn summary_line(out: &str) -> String {
    out.lines()
        .rfind(|l| l.starts_with("check-manifests-applied: "))
        .unwrap_or_else(|| panic!("the check prints its one-line summary: {out}"))
        .to_string()
}

#[test]
fn every_object_present_is_clean_and_the_summary_counts_skipped_separately() {
    let c = Case::new("clean");
    c.present(&Case::all_objects());
    let (rc, out, err) = c.run_check(&[]);
    assert_eq!(rc, 0, "{out}\n{err}");
    assert_eq!(
        summary_line(&out),
        "check-manifests-applied: 9 present, 0 missing, 0 skipped, 0 drifted, 0 unreadable (of 9)"
    );
}

#[test]
fn a_skipped_instances_absent_objects_are_skipped_not_missing() {
    let c = Case::new("skipped");
    let prod: Vec<String> = Case::all_objects()
        .into_iter()
        .filter(|o| !o.ends_with("/boss-playground") && !o.ends_with("/boss-playground/"))
        .collect();
    c.present(&prod);
    let (rc, out, err) = c.run_check(&[("BOSS_INSTANCES_SKIPPED", SKIPPED)]);
    assert_eq!(
        rc, 0,
        "the only absences belong to the skipped instance, so the tree and the \
         cluster agree: {out}\n{err}"
    );
    assert!(
        !err.contains("MISSING"),
        "nothing is reported missing: {err}"
    );
    assert_eq!(
        summary_line(&out),
        "check-manifests-applied: 5 present, 0 missing, 4 skipped (boss-playground: secrets absent), 0 drifted, 0 unreadable (of 9)"
    );
    assert!(
        out.contains("skip    Namespace/boss-playground — instance boss-playground skipped by the converge (secrets absent)")
            && out.contains("skip    Deployment/boss (ns boss-playground) — instance boss-playground skipped by the converge (secrets absent)"),
        "each skipped object is named with the instance and the reason: {out}"
    );
}

#[test]
fn a_missing_object_in_an_applied_instance_still_fails_by_name_beside_the_skip() {
    let c = Case::new("missing");
    let prod: Vec<String> = Case::all_objects()
        .into_iter()
        .filter(|o| !o.contains("boss-playground") && o != "Service/boss-gateway/boss")
        .collect();
    c.present(&prod);
    let (rc, out, err) = c.run_check(&[("BOSS_INSTANCES_SKIPPED", SKIPPED)]);
    assert_eq!(rc, 1, "a prod object absent is a real miss: {out}\n{err}");
    assert!(
        err.contains("MISSING Service/boss-gateway (ns boss)"),
        "the missing object is named: {err}"
    );
    assert_eq!(
        summary_line(&out),
        "check-manifests-applied: 4 present, 1 missing, 4 skipped (boss-playground: secrets absent), 0 drifted, 0 unreadable (of 9)"
    );
}

#[test]
fn a_skip_the_converge_did_not_declare_is_still_a_miss() {
    let c = Case::new("undeclared");
    let prod: Vec<String> = Case::all_objects()
        .into_iter()
        .filter(|o| !o.contains("boss-playground"))
        .collect();
    c.present(&prod);
    let (rc, out, err) = c.run_check(&[]);
    assert_eq!(rc, 1, "{out}\n{err}");
    assert_eq!(
        summary_line(&out),
        "check-manifests-applied: 5 present, 4 missing, 0 skipped, 0 drifted, 0 unreadable (of 9)"
    );
    assert!(err.contains("MISSING Deployment/boss (ns boss-playground)"));
}

#[test]
fn a_declared_skip_whose_objects_are_present_counts_them_present() {
    let c = Case::new("present-anyway");
    c.present(&Case::all_objects());
    let (rc, out, err) = c.run_check(&[("BOSS_INSTANCES_SKIPPED", SKIPPED)]);
    assert_eq!(rc, 0, "{out}\n{err}");
    assert_eq!(
        summary_line(&out),
        "check-manifests-applied: 9 present, 0 missing, 0 skipped, 0 drifted, 0 unreadable (of 9)",
        "a skip is only a reason for an ABSENCE; what is there is verified"
    );
}

// --- the runner: the summary and the failing stage reach the packet -----

/// The verify block, lifted from the runner between its two markers, so
/// the test exercises the shipped text rather than a copy of it.
fn verify_block() -> String {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let start = src
        .find("STAGE=\"verify manifests\"")
        .expect("the runner has the verify stage");
    let end = src[start..]
        .find("_stage_done verify_s")
        .expect("the verify stage ends with its timing");
    src[start..start + end].to_string()
}

/// Runs the verify block with a stub check at `$REPO/infra/cluster/
/// check-manifests-applied.sh` that prints `check_out` and exits
/// `check_rc`, recording through the real run-summary.sh.
fn run_verify(
    dir: &Path,
    check_rc: i32,
    check_out: &str,
) -> (i32, String, String, serde_json::Value) {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("infra/cluster")).unwrap();
    write_exec(
        &repo.join("infra/cluster/check-manifests-applied.sh"),
        &format!(
            "#!/bin/sh\necho \"skipped=${{BOSS_INSTANCES_SKIPPED-unset}}\" > \"{log}\"\ncat <<'OUT'\n{check_out}\nOUT\nexit {check_rc}\n",
            log = dir.join("check-env").display()
        ),
    );
    let script = format!(
        "set -euo pipefail\nREPO={repo}\nKUBECONFIG_PATH={kc}\nINSTANCES_SKIPPED='{skipped}'\n\
         _stage_done() {{ :; }}\n. {lib}\n{block}\nexit 0\n",
        repo = repo.display(),
        kc = dir.join("kc.yaml").display(),
        skipped = SKIPPED,
        lib = repo_root().join(RUN_SUMMARY).display(),
        block = verify_block()
    );
    let summary = dir.join("summary.json");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .current_dir(dir);
    let (rc, out, err) = output(cmd);
    let recorded = std::fs::read_to_string(&summary)
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap_or(serde_json::Value::Null);
    (rc, out, err, recorded)
}

const CHECK_OUT: &str = "  skip    Namespace/boss-playground — instance boss-playground skipped by the converge (secrets absent)\n\
check-manifests-applied: 60 present, 0 missing, 21 skipped (boss-playground: secrets absent), 0 drifted, 0 unreadable (of 81)";

#[test]
fn the_runner_hands_the_check_its_skipped_instances_and_records_the_summary() {
    let dir = scratch_dir("skipped-instance-runner-ok");
    let (rc, out, err, recorded) = run_verify(&dir, 0, CHECK_OUT);
    assert_eq!(rc, 0, "{out}\n{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join("check-env"))
            .unwrap()
            .trim(),
        format!("skipped={SKIPPED}"),
        "the check is handed the SAME string the packet carries as instances_skipped"
    );
    assert_eq!(
        recorded["manifests_check"],
        "check-manifests-applied: 60 present, 0 missing, 21 skipped (boss-playground: secrets absent), 0 drifted, 0 unreadable (of 81)",
        "the check's one-line summary rides the packet: {recorded}"
    );
    assert!(
        out.contains("21 skipped (boss-playground"),
        "the check's report still reaches the journal: {out}"
    );
}

#[test]
fn a_failed_check_still_records_its_summary_on_the_packet() {
    let dir = scratch_dir("skipped-instance-runner-red");
    let failing = "  MISSING Service/boss-gateway (ns boss)\n\
check-manifests-applied: 59 present, 1 missing, 21 skipped (boss-playground: secrets absent), 0 drifted, 0 unreadable (of 81)";
    let (rc, out, err, recorded) = run_verify(&dir, 1, failing);
    assert_eq!(rc, 1, "a real miss fails the unit: {out}\n{err}");
    assert_eq!(
        recorded["manifests_check"],
        "check-manifests-applied: 59 present, 1 missing, 21 skipped (boss-playground: secrets absent), 0 drifted, 0 unreadable (of 81)",
        "the verdict names what failed without the journal: {recorded}"
    );
    assert!(err.contains("MANIFESTS CHECK FAILED (rc=1)"), "{err}");
}

/// The exit trap, lifted from the runner, so a run that dies at any
/// stage names it on the packet — not only through the ops-request.
fn finish_block() -> String {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let start = src
        .find("_finish() {")
        .expect("the runner has its exit trap");
    let end = src[start..]
        .find("trap _finish EXIT")
        .expect("the trap is armed");
    src[start..start + end].to_string()
}

fn run_finish(dir: &Path, stage: &str, outcome: &str, exit: i32) -> serde_json::Value {
    let snap = dir.join("snap");
    write_file(&snap, "");
    let script = format!(
        "set -euo pipefail\nREPO=x\nBOSS_RUNNER_SNAPSHOT={snap}\nOUTCOME='{outcome}'\nSTAGE='{stage}'\n\
         answer_converge_requests() {{ :; }}\n. {lib}\n{block}\ntrap _finish EXIT\nexit {exit}\n",
        snap = snap.display(),
        lib = repo_root().join(RUN_SUMMARY).display(),
        block = finish_block()
    );
    let summary = dir.join("summary.json");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .current_dir(dir);
    let (rc, _, err) = output(cmd);
    assert_eq!(rc, exit, "the trap keeps the exit code: {err}");
    std::fs::read_to_string(&summary)
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap_or(serde_json::Value::Null)
}

#[test]
fn a_run_that_dies_names_its_stage_on_the_packet() {
    let dir = scratch_dir("skipped-instance-failed-stage");
    let recorded = run_finish(&dir, "verify manifests", "", 1);
    assert_eq!(
        recorded["failed_stage"], "verify manifests",
        "the stage that exited nonzero is on the record: {recorded}"
    );
    let dir = scratch_dir("skipped-instance-clean-exit");
    let recorded = run_finish(&dir, "roll boss-playground abc1234", "converged=abc1234", 0);
    assert!(
        recorded.get("failed_stage").is_none(),
        "a clean exit names no failed stage: {recorded}"
    );
}
