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
//! ON CONCURRENCY. Gates run in PARALLEL since packet 28de3845: the
//! runner's workspace is a per-run emptyDir (seeded warm from the old
//! PVC, which survives as the seed + crate cache), so two gates cannot
//! see each other's tree and the 2026-08-24 crossed-receipts incident
//! is structurally impossible — proven shape: on 2026-08-26 six
//! emptyDir gates ran side by side and produced six correct
//! independent receipts. The old one-gate-per-shared-workspace refusal
//! (and the volumeattachment detach guard that served it) died in the
//! same car that made dying safe; the isolation contract is pinned by
//! boss-testing's gate_runner_parallel_workspace tests rather than
//! re-derived from the rendered manifest here.
//!
//! What remains bounded is the NODE, not correctness. Five parallel
//! gates put w-1 at 65% I/O pressure with CPU pressure at 0.00 and
//! stretched a 35-minute gate to 93 minutes — so this verb counts live
//! gate Jobs and refuses politely at [`DEFAULT_MAX_CONCURRENT`]
//! (override: BOSS_GATE_MAX_CONCURRENT), naming the running gates. The
//! count is best-effort against a race (two verbs counting at once can
//! both see N-1), which is acceptable now that over-admission costs
//! minutes, not verdicts; the scheduler's ephemeral-storage accounting
//! is the hard backstop on the disk.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::train::boss_user;

/// The COMPILED fallback for how many gates run at once — the last
/// resort when neither the env override nor the delivery policy can be
/// read. It is no longer the SOLE source: the number an operator tunes
/// lives in the `delivery_policy` registry (`gate_max_concurrent`),
/// which this verb fetches the same way the conductor does, so raising
/// the bound from 3 to 4 is a policy edit, not a code car. This constant
/// survives only so a gate can still run when the registry is
/// unreachable, and its value matches the seeded policy row
/// (boss-cli's `the_seeded_policy_equals_the_compiled_fallback` pins
/// the two — CLAUDE.md §9a).
///
/// Three is inside the measured comfort zone on w-1 (32 cores, one
/// NVMe): at FIVE concurrent gates I/O pressure sat at 65% while CPU
/// pressure stayed at 0.00, and per-gate wall time went from ~35 to ~93
/// minutes — total throughput still beat serial, but each verdict
/// arrived slower than two gates' worth of queueing.
const DEFAULT_MAX_CONCURRENT: usize = 3;

/// The placeholders the runner manifest carries.
const BRANCH_PLACEHOLDER: &str = "$GATE_BRANCH";
const PACKET_PLACEHOLDER: &str = "$GATE_RUN_JOB_ID";
const MODE_PLACEHOLDER: &str = "$GATE_MODE";
/// The branch, sanitized to DNS-label characters, so concurrent Jobs
/// are tellable apart: `gate-$GATE_NAME_HINT-<rand>`. Derived from the
/// branch by [`name_hint`] — never passed in.
const HINT_PLACEHOLDER: &str = "$GATE_NAME_HINT";

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

/// Does this rendered Job mount a PersistentVolumeClaim at
/// /gate-target — the PRE-PARALLEL manifest shape?
///
/// The shipped manifest's workspace is a per-run emptyDir (the seed
/// PVC mounts at /gate-seed), so a bare `contains("persistentVolumeClaim")`
/// stopped meaning "shared workspace" the day the seed shipped. But a
/// stale checkout, or `--manifest`, can still render the OLD shape
/// whose /gate-target IS the shared PVC — and for that shape the old
/// law still holds absolutely: two gates on one workspace disk cross
/// their receipts (2026-08-24; all three results discarded). So the
/// discriminator is the MOUNT, not the volume list: which volume backs
/// /gate-target, and is that volume a claim.
///
/// Reads both the inline mount style the manifests actually use
/// (`- {name: x, mountPath: /gate-target}`) and block-style entries —
/// a parser proven only against one spelling answers None against the
/// other and the guard silently never engages (da260655's shape).
pub(crate) fn pvc_backed_workspace(job_yaml: &str) -> bool {
    let mount_is_gate_target = |line: &str| {
        line.split("mountPath:").nth(1).is_some_and(|rest| {
            rest.trim_start()
                .trim_end_matches('}')
                .split([',', ' '])
                .next()
                == Some("/gate-target")
        })
    };
    // Which volume is mounted at /gate-target?
    let mut current_entry: Option<String> = None;
    let mut workspace: Option<String> = None;
    for line in job_yaml.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- name:") {
            current_entry = Some(rest.trim().to_string());
        }
        if t.contains("mountPath:") && mount_is_gate_target(t) {
            workspace = if t.contains("name:") {
                // Inline `- {name: x, mountPath: /gate-target}`.
                t.split("name:")
                    .nth(1)
                    .and_then(|a| a.trim_start().split([',', '}']).next())
                    .map(|s| s.trim().to_string())
            } else {
                // Block style: the entry opened by the last `- name:`.
                current_entry.clone()
            };
        }
    }
    let Some(ws) = workspace else {
        return false;
    };
    // Is that volume claim-backed?
    let mut in_entry = false;
    for line in job_yaml.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- name:") {
            in_entry = rest.trim() == ws;
            continue;
        }
        if in_entry && t.starts_with("persistentVolumeClaim") {
            return true;
        }
    }
    false
}

/// The branch, ground down to what a Kubernetes name/label value may
/// carry: lowercase alphanumerics and single dashes, at most 20 chars,
/// never starting or ending on a dash. `feat/gates-run-in-parallel`
/// becomes `feat-gates-run-in-pa`; a branch with no usable characters
/// falls back to `branch` rather than rendering an invalid manifest.
///
/// WHY: concurrent Jobs used to be `gate-8kx2p`, `gate-w6x6b` — a
/// refusal or a status line naming three of those names nothing. The
/// hint rides in `generateName: gate-<hint>-` (the API server still
/// appends its random suffix, which keeps names fresh) and in the
/// `boss.dev/branch` label.
pub(crate) fn name_hint(branch: &str) -> String {
    let mut out = String::new();
    for c in branch.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(20);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "branch".to_string()
    } else {
        out
    }
}

