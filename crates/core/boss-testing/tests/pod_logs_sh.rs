//! `infra/forge/pod-logs.sh` — the READ-ONLY ops verb that reads an
//! instance pod's log through the deploy runner's kubectl (backlog
//! c09cab0b, 2026-09-18: the playground's tenant publish failed
//! silently on a fresh database and no door could say why — the dev
//! pod's ServiceAccount cannot read pods in boss-playground, and
//! journal-tail is systemd only).
//!
//! Pinned here, against a stub kubectl that records its argv:
//!   * the verdict comes FIRST — one line per pod of the deployment
//!     (name, phase, ready, restarts, age) — then each tail under a
//!     `--- <pod> <container> (last N lines) ---` header
//!   * the tail is `kubectl logs --tail=N --prefix`, `--all-containers`
//!     when no container is named, `-c <container>` when one is
//!   * a pod in CrashLoopBackOff / Error also gets a `--previous` tail
//!     of 60 lines — the launcher's last words are in the PREVIOUS
//!     container, which is exactly what the packet could not read
//!   * refusals, before any kubectl call: a namespace instances.toml
//!     does not declare (derived through the renderer's `--instances`,
//!     never a hand list), lines outside 1..400, a name that is not a
//!     plain DNS label
//!   * the verb file: read-only, forge, the four params with patterns
//!     that admit no whitespace and no leading dash, lines bounded by
//!     `max`, timeout 60

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    repo_root().join("infra/forge/pod-logs.sh")
}

fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("pod-logs-{case}"))
}

/// The stub kubectl. Appends its argv (one word per line, `=== call ===`
/// between calls) to `$STUB_LOG` and answers by the verb it sees:
/// `get deployment <name> -o json` from `$STUB_DIR/deploy.json`,
/// `get pods ... -o json` from `$STUB_DIR/pods.json`, and `logs` with
/// a few lines that carry the pod, the container flag, the tail and
/// whether `--previous` was asked — so the test can read the argv off
/// the output as well as off the log.
fn write_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("kubectl");
    boss_testing::write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
{ printf '%s\n' "$@"; echo '=== call ==='; } >> "$STUB_LOG"
verb=""; kind=""; name=""; prev=no; tail=""; cont=""
while [ $# -gt 0 ]; do
    case "$1" in
        -n) shift ;;
        get) verb=get; kind="$2"; shift ;;
        logs) verb=logs; name="$2"; shift ;;
        --previous) prev=yes ;;
        --tail=*) tail="${1#--tail=}" ;;
        --all-containers) cont=all ;;
        -c) cont="$2"; shift ;;
    esac
    shift
done
case "$verb:$kind" in
    get:deployment)
        [ -f "$STUB_DIR/deploy.json" ] || { echo 'Error from server (NotFound): deployments.apps "absent" not found' >&2; exit 1; }
        cat "$STUB_DIR/deploy.json" ;;
    get:pods) cat "$STUB_DIR/pods.json" ;;
    logs:)
        for i in 1 2 3; do
            echo "[pod/$name/$cont] line $i tail=$tail previous=$prev"
        done ;;
    *) echo "stub kubectl: unexpected argv" >&2; exit 1 ;;
esac
"#,
    );
    stub
}

const DEPLOY: &str = r#"{"metadata":{"name":"boss"},"spec":{"selector":{"matchLabels":{"app":"boss","tier":"api"}}}}"#;

/// Two pods: one healthy, one whose `boss` container is in
/// CrashLoopBackOff with 7 restarts.
fn pods_json(now_minus: &str) -> String {
    format!(
        r#"{{"items":[
  {{"metadata":{{"name":"boss-7d9f-aaaaa","creationTimestamp":"{now_minus}"}},
    "status":{{"phase":"Running","containerStatuses":[
      {{"name":"boss","ready":true,"restartCount":0,"state":{{"running":{{}}}}}},
      {{"name":"gateway","ready":true,"restartCount":1,"state":{{"running":{{}}}}}}]}}}},
  {{"metadata":{{"name":"boss-7d9f-bbbbb","creationTimestamp":"{now_minus}"}},
    "status":{{"phase":"Running","containerStatuses":[
      {{"name":"boss","ready":false,"restartCount":7,"state":{{"waiting":{{"reason":"CrashLoopBackOff"}}}}}},
      {{"name":"gateway","ready":true,"restartCount":0,"state":{{"running":{{}}}}}}]}}}}
]}}"#
    )
}

