//! `reap-terminated-pods.sh` — Failed pods are reaped through a rendered
//! plan, never through a hand with an admin credential (backlog 85889a52).
//!
//! WHY. Measured 2026-09-23 18:36Z after train #587 rolled the dev pod:
//! six terminated boss-dev pods (Error / ContainerStatusUnknown, 8h to 8d
//! old, all on w-1) were still listed, every one left by an
//! ephemeral-storage eviction. Kubernetes does not collect them (the
//! controller-manager's terminated-pod-gc-threshold is 12500), the dev
//! session cannot delete pods (correctly), and undeclared-objects.sh
//! leaves Pod out of scope on purpose — so a human with admin
//! credentials was the only way to clear them. David, 2026-09-23: run it
//! on a period regardless, and also have an ad hoc trigger.
//!
//! THE SHAPE is design 17835005's, third caller after commission-a-disk
//! and merge-tenant-main: `plan-a-pod-reap` renders, `reap-terminated-
//! pods` deletes exactly the pods of the plan whose hash it is handed,
//! each with a uid precondition, and nothing else.
//!
//! THE FIXTURE is a manifest tree laid out like the repo (JSON in .yaml
//! files, which `kubectl create --dry-run -o json -f` echoes back, the
//! trick `undeclared_objects_sh.rs` uses) and a stub `kubectl` on
//! `BOSS_KUBECTL` whose cluster is a directory of pod lists. The stub
//! refuses a delete that carries no uid precondition, so a delete that
//! lost its body fails here rather than passing as a bare delete. The
//! clock is `BOSS_REAP_NOW`, so every age below is exact. Nothing here
//! touches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

/// 2026-09-23T12:00:00Z — the render time every case uses unless it says
/// otherwise.
const NOW: i64 = 1_790_164_800;

fn script() -> PathBuf {
    repo_root().join("infra/forge/reap-terminated-pods.sh")
}

/// The stand-in cluster: `pods-<ns>.json` is a namespace's pod array,
/// `deny-<ns>` makes its list Forbidden, `swap-<ns>-<name>` replaces the
/// pod's uid at the moment a delete for it arrives (a same-named pod
/// recreated in the race), `calls` logs every invocation and `deleted`
/// every delete the precondition admitted.
const STUB: &str = r#"#!/usr/bin/env bash
S="${KSTATE:?}"
echo "$*" >> "$S/calls"
ns=""; raw=""; file=""; words=()
while [ $# -gt 0 ]; do
    case "$1" in
        -n) ns="$2"; shift 2 ;;
        --raw) raw="$2"; shift 2 ;;
        -f) file="$2"; shift 2 ;;
        -o) shift 2 ;;
        --*) shift ;;
        *) words+=("$1"); shift ;;
    esac
done
f="$S/pods-$ns.json"
[ -f "$f" ] || echo '[]' > "$f"
case "${words[0]}" in
    create) cat "$file"; exit 0 ;;
    get)
        if [ -e "$S/deny-$ns" ]; then
            echo "Error from server (Forbidden): pods is forbidden: User \"dev-session\" cannot list resource \"pods\" in the namespace \"$ns\"" >&2
            exit 1
        fi
        if [ "${words[1]}" = pods ] && [ "${#words[@]}" -eq 2 ]; then
            jq '{apiVersion: "v1", kind: "List", items: .}' "$f"; exit 0
        fi
        name="${words[2]}"
        pod=$(jq -c --arg n "$name" '.[] | select(.metadata.name == $n)' "$f")
        [ -n "$pod" ] || { echo "Error from server (NotFound): pods \"$name\" not found" >&2; exit 1; }
        printf '%s\n' "$pod"; exit 0 ;;
    delete)
        [ -n "$raw" ] || { echo "stub: only a --raw delete is modelled: $*" >&2; exit 1; }
        ns=$(cut -d/ -f5 <<< "$raw"); name=$(cut -d/ -f7 <<< "$raw")
        f="$S/pods-$ns.json"
        body=$(cat)
        want=$(jq -r '.preconditions.uid // empty' <<< "$body" 2>/dev/null)
        [ -n "$want" ] || { echo "stub: a delete without a uid precondition: $body" >&2; exit 1; }
        if [ -f "$S/swap-$ns-$name" ]; then
            jq --arg n "$name" --arg u "$(cat "$S/swap-$ns-$name")" \
                'map(if .metadata.name == $n then .metadata.uid = $u else . end)' "$f" > "$f.new" && mv "$f.new" "$f"
        fi
        have=$(jq -r --arg n "$name" '.[] | select(.metadata.name == $n) | .metadata.uid' "$f")
        [ -n "$have" ] || { echo "Error from server (NotFound): pods \"$name\" not found" >&2; exit 1; }
        if [ "$have" != "$want" ]; then
            echo "Error from server (Conflict): Operation cannot be fulfilled on Pod \"$name\": Precondition failed: UID in precondition: $want, UID in object meta: $have" >&2
            exit 1
        fi
        jq --arg n "$name" 'map(select(.metadata.name != $n))' "$f" > "$f.new" && mv "$f.new" "$f"
        echo "$ns/$name $want" >> "$S/deleted"
        echo "{\"kind\":\"Pod\",\"metadata\":{\"name\":\"$name\",\"uid\":\"$want\"}}"
        exit 0 ;;