/// The concurrency bound: the env override when set, else `fallback`.
///
/// THE SOURCE CHAIN is env > policy > compiled. This function owns the
/// env leg — it is pure over the raw env value so the parsing rules pin
/// in tests — and takes the resolved `fallback` (the delivery policy's
/// `gate_max_concurrent`, or the compiled default when the registry was
/// unreadable) for when no override is set. The override keeps its old
/// meaning: it is the operator's escape hatch when the node has grown or
/// shrunk between policy edits.
///
/// An unparseable override REFUSES rather than silently meaning the
/// fallback — a typo'd bound that quietly becomes some other number is
/// the same defect class as the wrong-instance default this file already
/// refuses (packet aa783636): right sometimes, silently wrong when it
/// matters. Zero refuses too: it would deny every gate forever, which is
/// a misconfiguration, not a policy — set 1 to serialize.
pub(crate) fn max_concurrent_from(raw: Option<&str>, fallback: usize) -> Result<usize> {
    let Some(v) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(fallback);
    };
    let n: usize = v.parse().map_err(|_| {
        anyhow!(
            "BOSS_GATE_MAX_CONCURRENT={v} is not a count. Set a positive integer \
             (currently {fallback}), or unset it to take the delivery policy's bound."
        )
    })?;
    if n == 0 {
        bail!(
            "BOSS_GATE_MAX_CONCURRENT=0 would refuse every gate. Set 1 to \
             serialize, or unset it to take the delivery policy's bound ({fallback})."
        );
    }
    Ok(n)
}

/// The policy leg of the source chain: the `gate_max_concurrent` on the
/// active `train-conductor` delivery policy, or the compiled fallback
/// when the registry is unreachable, absent, or holds a nonsense value.
///
/// NEVER FAILS — a policy read that cannot answer must not stop a gate,
/// exactly as the conductor's `resolve_from` falls back rather than
/// wedging every train. A degraded read is warned about (one line) so
/// "the bound looks wrong" has a trail, then the compiled default
/// carries the gate.
async fn policy_max_concurrent(http: &reqwest::Client) -> usize {
    let fetched = api(
        http,
        reqwest::Method::GET,
        "/api/delivery/policy/train-conductor",
        None,
    )
    .await;
    let row = match fetched {
        Ok(Some(v)) if !v.is_null() => v,
        // No policy, a null answer, or an unreachable registry: the
        // compiled default is the honest fallback.
        _ => return DEFAULT_MAX_CONCURRENT,
    };
    // The API answers the bare row (or `{data: row}`); read either.
    let n = row
        .get("data")
        .unwrap_or(&row)
        .get("gate_max_concurrent")
        .and_then(Value::as_i64);
    match n {
        Some(n) if n > 0 => n as usize,
        _ => {
            eprintln!(
                "boss gate: delivery policy has no usable gate_max_concurrent — \
                 using the compiled bound of {DEFAULT_MAX_CONCURRENT}"
            );
            DEFAULT_MAX_CONCURRENT
        }
    }
}

/// The concurrency bound in force: env override > delivery policy >
/// compiled fallback.
async fn max_concurrent(http: &reqwest::Client) -> Result<usize> {
    let fallback = policy_max_concurrent(http).await;
    max_concurrent_from(
        std::env::var("BOSS_GATE_MAX_CONCURRENT").ok().as_deref(),
        fallback,
    )
}

/// The polite refusal at the concurrency bound, or None below it.
///
/// Pure, and it NAMES the running gates — the operator's next move is
/// to wait for or watch one of them, and a bound that says only "3
/// running" sends them off to run the kubectl this verb already ran.
pub(crate) fn crowd_refusal(live: &[String], max: usize) -> Option<String> {
    if live.len() < max {
        return None;
    }
    Some(format!(
        "{n} gate(s) already running ({names}) — at the concurrency bound of {max}.\n  \
         Every workspace is per-run so the verdicts stay independent, but the gates \
         share one build node and one seed disk: at five concurrent, I/O pressure hit \
         65% and a ~35-minute gate took ~93 (measured 2026-08-26). Wait for one to \
         finish, or raise BOSS_GATE_MAX_CONCURRENT if the node has grown.",
        n = live.len(),
        names = live.join(", "),
    ))
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
    let known = [
        BRANCH_PLACEHOLDER,
        PACKET_PLACEHOLDER,
        MODE_PLACEHOLDER,
        HINT_PLACEHOLDER,
    ];
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
        .replace(HINT_PLACEHOLDER, &name_hint(branch))
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

/// The park prose a gate carries so the auto-park handler can file the
/// car VERBATIM on green — the input half of the auto-park loop. Stamped
/// as `park_*` keys on the gate-run metadata by the `--park-*` flags;
/// absent means a plain gate that will not auto-park. The four fields a
/// receipt needs (summary/excludes/test/verified) are required together,
/// so a stamped intent is always enough to file a valid car; a
/// `backlog_item` edge is optional.
#[derive(Debug, Clone, Default)]
pub struct ParkIntent {
    pub summary: Option<String>,
    pub excludes: Option<String>,
    pub test: Option<String>,
    pub verified: Option<String>,
    pub backlog_item: Option<String>,
}

impl ParkIntent {
    /// True when no `--park-*` flag was given: a plain gate.
    pub fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.excludes.is_none()
            && self.test.is_none()
            && self.verified.is_none()
            && self.backlog_item.is_none()
    }

    /// Refuse a PARTIAL intent. Auto-park files a car with a full
    /// receipt, so if any park flag is given the four a receipt needs
    /// must all be given — better a refusal here than a car filed with an
    /// empty boundary or an unproven `verified` line.
    pub fn require_complete(&self) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let missing: Vec<&str> = [
            ("--park-summary", self.summary.is_none()),
            ("--park-excludes", self.excludes.is_none()),
            ("--park-test", self.test.is_none()),
            ("--park-verified", self.verified.is_none()),
        ]
        .into_iter()
        .filter(|(_, m)| *m)
        .map(|(f, _)| f)
        .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "auto-park needs a full receipt: {} not set. Pass all of \
                 --park-summary / --park-excludes / --park-test / --park-verified, or none.",
                missing.join(", ")
            )
        }
    }

    /// The metadata patch to MERGE onto the gate-run — only the fields
    /// set, keyed `park_*` so the auto-park handler reads them on green.
    pub fn metadata_patch(&self) -> Value {
        let mut m = serde_json::Map::new();
        let mut put = |k: &str, v: &Option<String>| {
            if let Some(v) = v {
                m.insert(k.to_string(), json!(v));
            }
        };
        put("park_summary", &self.summary);
        put("park_excludes", &self.excludes);
        put("park_test", &self.test);
        put("park_verified", &self.verified);
        put("park_backlog_item", &self.backlog_item);
        Value::Object(m)
    }
}

