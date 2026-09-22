//! EVERY converge tick observes the tunnel connector — the no-op tick
//! (`forge main unchanged`) as well as the deploying one — and records
//! the same fields the deploying tick does: `cloudflared`,
//! `tunnel_ingress`, and `observed_on: unchanged|deploy`. The runner's
//! unchanged block is LIFTED from the shipped text and RUN against a
//! stub `kubectl`, so every field below is one it actually recorded,
//! and every kubectl call below is one it actually made.
//!
//! WHY (backlog 0b7804f3; measured 2026-09-16 13:38Z). The retire verb
//! for boss-gcp's hand-written connector (infra/gcp/retire-cloudflared.sh)
//! proves the hand-over from the newest maintenance-cluster-converge
//! whose run step carries `cloudflared` + `tunnel_ingress`, and refuses
//! when that converge is older than 120 min. Those fields were written
//! ONLY on a deploying converge (the `tunnel connector` stage, after
//! the instance apply); the unchanged-main path recorded `unchanged`
//! and nothing about the connector. So on a quiet morning David's
//! `--for-real` was refused — 'converge 2afe48e8 completed 10:23:56 —
//! 194 min ago, older than the 120 min ceiling' (ops-request 7d05cb04)
//! — with no way to refresh the evidence except landing a car. Evidence
//! that only exists when something ships is the wrong shape for a
//! liveness fact.
//!
//! What each case pins: the unchanged tick records the three fields,
//! from the SAME lib function the deploying tick calls (one definition,
//! `observe_connector`); the skipped set it hands the ingress renderer
//! comes from the secret gate, read-only; it makes NO write to the
//! cluster — no apply, no patch, no create, no rollout restart, no set
//! image (an observation tick writes nothing); a gate read the
//! credential cannot make fails the tick and claims nothing, as it
//! fails the deploying tick's apply loop; and the runner's inline
//! recording is gone — the deploying path calls the lib with `deploy`.

use boss_testing::{repo_root, scratch_dir, tunnel_ingress_summary, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const LIB: &str = "infra/forge/cluster-deploy-lib.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const RUN_SUMMARY: &str = "infra/run-summary.sh";

/// What the runner's secret gate writes into `instances_skipped` when
/// the playground's Secrets are absent (cluster-deploy-lib.sh
/// `skipped_entry`, `<ns> (secrets absent: a, b)`). It is the ONE input
/// the ingress line below depends on, so the line itself is rendered
/// rather than spelled (boss_testing::tunnel_ingress_summary; backlog
/// e9423392).
const SKIPPED: &str = "boss-playground (secrets absent: boss-secrets)";

/// The runner's unchanged block, lifted between its two markers: the
/// stamp comparison that opens it, and the stage clock reset that
/// follows its closing `fi`.
fn unchanged_block() -> String {
    let src = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    let start = src
        .find("if [ \"$HEAD\" = \"$LAST\" ]; then")
        .expect("the runner compares the head to the stamp");
    let end = src[start..]
        .find("_stage_started=$(date +%s)")
        .expect("the stage clock is reset after the unchanged block");
    src[start..start + end].to_string()
}

/// A stub kubectl: logs every call; answers a client dry run from the
/// fixture JSON for the path asked about (the connector's manifest, or
/// an instance's directory); answers `get secret` from a present-list;
/// answers `rollout status` ok. Anything else is logged and exits 0 —
/// which is exactly how a write would be caught.
fn stub_kubectl(dir: &Path) -> PathBuf {
    let kubectl = dir.join("kubectl");
    write_exec(
        &kubectl,
        r#"#!/usr/bin/env bash
echo "kubectl $*" >> "$STUB_LOG"
all="$*"
last="${all##* }"
case "$all" in
  *"create --dry-run=client -o json -f "*)
    case "$last" in
      */cloudflared.yaml) cat "$STUB_CONNECTOR" ;;
      */boss-playground)  cat "$STUB_INSTANCE" ;;
      *) ;;
    esac ;;
  *"get secret -n "*)
    ns=$(printf '%s\n' "$@" | grep -A1 -x -- -n | tail -n1)
    if [ -n "${STUB_FORBIDDEN:-}" ] && [ "$last" = "$STUB_FORBIDDEN" ]; then
      echo "Error from server (Forbidden): secrets \"$last\" is forbidden: User cannot get resource \"secrets\" in namespace \"$ns\"" >&2; exit 1
    fi
    if grep -qx -- "$ns/$last" "$STUB_PRESENT"; then echo "NAME  TYPE  DATA"; exit 0; fi
    echo "Error from server (NotFound): secrets \"$last\" not found" >&2; exit 1 ;;
  *"rollout status deploy/"*)
    [ "${STUB_ROLLOUT:-ok}" = ok ] && { echo 'deployment "cloudflared" successfully rolled out'; exit 0; }
    echo "error: timed out waiting for the condition" >&2; exit 1 ;;