struct Run {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    log: String,
}

fn run_with(case: &str, args: &[&str], deploy: Option<&str>, pods: &str) -> Run {
    let dir = scratch(case);
    let stub = write_stub(&dir);
    let fixtures = dir.join("fixtures");
    boss_testing::create_dir(&fixtures);
    if let Some(d) = deploy {
        boss_testing::write_file(&fixtures.join("deploy.json"), d);
    }
    boss_testing::write_file(&fixtures.join("pods.json"), pods);
    let log = dir.join("argv.log");
    // The ops runner hands a verb no HOME; the script must not need one.
    let out = Command::new("bash")
        .arg(script())
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", stub.to_str().unwrap())
        .env("STUB_LOG", &log)
        .env("STUB_DIR", &fixtures)
        .output()
        .expect("bash runs");
    Run {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        log: std::fs::read_to_string(&log).unwrap_or_default(),
    }
}

fn run(case: &str, args: &[&str]) -> Run {
    // Created two hours ago, so the age column has something to say.
    let stamp = Command::new("date")
        .args(["-u", "-d", "2 hours ago", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .expect("GNU date");
    let stamp = String::from_utf8_lossy(&stamp.stdout).trim().to_string();
    run_with(case, args, Some(DEPLOY), &pods_json(&stamp))
}

/// The instance namespaces, read the way the script must read them:
/// the second column of `render-instance.sh --instances`.
fn instance_namespaces() -> Vec<String> {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/cluster/render-instance.sh"))
        .arg("--instances")
        .output()
        .expect("the renderer runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split('\t').nth(1).map(str::to_string))
        .collect()
}

/// The stub's calls, each as one argv joined by spaces.
fn calls(log: &str) -> Vec<String> {
    log.split("=== call ===\n")
        .map(|c| c.lines().collect::<Vec<_>>().join(" "))
        .filter(|c| !c.is_empty())
        .collect()
}

#[test]
fn the_verdict_comes_first_one_line_per_pod_then_the_tails() {
    let ns = instance_namespaces();
    let ns = ns
        .iter()
        .find(|n| *n != "boss")
        .expect("a non-source instance namespace");
    let r = run("verdict", &[ns, "boss", "120"]);
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    let lines: Vec<&str> = r.stdout.lines().collect();
    // Line 1 and 2: the pods, name phase ready restarts age — before
    // any log text.
    assert!(
        lines[0].starts_with("boss-7d9f-aaaaa ")
            && lines[0].contains("phase=Running")
            && lines[0].contains("ready=2/2")
            && lines[0].contains("restarts=1")
            && lines[0].contains("age=2h"),
        "first line is the first pod's verdict: {:?}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("boss-7d9f-bbbbb ")
            && lines[1].contains("ready=1/2")
            && lines[1].contains("restarts=7")
            && lines[1].contains("CrashLoopBackOff"),
        "second line is the crashing pod, naming the reason: {:?}",
        lines[1]
    );
    // Then the tails, one header per pod, all containers, prefixed.
    let h1 = lines
        .iter()
        .position(|l| *l == "--- boss-7d9f-aaaaa all-containers (last 120 lines) ---")
        .expect("the healthy pod's header");
    assert!(h1 >= 2, "headers come after the verdict lines");
    assert!(
        lines[h1 + 1].starts_with("[pod/boss-7d9f-aaaaa/all] line 1 tail=120 previous=no"),
        "the tail follows its header: {:?}",
        lines[h1 + 1]
    );
    assert!(
        lines.contains(&"--- boss-7d9f-bbbbb all-containers (last 120 lines) ---"),
        "{}",
        r.stdout
    );
    // The kubectl argv: namespace, the deployment's own selector, the
    // tail, all containers, prefix.
    let calls = calls(&r.log);
    assert!(
        calls[0].starts_with(&format!("-n {ns} get deployment boss -o json")),
        "{calls:?}"
    );
    assert!(
        calls[1].starts_with(&format!("-n {ns} get pods -l app=boss,tier=api -o json")),
        "the pods are found by the deployment's selector: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c
                == &format!("-n {ns} logs boss-7d9f-aaaaa --all-containers --prefix --tail=120")),
        "{calls:?}"
    );
}

#[test]
fn a_crashing_pod_also_gets_the_previous_tail_of_60_lines() {
    let ns = instance_namespaces();
    let ns = ns
        .iter()
        .find(|n| *n != "boss")
        .expect("a non-source instance namespace");
    let r = run("previous", &[ns, "boss", "50"]);
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout
            .contains("--- boss-7d9f-bbbbb boss previous (last 60 lines) ---"),
        "{}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains("[pod/boss-7d9f-bbbbb/boss] line 1 tail=60 previous=yes"),
        "{}",
        r.stdout
    );
    let calls = calls(&r.log);
    assert!(
        calls
            .iter()
            .any(|c| c
                == &format!("-n {ns} logs boss-7d9f-bbbbb -c boss --previous --prefix --tail=60")),
        "{calls:?}"
    );
    // The healthy pod gets no --previous, and the crashing pod's
    // healthy container gets none either.
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("boss-7d9f-aaaaa") && c.contains("--previous")),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("-c gateway") && c.contains("--previous")),
        "{calls:?}"
    );
}