esac
echo "stub: unmodelled call: $*" >&2
exit 1
"#;

struct Fixture {
    root: PathBuf,
    tree: PathBuf,
    state: PathBuf,
    kubectl: PathBuf,
}

fn obj(kind: &str, ns: &str, name: &str) -> String {
    if ns.is_empty() {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}"}}}}"#)
    } else {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}","namespace":"{ns}"}}}}"#)
    }
}

/// A pod as the API serves it. `created` is RFC 3339, as
/// `metadata.creationTimestamp` is.
fn pod(name: &str, uid: &str, phase: &str, created: &str) -> Value {
    json!({
        "kind": "Pod",
        "metadata": {"name": name, "uid": uid, "creationTimestamp": created},
        "spec": {"nodeName": "w-1"},
        "status": {"phase": phase}
    })
}

/// The eviction the packet measured: reason, message, node, the
/// container the kubelet killed, and WHEN it failed — the `Ready`
/// condition's lastTransitionTime, which is where the live evictions of
/// 2026-09-23 carry it (boss-dev-bc5b956bf-wvsrg: created 10:08, failed
/// 17:42). A dev pod lives for days before it is evicted, so its age
/// says nothing about how long ago it died.
fn evicted(name: &str, uid: &str, created: &str, failed: &str) -> Value {
    let mut p = pod(name, uid, "Failed", created);
    p["status"]["conditions"] = json!([
        {"type": "PodScheduled", "status": "True", "lastTransitionTime": created},
        {"type": "Ready", "status": "False", "reason": "PodFailed", "lastTransitionTime": failed}
    ]);
    p["status"]["reason"] = json!("Evicted");
    p["status"]["message"] = json!(
        "The node was low on resource: ephemeral-storage. Threshold quantity: 10Gi, available: 9Gi."
    );
    p["status"]["containerStatuses"] = json!([
        {"name": "session", "state": {"terminated": {"reason": "ContainerStatusUnknown", "exitCode": 137}}}
    ]);
    p
}

fn fixture(case: &str) -> Fixture {
    let root = scratch_dir(&format!("reap-terminated-pods-{case}"));
    let tree = root.join("tree");
    let dir = tree.join("infra/cluster/manifests");
    std::fs::create_dir_all(&dir).unwrap();
    // The two namespaces the tree owns, plus enough files and objects to
    // clear the derivation's "the scrape broke" floors (10 files, 20
    // objects) rather than relaxing them for a fixture.
    std::fs::write(
        dir.join("namespaces.yaml"),
        format!(
            "{}\n{}\n",
            obj("Namespace", "", "boss"),
            obj("Namespace", "", "boss-dev")
        ),
    )
    .unwrap();
    for i in 0..11 {
        std::fs::write(
            dir.join(format!("cm-{i}.yaml")),
            format!(
                "{}\n{}\n",
                obj("ConfigMap", "boss", &format!("cm-{i}-a")),
                obj("ConfigMap", "boss-dev", &format!("cm-{i}-b"))
            ),
        )
        .unwrap();
    }
    let state = root.join("cluster");
    std::fs::create_dir_all(&state).unwrap();
    let kubectl = root.join("kubectl-stub");
    write_exec(&kubectl, STUB);
    Fixture {
        root,
        tree,
        state,
        kubectl,
    }
}

