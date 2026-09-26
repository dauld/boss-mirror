//! A maintenance packet names the host that ran it, and a host opens,
//! recovers and closes only its OWN packet (backlog 79f7678b, 2026-09-26).
//!
//! MEASURED 2026-09-26 (triage run 23f617bb, origin/main d49bbfde). One
//! kind, `maintenance-estate-observe-host`, runs on TWO hosts: the forge
//! every 15 minutes (`HOST_ID=forge`) and boss-gcp daily at 10:25
//! (`HOST_ID=boss-gcp`). Both units pass through
//! `infra/boss-maintenance-wrap.sh` and `infra/boss-step.sh`, and neither
//! script read `HOST_ID`: the wrap looked up the open packet by KIND alone
//! and stamped `{chore}` and nothing else; boss-step counted open packets
//! by kind alone and wrote no host on the run step. Of the newest 200
//! packets of that kind, 198 said "(forge)" in their title and 2 said
//! nothing, and every run step held exactly `[audience, result]` — so on
//! /it/estate one host's observer could hide the other's silence, and the
//! loops section (0d9b2960) read "not named on the packet". Worse, in the
//! code: boss-gcp's run starting while a forge packet is open took it as
//! "recovery" and its ExecStopPost completed the FORGE packet's run step;
//! two open at once made boss-step refuse (exit 1).
//!
//! WHAT THIS PINS, driven through the REAL two scripts against a stub
//! jobs API (`boss-api-curl.sh` beside them — both resolve it next to
//! themselves, so copying them in is what makes the stub the one used):
//!   * the host is `HOST_ID`, else the `BOSS_NODE_ID` a converge unit
//!     declares, else the machine's own name (`uname -n`);
//!   * the wrap stamps it on the packet's metadata as `host` — the key
//!     /it/estate already reads (`md.host` in apps/web/src/it/estate);
//!   * the wrap's recovery and boss-step's one-open check are keyed by
//!     kind AND host, so two hosts under one kind each keep, recover and
//!     close their own packet, and the run step records `host`;
//!   * a Kubernetes pod with no `HOST_ID` stamps NO host: its name is the
//!     pod's, new on every run, so keying on it would orphan a failed
//!     run's packet that the next run is meant to recover. Those chores
//!     keep the kind-only key they had;
//!   * a packet filed before hosts were named (no `host`) is still
//!     recovered — by the first run of its kind, as before — rather than
//!     left open forever by a key it never carried.

use boss_testing::{repo_root, scratch_dir, write_exec};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

const WRAP: &str = "infra/boss-maintenance-wrap.sh";
const STEP: &str = "infra/boss-step.sh";
const KIND: &str = "maintenance-estate-observe-host";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// A stub jobs API with a JSON-array store: GET lists the open jobs of
/// a kind, POST files one with a single active `run` step, PUT completes
/// that step (and closes the job, the way the run step's completion does
/// for a maintenance packet).
const STUB_API: &str = r#"#!/usr/bin/env bash
set -euo pipefail
method=GET; data=""; url=""
while [ $# -gt 0 ]; do
    case "$1" in
        -X) method="$2"; shift 2 ;;
        -H) shift 2 ;;
        -d) data="$2"; shift 2 ;;
        http://*|https://*) url="$1"; shift ;;
        *) shift ;;
    esac