#[test]
fn a_named_container_narrows_every_tail_to_it() {
    let ns = instance_namespaces();
    let ns = ns
        .iter()
        .find(|n| *n != "boss")
        .expect("a non-source instance namespace");
    let r = run("container", &[ns, "boss", "10", "gateway"]);
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout
            .contains("--- boss-7d9f-aaaaa gateway (last 10 lines) ---"),
        "{}",
        r.stdout
    );
    let calls = calls(&r.log);
    assert!(
        calls
            .iter()
            .any(|c| c == &format!("-n {ns} logs boss-7d9f-aaaaa -c gateway --prefix --tail=10")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains("--all-containers")),
        "{calls:?}"
    );
    // gateway is not the crashing container, so no previous tail.
    assert!(!calls.iter().any(|c| c.contains("--previous")), "{calls:?}");
}

#[test]
fn a_namespace_outside_instances_toml_is_refused_before_any_kubectl_call() {
    for bad in ["boss-dev", "kube-system", "default"] {
        let r = run(&format!("ns-{bad}"), &[bad, "boss", "100"]);
        assert_eq!(r.status.code(), Some(2), "{bad}: {}{}", r.stdout, r.stderr);
        assert!(
            r.stdout.is_empty(),
            "a refusal prints no answer: {}",
            r.stdout
        );
        assert!(r.stderr.contains(bad), "{}", r.stderr);
        for ns in instance_namespaces() {
            assert!(
                r.stderr.contains(&ns),
                "the refusal names the namespaces it does serve: {}",
                r.stderr
            );
        }
        assert!(r.log.is_empty(), "kubectl was called: {}", r.log);
    }
}

#[test]
fn lines_outside_1_to_400_and_bad_names_are_refused() {
    let ns = instance_namespaces();
    let ns = ns
        .iter()
        .find(|n| *n != "boss")
        .expect("a non-source instance namespace");
    for (case, args) in [
        ("lines-401", vec![ns.as_str(), "boss", "401"]),
        ("lines-0", vec![ns.as_str(), "boss", "0"]),
        ("lines-text", vec![ns.as_str(), "boss", "ten"]),
        ("lines-dash", vec![ns.as_str(), "boss", "-5"]),
        ("deploy-upper", vec![ns.as_str(), "Boss", "10"]),
        ("deploy-dash", vec![ns.as_str(), "-boss", "10"]),
        ("deploy-slash", vec![ns.as_str(), "deploy/boss", "10"]),
        ("container-bad", vec![ns.as_str(), "boss", "10", "boss_x"]),
        ("too-few", vec![ns.as_str(), "boss"]),
        ("too-many", vec![ns.as_str(), "boss", "10", "boss", "extra"]),
    ] {
        let r = run(case, &args);
        assert_eq!(r.status.code(), Some(2), "{case}: {}{}", r.stdout, r.stderr);
        assert!(r.stdout.is_empty(), "{case}: a refusal prints no answer");
        assert!(r.log.is_empty(), "{case}: kubectl was called: {}", r.log);
    }
}