esac
exit 0
"#,
    );
    // The connector's manifest, shaped like cloudflared.yaml: one Secret
    // volume. The name is deliberately not the real one — a `skipped`
    // line naming it proves the lib derived it from the manifest.
    write_file(
        &dir.join("connector.json"),
        r#"{"kind":"Deployment","metadata":{"name":"cloudflared","namespace":"boss"},"spec":{"template":{"spec":{
  "volumes":[{"name":"creds","secret":{"secretName":"tunnel-creds-fixture"}}]}}}}
"#,
    );
    // The playground's manifests: one required Secret.
    write_file(
        &dir.join("instance.json"),
        r#"{"kind":"Deployment","metadata":{"name":"boss","namespace":"boss-playground"},"spec":{"template":{"spec":{
  "containers":[{"name":"boss","env":[
    {"name":"DATABASE_URL","valueFrom":{"secretKeyRef":{"name":"boss-secrets","key":"database-url"}}}]}]}}}}
"#,
    );
    kubectl
}

struct Run {
    rc: i32,
    out: String,
    err: String,
    recorded: serde_json::Value,
    calls: String,
}

/// Runs the unchanged block as the runner would reach it — HEAD equal
/// to the stamp, the real renderers against the real tree, the stub as
/// both `$K` and what `kubectl_seeing` hands back — recording through
/// the real run-summary.sh.
fn run_unchanged(name: &str, present: &[&str], env: &[(&str, &str)]) -> Run {
    let dir = scratch_dir(&format!("tick-observes-{name}"));
    let kubectl = stub_kubectl(&dir);
    let present_file = dir.join("present");
    write_file(&present_file, &format!("{}\n", present.join("\n")));
    let script = format!(
        "set -euo pipefail\nREPO='{repo}'\nHEAD=abc1234\nLAST=abc1234\nSTAGE=start\nOUTCOME=''\n\
         K='{k}'\nkubectl_seeing() {{ echo '{k}'; }}\n\
         . '{summary}'\n. '{lib}'\n{block}\necho FELL-THROUGH\nexit 9\n",
        repo = repo_root().display(),
        k = kubectl.display(),
        summary = repo_root().join(RUN_SUMMARY).display(),
        lib = repo_root().join(LIB).display(),
        block = unchanged_block()
    );
    let summary = dir.join("summary.json");
    let log = dir.join("calls");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("STUB_LOG", &log)
        .env("STUB_PRESENT", &present_file)
        .env("STUB_CONNECTOR", dir.join("connector.json"))
        .env("STUB_INSTANCE", dir.join("instance.json"))
        .env_remove("BOSS_CLUSTER_TREE")
        .current_dir(&dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    Run {
        rc: out.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&out.stdout).to_string(),
        err: String::from_utf8_lossy(&out.stderr).to_string(),
        recorded: std::fs::read_to_string(&summary)
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap_or(serde_json::Value::Null),
        calls: std::fs::read_to_string(&log).unwrap_or_default(),
    }
}

/// A kubectl call that would change the cluster. Everything the
/// observation is allowed is a read: `get`, a client dry run, and
/// `rollout status`.
fn is_a_write(call: &str) -> bool {
    let read = call.contains(" get ")
        || call.contains("--dry-run=client")
        || call.contains(" rollout status ");
    !read
}

