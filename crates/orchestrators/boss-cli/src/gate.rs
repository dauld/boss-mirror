//! `boss gate <branch>` — launching a gate is one verb, not seven steps.
//!
//! WHY THIS EXISTS (filed 51ca3405). Launching a gate by hand was:
//! write the gate-run packet JSON, POST it, `sed` three placeholders
//! into the runner manifest, split the Job document out of the
//! multi-doc YAML because it uses `generateName` and cannot be
//! `apply`-ed, delete the previous Job, create the new one, then
//! hand-roll a watcher. None of that is judgement; it is the same
//! sequence every time, and on 2026-08-26 it was performed nine times
//! in one afternoon.
//!
//! The tell that it should be a verb is not that it is tedious. It is
//! that doing it CORRECTLY by hand still produces orphans: two open
//! gate-run packets (1fcad667, 2267121b) exist against the same branch
//! because a run died and the single-use packet discipline meant filing
//! a fresh one. Only something that owns the packet's lifecycle can
//! avoid that, which is why this reuses an open packet for the same
//! branch and sha instead of filing a duplicate.
//!
//! ON CONCURRENCY, WHERE THE FILED PROPOSAL IS NOW OUT OF DATE. It
//! asked for a flat refusal to start a second gate, "because there is
//! one gate disk and two concurrent gates crossed verdicts on
//! 2026-08-24". That was true of the manifest that mounts the shared
//! `gate-runner-disk` PVC, and it is not true of the local-disk
//! variant: on 2026-08-26 six gates ran side by side, each with its own
//! `emptyDir` workspace, and all six produced correct independent
//! receipts. So the rule is not "one gate" — it is "one gate PER SHARED
//! WORKSPACE". The refusal is derived from the rendered manifest rather
//! than hardcoded, so pointing `--manifest` at a local-disk runner
//! lifts it automatically and pointing it at the PVC runner restores
//! it.
//!
//! Concurrency past that is bounded by the node, not by correctness.
//! Five parallel gates put w-1 at 65% I/O pressure with CPU pressure at
//! 0.00 and stretched a 35-minute gate to 93 minutes — so a soft warning
//! at [`CROWDED`] says so, and does not refuse.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::train::boss_user;

/// Concurrent local-disk gates past which the node, not the gate, is
/// the constraint. Measured on w-1 (32 cores, one NVMe): at five
/// concurrent gates I/O pressure sat at 65% while CPU pressure stayed
/// at 0.00, and per-gate wall time went from ~35 to ~93 minutes.
/// Throughput was still better than running them one at a time — which
/// is why this warns rather than refuses.
const CROWDED: usize = 3;

/// The placeholders the runner manifest carries.
const BRANCH_PLACEHOLDER: &str = "$GATE_BRANCH";
const PACKET_PLACEHOLDER: &str = "$GATE_RUN_JOB_ID";
const MODE_PLACEHOLDER: &str = "$GATE_MODE";

/// Every `$GATE_*` token the manifest mentions, longest form intact.
///
/// Hand-rolled rather than a regex because boss-cli does not carry one
/// and this is a five-line scan: find the sigil, take the run of
/// identifier characters after it. Taking the WHOLE run is the point —
/// it is what distinguishes `$GATE_MODE_OVERRIDE` from `$GATE_MODE`.
fn gate_tokens(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = manifest.as_bytes();
    let mut i = 0;
    while let Some(pos) = manifest[i..].find("$GATE_") {
        let start = i + pos;
        let mut end = start + 1; // past the '$'
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        out.push(manifest[start..end].to_string());
        i = end;
    }
    out.sort();
    out.dedup();
    out
}

/// Does this rendered Job take a workspace that another gate could be
/// using at the same time?
///
/// The question is only about the gate's WORKSPACE volume. A
/// `persistentVolumeClaim` is shared across pods; an `emptyDir` is
/// created per pod and cannot be. Two gates on one PVC is not slow, it
/// is WRONG: each `git checkout -f -B` yanks the tree from under the
/// other and both write the same receipt path, so on 2026-08-24 the
/// verdicts came back crossed — a receipt naming branch A's head
/// reported under branch B. All three results had to be thrown away.
pub(crate) fn workspace_is_shared(job_yaml: &str) -> bool {
    job_yaml.contains("persistentVolumeClaim")
}