#[test]
fn a_deployment_that_is_not_there_is_cannot_answer_not_no_pods() {
    let ns = instance_namespaces();
    let ns = ns
        .iter()
        .find(|n| *n != "boss")
        .expect("a non-source instance namespace");
    let r = run_with("absent", &[ns, "absent", "10"], None, r#"{"items":[]}"#);
    assert_eq!(r.status.code(), Some(4), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout.is_empty(),
        "no verdict on a failed read: {}",
        r.stdout
    );
    assert!(
        r.stderr.contains("CANNOT ANSWER") && r.stderr.contains("NotFound"),
        "kubectl's own words ride the refusal: {}",
        r.stderr
    );
}

#[test]
fn a_deployment_with_no_pods_says_so_as_its_verdict() {
    let ns = instance_namespaces();
    let ns = ns
        .iter()
        .find(|n| *n != "boss")
        .expect("a non-source instance namespace");
    let r = run_with(
        "empty",
        &[ns, "boss", "10"],
        Some(DEPLOY),
        r#"{"items":[]}"#,
    );
    assert_eq!(r.status.code(), Some(0), "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout.starts_with("deployment boss in ") && r.stdout.contains("no pods"),
        "{}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("---"),
        "no tail for no pod: {}",
        r.stdout
    );
}

/// The namespaces the verb serves are instances.toml's, read through
/// the renderer — the script carries no namespace literal in its code
/// lines (a hand list is the §9a pair that drifts when an instance is
/// added).
#[test]
fn the_namespace_set_is_derived_not_listed() {
    let src = std::fs::read_to_string(script()).unwrap();
    assert!(
        src.contains("render-instance.sh") && src.contains("--instances"),
        "the script reads the renderer's --instances"
    );
    let code: Vec<&str> = src
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    for ns in instance_namespaces() {
        assert!(
            !code
                .iter()
                .any(|l| l.contains(&format!("\"{ns}\"")) || l.contains(&format!("'{ns}'"))),
            "the script names the namespace {ns} in code — derive it"
        );
    }
}

/// Whether `s` matches the verb file's `pattern` — an ERE, the way the
/// runner's jq `test()` reads it; bash's `=~` is the engine every box
/// running this suite has.
fn matches(pattern: &str, s: &str) -> bool {
    Command::new("bash")
        .args(["-c", r#"[[ "$2" =~ $1 ]]"#, "matches", pattern, s])
        .status()
        .expect("bash runs")
        .success()
}

#[test]
fn the_verb_file_is_a_read_only_forge_verb_with_bounded_params() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join("infra/ops/verbs/pod-logs.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(
        v["argv"],
        serde_json::json!(["infra/forge/pod-logs.sh", "{1}", "{2}", "{3}", "{4}"])
    );
    let about = v["about"].as_str().unwrap();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"));
    assert!(
        about.contains("c09cab0b"),
        "about names the packet that asked"
    );
    assert!(
        about.contains("log line"),
        "about says a log line is whatever the process printed — the caller reads it"
    );
    assert_eq!(v["timeout"], serde_json::json!(60));
    let params = v["params"].as_array().unwrap();
    let names: Vec<&str> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["namespace", "deployment", "lines", "container"]);
    let label = params[0]["pattern"].as_str().unwrap();
    assert_eq!(params[0]["pattern"], params[1]["pattern"]);
    assert_eq!(params[1]["pattern"], params[3]["pattern"]);
    for ok in ["boss", "boss-playground", "boss-jobs-api", "a"] {
        assert!(matches(label, ok), "{ok}");
    }
    for bad in [
        "Boss",
        "-boss",
        "boss x",
        "deploy/boss",
        "boss_x",
        "",
        "a\tb",
    ] {
        assert!(!matches(label, bad), "{bad:?} must be refused");
    }
    let lines = params[2]["pattern"].as_str().unwrap();
    assert!(matches(lines, "1") && matches(lines, "400"));
    assert!(!matches(lines, "-1") && !matches(lines, "1000") && !matches(lines, "ten"));
    assert_eq!(params[2]["max"], serde_json::json!(400));
    assert!(
        params[2].get("default").is_none(),
        "lines is required: the caller says how much it wants to read"
    );
    assert_eq!(params[3]["optional"], serde_json::json!(true));
    for p in &params[..3] {
        assert!(
            p.get("optional").is_none() && p.get("default").is_none(),
            "{p}"
        );
    }
    let script = script();
    assert!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o111 != 0);
}