/// Close a just-registered gate-run whose launch was REFUSED before any
/// Job existed, so the refusal leaves no orphan (ed7f1355: the shared-
/// workspace guard fired after the packet was filed, the packet sat
/// open with no runner to ever complete it, closing it honestly meant
/// hand-writing a receipt, and that hand-written head then shadowed the
/// real green in `boss park`). Machine-written `lost` with an empty
/// head — the launch never resolved one, and an empty head is exactly
/// what keeps this receipt from ever matching a real one.
///
/// Best-effort by design: the refusal is the primary fact and must
/// surface either way; a failed close is reported beside it rather than
/// replacing it.
async fn close_refused(http: &reqwest::Client, packet: &str, reason: &str) {
    let result = async {
        let job = api(
            http,
            reqwest::Method::GET,
            &format!("/api/jobs/{packet}"),
            None,
        )
        .await?
        .ok_or_else(|| anyhow!("gate-run {packet} vanished"))?;
        let step_id = job
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| s.get("title").and_then(Value::as_str) == Some("Record the receipt"))
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("gate-run {packet} has no verdict step"))?
            .to_string();
        let receipt = serde_json::to_string(&json!({
            "verdict": "lost",
            "head": "",
            "mode": "",
            "fails": [format!("launch refused before any Job was created: {reason}")],
        }))?;
        api(
            http,
            reqwest::Method::PUT,
            &format!("/api/jobs/{packet}/steps/{step_id}"),
            Some(json!({
                "status": "completed",
                "metadata": { "verdict": "lost", "receipt": receipt },
            })),
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => println!(
            "boss gate: refused launch closed its own packet ({} lost)",
            &packet[..8.min(packet.len())]
        ),
        Err(e) => eprintln!(
            "boss gate: could not close the refused packet {packet}: {e:#}\n  \
             close it by hand or the overdue alarm will find it."
        ),
    }
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

/// The head the runner actually gated, read off the receipt it reported
/// onto the packet's record-verdict step.
///
/// THE PACKET'S OWN `sha` CAN LIE (410bf724). This verb resolves the
/// branch with `ls-remote` and files that sha; the RUNNER then clones
/// from the forge and gates whatever head it finds. Move the branch in
/// between and the two differ — and only the receipt, written by the
/// process that ran the checks, says which tree the verdict is about.
///
/// The receipt travels as a JSON STRING inside the step metadata (the
/// same encoding `boss receipt` parses), so it needs a second parse. A
/// runner that died before a receipt reports prose in this field
/// ("runner died before a receipt: …"), which fails that parse and
/// correctly reads as "no gated head".
pub(crate) fn receipt_head(packet: &Value) -> Option<String> {
    let raw = packet
        .get("steps")?
        .as_array()?
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("record-verdict"))?
        .pointer("/metadata/receipt")?
        .as_str()?;
    serde_json::from_str::<Value>(raw)
        .ok()?
        .get("head")?
        .as_str()
        .filter(|h| !h.is_empty())
        .map(str::to_string)
}

/// The metadata correction a packet is owed once its receipt lands.
///
/// `recorded` is the `sha` the packet carries (what this verb resolved
/// before launching); `gated` is the receipt's head (what the runner
/// checked out and ran the gate against). When they differ the packet
/// is lying about which tree its verdict covers, so the truthful head
/// takes over the `sha` field and the request survives as
/// `requested_head` — provenance, not a key. `None` means the packet
/// already tells the truth, or there is no receipt to correct it with.
pub(crate) fn truth_patch(recorded: &str, gated: Option<&str>) -> Option<Value> {
    let gated = gated?;
    (!gated.is_empty() && gated != recorded).then(|| {
        let mut patch = serde_json::Map::new();
        patch.insert("sha".into(), json!(gated));
        if !recorded.is_empty() {
            patch.insert("requested_head".into(), json!(recorded));
        }
        Value::Object(patch)
    })
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
            // KEY ON THE TRUTHFUL HEAD. A packet with a receipt is
            // about the tree the RUNNER gated, so the candidate is
            // compared against the receipt's head — the requested sha
            // stops being a key the moment a receipt exists, because
            // the two differ exactly when the branch moved between this
            // verb's resolve and the runner's clone (410bf724). A
            // still-running gate has no receipt yet, so the requested
            // sha remains the only available key and keeps the job it
            // has always done.
            m("branch") == branch
                && match receipt_head(j) {
                    Some(gated) => gated == sha,
                    None => m("sha") == sha,
                }
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
/// RFC3339, whole seconds, `Z`. Several verbs stamp `completed_at` — the
/// conductor on the steps it completes, `boss park` on scope/build/gate,
/// `boss prove` on proven, and now the dispatcher's auto-park handler —
/// and cycle time is the difference between stamps written by
/// *different* verbs. A format that varied by writer would still look
/// right in every packet and only be wrong in the arithmetic, so it is
/// defined ONCE, in `boss_jobs::car`, and everyone delegates here.
pub(crate) fn stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    boss_jobs::car::stamp(now)
}