/// Substitute the runner manifest's placeholders and return the single
/// document that is the Job.
///
/// The manifest is multi-document on purpose (it carries the PVC and
/// RBAC beside the Job) and the Job uses `generateName`, which
/// `kubectl apply` rejects — so the Job has to be separated out and
/// `create`-ed. Doing that with `sed` and a hand-written splitter is
/// four of the seven steps this verb replaces.
pub(crate) fn render_job(
    manifest: &str,
    branch: &str,
    packet_id: &str,
    mode: &str,
) -> Result<String> {
    // VALIDATE BEFORE SUBSTITUTING, because substitution destroys the
    // evidence. `$GATE_MODE` is a prefix of `$GATE_MODE_OVERRIDE`, so a
    // plain replace would rewrite the first half of an unknown
    // placeholder and leave something that no longer looks wrong —
    // the manifest would render "cleanly" and the Job would run with a
    // mangled value. Checking first is the only order that can catch it.
    let known = [BRANCH_PLACEHOLDER, PACKET_PLACEHOLDER, MODE_PLACEHOLDER];
    for token in gate_tokens(manifest) {
        if !known.contains(&token.as_str()) {
            bail!(
                "runner manifest uses {token}, which `boss gate` does not know how to fill. \
                 Teach this verb the placeholder rather than letting it render a Job with a \
                 half-substituted value."
            );
        }
    }

    let filled = manifest
        .replace(BRANCH_PLACEHOLDER, branch)
        .replace(PACKET_PLACEHOLDER, packet_id)
        .replace(MODE_PLACEHOLDER, mode);

    let job = filled
        .split("\n---")
        .find(|doc| doc.contains("kind: Job"))
        .map(str::to_string)
        .context("runner manifest contains no `kind: Job` document")?;
    Ok(job)
}

/// The body that files a gate-run packet.
///
/// PURE, AND TESTED, BECAUSE THE API IS PICKIER THAN IT LOOKS. This
/// shipped without `tags` and the jobs API refuses that outright —
/// `422 invalid job body: missing field 'tags'` — so `boss gate` could
/// never file a packet and therefore never ran a gate. The gate that
/// merged it was green and its unit tests passed: nothing in the tree
/// exercised the one call that talks to the API. Found on 2026-08-27 by
/// running the verb rather than by reading it, which is what a
/// proven-in-prod step is for.
///
/// Every field `Job` declares without a serde default has to be here:
/// `kind`, `subject`, `title`, `owner_id`, `status`, `priority`,
/// `metadata`, `tags`. `opened_on` is deliberately **not** — the create
/// handler stamps it from the authoritative (sim-aware) clock precisely
/// so operator-initiated creates inherit it rather than guessing.
pub(crate) fn gate_run_body(branch: &str, sha: &str, manifest: &str) -> Value {
    json!({
        "kind": "gate-run",
        "title": format!("Gate: {branch}"),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": "emp-david",
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": {
            "branch": branch,
            "sha": sha,
            "runner": manifest,
        },
    })
}

/// Normalise `--mode` into what `gate.sh` actually accepts, or refuse.
///
/// THE HELP TEXT NAMED A VALUE THE RUNNER REJECTS. It said `e.g.
/// "auto"`; gate.sh accepts `--auto`; the mode travels through verbatim
/// as `$GATE_MODE`. So following the documentation produced
/// `gate.sh: unknown arg: auto` — and not at the command line. It
/// produced it after a packet was filed, a manifest rendered, a Job
/// created, a pod scheduled and the repo cloned. On 2026-08-27 that
/// cost a whole gate slot (job gate-w6x6b, packet 37af315b) for a typo.
///
/// Two changes, and the order is the point. The friendly spelling is
/// ACCEPTED, so `auto` now means what the help always claimed; and
/// anything unrecognised is refused HERE, before the cluster is
/// touched. That is the same shape as [`render_job`]'s placeholder
/// check — validate before acting, because acting destroys the evidence.
///
/// `-p <crate>` passes through unexamined, deliberately. gate.sh owns
/// whether a crate exists, and it already refuses a `-p` set that does
/// not cover what the tree changed; re-deciding that here would be a
/// second definition to drift from (CLAUDE.md §9a).
pub(crate) fn normalize_mode(mode: &str) -> Result<String> {
    let m = mode.trim();
    match m {
        "" => Ok(String::new()),
        "auto" | "--auto" => Ok("--auto".to_string()),
        _ if m.starts_with("-p ") && m.len() > 3 => Ok(m.to_string()),
        _ => bail!(
            "`--mode {m}` is not a gate mode. gate.sh accepts `--auto` (or plain \
             `auto`, which means the same here) and `-p <crate>`; omit --mode for a \
             full gate.\n  Refusing now rather than after a pod is scheduled and the \
             repo cloned — which is where this used to be discovered."
        ),
    }
}