fn set_pods(f: &Fixture, ns: &str, pods: &[Value]) {
    std::fs::write(
        f.state.join(format!("pods-{ns}.json")),
        serde_json::to_string(&Value::Array(pods.to_vec())).unwrap(),
    )
    .unwrap();
}

fn pods_in(f: &Fixture, ns: &str) -> Vec<String> {
    let p = f.state.join(format!("pods-{ns}.json"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| p["metadata"]["name"].as_str().unwrap().to_string())
        .collect()
}

fn deleted(f: &Fixture) -> String {
    std::fs::read_to_string(f.state.join("deleted")).unwrap_or_default()
}

/// The ordinary cluster: one eviction in boss-dev (two days old), one in
/// boss (a day old), and one of each thing the reap must leave alone.
fn ordinary(f: &Fixture) {
    let mut job_pod = pod(
        "gate-run-x-abcde",
        "u-job",
        "Failed",
        "2026-09-20T00:00:00Z",
    );
    job_pod["metadata"]["ownerReferences"] =
        json!([{"kind": "Job", "name": "gate-run-x", "controller": true}]);
    set_pods(
        f,
        "boss-dev",
        &[
            evicted(
                "boss-dev-7d9f-abcde",
                "u-dev-old",
                "2026-09-21T12:00:00Z",
                "2026-09-21T18:00:00Z",
            ),
            // Three days old but FAILED ten minutes ago: inside the
            // window, so not yet a candidate. Keyed on creation it would
            // be reaped the moment it died.
            evicted(
                "boss-dev-7d9f-fresh",
                "u-dev-new",
                "2026-09-20T00:00:00Z",
                "2026-09-23T11:50:00Z",
            ),
            pod(
                "boss-dev-7d9f-alive",
                "u-run",
                "Running",
                "2026-09-01T00:00:00Z",
            ),
            pod(
                "backup-29123-zzzzz",
                "u-ok",
                "Succeeded",
                "2026-09-01T00:00:00Z",
            ),
            job_pod,
        ],
    );
    let mut unknown = pod(
        "boss-jobs-5f6c-qwert",
        "u-boss-old",
        "Failed",
        "2026-09-22T12:00:00Z",
    );
    unknown["status"]["containerStatuses"] = json!([
        {"name": "jobs", "state": {"terminated": {"reason": "Error", "exitCode": 1}}}
    ]);
    set_pods(f, "boss", &[unknown]);
    // A namespace the tree does not own: never listed, never touched.
    set_pods(
        f,
        "kube-system",
        &[evicted(
            "coredns-x",
            "u-kube",
            "2026-09-01T00:00:00Z",
            "2026-09-01T00:00:00Z",
        )],
    );
}

/// Run the script; (exit code, stdout, stderr).
fn run_at(f: &Fixture, now: i64, args: &[&str]) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(script())
        .args(args)
        .env("BOSS_CLUSTER_TREE", &f.tree)
        .env("BOSS_KUBECTL", &f.kubectl)
        .env("KSTATE", &f.state)
        .env("BOSS_REAP_NOW", now.to_string())
        .output()
        .expect("the script runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn plan_at(f: &Fixture, now: i64) -> (i32, String, String) {
    run_at(f, now, &["--plan"])
}

fn sha256(bytes: &str) -> String {
    let dir = scratch_dir("reap-terminated-pods-hash");
    let p = dir.join("plan");
    std::fs::write(&p, bytes).unwrap();
    let out = Command::new("sha256sum").arg(&p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn hash_on(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|l| l.strip_prefix("plan-sha256: "))
        .unwrap_or_else(|| panic!("no plan-sha256 on stderr:\n{stderr}"))
        .to_string()
}

#[test]
fn the_namespaces_are_the_derivations_own() {
    let f = fixture("namespaces");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/cluster/undeclared-objects.sh"))
        .arg("--namespaces")
        .env("BOSS_CLUSTER_TREE", &f.tree)
        .env("BOSS_KUBECTL", &f.kubectl)
        .env("KSTATE", &f.state)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "boss\nboss-dev\n");
}

#[test]
fn the_plan_names_each_failed_pod_its_uid_and_why_it_died() {
    let f = fixture("plan");
    ordinary(&f);
    let (code, out, err) = plan_at(&f, NOW);
    assert_eq!(code, 0, "plan refused:\n{err}");
    for needle in [
        "plan: reap-terminated-pods",
        "namespaces: boss boss-dev",
        "pod boss-dev/boss-dev-7d9f-abcde",
        "uid: u-dev-old",
        "node: w-1",
        "created: 2026-09-21T12:00:00Z",
        "failed by: 2026-09-21T18:00:00Z",
        // No condition recorded a transition: creation is all there is.
        "failed by: 2026-09-22T12:00:00Z",
        "reason: Evicted",
        "message: The node was low on resource: ephemeral-storage. Threshold quantity: 10Gi, available: 9Gi.",
        "session: ContainerStatusUnknown (exit 137)",
        "pod boss/boss-jobs-5f6c-qwert",
        "jobs: Error (exit 1)",
        "== pods to reap (2) ==",
    ] {
        assert!(out.contains(needle), "the plan lacks `{needle}`:\n{out}");
    }
    // Sorted by namespace then name, so the order is a fact of the set.
    let a = out.find("pod boss/boss-jobs").unwrap();
    let b = out.find("pod boss-dev/boss-dev-7d9f-abcde").unwrap();
    assert!(a < b, "{out}");
    for absent in [
        "boss-dev-7d9f-fresh", // inside the window
        "boss-dev-7d9f-alive", // Running
        "backup-29123-zzzzz",  // Succeeded — the history limits own it
        "gate-run-x-abcde",    // a Job's pod — the Job's own retention owns it
        "coredns-x",           // a namespace the tree does not own
    ] {
        assert!(
            !out.contains(absent),
            "the plan names `{absent}`, which it must leave alone:\n{out}"
        );
    }
    // Only the owned namespaces were ever listed.
    let calls = std::fs::read_to_string(f.state.join("calls")).unwrap();
    assert!(!calls.contains("kube-system"), "{calls}");
    // No clock-relative age, no scratch path in what is signed.
    // (By line prefix: a bare `age:` needle matches `message:`.)
    assert!(
        !out.contains(" ago") && !out.lines().any(|l| l.trim_start().starts_with("age")),
        "an age varies between renders:\n{out}"
    );
    assert!(!out.contains(f.root.to_str().unwrap()), "{out}");
    // The hash is of the plan's own bytes, and is not inside them.
    assert_eq!(hash_on(&err), sha256(&out), "{err}");
    // A plan deletes nothing.
    assert!(deleted(&f).is_empty());
}

/// Byte-identical across renders AND across a moved clock that selects
/// the same set — which a one-second test could not see (the lesson
/// `the_same_state_renders_the_same_bytes_and_a_moved_fact_does_not`
/// records for commission-a-disk: a coarse clock passes a fast test).
#[test]
fn the_same_state_renders_the_same_bytes_whatever_the_clock_says() {
    let f = fixture("determinism");
    ordinary(&f);
    let (_, first, _) = plan_at(&f, NOW);
    let (_, second, _) = plan_at(&f, NOW);
    // Five minutes later the fresh pod is still inside its hour.
    let (_, later, _) = plan_at(&f, NOW + 300);
    assert_eq!(first, second);
    assert_eq!(
        first, later,
        "a clock that selects the same pods must render the same bytes"
    );
}

#[test]
fn an_empty_set_is_a_plan_and_its_apply_deletes_nothing() {
    let f = fixture("empty");
    set_pods(
        &f,
        "boss-dev",
        &[pod(
            "boss-dev-alive",
            "u1",
            "Running",
            "2026-09-01T00:00:00Z",
        )],
    );
    let (code, out, err) = plan_at(&f, NOW);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("== pods to reap (0) =="), "{out}");
    assert!(out.contains("nothing to reap"), "{out}");
    let (code, stdout, err) = run_at(&f, NOW, &[&hash_on(&err)]);
    assert_eq!(code, 0, "{stdout}\n{err}");
    assert!(deleted(&f).is_empty());
    let calls = std::fs::read_to_string(f.state.join("calls")).unwrap();
    assert!(!calls.contains("delete"), "{calls}");
}