/// `git ls-remote` the branch so the packet records a real head.
///
/// Falls back to the symbolic `origin/<branch>` rather than failing:
/// the runner resolves the branch itself, and a missing sha degrades
/// the packet's record without stopping the gate. It is warned about,
/// because a receipt is worth much less when nobody can say which tree
/// it vouched for.
pub(crate) fn resolve_sha(branch: &str) -> String {
    let out = crate::git_auth::command()
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

/// The gate Jobs matching a label selector, as a NAME/SUCCEEDED/FAILED
/// table — one kubectl shape shared by the per-packet attach check and
/// the concurrency count, so the two cannot drift in how they read a
/// Job's liveness.
///
/// FAILS CLOSED (bails on any kubectl failure), and the callers keep
/// it that way on purpose. The old pods-based count once degraded an
/// error to zero, and zero was exactly the value that satisfied the
/// guard it fed — an unreadable cluster read as "healthy and idle".
/// The stake today is smaller (over-admission wastes node-minutes, not
/// verdicts — workspaces are per-run) but kubectl is needed to CREATE
/// the Job anyway, so a cluster too sick to answer this was never
/// going to run the gate either.
fn gate_jobs_table(namespace: &str, selector: &str) -> Result<String> {
    let out = kubectl(namespace)
        .args([
            "get",
            "jobs",
            "-l",
            selector,
            "--no-headers",
            "-o",
            "custom-columns=NAME:.metadata.name,S:.status.succeeded,F:.status.failed",
        ])
        .output()
        .context("kubectl get jobs — is KUBECONFIG set and the cluster reachable?")?;
    if !out.status.success() {
        bail!(
            "kubectl get jobs -l {selector} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every gate Job still live, by name. Jobs, not pods, deliberately:
/// a just-launched gate's pod sits Pending (scheduling, image pull,
/// volume attach) where a `status.phase=Running` field selector cannot
/// see it — two quick `boss gate` calls would each count zero and
/// together over-fill the node. A Job with neither `succeeded` nor
/// `failed` set is live from the moment `kubectl create` returns.
fn running_gates(namespace: &str) -> Result<Vec<String>> {
    Ok(live_gates(&gate_jobs_table(namespace, "app=gate-runner")?))
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    branch: &str,
    mode: Option<String>,
    manifest: Option<PathBuf>,
    namespace: &str,
    wait: bool,
    dry: bool,
    park: ParkIntent,
) -> Result<()> {
    let manifest_path =
        manifest.unwrap_or_else(|| PathBuf::from("infra/gate-runner/gate-runner.yaml"));
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading runner manifest {}", manifest_path.display()))?;

    // BEFORE the sha lookup, the packet, the manifest and kubectl —
    // a bad mode, a half-filled park intent, or a typo'd concurrency
    // bound should cost a line of output, not a gate slot.
    let mode = normalize_mode(&mode.unwrap_or_default())?;
    park.require_complete()?;
    let http = reqwest::Client::new();
    // The concurrency bound: env override > delivery policy > compiled.
    // Fetched here, before the packet, so a bad env override refuses
    // without side effects — the policy read never fails, it falls back.
    let max = max_concurrent(&http).await?;
    let sha = resolve_sha(branch);

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
    let mut reused = false;
    let packet = match reusable_packet(&open, branch, &sha) {
        Some(id) => {
            println!(
                "boss gate: reusing open gate-run packet {}",
                &id[..8.min(id.len())]
            );
            reused = true;
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

    // Stamp the park intent onto the gate-run so the auto-park handler
    // can file the car verbatim on green. A PATCH so it works whether the
    // packet was just created or reused, and merges rather than replaces.
    if !park.is_empty() && !dry {
        api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(park.metadata_patch()),
        )
        .await
        .context("stamping park intent onto the gate-run")?;
        println!("boss gate: park intent stamped — this branch auto-parks on green");
    }

    let job = render_job(&manifest_text, branch, &packet, &mode)?;

    // A REUSED PACKET MAY ALREADY BE GATING. This verb's own closing
    // line used to send the operator straight into the failure: run
    // `boss gate <branch>`, then `boss gate <branch> --wait` as
    // instructed, and the second invocation reused the open packet and
    // created a SECOND Job against it. Both raced on one gate-target,
    // one died, and the survivor's green verdict was recorded as `lost`.
    // Attaching is what the advice always meant, so do that instead.
    if reused
        && !dry
        && let Some(name) = live_gate_for_packet(namespace, &packet)?
    {
        println!("boss gate: packet {packet} is already being gated by {name}");
        if wait {
            println!("boss gate: attaching to it — a second Job would race it");
            return wait_for_verdict(&http, &packet, namespace, &name).await;
        }
        println!(
            "boss gate: not starting a second Job. Follow this one with \
             `boss gate {branch} --wait`, which attaches."
        );
        return Ok(());
    }

    // The concurrency bound. Workspaces are per-run (pinned by
    // boss-testing's gate_runner_parallel_workspace tests), so a
    // second gate is SAFE — the bound protects the build node's disk
    // and I/O, not any verdict. A refusal from here on happens AFTER
    // the packet was filed — close what we just opened so the refusal
    // leaves no orphan (ed7f1355), then surface it.
    let live = match running_gates(namespace) {
        Ok(l) => l,
        Err(e) => {
            let e = e.context(
                "cannot count running gates, so the concurrency bound cannot be \
                 enforced — refusing rather than assuming the build node is free",
            );
            if !reused && !dry {
                close_refused(&http, &packet, &format!("{e:#}")).await;
            }
            return Err(e);
        }
    };
    // LEGACY-MANIFEST GUARD. If the rendered Job's /gate-target is
    // still a PVC (a stale checkout, or --manifest at the pre-parallel
    // runner), the old law holds absolutely: one gate per shared
    // workspace, because two on one disk cross their receipts
    // (2026-08-24). The bounded rule below only applies to per-run
    // workspaces.
    if pvc_backed_workspace(&job) && !live.is_empty() {
        let why = format!(
            "{n} gate(s) already running ({names}) and {manifest} mounts a SHARED \
             workspace at /gate-target — the pre-parallel runner shape.\n  Two gates \
             on one disk cross their receipts (2026-08-24: a receipt naming one \
             branch's head reported under another; all three results discarded).\n  \
             Wait for the running gate, or update the checkout so the manifest's \
             workspace is a per-run emptyDir seeded from /gate-seed.",
            n = live.len(),
            names = live.join(", "),
            manifest = manifest_path.display(),
        );
        if !reused && !dry {
            close_refused(&http, &packet, &why).await;
        }
        bail!("{why}");
    }
    if let Some(why) = crowd_refusal(&live, max) {
        if !reused && !dry {
            close_refused(&http, &packet, &why).await;
        }
        bail!("{why}");
    }
    if !live.is_empty() {
        println!(
            "boss gate: {} gate(s) already running ({}) — workspaces are per-run, \
             verdicts stay independent; launching alongside",
            live.len(),
            live.join(", ")
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
        println!(
            "boss gate: not waiting — `boss gate {branch} --wait` ATTACHES to this Job, \
             or read the packet."
        );
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
             passed. The workspace was per-run and died with the pod, so the surviving copy \
             of the receipt is the pod log — `kubectl logs job/<job>` (the `gate-runner: \
             receipt` line), kept for a day after the Job ends. Read it rather than \
             re-running 40 minutes of gate on the assumption this was a failure."
                .to_string(),
        );
    }
    Some(
        "the gate Job finished but the packet never reported a verdict.\n           The run completed and echoed its receipt to stdout before reporting, so \
         `kubectl logs job/<job>` (the `gate-runner: receipt` line) holds the answer; \
         the reporting call is what went missing."
            .to_string(),
    )
}

/// Poll the PACKET, not the pod.
///
/// The runner self-reports its verdict onto the gate-run packet, and
/// the packet is the record that outlives the pod — a gate whose
/// container exited 0 can leave its pod `1/2 NotReady` for hours
/// because a sidecar never exits, so pod phase is the wrong thing to
/// How long the system of record may be absent before the absence is
/// the answer. A deploy rolls it for tens of seconds; three minutes is
/// far past that and still far short of a gate's runtime.
const ABSENCE_TOLERANCE: std::time::Duration = std::time::Duration::from_secs(180);

/// Is this the shape of error a restart produces, or a real one?
///
/// Deliberately takes the rendered message rather than the error, so
/// the rule is a pure string decision a test can state outright — the
/// alternative is matching on reqwest internals, which is both harder
/// to read and harder to pin.
pub(crate) fn is_transient(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("connection refused")
        || m.contains("tcp connect error")
        || m.contains("error sending request")
        || m.contains("connection reset")
        || m.contains("broken pipe")
        || m.contains("timed out")
        || m.contains("dns error")
}

/// watch. Reading the packet is also what any other actor would do.
async fn wait_for_verdict(
    http: &reqwest::Client,
    packet: &str,
    namespace: &str,
    job_name: &str,
) -> Result<()> {
    // A MOMENTARY ABSENCE IS NOT A FAILURE.
    //
    // Every train deploy rolls the boss Deployment, and the jobs API
    // goes with it — so the system of record disappears for tens of
    // seconds on a schedule this verb cannot see. This loop used to
    // treat that as fatal: `Connection refused` ended the wait and
    // returned non-zero, which to a caller is indistinguishable from a
    // red gate. Nothing was actually lost — the gate JOB is unaffected
    // and keeps running — but an agent reading that as a failure
    // re-gates, spending another ~11 minutes of cluster time on a run
    // that was already healthy. Same reasoning as car 28662b18 one
    // level up: a dropped lookup does not red a train (de5f22b6).
    //
    // Observed on 2026-08-30: the SoR dropped mid-gate, this poller
    // died, and the gate it was watching went green on its own.
    let mut absent_since: Option<std::time::Instant> = None;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        let fetched = match api(
            http,
            reqwest::Method::GET,
            &format!("/api/jobs/{packet}"),
            None,
        )
        .await
        {
            Ok(v) => {
                absent_since = None;
                v
            }
            Err(e) if is_transient(&format!("{e:#}")) => {
                let since = *absent_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() > ABSENCE_TOLERANCE {
                    bail!(
                        "the system of record has been unreachable for over {}s, which is \
                         longer than a deploy takes — giving up on the WAIT, not on the \
                         gate.\n  The Job {job_name} may still be running; read the packet \
                         {packet} when the API is back.\n  Last error: {e:#}",
                        ABSENCE_TOLERANCE.as_secs()
                    );
                }
                eprintln!(
                    "boss gate: system of record unreachable ({}s) — a deploy rolls it \
                     briefly; the gate Job is unaffected, still waiting",
                    since.elapsed().as_secs()
                );
                continue;
            }
            Err(e) => return Err(e),
        };
        let Some(job) = fetched else {
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
            record_gated_head(http, packet, &job).await;
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

/// Once the runner has reported, make the packet tell the runner's
/// truth (410bf724).
///
/// The packet's `sha` was resolved by this verb BEFORE the runner
/// cloned; the receipt's head is what the runner actually gated. When
/// the branch moved in between, the packet lies about which tree its
/// verdict covers — and [`reusable_packet`] used to key the next
/// relaunch on that lie. The correction goes through `PATCH
/// /api/jobs/{id}/metadata`, which MERGES top-level keys, so `sha`
/// takes the receipt's head and the request survives as
/// `requested_head`.
///
/// BEST-EFFORT, LOUDLY. The verdict is already known by the time this
/// runs, and failing the wait over an annotation would report a green
/// gate as red — the exact confusion cf0021ae exists about. But a
/// silent failure leaves the lie in place, so it warns with what the
/// packet still wrongly records.
async fn record_gated_head(http: &reqwest::Client, packet: &str, job: &Value) {
    let recorded = job
        .pointer("/metadata/sha")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(patch) = truth_patch(recorded, receipt_head(job).as_deref()) else {
        return;
    };
    let gated = patch["sha"].as_str().unwrap_or_default().to_string();
    match api(
        http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{packet}/metadata"),
        Some(patch),
    )
    .await
    {
        Ok(_) => println!(
            "boss gate: the branch moved between resolve and clone — packet {packet} \
             now records the head the runner gated ({gated}); the requested \
             {recorded} is kept as requested_head"
        ),
        Err(e) => eprintln!(
            "boss gate: could not correct packet {packet}'s head to the receipt's \
             {gated} — it still records {recorded}, which the runner did NOT gate. \
             The receipt on the record-verdict step is the truthful record.\n  {e:#}"
        ),
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

/// Which of these Jobs are still running?
///
/// Parses `kubectl get jobs -o custom-columns=NAME,SUCCEEDED,FAILED`
/// rows. A Job is LIVE when it has neither succeeded nor failed —
/// kubectl prints `<none>` for both while it runs, and `<none>` parses
/// to zero, which is the honest reading here: nothing has completed.
pub(crate) fn live_gates(rows: &str) -> Vec<String> {
    rows.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let name = f.next()?;
            let count = |s: Option<&str>| s.unwrap_or("0").parse::<i32>().unwrap_or(0);
            let succeeded = count(f.next());
            let failed = count(f.next());
            (succeeded == 0 && failed == 0).then(|| name.to_string())
        })
        .collect()
}

/// Is a Job already gating this packet?
///
/// FAILS CLOSED (via [`gate_jobs_table`]): the dangerous act is
/// CREATING a second Job against the same packet — two Jobs racing to
/// report one verdict — so an unreadable cluster must not read as
/// "nothing is running".
fn live_gate_for_packet(namespace: &str, packet: &str) -> Result<Option<String>> {
    let table =
        gate_jobs_table(namespace, &format!("boss.dev/packet={packet}")).with_context(|| {
            format!(
                "cannot tell whether packet {} is already being gated",
                &packet[..8.min(packet.len())]
            )
        })?;
    Ok(live_gates(&table).into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn park_full() -> ParkIntent {
        ParkIntent {
            summary: Some("does a thing. and more.".into()),
            excludes: Some("not that".into()),
            test: Some("ran the suite".into()),
            verified: Some("observed working".into()),
            backlog_item: None,
        }
    }

    #[test]
    fn a_plain_gate_carries_no_park_intent() {
        let p = ParkIntent::default();
        assert!(p.is_empty());
        assert!(p.require_complete().is_ok());
        assert_eq!(p.metadata_patch(), serde_json::json!({}));
    }

    #[test]
    fn a_complete_intent_stamps_only_park_keys() {
        let p = park_full();
        assert!(!p.is_empty());
        assert!(p.require_complete().is_ok());
        assert_eq!(
            p.metadata_patch(),
            serde_json::json!({
                "park_summary": "does a thing. and more.",
                "park_excludes": "not that",
                "park_test": "ran the suite",
                "park_verified": "observed working",
            })
        );
    }

    #[test]
    fn a_partial_intent_is_refused_naming_the_missing_flags() {
        // Opting into auto-park with only a summary would file a car with
        // an empty boundary and an unproven `verified` line.
        let p = ParkIntent {
            summary: Some("does a thing".into()),
            ..Default::default()
        };
        let err = p.require_complete().unwrap_err().to_string();
        // Check the MISSING clause (before "not set"); the guidance after
        // it names all four flags on purpose.
        let missing = err.split("not set").next().unwrap_or("");
        assert!(missing.contains("--park-excludes"), "{err}");
        assert!(missing.contains("--park-test"), "{err}");
        assert!(missing.contains("--park-verified"), "{err}");
        assert!(!missing.contains("--park-summary"), "{err}");
    }

    #[test]
    fn a_backlog_item_rides_along_when_the_four_are_present() {
        let mut p = park_full();
        p.backlog_item = Some("7c9e376d".into());
        assert!(p.require_complete().is_ok());
        assert_eq!(p.metadata_patch()["park_backlog_item"], "7c9e376d");
    }

    /// THE RACE THIS CLOSES. `boss gate` printed "`boss gate --wait`
    /// follows it", and following that advice created a SECOND Job
    /// against the same reused packet. Two Jobs then raced to report
    /// one verdict: one died at 70s, the other went green, and the
    /// `--wait` guard recorded the packet as `lost` while the gate that
    /// actually ran was passing (5703c784).
    #[test]
    fn a_running_job_is_found_so_a_second_is_never_created() {
        let running = "gate-2cn2l   <none>   <none>";
        assert_eq!(live_gates(running), vec!["gate-2cn2l".to_string()]);
    }

    #[test]
    fn a_finished_job_is_not_live() {
        assert!(live_gates("gate-abc12   1   <none>").is_empty());
        assert!(live_gates("gate-abc12   <none>   1").is_empty());
        assert!(live_gates("").is_empty());
    }

    /// A packet that was gated before and is being gated again has both
    /// a finished Job and a live one. The live one is the answer.
    #[test]
    fn a_finished_job_does_not_hide_a_live_one() {
        let rows = "gate-old11   1   <none>\ngate-new22   <none>   <none>";
        assert_eq!(live_gates(rows), vec!["gate-new22".to_string()]);
    }

    /// Concurrent gates are ALL reported, in order — the crowd refusal
    /// names them, and a bound that miscounts admits past the node.
    #[test]
    fn every_live_gate_is_counted_not_just_the_first() {
        let rows = "gate-feat-x-ab1   <none>   <none>\n\
                    gate-done-cd2     1        <none>\n\
                    gate-fix-y-ef3    <none>   <none>";
        assert_eq!(
            live_gates(rows),
            vec!["gate-feat-x-ab1".to_string(), "gate-fix-y-ef3".to_string()]
        );
    }

    /// THE BOUND, below and at. Below: silence (None), because gates in
    /// parallel is now the designed state, not an anomaly to warn about.
    /// At: a refusal that NAMES the running gates — the operator's next
    /// verb targets one of them.
    #[test]
    fn the_crowd_refusal_fires_at_the_bound_and_names_the_gates() {
        let live: Vec<String> = vec!["gate-feat-x-ab1".into(), "gate-fix-y-ef3".into()];
        assert_eq!(crowd_refusal(&live, 3), None, "below the bound is silence");

        let msg = crowd_refusal(&live, 2).expect("at the bound refuses");
        assert!(msg.contains("gate-feat-x-ab1"), "{msg}");
        assert!(msg.contains("gate-fix-y-ef3"), "{msg}");
        assert!(
            msg.contains("BOSS_GATE_MAX_CONCURRENT"),
            "the refusal must name the override, or the bound reads as a wall: {msg}"
        );
        assert!(
            crowd_refusal(&live, 1).is_some(),
            "past the bound refuses too (gates launched before a lower bound was set)"
        );
    }

    #[test]
    fn an_idle_cluster_admits_even_at_bound_one() {
        assert_eq!(crowd_refusal(&[], 1), None);
    }

    /// The env override: absent means the FALLBACK (the delivery
    /// policy's bound, resolved by the caller), a count means that count,
    /// and GARBAGE REFUSES rather than silently meaning the fallback — a
    /// typo that becomes some other number is the aa783636 defect shape
    /// (right sometimes, silently wrong when it matters).
    #[test]
    fn the_concurrency_bound_parses_or_refuses() {
        // Absent / blank override → the fallback the caller passed, which
        // is the policy value in production and the compiled default when
        // the registry was unreadable. Two fallbacks prove it is the
        // argument, not a baked-in 3.
        assert_eq!(max_concurrent_from(None, 4).unwrap(), 4);
        assert_eq!(
            max_concurrent_from(None, DEFAULT_MAX_CONCURRENT).unwrap(),
            DEFAULT_MAX_CONCURRENT
        );
        assert_eq!(max_concurrent_from(Some(""), 4).unwrap(), 4);
        assert_eq!(max_concurrent_from(Some("  "), 7).unwrap(), 7);

        // A set override WINS over the fallback — the operator's escape
        // hatch when the node grew or shrank between policy edits.
        assert_eq!(max_concurrent_from(Some("5"), 3).unwrap(), 5);
        assert_eq!(max_concurrent_from(Some(" 1 "), 9).unwrap(), 1);

        for bad in ["three", "-1", "2.5"] {
            let err = max_concurrent_from(Some(bad), 3).expect_err("garbage must refuse");
            assert!(
                err.to_string().contains("BOSS_GATE_MAX_CONCURRENT"),
                "the refusal must name the variable: {err}"
            );
        }
        // Zero would refuse every gate forever — a misconfiguration,
        // not a policy. The message teaches `1` for serialize.
        let err = max_concurrent_from(Some("0"), 3).expect_err("zero must refuse");
        assert!(err.to_string().contains("Set 1 to serialize"), "{err}");
    }

    /// The name hint: branch characters a Job name/label can carry,
    /// bounded, never edge-dashed, never empty.
    #[test]
    fn the_name_hint_is_label_safe_and_recognizable() {
        assert_eq!(name_hint("fix/a-thing"), "fix-a-thing");
        assert_eq!(
            name_hint("feat/gates-run-in-parallel"),
            "feat-gates-run-in-pa"
        );
        // A truncation that lands on a dash must trim it — a label
        // value may not end on '-'. Sanitized this is
        // `abcde-abcde-abcde-a-x` (21); cut at 20 it ends on the dash.
        assert_eq!(name_hint("abcde/abcde/abcde/a/x"), "abcde-abcde-abcde-a");
        // Case folds, symbol runs collapse to one dash, edges stay
        // alphanumeric.
        assert_eq!(name_hint("Fix//Weird__Branch"), "fix-weird-branch");
        assert_eq!(
            name_hint("///"),
            "branch",
            "no usable characters still renders"
        );
        for hint in [name_hint("feat/x"), name_hint("///"), name_hint("A--B")] {
            assert!(hint.len() <= 20);
            assert!(
                hint.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            );
            assert!(!hint.starts_with('-') && !hint.ends_with('-'), "{hint}");
        }
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
---\napiVersion: batch/v1\nkind: Job\nmetadata:\n  generateName: gate-$GATE_NAME_HINT-\n\
  labels: {boss.dev/branch: $GATE_NAME_HINT}\nspec:\n  template:\n\
    spec:\n      containers:\n        - name: gate\n          env:\n\
            - {name: GATE_BRANCH, value: $GATE_BRANCH}\n\
            - {name: GATE_RUN_JOB_ID, value: $GATE_RUN_JOB_ID}\n\
            - {name: GATE_MODE, value: $GATE_MODE}\n\
      volumes:\n        - name: gate-workspace\n          emptyDir: {}\n";

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
        // The name hint is DERIVED from the branch, never passed in —
        // concurrent Jobs must be tellable apart in `kubectl get jobs`.
        assert!(
            job.contains("generateName: gate-fix-a-thing-"),
            "the Job name must carry the sanitized branch: {job}"
        );
        assert!(
            job.contains("boss.dev/branch: fix-a-thing"),
            "the branch label must carry the same hint: {job}"
        );
        assert!(!job.contains("$GATE_NAME_HINT"), "no placeholder survives");
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

    /// THE LEGACY-MANIFEST DISCRIMINATOR. A PVC in the Job stopped
    /// meaning "shared workspace" when the seed shipped — every
    /// rendered Job now carries the seed claim. What still means it is
    /// a claim-backed volume MOUNTED at /gate-target, which is exactly
    /// the pre-parallel manifest a stale checkout renders. Miss it and
    /// the new bounded rule admits three gates onto one disk — the
    /// 2026-08-24 crossed receipts, reintroduced through skew.
    #[test]
    fn the_pre_parallel_workspace_shape_is_still_recognized() {
        // The old shipped shape: inline mount, PVC-backed workspace.
        let legacy = "\
kind: Job\n\
          volumeMounts:\n\
            - {name: gate-runner-disk, mountPath: /gate-target}\n\
      volumes:\n\
        - name: gate-runner-disk\n\
          persistentVolumeClaim: {claimName: gate-runner-disk}\n";
        assert!(pvc_backed_workspace(legacy));

        // The parallel shape: emptyDir workspace, the PVC only a seed.
        let parallel = "\
kind: Job\n\
          volumeMounts:\n\
            - {name: gate-workspace, mountPath: /gate-target}\n\
            - {name: gate-seed, mountPath: /gate-seed}\n\
      volumes:\n\
        - name: gate-workspace\n\
          emptyDir: {sizeLimit: 100Gi}\n\
        - name: gate-seed\n\
          persistentVolumeClaim: {claimName: gate-runner-disk}\n";
        assert!(
            !pvc_backed_workspace(parallel),
            "the seed claim must not read as a shared workspace — that heuristic \
             would re-serialize every gate"
        );
    }

    /// Both yaml spellings, because a parser proven against one style
    /// answers false against the other and the guard silently never
    /// engages (da260655's failure shape).
    #[test]
    fn the_workspace_discriminator_reads_block_style_mounts_too() {
        let block = "\
kind: Job\n\
          volumeMounts:\n\
            - name: gate-runner-disk\n\
              mountPath: /gate-target\n\
      volumes:\n\
        - name: gate-runner-disk\n\
          persistentVolumeClaim:\n\
            claimName: gate-runner-disk\n";
        assert!(pvc_backed_workspace(block));
        assert!(
            !pvc_backed_workspace("kind: Job\nvolumes:\n  - name: x\n    emptyDir: {}\n"),
            "no /gate-target mount at all is not a shared workspace"
        );
    }

    /// The SHIPPED manifest renders as parallel-safe — the guard must
    /// not re-serialize production (checked against the real file, so
    /// a manifest edit that regresses the shape fails here by name).
    #[test]
    fn the_shipped_manifest_is_not_the_legacy_shape() {
        let manifest = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../infra/gate-runner/gate-runner.yaml"),
        )
        .expect("shipped runner manifest readable");
        let job = render_job(&manifest, "feat/x", "pkt", "").expect("renders");
        assert!(
            !pvc_backed_workspace(&job),
            "the shipped manifest's /gate-target must stay a per-run emptyDir"
        );
    }

    /// A packet whose gate resolved X, whose runner then cloned and
    /// gated Y because the branch moved in between (410bf724). The
    /// runner's receipt on the record-verdict step is the truthful
    /// record; the packet's `sha` is only what this verb asked for.
    fn moved_packet() -> Vec<Value> {
        vec![json!({
            "id": "bbbbbbbb-2222",
            "metadata": {"branch": "fix/x", "sha": "deadbeef"},
            "steps": [{
                "spec_slug": "record-verdict",
                "metadata": {
                    "verdict": "green",
                    "receipt": "{\"verdict\":\"green\",\"head\":\"cafebabe\",\"mode\":\"full\",\"fails\":[]}"
                }
            }]
        })]
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

        // THE BRANCH MOVED BETWEEN RESOLVE AND CLONE (410bf724). The
        // packet requested deadbeef but its receipt gated cafebabe, and
        // a candidate at cafebabe IS the tree that receipt vouches for
        // — reuse it, or every relaunch files an orphan against a
        // perfectly good verdict.
        assert_eq!(
            reusable_packet(&moved_packet(), "fix/x", "cafebabe").as_deref(),
            Some("bbbbbbbb-2222")
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

        // Once a receipt exists, the REQUESTED sha stops being a key at
        // all. The packet asked for deadbeef but the runner gated
        // cafebabe — so a candidate at deadbeef must NOT reuse it (that
        // tree was never gated), and neither may an unrelated head.
        assert_eq!(reusable_packet(&moved_packet(), "fix/x", "deadbeef"), None);
        assert_eq!(reusable_packet(&moved_packet(), "fix/x", "0123abcd"), None);
    }

    #[test]
    fn a_packet_without_metadata_does_not_panic() {
        let open = vec![json!({"id": "no-metadata"})];
        assert_eq!(reusable_packet(&open, "fix/x", "deadbeef"), None);
    }

    /// The receipt is a JSON string on the record-verdict step — the
    /// encoding run.sh writes and `boss receipt` reads.
    #[test]
    fn the_gated_head_is_read_off_the_receipt() {
        assert_eq!(
            receipt_head(&moved_packet()[0]).as_deref(),
            Some("cafebabe")
        );
    }

    /// A runner that died before a receipt reports PROSE in the receipt
    /// field ("runner died before a receipt: line 103"). That must read
    /// as "no gated head", not as a parse panic or a bogus key — the
    /// requested sha stays the reuse key for such a packet.
    #[test]
    fn a_lost_run_and_a_bare_packet_have_no_gated_head() {
        let lost = json!({
            "id": "cccccccc-3333",
            "metadata": {"branch": "fix/x", "sha": "deadbeef"},
            "steps": [{
                "spec_slug": "record-verdict",
                "metadata": {"verdict": "lost",
                             "receipt": "runner died before a receipt: line 103"}
            }]
        });
        assert_eq!(receipt_head(&lost), None);
        // …so the requested sha still keys reuse for it.
        assert_eq!(
            reusable_packet(&[lost], "fix/x", "deadbeef").as_deref(),
            Some("cccccccc-3333")
        );
        // No steps at all: a gate still running, or a list endpoint
        // that omitted them — either way, no receipt, no gated head.
        assert_eq!(receipt_head(&json!({"id": "x"})), None);
    }

    /// THE CORRECTION THE PACKET IS OWED (410bf724): requested X, gated
    /// Y — `sha` takes the truth, the request survives as provenance.
    #[test]
    fn a_moved_head_produces_a_truth_patch() {
        let p = truth_patch("deadbeef", Some("cafebabe")).expect("the packet lies; correct it");
        assert_eq!(p["sha"], "cafebabe");
        assert_eq!(p["requested_head"], "deadbeef");
    }

    /// The ls-remote fallback records the SYMBOLIC ref instead of a
    /// head. The receipt upgrades that degraded record to a real sha.
    #[test]
    fn a_symbolic_fallback_is_upgraded_to_the_gated_head() {
        let p = truth_patch("origin/fix/x", Some("cafebabe")).expect("upgrade the symbolic ref");
        assert_eq!(p["sha"], "cafebabe");
        assert_eq!(p["requested_head"], "origin/fix/x");
    }

    /// A truthful packet needs no correction, and a silent runner has
    /// no truth to correct WITH — neither may produce a PATCH, or every
    /// wait would write a no-op annotation onto every packet.
    #[test]
    fn a_truthful_packet_gets_no_patch() {
        assert_eq!(truth_patch("deadbeef", Some("deadbeef")), None);
        assert_eq!(truth_patch("deadbeef", None), None);
        assert_eq!(truth_patch("deadbeef", Some("")), None);
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
    ///
    /// Where the answer lives CHANGED with per-run workspaces: the
    /// receipt file dies with the pod's emptyDir, so pointing the
    /// operator at /gate-target/receipt.json would point at a disk that
    /// no longer exists. The surviving copy is the pod log — run.sh
    /// echoes the receipt to stdout before reporting, for exactly this
    /// moment (pinned by run_sh_verdict.rs).
    #[test]
    fn a_dead_job_with_a_silent_packet_stops_and_refuses_to_call_it_red() {
        let msg = silent_packet_verdict(true, true).expect("a dead Job must end the wait");
        assert!(msg.contains("NOT the same as a red gate"), "{msg}");
        assert!(
            msg.contains("kubectl logs"),
            "it must say where the answer actually lives — the pod log, not a \
             workspace that died with the pod: {msg}"
        );
        assert!(
            !msg.contains("receipt.json"),
            "the receipt FILE is per-run now and gone with the pod; naming it \
             sends the operator to mount a disk that does not exist: {msg}"
        );
    }

    /// A Job that finished cleanly but never reported is a different
    /// story — the run completed, so the pod log holds the answer.
    #[test]
    fn a_finished_job_with_a_silent_packet_points_at_the_pod_log() {
        let msg = silent_packet_verdict(true, false).expect("a finished Job must end the wait");
        assert!(msg.contains("kubectl logs"), "{msg}");
        assert!(
            !msg.contains("NOT the same as a red gate"),
            "that caveat belongs to the failed case only: {msg}"
        );
    }

    /// THE ERROR THAT KILLED A HEALTHY WAIT. The system of record went
    /// down mid-gate on 2026-08-30 while the boss Deployment rolled;
    /// the poller died with this, and the gate it was watching went
    /// green on its own (de5f22b6).
    #[test]
    fn a_restart_looks_transient() {
        for msg in [
            "jobs api GET /api/jobs/156bf036: error sending request for url              (http://10.20.0.34:7900/api/jobs/156bf036): client error (Connect):              tcp connect error: Connection refused (os error 111)",
            "operation timed out",
            "connection reset by peer",
            "dns error: failed to lookup address",
        ] {
            assert!(is_transient(msg), "should ride this out: {msg}");
        }
    }

    /// ...and a real refusal is NOT transient, or the wait would hang
    /// for three minutes on something that will never resolve. This is
    /// the half that stops the tolerance becoming a blindfold.
    #[test]
    fn a_real_answer_is_not_transient() {
        for msg in [
            "jobs api GET /api/jobs/x: 403 forbidden: job is outside your scope",
            "jobs api GET /api/jobs/x: 404 job not found",
            "invalid job id",
            "the gate receipt names no head",
        ] {
            assert!(!is_transient(msg), "should fail fast: {msg}");
        }
    }

    /// The tolerance is far past a deploy and far short of a gate, so
    /// riding out a restart can never be mistaken for waiting out a
    /// real outage.
    #[test]
    fn the_absence_tolerance_sits_between_a_deploy_and_a_gate() {
        assert!(
            ABSENCE_TOLERANCE.as_secs() >= 120,
            "a deploy takes tens of seconds"
        );
        assert!(
            ABSENCE_TOLERANCE.as_secs() <= 600,
            "a gate takes ~11 minutes"
        );
    }
}