/// The gate-run packet for this exact branch and sha, if one is already
/// open.
///
/// Reuse rather than file-a-fresh-one is the whole reason this is a
/// verb: a died run leaves an open packet, and the by-hand discipline
/// of "one packet per run" turns that into a permanent orphan.
pub(crate) fn reusable_packet(open: &[Value], branch: &str, sha: &str) -> Option<String> {
    open.iter()
        .find(|j| {
            let md = j.get("metadata").and_then(Value::as_object);
            let m = |k: &str| {
                md.and_then(|m| m.get(k))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            };
            m("branch") == branch && m("sha") == sha
        })
        .and_then(|j| j.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

/// The message a caller gets when `BOSS_JOBS_URL` is unset. Kept apart
/// from the lookup so a test can assert what it teaches without
/// touching process environment.
pub(crate) fn no_instance_message() -> String {
    "BOSS_JOBS_URL is not set, and this verb has no default on purpose.\n\
     The system of record is http://10.20.0.34:7900 (the cluster).\n\
     boss-gcp's http://127.0.0.1:7900 is a SECOND, older, complete \
     deployment holding different data — reading it does not error, it \
     answers, which is worse.\n\
     Set it explicitly, e.g.:\n    \
     BOSS_JOBS_URL=http://10.20.0.34:7900 boss gate <branch> --wait"
        .to_string()
}

/// The jobs API this verb talks to. **NO DEFAULT, deliberately.**
///
/// It used to fall back to `http://127.0.0.1:7900`. On boss-gcp that is
/// not the system of record — it is a second, older, complete BOSS
/// stack, and a wrong instance does not fail, it answers. On
/// 2026-08-27 a `?kind=gate-run` read returned `total: 0` from the local
/// stack while the cluster held 51 packets; a `gate-run v1` spec was
/// then authored against that zero, which would have regressed the live
/// v2 to a worse 3-step v1 on the next bundle reconcile. It was caught
/// by noticing two packet counts disagreed — luck, not process.
///
/// The fix is not a better default. A default that is right on one host
/// and silently wrong on another IS the defect (packet aa783636), so a
/// verb that cannot reach the right instance now reaches none.
fn jobs_base() -> Result<String> {
    std::env::var("BOSS_JOBS_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("{}", no_instance_message()))
}

pub(crate) async fn api(
    http: &reqwest::Client,
    method: reqwest::Method,
    path: &str,
    payload: Option<Value>,
) -> Result<Option<Value>> {
    let mut req = http
        .request(method.clone(), format!("{}{path}", jobs_base()?))
        .header("x-boss-user", boss_user())
        .header("content-type", "application/json");
    if let Some(p) = &payload {
        req = req.json(p);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("jobs api {method} {path}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("jobs api {method} {path} -> {status}: {}", body.trim());
    }
    if body.trim().is_empty() {
        return Ok(None);
    }
    Ok(serde_json::from_str(&body).ok())
}

pub(crate) fn rows(v: Option<Value>) -> Vec<Value> {
    v.and_then(|v| {
        v.get("data")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| v.as_array().cloned())
    })
    .unwrap_or_default()
}

/// The instant a step completed, in the one format every verb writes.
///
/// RFC3339, whole seconds, `Z`. Three verbs stamp `completed_at` — the
/// conductor on the steps it completes, `boss park` on scope/build/gate,
/// `boss prove` on proven — and cycle time is the difference between
/// stamps written by *different* verbs. A format that varied by writer
/// would still look right in every packet and only be wrong in the
/// arithmetic, so it is defined once here rather than three times.
pub(crate) fn stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// `git ls-remote` the branch so the packet records a real head.
///
/// Falls back to the symbolic `origin/<branch>` rather than failing:
/// the runner resolves the branch itself, and a missing sha degrades
/// the packet's record without stopping the gate. It is warned about,
/// because a receipt is worth much less when nobody can say which tree
/// it vouched for.
pub(crate) fn resolve_sha(branch: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["ls-remote", "origin", &format!("refs/heads/{branch}")])
        .output();
    if let Some(sha) = out.ok().filter(|o| o.status.success()).and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .next()
            .filter(|s| s.len() >= 7)
            .map(str::to_string)
    }) {
        return sha;
    }
    eprintln!(
        "boss gate: could not resolve {branch} via `git ls-remote origin` — recording the \
         symbolic ref. The receipt will not name a head."
    );
    format!("origin/{branch}")
}

fn kubectl(namespace: &str) -> std::process::Command {
    let mut c = std::process::Command::new("kubectl");
    c.args(["-n", namespace]);
    c
}