done
store="$STUB_STORE"
[ -s "$store" ] || echo '[]' > "$store"
path="/${url#*://*/}"
case "$method" in
    GET)
        kind=$(printf '%s' "$path" | sed -n 's/.*[?&]kind=\([^&]*\).*/\1/p')
        jq -c --arg k "$kind" \
            '[.[] | select(.kind == $k and .status == "open")] | {data: ., total: length}' "$store"
        ;;
    POST)
        id="job-$(jq length "$store")"
        jq -c --arg id "$id" --argjson body "$data" \
            '. + [$body + {id: $id, steps: [{id: ("step-" + $id), spec_slug: "run", title: "run", status: "active", metadata: {}}]}]' \
            "$store" > "$store.new"
        mv "$store.new" "$store"
        jq -c --arg id "$id" '.[] | select(.id == $id)' "$store"
        ;;
    PUT)
        jid=$(printf '%s' "$path" | sed -n 's#^/api/jobs/\([^/]*\)/steps/.*#\1#p')
        jq -c --arg j "$jid" --argjson p "$data" \
            'map(if .id == $j then .steps[0] += $p | .status = "closed" else . end)' \
            "$store" > "$store.new"
        mv "$store.new" "$store"
        echo '{}'
        ;;
esac
"#;

struct Api {
    bin: PathBuf,
    store: PathBuf,
}

impl Api {
    fn new(tag: &str) -> Api {
        let bin = scratch_dir(&format!("loop-host-{tag}"));
        write_exec(&bin.join("boss-maintenance-wrap.sh"), &read(WRAP));
        write_exec(&bin.join("boss-step.sh"), &read(STEP));
        write_exec(&bin.join("boss-api-curl.sh"), STUB_API);
        let store = bin.join("store.json");
        std::fs::write(&store, "[]").unwrap();
        Api { bin, store }
    }

    fn jobs(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.store).unwrap();
        match serde_json::from_str(&text).unwrap() {
            Value::Array(rows) => rows,
            other => panic!("store is not an array: {other}"),
        }
    }

    /// Plant a packet as the pre-2026-09-26 wrap filed it: no host.
    fn plant_hostless_open(&self) {
        let job = serde_json::json!([{
            "id": "job-legacy", "kind": KIND, "status": "open",
            "metadata": {"chore": KIND},
            "steps": [{"id": "step-legacy", "spec_slug": "run", "title": "run",
                       "status": "active", "metadata": {}}]
        }]);
        std::fs::write(&self.store, job.to_string()).unwrap();
    }

    /// Run one of the two scripts the way a unit does. `host` is the
    /// unit's HOST_ID (None: unset); `pod` sets the variable every
    /// Kubernetes container carries.
    fn run(&self, script: &str, args: &[&str], host: Option<&str>, pod: bool) -> Out {
        self.run_env(script, args, host, pod, &[])
    }

    fn run_env(
        &self,
        script: &str,
        args: &[&str],
        host: Option<&str>,
        pod: bool,
        env: &[(&str, &str)],
    ) -> Out {
        let mut cmd = Command::new("bash");
        cmd.arg(self.bin.join(script))
            .args(args)
            .env("BOSS_JOBS_URL", "http://jobs.test:7900")
            .env("STUB_STORE", &self.store)
            .env_remove("HOST_ID")
            .env_remove("BOSS_NODE_ID")
            .env_remove("KUBERNETES_SERVICE_HOST")
            .env_remove("SERVICE_RESULT")
            .env_remove("EXIT_STATUS")
            .env_remove("BOSS_RUN_SUMMARY_FILE")
            .env_remove("BOSS_STEP_DRY_RUN")
            .env_remove("BOSS_MACHINE_TOKEN");
        if let Some(h) = host {
            cmd.env("HOST_ID", h);
        }
        if pod {
            cmd.env("KUBERNETES_SERVICE_HOST", "10.96.0.1");
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap_or_else(|e| panic!("run {script}: {e}"));
        Out {
            rc: out.status.code().unwrap_or(-1),
            text: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        }
    }

    fn wrap(&self, host: Option<&str>, pod: bool) -> Out {
        self.run(
            "boss-maintenance-wrap.sh",
            &[KIND, "estate host observation"],
            host,
            pod,
        )
    }

    fn step(&self, host: Option<&str>, pod: bool) -> Out {
        self.run("boss-step.sh", &[KIND, "run", "result=ok"], host, pod)
    }
}

struct Out {
    rc: i32,
    text: String,
}

