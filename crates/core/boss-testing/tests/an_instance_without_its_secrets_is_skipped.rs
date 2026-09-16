//! An instance whose Secrets are not minted yet is SKIPPED by the
//! converge, by name, with exit 0 — never a twelve-minute rollout wait
//! ending in "Maintenance failed" on every train. The lib functions in
//! `infra/forge/cluster-deploy-lib.sh` are RUN against a stub `kubectl`,
//! so every verdict below is one they actually reached.
//!
//! WHY (backlog 07d7549c, car cb784b3a held on the dock 2026-09-16). As
//! first built, the runner applied the playground and then waited on
//! `rollout status` — 420 s, then 300 s for the rollback — for pods that
//! could not start because `boss-secrets`, `boss-session-key` and the
//! rest live out of tree and are minted by David, once, per namespace.
//! Until that ceremony every train's converge would have ended
//! "Maintenance failed" at `roll boss-playground`. David decided "prod
//! converges on every train; just be very reliable": a converge that
//! fails on a known, named, human-owned precondition is not reliable,
//! it is a scheduled alarm. So the runner asks first — every Secret the
//! RENDERED manifests require, derived from the YAML (secretKeyRef,
//! secret volumes, imagePullSecrets; `optional: true` excluded) and
//! never hardcoded — and an instance missing any is skipped whole, the
//! names ride the converge packet as `instances_skipped`, and the exact
//! `kubectl create secret` shapes (names and keys, never values) are
//! printed for the person who mints them.
//!
//! What each case pins: the derivation reads every reference form and
//! drops the optional ones; a Secret the stub answers "not found" for
//! is named as absent (a missing NAMESPACE reads as absent too — that is
//! the first converge after landing); all present is "apply"; a read
//! the credential cannot make is "cannot tell", distinct from both; the
//! shapes name keys and never values; and the runner's own loop calls
//! the gate before the apply and records the skip.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// A fixture: rendered "manifests" as JSON (valid YAML, and what
/// `kubectl create --dry-run=client -o json` would echo back — the
/// undeclared-objects idiom), plus a stub kubectl that cats them for a
/// dry run and answers `get secret` from a list of present names.
struct Case {
    root: PathBuf,
    kubectl: PathBuf,
    manifests: PathBuf,
    present: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("instance-secrets-{name}"));
        let manifests = root.join("manifests");
        std::fs::create_dir_all(&manifests).unwrap();
        let present = root.join("present");
        write_file(&present, "");
        // One Deployment shaped like boss.yaml's: an imagePullSecret, an
        // initContainer keyed on boss-secrets, a required env ref, two
        // optional env refs (inline and block form), a required secret
        // volume and an optional one.
        write_file(
            &manifests.join("boss.yaml"),
            r#"{"kind":"Deployment","metadata":{"name":"boss","namespace":"boss-x"},"spec":{"template":{"spec":{
  "imagePullSecrets":[{"name":"forgejo-registry"}],
  "initContainers":[{"name":"boss-init","env":[
    {"name":"PGPASSWORD","valueFrom":{"secretKeyRef":{"name":"boss-secrets","key":"postgres-password"}}}]}],
  "containers":[{"name":"boss","env":[
    {"name":"DATABASE_URL","valueFrom":{"secretKeyRef":{"name":"boss-secrets","key":"database-url"}}},
    {"name":"BOSS_OIDC_CLIENT_SECRET","valueFrom":{"secretKeyRef":{"name":"boss-oidc","key":"client-secret"}}},
    {"name":"BOSS_BROKER_FORGEJO_TOKEN","valueFrom":{"secretKeyRef":{"name":"boss-credential-broker-root","key":"forgejo-token","optional":true}}}]}],
  "volumes":[
    {"name":"boss-session-key","secret":{"secretName":"boss-session-key"}},
    {"name":"maybe","secret":{"secretName":"boss-break-glass-enroll","optional":true}}]}}}}