#[test]
fn the_write_deletes_exactly_the_planned_uids_and_prints_them_first() {
    let f = fixture("write");
    ordinary(&f);
    let (_, plan, err) = plan_at(&f, NOW);
    let hash = hash_on(&err);

    // A hash that is not this plan's deletes nothing, and names both.
    let wrong = "0".repeat(64);
    let (code, _, err) = run_at(&f, NOW, &[&wrong]);
    assert_eq!(code, 78, "{err}");
    assert!(err.contains(&wrong) && err.contains(&hash), "{err}");
    assert!(deleted(&f).is_empty(), "a refused write deleted a pod");

    let (code, stdout, err) = run_at(&f, NOW, &[&hash]);
    assert_eq!(code, 0, "{stdout}\n{err}");
    // THE CAPTURE BEFORE THE DELETE: the plan, reason and message
    // included, is on the output before any pod is gone.
    assert!(stdout.contains(&plan), "the plan is not printed:\n{stdout}");
    assert_eq!(
        deleted(&f),
        "boss/boss-jobs-5f6c-qwert u-boss-old\nboss-dev/boss-dev-7d9f-abcde u-dev-old\n"
    );
    let left = pods_in(&f, "boss-dev");
    for keep in [
        "boss-dev-7d9f-fresh",
        "boss-dev-7d9f-alive",
        "backup-29123-zzzzz",
        "gate-run-x-abcde",
    ] {
        assert!(left.contains(&keep.to_string()), "{keep} was deleted");
    }
    assert_eq!(pods_in(&f, "kube-system"), vec!["coredns-x".to_string()]);
}