fn ok(o: &Out, what: &str) {
    assert_eq!(o.rc, 0, "{what} exited {}:\n{}", o.rc, o.text);
}

fn packet_host(job: &Value) -> Option<&str> {
    job["metadata"]["host"].as_str()
}

fn run_step(job: &Value) -> &Value {
    &job["steps"][0]
}

fn by_host<'a>(jobs: &'a [Value], host: &str) -> &'a Value {
    jobs.iter()
        .find(|j| packet_host(j) == Some(host))
        .unwrap_or_else(|| panic!("no packet names host {host}: {jobs:#?}"))
}

/// THE PACKET'S CASE. The forge's packet is open when boss-gcp's run
/// starts: boss-gcp files its OWN, and each host's ExecStopPost closes
/// its own — neither refuses on two open, neither completes the other's.
#[test]
fn two_hosts_under_one_kind_each_open_and_close_their_own_packet() {
    let api = Api::new("two-hosts");
    ok(&api.wrap(Some("forge"), false), "forge wrap");
    let gcp = api.wrap(Some("boss-gcp"), false);
    ok(&gcp, "boss-gcp wrap");
    assert!(
        !gcp.text.contains("recovery"),
        "boss-gcp took the forge's open packet as its own recovery:\n{}",
        gcp.text
    );
    let jobs = api.jobs();
    assert_eq!(jobs.len(), 2, "one packet per host: {jobs:#?}");
    assert_eq!(by_host(&jobs, "forge")["metadata"]["chore"], KIND);
    assert_eq!(by_host(&jobs, "boss-gcp")["metadata"]["chore"], KIND);

    ok(&api.step(Some("boss-gcp"), false), "boss-gcp step");
    let jobs = api.jobs();
    let (forge, gcp) = (by_host(&jobs, "forge"), by_host(&jobs, "boss-gcp"));
    assert_eq!(gcp["status"], "closed", "boss-gcp closed its own: {gcp:#}");
    assert_eq!(run_step(gcp)["metadata"]["host"], "boss-gcp");
    assert_eq!(run_step(gcp)["metadata"]["result"], "ok");
    assert_eq!(
        forge["status"], "open",
        "boss-gcp's close must not touch the forge's packet: {forge:#}"
    );

    ok(&api.step(Some("forge"), false), "forge step");
    let jobs = api.jobs();
    let forge = by_host(&jobs, "forge");
    assert_eq!(forge["status"], "closed", "{forge:#}");
    assert_eq!(run_step(forge)["metadata"]["host"], "forge");
}

/// The recovery the wrap exists for survives, scoped: a host whose last
/// run left its packet open reuses THAT packet, and another host's open
/// packet of the same kind neither triggers nor absorbs it.
#[test]
fn a_hosts_open_packet_is_recovered_by_that_host_alone() {
    let api = Api::new("recovery");
    ok(&api.wrap(Some("forge"), false), "forge wrap");
    ok(&api.wrap(Some("boss-gcp"), false), "boss-gcp wrap");
    let again = api.wrap(Some("forge"), false);
    ok(&again, "forge wrap again");
    assert!(again.text.contains("recovery"), "{}", again.text);
    assert_eq!(api.jobs().len(), 2, "no third packet: {:#?}", api.jobs());
}