"#,
        );
        // A chore shaped like the CronJobs': a block-form optional ref
        // beside a required one, and the TLS front's volume.
        write_file(
            &manifests.join("boss-chore.yaml"),
            r#"{"kind":"CronJob","metadata":{"name":"chore","namespace":"boss-x"},"spec":{"jobTemplate":{"spec":{"template":{"spec":{
  "containers":[{"name":"c","env":[
    {"name":"DATABASE_URL","valueFrom":{"secretKeyRef":{"name":"boss-secrets","key":"database-url"}}},
    {"name":"BOSS_MACHINE_TOKEN","valueFrom":{"secretKeyRef":{"name":"boss-secrets","key":"machine-token","optional":true}}}]}],
  "volumes":[{"name":"tls","secret":{"secretName":"boss-tls"}}]}}}}}}
"#,
        );
        let kubectl = root.join("kubectl");
        write_exec(
            &kubectl,
            r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
all="$*"
last="${all##* }"
case "$all" in
  *"create --dry-run=client -o json -f "*)
    for f in "$last"/*.yaml; do cat "$f"; done ;;
  *"get secret -n "*)
    ns=$(printf '%s\n' "$@" | grep -A1 -x -- -n | tail -n1)
    name="$last"
    if [ -n "${STUB_FORBIDDEN:-}" ] && [ "$name" = "$STUB_FORBIDDEN" ]; then
      echo "Error from server (Forbidden): secrets \"$name\" is forbidden: User cannot get resource \"secrets\" in namespace \"$ns\"" >&2; exit 1
    fi
    if [ -n "${STUB_NO_NAMESPACE:-}" ]; then
      echo "Error from server (NotFound): namespaces \"$ns\" not found" >&2; exit 1
    fi
    if grep -qx -- "$name" "$STUB_PRESENT"; then echo "NAME  TYPE  DATA"; exit 0; fi
    echo "Error from server (NotFound): secrets \"$name\" not found" >&2; exit 1 ;;
esac
exit 0
"#,
        );
        Self {
            root,
            kubectl,
            manifests,
            present,
        }
    }

    fn present(&self, names: &[&str]) {
        write_file(&self.present, &format!("{}\n", names.join("\n")));
    }

    /// Source the lib and run one function with the stub as its kubectl.
    fn run(&self, body: &str, env: &[(&str, &str)]) -> (i32, String, String) {
        let script = format!(". '{}'\n{}\n", repo_root().join(LIB).display(), body);
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&script)
            .env("STUB_LOG", self.root.join("calls"))
            .env("STUB_PRESENT", &self.present)
            .env("K", &self.kubectl)
            .env("M", &self.manifests);
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
}

fn require_jq() {
    assert!(
        has("jq"),
        "jq is missing, and this suite would otherwise pass by skipping — \
         jq is declared in infra/forge/boss-ci/required-tools.txt"
    );
}

#[test]
fn the_required_secrets_are_derived_from_the_rendered_manifests() {
    require_jq();
    let c = Case::new("derive");
    let (rc, out, err) = c.run(r#"manifest_secrets "$K" "$M""#, &[]);
    assert_eq!(rc, 0, "{err}");
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        vec![
            "boss-oidc",
            "boss-secrets",
            "boss-session-key",
            "boss-tls",
            "forgejo-registry"
        ],
        "every required reference form, once each, sorted; the optional ones \
         (boss-credential-broker-root, boss-break-glass-enroll, the block-form \
         machine-token ref) excluded — the pod boots without them"
    );
    // The keys, for the shapes the operator is handed: name<TAB>key.
    let (rc, out, _) = c.run(r#"manifest_secret_keys "$K" "$M""#, &[]);
    assert_eq!(rc, 0);
    let keys: Vec<&str> = out.lines().collect();
    assert!(keys.contains(&"boss-secrets\tdatabase-url"));
    assert!(keys.contains(&"boss-secrets\tpostgres-password"));
    assert!(keys.contains(&"boss-oidc\tclient-secret"));
    assert!(
        !keys
            .iter()
            .any(|k| k.starts_with("boss-credential-broker-root")),
        "an optional ref names no key to mint"
    );
}

#[test]
fn an_absent_secret_skips_the_instance_by_name_and_a_missing_namespace_reads_as_absent() {
    require_jq();
    let c = Case::new("absent");
    c.present(&["boss-secrets", "boss-oidc", "forgejo-registry"]);
    let (rc, out, err) = c.run(r#"instance_secret_gate "$K" "$K" boss-x "$M""#, &[]);
    assert_eq!(
        rc, 1,
        "an absent Secret is SKIP (1), not apply (0) or cannot-tell (2): {err}"
    );
    assert_eq!(
        out.trim(),
        "boss-session-key boss-tls",
        "stdout is exactly the absent names, for the packet field"
    );
    assert!(
        err.contains("create secret") && err.contains("boss-x"),
        "the shapes to mint are printed, in the instance's namespace: {err}"
    );
    // The first converge after landing: the namespace itself does not
    // exist yet. That is "absent", not "cannot tell".
    let (rc, out, _) = c.run(
        r#"instance_secret_gate "$K" "$K" boss-x "$M""#,
        &[("STUB_NO_NAMESPACE", "1")],
    );
    assert_eq!(rc, 1);
    assert_eq!(
        out.trim(),
        "boss-oidc boss-secrets boss-session-key boss-tls forgejo-registry"
    );
}

#[test]
fn every_secret_present_is_apply_and_a_forbidden_read_is_cannot_tell() {
    require_jq();
    let c = Case::new("present");
    c.present(&[
        "boss-secrets",
        "boss-oidc",
        "forgejo-registry",
        "boss-session-key",
        "boss-tls",
    ]);
    let (rc, out, err) = c.run(r#"instance_secret_gate "$K" "$K" boss-x "$M""#, &[]);
    assert_eq!(rc, 0, "all present is apply: {err}");
    assert!(out.trim().is_empty(), "nothing is absent: {out}");
    let (rc, _, err) = c.run(
        r#"instance_secret_gate "$K" "$K" boss-x "$M""#,
        &[("STUB_FORBIDDEN", "boss-tls")],
    );
    assert_eq!(
        rc, 2,
        "a read the credential cannot make is CANNOT TELL, never 'absent'"
    );
    assert!(
        err.contains("boss-tls") && err.to_lowercase().contains("forbidden"),
        "{err}"
    );
}

#[test]
fn the_shapes_name_keys_and_never_values() {
    require_jq();
    let c = Case::new("shapes");
    c.present(&[]);
    let (rc, _, err) = c.run(r#"instance_secret_gate "$K" "$K" boss-x "$M""#, &[]);
    assert_eq!(rc, 1);
    assert!(
        err.contains("kubectl -n boss-x create secret generic boss-secrets --from-literal=database-url=... --from-literal=postgres-password=..."),
        "keyed secrets are handed as --from-literal with every key the manifests read: {err}"
    );
    assert!(
        err.contains("kubectl -n boss-x create secret generic boss-session-key --from-file="),
        "a volume secret is handed as --from-file with prod's copy as the shape: {err}"
    );
    assert!(
        err.contains("kubectl -n boss-x create secret docker-registry forgejo-registry"),
        "an imagePullSecret is a docker-registry secret: {err}"
    );
}

#[test]
fn the_runner_asks_before_it_applies_a_second_instance_and_records_the_skip() {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let loop_start = src
        .find("STAGE=\"apply instances\"")
        .expect("the runner has the apply-instances stage");
    let after = &src[loop_start..];
    let gate = after
        .find("instance_secret_gate")
        .expect("the loop calls the gate");
    let apply = after
        .find("apply_instance \"$ins_ns\"")
        .expect("the loop applies the instance");
    assert!(gate < apply, "the gate is asked BEFORE the apply");
    assert!(
        after.contains("run_summary_field instances_skipped"),
        "the skip rides the packet"
    );
    assert!(
        after.contains("run_summary_field instances_applied"),
        "beside the applied list"
    );
    // The roll loop at the end skips what the apply loop skipped.
    let roll = after
        .find("roll_deployment")
        .expect("the instances are rolled");
    assert!(
        after[roll - 900..roll].contains(",$INSTANCES_APPLIED,"),
        "only an APPLIED instance is rolled — a skipped one has nothing of this head in it"
    );
}