/// Gate Jobs whose pods are still running.
fn running_gates(namespace: &str) -> Result<usize> {
    let out = kubectl(namespace)
        .args([
            "get",
            "pods",
            "--no-headers",
            "--field-selector=status.phase=Running",
        ])
        .output()
        .context("kubectl get pods — is KUBECONFIG set and the cluster reachable?")?;
    if !out.status.success() {
        bail!(
            "kubectl get pods failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("gate-"))
        .count())
}

/// What to do when the running-gate count could not be read.
///
/// AN ERROR IS NOT ZERO. This was `running_gates(ns).unwrap_or(0)`, and
/// zero is precisely the value that satisfies the guard below it —
/// `if shared && running > 0`. So any failure at all (no KUBECONFIG, an
/// unreachable API server, a service account that may create Jobs but
/// not list pods) read as "the cluster is healthy and idle" and the
/// guard passed. The one condition that must never be guessed was
/// guessed, in the permissive direction: the guard failed OPEN.
///
/// What it protects is not hypothetical. Its own message records the
/// cost: on 2026-08-24 two gates sharing one disk crossed their
/// receipts, a receipt naming one branch's head was reported under
/// another, and all three results were discarded.
///
/// So: on a SHARED workspace an unreadable count refuses, carrying the
/// diagnostic `running_gates` already writes ("is KUBECONFIG set and
/// the cluster reachable?") — which the discard also threw away, so the
/// operator met a raw kubectl error from the later create instead.
/// On a private workspace the count only drives an advisory notice, and
/// there a missing hint is the whole cost, so it degrades to zero.
///
/// Pure, so the rule is pinned by tests rather than by this comment.
fn resolve_running(shared: bool, running: Result<usize>) -> Result<usize> {
    match running {
        Ok(n) => Ok(n),
        Err(e) if shared => Err(e).context(
            "cannot verify whether another gate is already running, and this runner \
             mounts a SHARED /gate-target — refusing rather than assuming it is free",
        ),
        Err(_) => Ok(0),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    branch: &str,
    mode: Option<String>,
    manifest: Option<PathBuf>,
    namespace: &str,
    wait: bool,
    dry: bool,
) -> Result<()> {
    let manifest_path =
        manifest.unwrap_or_else(|| PathBuf::from("infra/gate-runner/gate-runner.yaml"));
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading runner manifest {}", manifest_path.display()))?;

    // BEFORE the sha lookup, the packet, the manifest and kubectl —
    // a bad mode should cost a line of output, not a gate slot.
    let mode = normalize_mode(&mode.unwrap_or_default())?;
    let sha = resolve_sha(branch);
    let http = reqwest::Client::new();

    // Reuse before filing. See `reusable_packet`.
    let open = rows(
        api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&status=open&limit=100",
            None,
        )
        .await?,
    );
    let packet = match reusable_packet(&open, branch, &sha) {
        Some(id) => {
            println!(
                "boss gate: reusing open gate-run packet {}",
                &id[..8.min(id.len())]
            );
            id
        }
        None => {
            if dry {
                println!("boss gate: DRY would file a gate-run packet for {branch}@{sha}");
                "dry-run-packet".to_string()
            } else {
                let created = api(
                    &http,
                    reqwest::Method::POST,
                    "/api/jobs",
                    Some(gate_run_body(
                        branch,
                        &sha,
                        &manifest_path.display().to_string(),
                    )),
                )
                .await?;
                created
                    .as_ref()
                    .and_then(|c| c.get("data").unwrap_or(c).get("id"))
                    .and_then(Value::as_str)
                    .context("jobs api did not return an id for the new gate-run packet")?
                    .to_string()
            }
        }
    };

    let job = render_job(&manifest_text, branch, &packet, &mode)?;

    // The concurrency rule, derived rather than hardcoded.
    let shared = workspace_is_shared(&job);
    let running = resolve_running(shared, running_gates(namespace))?;
    if shared && running > 0 {
        bail!(
            "{} gate(s) already running and {} mounts a SHARED workspace.\n  \
             Two gates on one disk cross their receipts — on 2026-08-24 a receipt naming \
             one branch's head was reported under another, and all three results were \
             discarded.\n  Wait for the running gate, or use a runner manifest whose \
             /gate-target is an emptyDir.",
            running,
            manifest_path.display()
        );
    }
    if !shared && running >= CROWDED {
        eprintln!(
            "boss gate: {running} gates already running. Each has its own workspace so the \
             verdicts are safe, but the node becomes the constraint — measured at five \
             concurrent gates: 65% I/O pressure, 0.00 CPU pressure, per-gate wall time \
             ~35min -> ~93min. Proceeding."
        );
    }

    if dry {
        println!("boss gate: DRY would create a Job for {branch} (packet {packet})");
        return Ok(());
    }

    let mut child = kubectl(namespace)
        .args(["create", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("spawning kubectl create — is kubectl on PATH?")?;
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .context("kubectl stdin")?
            .write_all(job.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("kubectl create failed for {branch}");
    }
    let created = String::from_utf8_lossy(&out.stdout).trim().to_string();
    println!("boss gate: {created}");
    println!("boss gate: packet {packet}  branch {branch}@{sha}");

    if wait {
        // `job.batch/gate-xxxxx created` -> `job.batch/gate-xxxxx`
        let job_name = created
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        wait_for_verdict(&http, &packet, namespace, &job_name).await?;
    } else {
        println!("boss gate: not waiting — `boss gate --wait` follows it, or read the packet.");
    }
    Ok(())
}

/// What a silent packet means, given what the Job is doing.
///
/// `None` = keep waiting. `Some(msg)` = stop, and here is why.
///
/// USER FEEDBACK cf0021ae: "A green gate reports Failed when the pod dies
/// after the receipt is written." A gate ran 30/30 checks green on w-1,
/// wrote its receipt, and then the node rebooted; `backoffLimit: 0` failed
/// the Job instantly and `kubectl get job` showed failed=1 for a GREEN
/// gate. The verdict was on the PVC the whole time and had to be recovered
/// by mounting the disk in a throwaway pod.
///
/// This is that defect seen from the waiter's side, and it was worse:
/// `--wait` polled the packet in an unbounded loop with no idea the Job
/// had died, so it waited forever, silently, for a verdict that was never
/// coming. Forty minutes of gate followed by an indefinite hang.
///
/// The Job status is a signal about the RUN and never about the CODE, so
/// it is not treated as a verdict here — it is only used to decide that
/// no verdict is coming, and to say where the answer actually lives.
pub(crate) fn silent_packet_verdict(job_finished: bool, job_failed: bool) -> Option<String> {
    if !job_finished {
        return None;
    }
    if job_failed {
        return Some(
            "the gate Job failed without the packet ever reporting a verdict.\n               That is NOT the same as a red gate: the run died, and the code may well have \
             passed. The receipt is written to /gate-target/receipt.json before the pod \
             exits and survives it, so read the receipt rather than re-running 40 minutes \
             of gate on the assumption this was a failure."
                .to_string(),
        );
    }
    Some(
        "the gate Job finished but the packet never reported a verdict.\n           The run completed, so the receipt at /gate-target/receipt.json should hold the \
         answer; the reporting call is what went missing."
            .to_string(),
    )
}

/// Poll the PACKET, not the pod.
///
/// The runner self-reports its verdict onto the gate-run packet, and
/// the packet is the record that outlives the pod — a gate whose
/// container exited 0 can leave its pod `1/2 NotReady` for hours
/// because a sidecar never exits, so pod phase is the wrong thing to
/// watch. Reading the packet is also what any other actor would do.
async fn wait_for_verdict(
    http: &reqwest::Client,
    packet: &str,
    namespace: &str,
    job_name: &str,
) -> Result<()> {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        let Some(job) = api(
            http,
            reqwest::Method::GET,
            &format!("/api/jobs/{packet}"),
            None,
        )
        .await?
        else {
            continue;
        };
        let job = job.get("data").unwrap_or(&job).clone();
        let verdict = job
            .get("steps")
            .and_then(Value::as_array)
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("record-verdict"))
            })
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("verdict"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(v) = verdict {
            println!("boss gate: {v}");
            if v != "green" {
                bail!("gate verdict: {v}");
            }
            return Ok(());
        }
        // The packet is silent. Before sleeping again, find out whether
        // anything is still coming — an unbounded wait on a dead Job is
        // how this hung forever.
        let (finished, failed) = job_state(namespace, job_name);
        if let Some(why) = silent_packet_verdict(finished, failed) {
            bail!("{why}\n  Job: {job_name} (namespace {namespace}), packet: {packet}");
        }
    }
}