/// THE BOUNDARY, stated as a case. A pod ten minutes old at plan time is
/// not in the signed plan; by apply time it is past the hour. It must be
/// neither the reason the apply refuses nor something the apply deletes.
#[test]
fn a_pod_that_crossed_the_window_after_the_plan_is_not_deleted() {
    let f = fixture("boundary");
    ordinary(&f);
    let (_, _, err) = plan_at(&f, NOW);
    let hash = hash_on(&err);
    // Two hours later: boss-dev-7d9f-fresh is now 130 minutes old.
    let (code, stdout, err) = run_at(&f, NOW + 7200, &[&hash]);
    assert_eq!(code, 0, "{stdout}\n{err}");
    assert!(
        pods_in(&f, "boss-dev").contains(&"boss-dev-7d9f-fresh".to_string()),
        "a pod the approver never saw was deleted:\n{}",
        deleted(&f)
    );
    assert_eq!(deleted(&f).lines().count(), 2, "{}", deleted(&f));
}

/// THE BOUNDARY'S OTHER HALF. A pod that was Running when the plan was
/// rendered and failed while the plan waited for its passkey is not in
/// the signed set — and, being days old by creation, it would sit inside
/// every creation-ordered prefix and void the plan. Keyed on WHEN IT
/// FAILED it sorts after the cutoff the plan was rendered under, so the
/// plan still applies and the new failure is tomorrow's plan. Measured
/// shape: the dev pod is evicted about daily, so under creation keying a
/// plan left a day for its tap would almost never still apply.
#[test]
fn a_pod_that_failed_after_the_plan_neither_voids_it_nor_is_reaped() {
    let f = fixture("failed-later");
    ordinary(&f);
    let (_, _, err) = plan_at(&f, NOW);
    let hash = hash_on(&err);
    // The Running pod (created 2026-09-01) is evicted a minute after the
    // plan was rendered.
    let mut pods: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(f.state.join("pods-boss-dev.json")).unwrap())
            .unwrap();
    for p in pods.iter_mut() {
        if p["metadata"]["name"] == "boss-dev-7d9f-alive" {
            *p = evicted(
                "boss-dev-7d9f-alive",
                "u-run",
                "2026-09-01T00:00:00Z",
                "2026-09-23T12:01:00Z",
            );
        }
    }
    set_pods(&f, "boss-dev", &pods);
    let (code, stdout, err) = run_at(&f, NOW + 7200, &[&hash]);
    assert_eq!(code, 0, "{stdout}\n{err}");
    assert!(
        pods_in(&f, "boss-dev").contains(&"boss-dev-7d9f-alive".to_string()),
        "a pod the approver never saw was deleted:\n{}",
        deleted(&f)
    );
    assert_eq!(deleted(&f).lines().count(), 2, "{}", deleted(&f));
}

#[test]
fn a_pod_replaced_under_its_name_after_the_plan_is_a_refusal() {
    let f = fixture("replaced");
    ordinary(&f);
    let (_, _, err) = plan_at(&f, NOW);
    let hash = hash_on(&err);
    let mut again = evicted(
        "boss-dev-7d9f-abcde",
        "u-dev-OTHER",
        "2026-09-21T12:00:00Z",
        "2026-09-21T18:00:00Z",
    );
    again["spec"]["nodeName"] = json!("w-2");
    set_pods(&f, "boss-dev", &[again]);
    let (code, _, err) = run_at(&f, NOW, &[&hash]);
    assert_eq!(code, 78, "{err}");
    assert!(deleted(&f).is_empty(), "{}", deleted(&f));
}