/// With no HOST_ID the machine names itself — the fallback the forge's
/// cluster watchdog and cluster converge units take, which set none.
#[test]
fn without_host_id_the_machines_own_name_is_the_host() {
    let name = String::from_utf8(Command::new("uname").arg("-n").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(!name.is_empty(), "uname -n answered nothing");
    let api = Api::new("hostname");
    ok(&api.wrap(None, false), "wrap");
    ok(&api.step(None, false), "step");
    let jobs = api.jobs();
    assert_eq!(packet_host(&jobs[0]), Some(name.as_str()), "{jobs:#?}");
    assert_eq!(run_step(&jobs[0])["metadata"]["host"], name.as_str());
}

/// A pod's name is new on every CronJob run, so it is not a host: no
/// host is stamped, and the kind alone keys the packet as it did — the
/// next run still recovers a failed run's packet.
#[test]
fn a_pod_stamps_no_host_and_still_recovers_by_kind() {
    let api = Api::new("pod");
    ok(&api.wrap(None, true), "pod wrap");
    let jobs = api.jobs();
    assert_eq!(jobs.len(), 1);
    assert!(
        jobs[0]["metadata"].get("host").is_none(),
        "a pod name is not a host: {:#}",
        jobs[0]
    );
    let again = api.wrap(None, true);
    ok(&again, "pod wrap again");
    assert!(again.text.contains("recovery"), "{}", again.text);
    ok(&api.step(None, true), "pod step");
    let jobs = api.jobs();
    assert_eq!(jobs[0]["status"], "closed", "{:#}", jobs[0]);
    assert!(run_step(&jobs[0])["metadata"].get("host").is_none());
}

/// A packet filed before this change carries no host. The first run of
/// its kind still recovers and closes it, stamping its host on the run
/// step, rather than leaving it open forever under a key it never had.
#[test]
fn a_packet_filed_before_hosts_were_named_is_still_recovered() {
    let api = Api::new("legacy");
    api.plant_hostless_open();
    let w = api.wrap(Some("forge"), false);
    ok(&w, "wrap");
    assert!(w.text.contains("recovery"), "{}", w.text);
    assert_eq!(api.jobs().len(), 1, "{:#?}", api.jobs());
    ok(&api.step(Some("forge"), false), "step");
    let jobs = api.jobs();
    assert_eq!(jobs[0]["status"], "closed", "{:#}", jobs[0]);
    assert_eq!(run_step(&jobs[0])["metadata"]["host"], "forge");
}

/// The dry run the timers-leave-a-packet lint drives shows the host pair
/// too, and a caller's explicit `host=` wins, as every explicit pair does.
#[test]
fn the_dry_run_shows_the_host_and_an_explicit_host_wins() {
    let api = Api::new("dry");
    let dry = |args: &[&str]| -> String {
        let out = Command::new("bash")
            .arg(api.bin.join("boss-step.sh"))
            .args(args)
            .env("BOSS_STEP_DRY_RUN", "1")
            .env("BOSS_JOBS_URL", "http://jobs.test:7900")
            .env("HOST_ID", "forge")
            .env_remove("SERVICE_RESULT")
            .env_remove("BOSS_RUN_SUMMARY_FILE")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let plain = dry(&[KIND, "run", "result=ok"]);
    assert!(plain.lines().any(|l| l == "host=forge"), "{plain}");
    let explicit = dry(&[KIND, "run", "host=elsewhere"]);
    let hosts: Vec<&str> = explicit
        .lines()
        .filter(|l| l.starts_with("host="))
        .collect();
    assert_eq!(hosts, ["host=elsewhere"], "{explicit}");
}

/// A converge unit declares its estate id as BOSS_NODE_ID, not HOST_ID,
/// and its run step already carries it as node_id; the packet's host is
/// that same id, not whatever the machine calls itself.
#[test]
fn a_converge_units_node_id_is_the_host() {
    let api = Api::new("node-id");
    let node = [("BOSS_NODE_ID", "boss-gcp")];
    ok(
        &api.run_env(
            "boss-maintenance-wrap.sh",
            &[KIND, "converge"],
            None,
            false,
            &node,
        ),
        "wrap",
    );
    ok(
        &api.run_env(
            "boss-step.sh",
            &[KIND, "run", "result=ok"],
            None,
            false,
            &node,
        ),
        "step",
    );
    let jobs = api.jobs();
    assert_eq!(packet_host(&jobs[0]), Some("boss-gcp"), "{jobs:#?}");
    assert_eq!(run_step(&jobs[0])["metadata"]["host"], "boss-gcp");
}