/// Is the gate Job finished, and did it fail? `(false, _)` when the state
/// cannot be read — an unreadable Job is not evidence of anything, and
/// must not end the wait.
fn job_state(namespace: &str, job_name: &str) -> (bool, bool) {
    let out = kubectl(namespace)
        .args([
            "get",
            job_name,
            "-o",
            "jsonpath={.status.succeeded} {.status.failed}",
        ])
        .output();
    let Ok(o) = out else { return (false, false) };
    if !o.status.success() {
        return (false, false);
    }
    let t = String::from_utf8_lossy(&o.stdout);
    let mut it = t.split_whitespace();
    let succeeded: i32 = it.next().unwrap_or("0").parse().unwrap_or(0);
    let failed: i32 = it.next().unwrap_or("0").parse().unwrap_or(0);
    (succeeded > 0 || failed > 0, failed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SHARED WORKSPACE REFUSES WHEN IT CANNOT LOOK.
    ///
    /// The bug this pins is not "the error message was unhelpful". It is
    /// that `unwrap_or(0)` produced the exact value that satisfies
    /// `if shared && running > 0`, so an unreadable cluster passed the
    /// guard protecting against two gates crossing receipts on one disk.
    #[test]
    fn an_unreadable_count_refuses_on_a_shared_workspace() {
        let err = resolve_running(true, Err(anyhow!("kubectl get pods: no KUBECONFIG")))
            .expect_err("a shared workspace must not proceed on an unread count");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("SHARED"),
            "the refusal must say why it refused; got: {msg}"
        );
        assert!(
            msg.contains("KUBECONFIG"),
            "the underlying diagnostic must survive — discarding it is what sent an \
             operator to a raw kubectl error from the later create; got: {msg}"
        );
    }

    /// ...and a private workspace does not, because there the count only
    /// drives an advisory crowding notice. Refusing here would convert a
    /// missing hint into a blocked gate.
    #[test]
    fn an_unreadable_count_is_zero_on_a_private_workspace() {
        assert_eq!(
            resolve_running(false, Err(anyhow!("unreachable"))).expect("must not refuse"),
            0
        );
    }

    #[test]
    fn a_readable_count_passes_through_either_way() {
        assert_eq!(resolve_running(true, Ok(2)).expect("shared, readable"), 2);
        assert_eq!(resolve_running(false, Ok(3)).expect("private, readable"), 3);
        // Zero from a SUCCESSFUL read is still zero — the fix must not
        // turn a genuinely idle cluster into a refusal.
        assert_eq!(resolve_running(true, Ok(0)).expect("shared, idle"), 0);
    }

    /// THE DEFAULT THAT WAS RIGHT ON ONE HOST AND SILENTLY WRONG ON THE
    /// OTHER (packet aa783636).
    ///
    /// A refusal is only useful if it says WHICH instance to reach. The
    /// old fallback pointed at boss-gcp's second, older stack, and a read
    /// against it does not fail — it answers `total: 0` while the system
    /// of record holds 51 packets. So this pins the two things the
    /// message has to carry: the address of the system of record, and a
    /// warning that the tempting local one is a different deployment.
    #[test]
    fn refusing_without_an_instance_names_the_system_of_record() {
        let m = no_instance_message();
        assert!(
            m.contains("10.20.0.34:7900"),
            "the refusal must name the system of record, not just complain: {m}"
        );
        assert!(
            m.contains("127.0.0.1:7900"),
            "it must warn about the second deployment, which is the trap: {m}"
        );
        assert!(
            m.contains("BOSS_JOBS_URL"),
            "it must name the variable to set: {m}"
        );
    }

    /// An empty or whitespace value is not a configured instance. Left
    /// unguarded it would build request URLs like `/api/jobs`, which
    /// reqwest rejects as relative — a confusing failure a long way from
    /// the cause.
    #[test]
    fn an_empty_instance_is_treated_as_unset() {
        // `jobs_base` reads process env, so assert the filter directly on
        // the same predicate rather than mutating a global under test.
        for v in ["", "   ", "\t"] {
            assert!(
                Some(v.to_string())
                    .filter(|s| !s.trim().is_empty())
                    .is_none(),
                "{v:?} must not count as a configured instance"
            );
        }
    }

    /// THE FIELD WHOSE ABSENCE MADE THE WHOLE VERB A NO-OP.
    ///
    /// `POST /api/jobs` refuses a body without a top-level `tags` with
    /// `422 invalid job body: missing field 'tags'`. The verb shipped
    /// without it, so it could never file a packet and therefore never
    /// launched a gate — through a green gate and a merged car, because
    /// nothing in the tree exercised the call. Asserting the shape here
    /// is not a substitute for running it, but it stops this exact
    /// omission returning.
    #[test]
    fn the_packet_body_carries_every_field_the_api_demands() {
        let b = gate_run_body("feat/x", "abc123", "infra/gate-runner/gate-runner.yaml");
        // Exactly the `Job` fields with no serde default and no Option.
        for field in [
            "kind", "subject", "title", "owner_id", "status", "priority", "metadata", "tags",
        ] {
            assert!(
                b.get(field).is_some(),
                "gate-run body is missing top-level `{field}` — the jobs API refuses it"
            );
        }
        assert!(
            b.get("tags").and_then(Value::as_array).is_some(),
            "`tags` must be an array, not merely present"
        );
        assert_eq!(b["kind"], "gate-run");
        assert_eq!(b["metadata"]["branch"], "feat/x");
        assert_eq!(b["metadata"]["sha"], "abc123");
    }

    /// The create handler injects `opened_on` off the authoritative
    /// clock when the body omits it. Sending our own would substitute a
    /// caller's idea of the date for the company's.
    #[test]
    fn the_packet_lets_the_api_stamp_the_open_date() {
        let b = gate_run_body("feat/x", "abc123", "infra/gate-runner/gate-runner.yaml");
        assert!(
            b.get("opened_on").is_none(),
            "`opened_on` must be left to the create handler's clock"
        );
    }

    /// THE TEST THAT WOULD HAVE CAUGHT THE ORIGINAL BUG.
    ///
    /// Listing field names, as the two tests above do, only pins what I
    /// already know to look for — and what shipped broken was a field I
    /// did not know to look for. `POST /api/jobs` deserializes the body
    /// into `boss_core::job::Job` and returns `422 invalid job body: {e}` on
    /// failure, so running that same deserialization here asks the
    /// authoritative type what it requires instead of me guessing.
    ///
    /// `opened_on` is injected by the handler before it deserializes
    /// (operator creates omit it), so injecting it here reproduces what
    /// the type actually sees.
    #[test]
    fn the_body_deserializes_into_the_job_type_the_api_parses_it_as() {
        let mut b = gate_run_body("feat/x", "abc123", "infra/gate-runner/gate-runner.yaml");
        b.as_object_mut()
            .expect("body is an object")
            .insert("opened_on".into(), json!("2026-08-27"));

        let job: boss_core::job::Job = serde_json::from_value(b)
            .expect("gate-run body must deserialize into Job — this is verbatim what the API does");

        assert_eq!(job.kind, "gate-run");
        assert_eq!(job.metadata["branch"], "feat/x");
        assert!(job.tags.is_empty());
    }

    /// The runner path travels on the packet so a reader can tell which
    /// rig produced a verdict without guessing from the branch name.
    #[test]
    fn the_packet_records_which_runner_manifest_rendered_it() {
        let b = gate_run_body("feat/x", "abc123", "infra/gate-runner/local.yaml");
        assert_eq!(b["metadata"]["runner"], "infra/gate-runner/local.yaml");
    }

    /// THE SPELLING THE HELP TEXT ALWAYS PROMISED.
    #[test]
    fn the_friendly_spelling_of_auto_is_accepted() {
        assert_eq!(normalize_mode("auto").unwrap(), "--auto");
        assert_eq!(normalize_mode("--auto").unwrap(), "--auto");
        // Whitespace is a typo, not a mode.
        assert_eq!(normalize_mode("  auto  ").unwrap(), "--auto");
    }

    #[test]
    fn no_mode_means_a_full_gate() {
        assert_eq!(normalize_mode("").unwrap(), "");
        assert_eq!(normalize_mode("   ").unwrap(), "");
    }

    /// gate.sh owns whether the crate exists; this only checks shape.
    #[test]
    fn a_scoped_mode_passes_through_untouched() {
        assert_eq!(normalize_mode("-p boss-jobs").unwrap(), "-p boss-jobs");
        assert_eq!(
            normalize_mode("-p boss-jobs -p boss-cli").unwrap(),
            "-p boss-jobs -p boss-cli"
        );
    }

    /// THE CASE THAT COST A GATE SLOT. `-p` with nothing after it is the
    /// same class — a mode the runner will reject once it is far too
    /// late to say so cheaply.
    #[test]
    fn an_unknown_mode_is_refused_before_anything_is_scheduled() {
        for bad in ["autp", "full", "--fast", "-p", "-p ", "auto --auto"] {
            let err = normalize_mode(bad)
                .expect_err(&format!("`--mode {bad}` must be refused, not forwarded"));
            let msg = err.to_string();
            assert!(
                msg.contains("not a gate mode"),
                "the refusal must say what is wrong: {msg}"
            );
            assert!(
                msg.contains("--auto") && msg.contains("-p <crate>"),
                "the refusal must name what IS accepted, or it just says no: {msg}"
            );
        }
    }

    const MANIFEST: &str = "\
apiVersion: v1\nkind: PersistentVolumeClaim\nmetadata:\n  name: gate-runner-disk\n\
---\napiVersion: batch/v1\nkind: Job\nmetadata:\n  generateName: gate-\nspec:\n  template:\n\
    spec:\n      containers:\n        - name: gate\n          env:\n\
            - {name: GATE_BRANCH, value: $GATE_BRANCH}\n\
            - {name: GATE_RUN_JOB_ID, value: $GATE_RUN_JOB_ID}\n\
            - {name: GATE_MODE, value: $GATE_MODE}\n\
      volumes:\n        - name: gate-runner-disk\n          emptyDir: {}\n";

    #[test]
    fn rendering_fills_every_placeholder_and_keeps_only_the_job() {
        let job = render_job(MANIFEST, "fix/a-thing", "pkt-1", "full").expect("renders");
        assert!(job.contains("kind: Job"));
        assert!(
            !job.contains("PersistentVolumeClaim"),
            "the PVC document must not be created"
        );
        assert!(job.contains("fix/a-thing"));
        assert!(job.contains("pkt-1"));
        assert!(job.contains("full"));
    }

    /// A manifest that grows a placeholder this verb does not know
    /// about must fail loudly. The alternative is a gate that runs with
    /// a literal or half-substituted value and reports a verdict about
    /// nothing.
    #[test]
    fn an_unknown_placeholder_is_refused() {
        let m = MANIFEST.replace("$GATE_MODE", "$GATE_TIMEOUT");
        let err = render_job(&m, "b", "p", "full").expect_err("must refuse");
        assert!(format!("{err}").contains("$GATE_TIMEOUT"), "{err}");
    }

    /// THE ONE THE FIRST DRAFT GOT WRONG. `$GATE_MODE` is a prefix of
    /// `$GATE_MODE_OVERRIDE`, so substituting first would rewrite the
    /// front of an unknown placeholder and leave `full_OVERRIDE` —
    /// which no longer looks like a placeholder, so a
    /// check-after-substitute would pass and the Job would run with a
    /// mangled value. Validating first is the only order that catches
    /// it.
    #[test]
    fn a_placeholder_that_extends_a_known_one_is_still_refused() {
        let m = MANIFEST.replace("$GATE_MODE", "$GATE_MODE_OVERRIDE");
        let err = render_job(&m, "b", "p", "full").expect_err("must refuse");
        assert!(format!("{err}").contains("$GATE_MODE_OVERRIDE"), "{err}");
    }

    #[test]
    fn tokens_are_read_whole_not_by_prefix() {
        let found = gate_tokens("a $GATE_MODE b $GATE_MODE_OVERRIDE c $GATE_BRANCH");
        assert_eq!(
            found,
            vec![
                "$GATE_BRANCH".to_string(),
                "$GATE_MODE".to_string(),
                "$GATE_MODE_OVERRIDE".to_string()
            ]
        );
    }

    #[test]
    fn a_manifest_with_no_job_is_refused() {
        let err = render_job("kind: ConfigMap\n", "b", "p", "").expect_err("must refuse");
        assert!(format!("{err}").contains("kind: Job"), "{err}");
    }

    /// THE ONE THAT PROTECTS A RECEIPT. An emptyDir workspace is
    /// per-pod and safe to run beside another; a claim is not.
    #[test]
    fn a_claim_is_shared_and_an_emptydir_is_not() {
        let local = render_job(MANIFEST, "b", "p", "").expect("renders");
        assert!(!workspace_is_shared(&local));

        let pvc_manifest = MANIFEST.replace(
            "          emptyDir: {}",
            "          persistentVolumeClaim: {claimName: gate-runner-disk}",
        );
        let shared = render_job(&pvc_manifest, "b", "p", "").expect("renders");
        assert!(
            workspace_is_shared(&shared),
            "a claimed workspace must be recognised as shared, or two gates will cross \
             their receipts as they did on 2026-08-24"
        );
    }

    #[test]
    fn an_open_packet_for_the_same_branch_and_sha_is_reused() {
        let open = vec![json!({
            "id": "aaaaaaaa-1111",
            "metadata": {"branch": "fix/x", "sha": "deadbeef"}
        })];
        assert_eq!(
            reusable_packet(&open, "fix/x", "deadbeef").as_deref(),
            Some("aaaaaaaa-1111")
        );
    }

    /// Same branch, NEW head is a different run and must get its own
    /// packet — a receipt is about a tree, not about a branch name.
    #[test]
    fn a_new_head_on_the_same_branch_is_not_reused() {
        let open = vec![json!({
            "id": "aaaaaaaa-1111",
            "metadata": {"branch": "fix/x", "sha": "deadbeef"}
        })];
        assert_eq!(reusable_packet(&open, "fix/x", "cafebabe"), None);
        assert_eq!(reusable_packet(&open, "fix/y", "deadbeef"), None);
    }

    #[test]
    fn a_packet_without_metadata_does_not_panic() {
        let open = vec![json!({"id": "no-metadata"})];
        assert_eq!(reusable_packet(&open, "fix/x", "deadbeef"), None);
    }

    /// A RUNNING JOB MEANS KEEP WAITING — the common case, and the one a
    /// wrong answer here would break.
    #[test]
    fn a_running_job_does_not_end_the_wait() {
        assert!(silent_packet_verdict(false, false).is_none());
        assert!(silent_packet_verdict(false, true).is_none());
    }

    /// THE FEEDBACK'S CASE (cf0021ae): the pod died, the Job says failed,
    /// and the packet never reported. The waiter must stop — and must NOT
    /// call it a red gate, because the code may have passed.
    #[test]
    fn a_dead_job_with_a_silent_packet_stops_and_refuses_to_call_it_red() {
        let msg = silent_packet_verdict(true, true).expect("a dead Job must end the wait");
        assert!(msg.contains("NOT the same as a red gate"), "{msg}");
        assert!(
            msg.contains("receipt.json"),
            "it must say where the answer actually lives: {msg}"
        );
    }

    /// A Job that finished cleanly but never reported is a different
    /// story — the run completed, so the receipt should hold the answer.
    #[test]
    fn a_finished_job_with_a_silent_packet_points_at_the_receipt() {
        let msg = silent_packet_verdict(true, false).expect("a finished Job must end the wait");
        assert!(msg.contains("receipt.json"), "{msg}");
        assert!(
            !msg.contains("NOT the same as a red gate"),
            "that caveat belongs to the failed case only: {msg}"
        );
    }
}