/// The race the plan hash cannot see: a same-named pod appears between
/// the re-render and the delete. The uid precondition is what refuses
/// it — at the API server, not in this script.
#[test]
fn a_same_named_pod_at_the_moment_of_delete_is_refused_by_the_precondition() {
    let f = fixture("race");
    ordinary(&f);
    let (_, _, err) = plan_at(&f, NOW);
    let hash = hash_on(&err);
    std::fs::write(f.state.join("swap-boss-boss-jobs-5f6c-qwert"), "u-new-pod").unwrap();
    let (code, stdout, err) = run_at(&f, NOW, &[&hash]);
    assert_eq!(code, 1, "{stdout}\n{err}");
    assert!(err.contains("Precondition failed"), "{err}");
    assert!(
        pods_in(&f, "boss").contains(&"boss-jobs-5f6c-qwert".to_string()),
        "the new pod was deleted"
    );
}

#[test]
fn a_namespace_this_credential_cannot_list_is_a_failure_not_an_empty_plan() {
    let f = fixture("forbidden");
    ordinary(&f);
    std::fs::write(f.state.join("deny-boss"), "").unwrap();
    let (code, out, err) = plan_at(&f, NOW);
    assert_eq!(code, 1, "{out}\n{err}");
    assert!(
        out.is_empty(),
        "a namespace nobody read is not empty:\n{out}"
    );
    assert!(
        err.contains("Forbidden"),
        "the refusal carries kubectl's words:\n{err}"
    );
}

#[test]
fn a_write_with_no_approved_hash_is_refused() {
    let f = fixture("nohash");
    ordinary(&f);
    for args in [&[][..], &["not-a-hash"][..]] {
        let (code, _, err) = run_at(&f, NOW, args);
        assert_eq!(code, 78, "{args:?}: {err}");
    }
    assert!(deleted(&f).is_empty());
}

/// The structural property: `--plan` exits before the only line that
/// deletes. Read by line, code only — the header talks about the delete
/// at length.
#[test]
fn the_plan_branch_exits_before_the_delete() {
    let body = std::fs::read_to_string(script()).unwrap();
    let code: Vec<(usize, &str)> = body
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with('#')
        })
        .collect();
    let exit = code
        .iter()
        .find(|(_, l)| l.contains("PLAN") && l.contains("exit 0"))
        .map(|(i, _)| *i)
        .expect("the --plan branch no longer exits");
    let delete = code
        .iter()
        .find(|(_, l)| l.contains(" delete --raw ") && !l.trim_start().starts_with("echo "))
        .map(|(i, _)| *i)
        .expect("the script no longer deletes — this pin reads the wrong file");
    assert!(
        exit < delete,
        "--plan exits at line {} after the delete at line {}",
        exit + 1,
        delete + 1
    );
}

#[test]
fn the_plan_verb_needs_no_approval_and_the_write_verb_does() {
    let read = |n: &str| -> Value {
        let p = repo_root().join(format!("infra/ops/verbs/{n}.json"));
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
    };
    let plan = read("plan-a-pod-reap");
    let write = read("reap-terminated-pods");
    assert_ne!(
        plan["requires_approval"], true,
        "the read-only half must run today"
    );
    assert_eq!(
        write["requires_approval"], true,
        "the delete must wait for a passkey"
    );
    for v in [&plan, &write] {
        assert_eq!(v["argv"][0], "infra/forge/reap-terminated-pods.sh");
        assert_eq!(v["hosts"], json!(["forge"]));
    }
    assert_eq!(plan["argv"][1], "--plan");
    // The write's LAST param is the plan's hash — the thing the passkey
    // signs — and there is no other: the scope is derived, never passed.
    let names: Vec<&str> = write["params"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["plan_sha256"]);
    // REQUIRED, not optional: an absent hash is refused by the runner,
    // naming the missing arg, before anything runs — `optional` would
    // drop the word and send the script a bare call instead.
    assert_ne!(write["params"][0]["optional"], true, "{}", write["params"]);
    assert!(
        plan["params"].as_array().unwrap().is_empty(),
        "{}",
        plan["params"]
    );
}