#[test]
fn an_unchanged_tick_records_the_connector_and_the_ingress_from_the_gates_skipped_set() {
    // The playground's Secret absent: the gate skips it, the ingress
    // summary serves its hostname from prod and says why, the connector
    // (its own Secret present) reads connected — all on a tick that
    // deployed nothing.
    let r = run_unchanged("skipped", &["boss/tunnel-creds-fixture"], &[]);
    assert_eq!(
        r.rc, 0,
        "the unchanged tick still exits 0:\n{}\n{}",
        r.out, r.err
    );
    assert!(!r.out.contains("FELL-THROUGH"), "the block exits itself");
    assert_eq!(r.recorded["unchanged"], "abc1234", "{}", r.recorded);
    assert_eq!(r.recorded["cloudflared"], "connected", "{}", r.recorded);
    assert_eq!(
        r.recorded["tunnel_ingress"],
        tunnel_ingress_summary(SKIPPED),
        "the renderer is handed the gate's skipped set, exactly as the deploying tick hands it: {}",
        r.recorded
    );
    assert_eq!(r.recorded["observed_on"], "unchanged", "{}", r.recorded);
    assert!(
        r.calls
            .contains("get secret -n boss-playground boss-secrets")
            && r.calls.contains("get secret -n boss tunnel-creds-fixture")
            && r.calls
                .contains("rollout status deploy/cloudflared -n boss"),
        "the skipped set and the connector were READ, not assumed: {}",
        r.calls
    );

    // Everything present: the hostname routes to its own namespace.
    let r = run_unchanged(
        "applied",
        &["boss/tunnel-creds-fixture", "boss-playground/boss-secrets"],
        &[],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(
        r.recorded["tunnel_ingress"],
        tunnel_ingress_summary(""),
        "{}",
        r.recorded
    );
    assert_eq!(r.recorded["cloudflared"], "connected");
}

#[test]
fn an_observation_tick_writes_nothing_to_the_cluster() {
    let r = run_unchanged("reads-only", &["boss/tunnel-creds-fixture"], &[]);
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    let writes: Vec<&str> = r.calls.lines().filter(|l| is_a_write(l)).collect();
    assert!(
        writes.is_empty(),
        "an unchanged tick observes and never applies, patches, creates, restarts or rolls: {writes:?}"
    );
    assert!(
        !r.calls.is_empty(),
        "and it did read — an empty log would be a block that ran nothing"
    );
    // The runner's own text says so, where the next reader will look.
    let block = unchanged_block();
    assert!(
        block
            .to_lowercase()
            .contains("writes nothing to the cluster"),
        "the block names its own bound: {block}"
    );
}

#[test]
fn a_gate_read_the_credential_cannot_make_fails_the_tick_and_claims_nothing() {
    // The deploying tick's apply loop fails on a gate read it cannot
    // make (rc 2: "cannot tell whether NS has its Secrets"); the
    // observing tick does the same, because a guessed skipped set would
    // become a guessed ingress map on the packet — evidence the retire
    // verb reads.
    let r = run_unchanged(
        "forbidden",
        &["boss/tunnel-creds-fixture"],
        &[("STUB_FORBIDDEN", "boss-secrets")],
    );
    assert_eq!(
        r.rc, 1,
        "a read the credential cannot make is a failed tick, not a quiet pass: {}\n{}",
        r.out, r.err
    );
    assert!(
        r.err.contains("cannot tell which instances are skipped"),
        "the journal names what could not be read: {}",
        r.err
    );
    for field in ["cloudflared", "tunnel_ingress", "observed_on"] {
        assert!(
            r.recorded.get(field).is_none(),
            "nothing is claimed about the connector for a skipped set nobody could read: {}",
            r.recorded
        );
    }
    // A connector that has not rolled out reads not-ready on this tick
    // too — the same word the deploying tick uses.
    let r = run_unchanged(
        "not-ready",
        &["boss/tunnel-creds-fixture", "boss-playground/boss-secrets"],
        &[("STUB_ROLLOUT", "timeout")],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.out, r.err);
    assert_eq!(r.recorded["cloudflared"], "not-ready", "{}", r.recorded);
}

#[test]
fn both_ticks_record_through_one_definition() {
    let lib = std::fs::read_to_string(repo_root().join(LIB)).unwrap();
    let f = lib
        .find("observe_connector() {")
        .expect("the lib defines observe_connector");
    let body = &lib[f..f + lib[f..].find("\n}\n").expect("a body")];
    for field in ["cloudflared", "tunnel_ingress", "observed_on"] {
        assert!(
            body.contains(&format!("run_summary_field {field}")),
            "observe_connector records `{field}`: {body}"
        );
    }
    assert!(
        body.contains("$(connector_status ")
            && body.contains("render-tunnel-config.sh\" --summary"),
        "the fields are derived by the same reads as before: {body}"
    );

    let runner = std::fs::read_to_string(repo_root().join(RUNNER)).unwrap();
    for field in ["cloudflared", "tunnel_ingress"] {
        assert!(
            !runner.contains(&format!("run_summary_field {field} ")),
            "the runner's inline `{field}` recording is deleted — one definition, in the lib"
        );
    }
    let unchanged = unchanged_block();
    assert!(
        unchanged.contains("observe_connector \"$K\"") && unchanged.contains(" unchanged\n"),
        "the unchanged tick calls the lib, tagged unchanged: {unchanged}"
    );
    let stamped = runner
        .find("OUTCOME=\"converged=$HEAD\"")
        .expect("the runner marks the roll real");
    let deploying = &runner[stamped..];
    let call = deploying
        .find("observe_connector \"$K\"")
        .expect("the deploying tick calls the lib after the roll");
    assert!(
        deploying[call..call + 200].contains(" deploy\n"),
        "tagged deploy: {}",
        &deploying[call..call + 200]
    );
}
