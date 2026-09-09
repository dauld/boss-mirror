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
//! gate Jobs and holds at [`DEFAULT_MAX_CONCURRENT`] (override:
//! BOSS_GATE_MAX_CONCURRENT). The count is best-effort against a race
//! (two verbs counting at once can both see N-1), which is acceptable
//! now that over-admission costs minutes, not verdicts; the scheduler's
//! ephemeral-storage accounting is the hard backstop on the disk.
//!
//! ON THE BOUND: IT QUEUES, AND A REFUSAL FILES NOTHING (fd217c65).
//! Measured 2026-09-08: 24 green gates, 6 red — and 21 launches refused
//! at the bound. Each refusal had ALREADY filed its gate-run packet, so
//! it closed the packet with the only terminal the protocol offers a run
//! that produced no verdict: `lost`, whose label is "Gate lost
//! (environment died)". Nothing died; the node was busy. Twenty-one
//! phantom runs polluted the day's history and the yard's counts, and
//! five builders each hand-rolled a five-minute retry loop — two
//! collided on a shared script, and one outlived the session that made
//! it and re-gated an already-green branch into a twin car.
//!
//! Both halves are fixed here, and they are the same fix seen twice: a
//! station holds, it does not drop. [`admission`] decides Launch / Queue
//! / Refuse from observations taken BEFORE any packet exists, so a
//! refusal files nothing; and at the bound a `--wait` caller takes a
//! place in line ([`QUEUED_AT`]) that this process holds by heartbeat
//! until a slot frees, oldest first. The launcher is this verb, not the
//! conductor and not a dispatcher rule: creating a gate Job needs the
//! Kubernetes credential and the runner manifest, and the conductor
//! holds neither by design (`infra/cluster/manifests/boss-conductor.yaml`
//! — "RBAC: NONE"). "A slot is free" is also a CLUSTER fact that no BOSS
//! event announces — a gate Job can die without reporting — so an
//! event-driven launcher would stall the line permanently on exactly the
//! failure the queue exists to survive, where a poll re-derives it every
//! time. And a place held by a live process cannot outlive its
//! session, which is the second defect above, structurally.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::identity;

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

/// How many gate-runs may hold a place in line at once.
///
/// Four waves at the measured median: the last place in a full line is
/// about 90 minutes out. Past that the queue would be promising a slot
/// the node will not reach before the builder's session ends, and a
/// refusal that says "come back" is kinder than a place that quietly
/// expires. A full queue REFUSES and files nothing, the same discipline
/// as every other refusal here.
const QUEUE_CAP: usize = 12;

/// How long a place in line survives without a heartbeat, in seconds.
///
/// THE POINT OF THE WHOLE MECHANISM. A place is held by a LIVE process:
/// the waiting `boss gate` refreshes it every poll. On 2026-09-08 a
/// builder's session ended and its DETACHED retry loop kept running,
/// re-gating an already-green branch into a twin car. A place that stops
/// being refreshed is skipped, so the line behind a dead holder moves
/// instead of waiting on a ghost. Ten poll intervals of slack, which
/// comfortably absorbs an SoR roll.
const QUEUE_PLACE_TTL_SECS: i64 = 300;

/// How often a waiting gate re-reads the line and the cluster.
const QUEUE_POLL: std::time::Duration = std::time::Duration::from_secs(30);

/// The median gate, in whole minutes — measured 2026-09-08 across the
/// day's 30 completed runs (median 18.2, p90 24.5, max 27.1). Used for
/// exactly one thing: telling a waiting builder what a place costs, so
/// the choice to wait or come back is made on a number.
const MEDIAN_GATE_MINUTES: u64 = 18;

/// The instant a place in line was taken — the queue's ordering key.
///
/// Defined in `boss_jobs::yard` and re-exported here because BOTH sides
/// need it and a fact that lives twice drifts (§9a): this verb writes it
/// and the yard reads it to keep a waiting run out of the gate bays.
pub(crate) use boss_jobs::yard::QUEUED_AT;

/// The heartbeat the holder of a place refreshes while it waits. Written
/// and read only here — the yard cares whether a run is queued, never
/// how recently its holder said so.
pub(crate) const QUEUE_HEARTBEAT_AT: &str = "queue_heartbeat_at";

/// An RFC3339 stamp as an instant. `None` for anything that does not
/// parse: a stamp that cannot be read is no stamp, never a guess into
/// the head of the line.
fn parse_instant(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// What the pre-flight observations say happens next.
///
/// EVERY REFUSAL IS A VALUE COMPUTED HERE, from observations taken
/// before any packet is filed — the whole of part 1 of fd217c65. The
/// alternative fix, a `refused` terminal on the gate-run, needs a new
/// workflow version: the protocol's `verdict` field is the closed enum
/// `green|failed|lost` and each terminal is an `outcome` step keyed on
/// it. That is a registry change to record a state that should not be
/// recorded at all — a refusal is not an outcome of a run, it is the
/// absence of one. So: no packet until there is a Job to attach it to,
/// or a place in line for a live process to hold.
#[derive(Debug)]
pub(crate) enum Admission {
    /// A slot is free: file the packet and create the Job.
    Launch,
    /// At the bound, with a live process to hold the place: file the
    /// packet, mark it queued, launch when the line reaches it.
    Queue,
    /// NOTHING IS FILED. The reason is the operator's whole answer.
    Refuse(String),
}

/// Launch, queue, or refuse — decided before a packet exists.
pub(crate) fn admission(
    live: &[String],
    max: usize,
    pvc_workspace: bool,
    manifest: &str,
    wait: bool,
    queue_depth: usize,
    queue_cap: usize,
) -> Admission {
    // LEGACY-MANIFEST GUARD, and it never queues. If the runner's
    // /gate-target is still a PVC (a stale checkout, or `--manifest` at
    // the pre-parallel runner), the old law holds absolutely: one gate
    // per shared workspace, because two on one disk cross their receipts
    // (2026-08-24: a receipt naming one branch's head reported under
    // another; all three results discarded). Queueing that would only
    // postpone the crossing to when the place comes due.
    if pvc_workspace && !live.is_empty() {
        return Admission::Refuse(format!(
            "{n} gate(s) already running ({names}) and {manifest} mounts a SHARED \
             workspace at /gate-target — the pre-parallel runner shape.\n  Two gates \
             on one disk cross their receipts (2026-08-24: a receipt naming one \
             branch's head reported under another; all three results discarded).\n  \
             Wait for the running gate, or update the checkout so the manifest's \
             workspace is a per-run emptyDir seeded from /gate-seed.",
            n = live.len(),
            names = live.join(", "),
        ));
    }
    let Some(crowd) = crowd_refusal(live, max) else {
        return Admission::Launch;
    };
    // WITHOUT `--wait` THERE IS NO LAUNCHER. A queued packet is started
    // by the process holding its place; a caller that exits leaves one
    // nothing would ever start — the orphan in different clothes. So the
    // bound still refuses here, and names the flag that queues.
    if !wait {
        return Admission::Refuse(format!(
            "{crowd}\n  Or hold a place in line: `--wait` QUEUES at the bound \
             ({queue_depth} waiting now) and launches when a slot frees, oldest first — \
             one call, no retry loop. Without it there is no process to start a queued \
             run, so this refuses rather than filing a packet nothing would ever launch."
        ));
    }
    if queue_depth >= queue_cap {
        return Admission::Refuse(queue_full_refusal(queue_depth, queue_cap, max));
    }
    Admission::Queue
}

/// The refusal when the line is at its cap. Files nothing, and says how
/// far out the back of the line already is.
pub(crate) fn queue_full_refusal(depth: usize, cap: usize, max: usize) -> String {
    format!(
        "the gate queue is full: {depth} run(s) waiting for a slot, cap {cap}.\n  \
         The back of a full line is about {mins} min out at the measured median gate \
         ({MEDIAN_GATE_MINUTES} min, 2026-09-08) — longer than this refusal costs to \
         repeat.\n  Nothing was filed. Come back when the line is shorter, or raise \
         BOSS_GATE_MAX_CONCURRENT if the node has grown.",
        mins = estimated_wait_minutes(depth, max),
    )
}

/// The line, oldest place first, dead places dropped.
///
/// THE ORDER BELONGS TO THE SYSTEM OF RECORD, not to whichever builder
/// polled first. Five hand-rolled retry loops on 2026-09-08 were five
/// different orderings racing, and two of them shared a script file and
/// logged one builder's attempts under another's.
pub(crate) fn queue_order(
    open: &[Value],
    now: chrono::DateTime<chrono::Utc>,
    ttl_secs: i64,
) -> Vec<String> {
    let mut places: Vec<(chrono::DateTime<chrono::Utc>, String)> = open
        .iter()
        .filter_map(|j| {
            let md = j.get("metadata")?;
            let taken = md
                .get(QUEUED_AT)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .and_then(parse_instant)?;
            // No heartbeat yet means the place was just taken: the stamp
            // that ordered it is also the first proof it is held.
            let beat = md
                .get(QUEUE_HEARTBEAT_AT)
                .and_then(Value::as_str)
                .and_then(parse_instant)
                .unwrap_or(taken);
            if (now - beat).num_seconds() > ttl_secs {
                return None;
            }
            let id = j.get("id").and_then(Value::as_str)?;
            Some((taken, id.to_string()))
        })
        .collect();
    // By instant, then id: two places taken in the same second still
    // order the same way for every reader.
    places.sort();
    places.into_iter().map(|(_, id)| id).collect()
}

/// How many places are ahead of this caller, given the line and the
/// packet it would reuse.
///
/// A PACKET ALREADY IN THE LINE IS NOT AHEAD OF ITSELF. A builder whose
/// session died and who runs the verb again reuses the packet still
/// holding their place; counting it would refuse them at the cap over
/// their own place, which is a cap that cannot tell a newcomer from a
/// returner.
pub(crate) fn places_ahead(order: &[String], reuse: Option<&str>) -> usize {
    order.iter().filter(|id| Some(id.as_str()) != reuse).count()
}

/// May the place at 0-based `position` take a slot now?
///
/// Oldest first, and only into a slot that is actually free: second in
/// line waits for the second slot, so two waiters released by one
/// finishing gate do not both launch onto a node with room for one.
pub(crate) const fn may_launch(live: usize, max: usize, position: usize) -> bool {
    live + position < max
}

/// Roughly what a place costs, a gate at a time. Arithmetic on the
/// measured median — a number to decide on, not a promise.
pub(crate) const fn estimated_wait_minutes(position: usize, max: usize) -> u64 {
    let per_wave = if max == 0 { 1 } else { max };
    (position / per_wave + 1) as u64 * MEDIAN_GATE_MINUTES
}

/// The line a waiting builder reads. It NAMES ITS PACKET, for the same
/// reason [`verdict_line`] does: gates run in parallel, waiters share a
/// console, and a line whose owner has to be guessed is no line.
pub(crate) fn queued_line(
    packet: &str,
    branch: &str,
    position: usize,
    depth: usize,
    max: usize,
) -> String {
    format!(
        "boss gate: QUEUED {} of {depth}  (packet {}, {branch}) — ~{} min at the measured \
         median; this process holds the place, nothing else has to poll",
        position + 1,
        &packet[..8.min(packet.len())],
        estimated_wait_minutes(position, max),
    )
}

/// Take (or retake) a place: the ordering key and the first heartbeat
/// written together, so a place is never ordered without being held.
pub(crate) fn queue_place_patch(now: chrono::DateTime<chrono::Utc>) -> Value {
    json!({ QUEUED_AT: stamp(now), QUEUE_HEARTBEAT_AT: stamp(now) })
}

/// Release the place. `null` DELETES the key on a metadata PATCH, and
/// that matters: a blank string reads as a queued run, which would hide
/// a genuinely running gate from the yard's bays.
pub(crate) fn queue_clear_patch() -> Value {
    json!({ QUEUED_AT: Value::Null, QUEUE_HEARTBEAT_AT: Value::Null })
}

/// Substitute the runner manifest's placeholders and return the single
/// document that is the Job.
///
/// The manifest is multi-document on purpose (it carries the PVC and
/// RBAC beside the Job) and the Job uses `generateName`, which
/// `kubectl apply` rejects — so the Job has to be separated out and
/// `create`-ed. Doing that with `sed` and a hand-written splitter is
/// four of the seven steps this verb replaces.
/// Refuse a manifest carrying a `$GATE_*` placeholder this verb cannot
/// fill — BEFORE substituting, because substitution destroys the
/// evidence. `$GATE_MODE` is a prefix of `$GATE_MODE_OVERRIDE`, so a
/// plain replace would rewrite the first half of an unknown placeholder
/// and leave something that no longer looks wrong: the manifest would
/// render "cleanly" and the Job would run with a mangled value.
///
/// Split out of [`render_job`] so `run` can refuse a bad manifest in its
/// pre-flight, where a refusal costs a line of output rather than a
/// filed packet (fd217c65). Same order as always — validate before
/// acting — one step earlier.
pub(crate) fn check_placeholders(manifest: &str) -> Result<()> {
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
    Ok(())
}

/// The `kind: Job` document of the multi-document runner manifest.
///
/// Split out of [`render_job`] for the same reason as
/// [`check_placeholders`]: the workspace-shape guard now runs before a
/// packet exists, so it has no packet id to substitute — and it needs
/// none. Substitution fills values inside `generateName` and `args`; it
/// never touches a volume or a mount, which is all
/// [`pvc_backed_workspace`] reads. ONE splitter, so the guard and the
/// launch cannot disagree about which document is the Job (§9a).
pub(crate) fn job_document(manifest: &str) -> Result<String> {
    manifest
        .split("\n---")
        .find(|doc| doc.contains("kind: Job"))
        .map(str::to_string)
        .context("runner manifest contains no `kind: Job` document")
}

pub(crate) fn render_job(
    manifest: &str,
    branch: &str,
    packet_id: &str,
    mode: &str,
) -> Result<String> {
    check_placeholders(manifest)?;
    let filled = manifest
        .replace(BRANCH_PLACEHOLDER, branch)
        .replace(PACKET_PLACEHOLDER, packet_id)
        .replace(HINT_PLACEHOLDER, &name_hint(branch))
        .replace(MODE_PLACEHOLDER, mode);
    job_document(&filled)
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
pub(crate) fn gate_run_body(
    branch: &str,
    sha: &str,
    manifest: &str,
    delivery_channel: Option<&str>,
) -> Value {
    let mut metadata = json!({
        "branch": branch,
        "sha": sha,
        "runner": manifest,
    });
    // The delivery channel this change ships on (data/config/software/
    // infra), derived from what it touches. Stamped so the car and the
    // channel-gated delivery can branch on it without re-deriving; absent
    // when the branch had no forge diff to classify.
    if let Some(dc) = delivery_channel {
        metadata["delivery_channel"] = json!(dc);
    }
    json!({
        "kind": "gate-run",
        "title": format!("Gate: {branch}"),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": "emp-david",
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": metadata,
    })
}

/// The park prose a gate carries so the auto-park handler can file the
/// car VERBATIM on green — the input half of the auto-park loop. Stamped
/// as `park_*` keys on the gate-run metadata by the `--park-*` flags;
/// absent means a plain gate that will not auto-park. The four fields a
/// receipt needs (summary/excludes/test/verified) are required together,
/// so a stamped intent is always enough to file a valid car; a
/// `backlog_item` edge is optional, and so is the PROOF the car will
/// carry: a `probe` + `expect` pair (the same shape `boss prove`
/// records, run by `boss prove --from-car` or by the arrival rule once
/// the car lands) or, for a car only an event can prove, a
/// `proof_event` line saying which event and how (backlog 28ac45ab).
#[derive(Debug, Clone, Default)]
pub struct ParkIntent {
    pub summary: Option<String>,
    pub excludes: Option<String>,
    pub test: Option<String>,
    pub verified: Option<String>,
    pub backlog_item: Option<String>,
    pub probe: Option<String>,
    pub expect: Option<String>,
    pub proof_event: Option<String>,
}

/// WHERE A RECORDED PROBE RUNS — the forge host's absence manifest.
///
/// A `--park-probe` is written HERE (the dev pod: kubectl, a
/// kubeconfig, the cluster one hop away) and RUN THERE
/// (`infra/forge/run-car-probe.sh` on the forge host, as david, in
/// /home/david/boss, when the car's train arrives). Two machines. The
/// forge is outside the cluster and holds no kubeconfig, so a probe
/// that reaches for `kubectl` is correct and unrunnable — and its
/// failure at arrival is an exit code, hours later, on a car.
///
/// This file lists the tools MEASURED absent from that host, so the
/// refusal happens at gate time on the builder's terminal instead. It
/// is data, not code: an absence measured next month is a line, not a
/// release. Backlog f9304366.
const FORGE_ABSENT_TOOLS: &str = include_str!("../../../../infra/forge/host-absent-tools.txt");

/// The manifest's live lines: one tool name each, comments and blanks
/// dropped.
pub fn forge_absent_tools() -> Vec<&'static str> {
    FORGE_ABSENT_TOOLS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// The commands a shell line would RUN, in command position — the first
/// word of the probe and of every segment after a `|`, `&&`, `||`, `;`,
/// a newline, a subshell or a substitution. Leading environment
/// assignments and flags are stepped over, as are the words that stand
/// in front of a program rather than being one (`if`, `sudo`, `env`, …),
/// and a path is reduced to its basename so `/usr/bin/kubectl` reads as
/// `kubectl`.
///
/// Deliberately not a shell parser: it does not know quoting, so a tool
/// name inside a quoted string that follows a separator can be read as
/// a command. The cost of that is a refusal the builder can reword; the
/// cost of the alternative is a shell parser to maintain in the CLI.
pub fn commands_invoked(probe: &str) -> Vec<&str> {
    const NOT_THE_PROGRAM: [&str; 17] = [
        "if", "then", "elif", "else", "fi", "while", "until", "for", "do", "done", "case", "esac",
        "!", "time", "sudo", "env", "command",
    ];
    probe
        .split(['|', '&', ';', '\n', '(', ')', '`', '{', '}'])
        .filter_map(|segment| {
            segment
                .split_whitespace()
                .map(|w| w.trim_matches(['"', '\'', '$', '\\']))
                .find(|w| {
                    !w.is_empty()
                        && !w.contains('=')
                        && !w.starts_with('-')
                        && !NOT_THE_PROGRAM.contains(w)
                })
                .map(|w| w.rsplit('/').next().unwrap_or(w))
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// The tool the forge does not have that this probe would need, if any.
pub fn probe_needs_absent_tool(probe: &str) -> Option<&'static str> {
    let absent = forge_absent_tools();
    commands_invoked(probe)
        .into_iter()
        .find_map(|c| absent.iter().find(|a| **a == c).copied())
}

/// The gate-run key each proof flag stamps. The auto-park handler
/// reads these and writes the car's `proof_*` keys
/// (`boss_jobs::car::PROOF_*`).
pub const PARK_PROBE: &str = "park_probe";
pub const PARK_EXPECT: &str = "park_expect";
pub const PARK_PROOF_EVENT: &str = "park_proof_event";

impl ParkIntent {
    /// True when no `--park-*` flag was given: a plain gate.
    pub fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.excludes.is_none()
            && self.test.is_none()
            && self.verified.is_none()
            && self.backlog_item.is_none()
            && self.probe.is_none()
            && self.expect.is_none()
            && self.proof_event.is_none()
    }

    /// Refuse a PARTIAL intent. Auto-park files a car with a full
    /// receipt, so if any park flag is given the four a receipt needs
    /// must all be given — better a refusal here than a car filed with an
    /// empty boundary or an unproven `verified` line.
    ///
    /// The proof flags have their own two rules, checked first because
    /// they are the cheaper mistake: `--park-probe` and `--park-expect`
    /// go together (a probe with no expectation is `echo hi`, and
    /// `boss prove` refuses that shape too), and a car is EITHER probed
    /// or event-bound — a probe next to a `--park-proof-event` says the
    /// builder did not decide which.
    pub fn require_complete(&self) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        match (&self.probe, &self.expect) {
            (Some(_), None) => anyhow::bail!(
                "--park-probe needs --park-expect '<string the probe must print>': a probe \
                 asserting nothing is `echo hi`, which exits 0 too. Say what the probe \
                 prints when the change is in production."
            ),
            (None, Some(_)) => anyhow::bail!(
                "--park-expect without --park-probe: there is no command for that string \
                 to come out of. Give --park-probe '<command run against production>'."
            ),
            _ => {}
        }
        if self.probe.is_some() && self.proof_event.is_some() {
            anyhow::bail!(
                "--park-probe and --park-proof-event together: a car is proven by a probe \
                 the machine can run, OR it waits for an event only a person can observe. \
                 Pick one — if the probe exists, the car is not event-bound."
            );
        }
        if let Some(probe) = &self.probe
            && let Some(tool) = probe_needs_absent_tool(probe)
        {
            anyhow::bail!(
                "--park-probe invokes `{tool}`, which the FORGE HOST does not have.\n\n\
                 A recorded probe does not run here. It runs on the forge host, as david, \
                 in /home/david/boss, when this car's train arrives \
                 (infra/forge/run-car-probe.sh) — a machine outside the cluster with no \
                 kubeconfig. Measured 2026-09-09 (f9304366): the first two cars ever to \
                 record a probe both used `kubectl`, both were right from this pod, and \
                 both came back as an exit code with empty streams.\n\n\
                 Give a probe the forge can run — the system of record over HTTP (curl \
                 $BOSS_JOBS_URL/api/...), the forge's own journal, the converged checkout \
                 — or, if only the cluster can show it, record the car as \
                 --park-proof-event and prove it by hand.\n\n\
                 The absence list is infra/forge/host-absent-tools.txt."
            );
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
        put(PARK_PROBE, &self.probe);
        put(PARK_EXPECT, &self.expect);
        put(PARK_PROOF_EVENT, &self.proof_event);
        Value::Object(m)
    }

    /// The patch that REMOVES park intent from a gate-run: every
    /// `park_*` key set to null, which the metadata door deletes. Applied
    /// to a reused packet when its branch has landed, so intent stamped
    /// before the landing cannot survive it (610537b2).
    pub fn clear_patch() -> Value {
        json!({
            "park_summary": Value::Null,
            "park_excludes": Value::Null,
            "park_test": Value::Null,
            "park_verified": Value::Null,
            "park_backlog_item": Value::Null,
            PARK_PROBE: Value::Null,
            PARK_EXPECT: Value::Null,
            PARK_PROOF_EVENT: Value::Null,
        })
    }
}

/// The HOLD a gate carries: `--hold <reason>` stamps `hold: <reason>`
/// on the gate-run so its green reads HELD (a brake deliberately on)
/// rather than stranded (a green someone forgot) — in `boss orient`,
/// the stranded-green alarm and the yard, which all read that one key
/// (69daaba2: a boss-dev manifest car waiting for a David-timed roll
/// was indistinguishable from a forgotten one). A hold never combines
/// with park intent: auto-park would file the car on green and board
/// it, which is the opposite of holding it. An empty reason is refused
/// — the marker IS the reason, and "held: (blank)" tells the next
/// reader nothing.
pub fn hold_guard(hold: Option<&str>, park: &ParkIntent) -> Result<Option<String>> {
    let Some(reason) = hold else {
        return Ok(None);
    };
    let reason = reason.trim();
    if reason.is_empty() {
        anyhow::bail!("--hold needs a reason: what is this green waiting for?");
    }
    if !park.is_empty() {
        anyhow::bail!(
            "--hold and --park-* cannot combine: a hold keeps the green off the dock, \
             auto-park files a car for it on green. Pass one or the other."
        );
    }
    Ok(Some(reason.to_string()))
}

/// Whether a post-creation failure in `run` must close the gate-run it
/// just filed. True exactly when WE created the packet this run
/// (`!reused`) AND it is not a dry run (`!dry`): a reused packet has its
/// own life to close elsewhere, and a dry run never filed anything.
///
/// ONLY SoR ROUND-TRIPS REACH THIS NOW. The three guards that used to
/// share it (`running_gates`, the PVC guard, `crowd_refusal`) moved
/// ahead of the packet into [`admission`], because a bounded refusal
/// recorded as `lost` is a lie about what happened (fd217c65). What is
/// left is the park-intent / queue-place PATCH, whose unguarded `?` used
/// to abort `run` on a transient blip and leave a packet open with no
/// runner Job — and for THAT, `lost` ("environment died") is the honest
/// verdict, because the environment is exactly what failed.
pub(crate) fn should_close_on_park_failure(reused: bool, dry: bool) -> bool {
    !reused && !dry
}

/// What is already known about a branch's landing, gathered BEFORE a
/// gate-run is filed or reused: the git verdict (`boss merged`'s rules,
/// called as a library — never re-derived here, see 26b3d203) and the
/// system of record's landed car, when it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Landing {
    /// Which signal answered, in words a refusal can print.
    pub how: String,
    /// `car <id8> merged as <ref> on train <id8>` from the landed car —
    /// the name the operator needs to find where the change went.
    pub landed_as: Option<String>,
}

/// PURE: has THIS head's content already landed on main?
///
/// Git answers first — ancestry, patch-id, or content, exactly as
/// `boss merged` decides. The car is consulted for the name of the
/// landing, and answers alone only when it boarded this exact head: a
/// landed car whose boarded head differs is a branch reused after its
/// landing, and the new commits are gateable. A stale clone that cannot
/// see main's objects reads Unknown from git and the car fills in.
pub(crate) fn landing(
    v: &crate::merged::Verdict,
    cars: &[Value],
    branch: &str,
    sha: &str,
) -> Option<Landing> {
    use crate::merged::{How, Verdict};
    let car = boss_jobs::car::landed_car_for(cars, branch);
    let md = |c: &Value, k: &str| {
        c.pointer(&format!("/metadata/{k}"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let boarded_this_head = car.and_then(|c| md(c, "boarded_head")).is_some_and(|h| {
        !h.is_empty() && !sha.is_empty() && (h.starts_with(sha) || sha.starts_with(&h))
    });
    let how = match v {
        Verdict::Merged(How::Ancestor) => "its head is an ancestor of main",
        Verdict::Merged(How::PatchesPresent) => "main already carries every patch on it",
        Verdict::Merged(How::ContentPresent) => {
            "main already holds its version of every file it changed"
        }
        _ if boarded_this_head => "its car boarded this exact head and merged",
        _ => return None,
    };
    let landed_as = car.map(|c| {
        let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
        let train = md(c, "train").unwrap_or_else(|| "?".into());
        format!(
            "car {} merged as {} on train {}",
            &id[..8.min(id.len())],
            md(c, "merge_ref").unwrap_or_else(|| "?".into()),
            &train[..8.min(train.len())]
        )
    });
    Some(Landing {
        how: how.to_string(),
        landed_as,
    })
}

/// What `boss gate` does about a branch that already landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LandedGuard {
    /// Nothing landed (or the guard was forced on an unlanded branch,
    /// which is harmless): gate as usual.
    Proceed,
    /// Landed, and the operator said why to gate it anyway — the note
    /// to print. Park intent is never carried through this door.
    Forced(String),
    /// Landed: the message, and no gate.
    Refuse(String),
}

/// PURE: never re-gate a branch whose content already landed — unless
/// told to, with a reason, and never with park intent.
///
/// The measured failure (610537b2): re-gating a landed branch reused its
/// old gate-run, which still carried park intent, and the green filed a
/// twin car that boarded an empty diff. The re-gate had a purpose (to
/// close a dead packet), so a reasoned `--force-regate` keeps that door;
/// `--park-*` on a landed branch has no purpose at all and is refused
/// outright. A landed branch's next step is deletion, and the refusal
/// says so.
pub(crate) fn landed_guard(
    branch: &str,
    sha: &str,
    landing: Option<&Landing>,
    force_reason: Option<&str>,
    park: &ParkIntent,
) -> LandedGuard {
    let Some(l) = landing else {
        return LandedGuard::Proceed;
    };
    let short = &sha[..7.min(sha.len())];
    let landed_as = l
        .landed_as
        .as_deref()
        .map(|a| format!("; {a}"))
        .unwrap_or_default();
    match force_reason {
        None => LandedGuard::Refuse(format!(
            "boss gate: REFUSED — {branch}@{short} already landed on main ({}{landed_as}).\n  \
             Re-gating landed content files nothing useful and, with park intent, files a \
             twin car that boards an empty diff (2026-09-08, train #261). Delete the branch \
             instead: git push origin --delete {branch}\n  \
             To gate it anyway (e.g. to close a dead gate-run packet), pass \
             --force-regate \"<reason>\" — without any --park-* flag.",
            l.how
        )),
        Some(_) if !park.is_empty() => LandedGuard::Refuse(format!(
            "boss gate: REFUSED — {branch}@{short} already landed on main ({}{landed_as}), \
             and --force-regate cannot carry park intent: a landed branch has nothing to \
             park, and a car for it is a twin. Drop the --park-* flags.",
            l.how
        )),
        Some(reason) => LandedGuard::Forced(format!(
            "boss gate: {branch}@{short} already landed on main ({}{landed_as}) — gating \
             anyway because: {reason}. Any park intent a reused packet carries is cleared, \
             so this green cannot file a twin.",
            l.how
        )),
    }
}

/// What `boss gate` does about a branch that is already gated green and
/// already carries a car.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GatedGuard {
    /// Nothing to stop: no live car, no current green, or a head that
    /// has moved since the receipt was written.
    Proceed,
    /// Gating anyway, with the operator's reason — the note to print.
    Forced(String),
    /// The message, and no gate.
    Refuse(String),
}

/// PURE: never re-gate a head that is ALREADY green and already has a
/// car — unless told to, with a reason.
///
/// The measured failure (02165b1d): the builder session that owned
/// feat/a-human-only-step-refuses-an-agent ended, but its detached retry
/// loop kept gating. Car d08a6418 boarded train #274 at 20:30:54; at
/// 20:31:20 the duplicate gate went green and auto-park filed twin car
/// ad54e95c, which sat on the dock until it was abandoned by hand. The
/// landed guard could not see it — the branch was green and in transit,
/// not merged. Both halves are fixed: the handler files nothing for a
/// branch with a live car, and this refuses the pointless launch that
/// starts it, one step earlier and without spending a gate slot.
///
/// The receipt test is `boss receipt`'s, not a second one: the car's
/// receipt (`regate_receipt` first, else the gate step) read against the
/// head about to be gated. A moved head is a real re-gate and proceeds;
/// a red or absent receipt proceeds; only "this exact head is already
/// green, and a car already carries it" is refused.
pub(crate) fn gated_car_guard(
    branch: &str,
    sha: &str,
    cars: &[Value],
    force_reason: Option<&str>,
) -> GatedGuard {
    let Some(car) = boss_jobs::car::open_car_for(cars, branch) else {
        return GatedGuard::Proceed;
    };
    let Some(receipt) = crate::receipt::select_receipt(car) else {
        return GatedGuard::Proceed;
    };
    let facts = crate::receipt::ReceiptFacts {
        gated_head: receipt
            .get("head")
            .and_then(Value::as_str)
            .map(str::to_string),
        branch_head: Some(sha.to_string()),
        // The head was resolved by the caller, so the ref is readable by
        // construction; `standing` needs to know that to answer at all.
        remote_readable: true,
        verdict: receipt
            .get("verdict")
            .and_then(Value::as_str)
            .map(str::to_string),
        mode: receipt
            .get("mode")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    if facts.verdict.as_deref() != Some("green")
        || crate::receipt::standing(&facts) != crate::receipt::Standing::Current
    {
        return GatedGuard::Proceed;
    }
    let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
    let id = &id[..8.min(id.len())];
    let train = car
        .pointer("/metadata/train")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let where_it_is = if boss_jobs::car::is_boarded(car) {
        format!("aboard train {}", &train[..8.min(train.len())])
    } else {
        "parked at the dock".to_string()
    };
    let short = &sha[..7.min(sha.len())];
    match force_reason {
        Some(reason) => GatedGuard::Forced(format!(
            "boss gate: {branch}@{short} is already green and car {id} carries it \
             ({where_it_is}) — gating anyway because: {reason}."
        )),
        None => GatedGuard::Refuse(format!(
            "boss gate: REFUSED — {branch}@{short} already has a CURRENT green receipt, \
             and car {id} ({where_it_is}) carries it.\n  \
             Re-gating an unchanged head files nothing useful: the second green repeats \
             the receipt the car already holds, and with park intent it files a TWIN car \
             that sits on the dock while the first one rides (2026-09-08, cars \
             d08a6418/ad54e95c, backlog 02165b1d).\n  \
             Push a new head and gate that, or — to re-gate this exact head anyway \
             (re-running a check that load-flaked, say) — pass \
             --force-regate \"<reason>\"."
        )),
    }
}

/// Gather the landing signals: a quiet fetch of main so the local
/// objects can answer the content comparison (a clone behind the forge
/// is how 26b3d203's wrong answers were made), then `boss merged`'s
/// observation, then the SoR's closed cars for the branch. Every probe
/// is allowed to fail; an unobservable signal is `None`, never a
/// landing.
async fn observe_landing(http: &reqwest::Client, branch: &str, sha: &str) -> Option<Landing> {
    let _ = crate::git_auth::command()
        .args(["fetch", "--quiet", "origin", "main"])
        .status();
    let v = crate::merged::verdict(&crate::merged::observe(".", "origin", branch));
    let cars = rows(
        api(
            http,
            reqwest::Method::GET,
            &format!("/api/jobs?kind=ship-a-change&status=closed&subject_id={branch}&limit=20"),
            None,
        )
        .await
        .unwrap_or(None),
    );
    landing(&v, &cars, branch, sha)
}

/// Close a just-registered gate-run whose setup FAILED after the packet
/// was filed, so the failure leaves no orphan (ed7f1355: a guard fired
/// after the packet was filed, the packet sat open with no runner to
/// ever complete it, closing it honestly meant hand-writing a receipt,
/// and that hand-written head then shadowed the real green in `boss
/// park`). Machine-written `lost` with an empty head — the launch never
/// resolved one, and an empty head is exactly what keeps this receipt
/// from ever matching a real one.
///
/// REACHED ONLY BY A GENUINE ENVIRONMENT FAILURE, since fd217c65: the
/// bound, the queue cap and the shared-workspace law are decided by
/// [`admission`] before a packet exists and file nothing. `lost` is the
/// truthful terminal for what is left here — a system of record that
/// went away mid-setup — and it was a lie for the 21 bounded refusals it
/// recorded on 2026-09-08.
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
    resolve_jobs_base(None)
}

/// Resolve the jobs-api base from an explicit `--jobs-url` flag, falling
/// back to `BOSS_JOBS_URL`, and REFUSING (never defaulting) when neither
/// is set. Every read verb — `gate`, `packet census`, `queue`, `prove` —
/// resolves through here so not one of them can quietly re-grow the
/// `http://127.0.0.1:7900` default that read boss-gcp's second, older
/// stack (packet aa783636). A default that is right on one host and
/// silently wrong on another IS the defect; a verb that cannot reach the
/// right instance now reaches none.
pub(crate) fn resolve_jobs_base(flag: Option<&str>) -> Result<String> {
    resolve_jobs_base_from(flag, std::env::var("BOSS_JOBS_URL").ok())
}

/// The pure core of [`resolve_jobs_base`]: flag wins, then env, then
/// refuse. Split out so a test can pin the precedence and the refusal
/// without mutating process environment — env writes are `unsafe` under
/// edition 2024, and racy across the parallel test runner.
pub(crate) fn resolve_jobs_base_from(flag: Option<&str>, env: Option<String>) -> Result<String> {
    flag.map(str::to_string)
        .filter(|v| !v.trim().is_empty())
        .or_else(|| env.filter(|v| !v.trim().is_empty()))
        .ok_or_else(|| anyhow!("{}", no_instance_message()))
}

pub(crate) async fn api(
    http: &reqwest::Client,
    method: reqwest::Method,
    path: &str,
    payload: Option<Value>,
) -> Result<Option<Value>> {
    api_at(http, &jobs_base()?, method, path, payload).await
}

/// Like [`api`], but against an explicit base rather than the resolved
/// one. The seam a paginating reader tests through: a stub socket can
/// answer without a `BOSS_JOBS_URL` anywhere in the environment.
///
/// EVERY operator verb — `gate`, `prove`, `park`, `job`, `orient`,
/// `rerail`, `docs` — writes through here, which is why this is where
/// the caller gets signed. A write carries the actor RUNNING the
/// command or is refused; a read carries them if known and says so
/// once if not. Neither is ever signed as the conductor: that is
/// backlog 5083d6f5, measured on the step a `boss prove` completed.
pub(crate) async fn api_at(
    http: &reqwest::Client,
    base: &str,
    method: reqwest::Method,
    path: &str,
    payload: Option<Value>,
) -> Result<Option<Value>> {
    let signature = identity::signature_for(&method, path, identity::caller());
    api_at_signed(http, base, method, path, payload, signature).await
}

/// [`api_at`] with the signing decision already made. The seam the
/// wire tests go through: WHO signs is a pure function of the
/// environment (`identity::signature_for`), and this is the half that
/// puts it on the socket — so a test can prove the header carries the
/// caller, and that a refused write never reaches the network, without
/// mutating the process's environment.
pub(crate) async fn api_at_signed(
    http: &reqwest::Client,
    base: &str,
    method: reqwest::Method,
    path: &str,
    payload: Option<Value>,
    signature: identity::Signature,
) -> Result<Option<Value>> {
    let signer = identity::apply(signature)?;
    let mut req = http
        .request(method.clone(), format!("{base}{path}"))
        .header("x-boss-user", identity::header(&signer))
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

/// Every open `ship-a-change` car, across ALL pages — the operator-verb
/// counterpart to the conductor's `train::list_all_pages`, built on the
/// `gate::api` client the CLI verbs speak through (a different client
/// from the conductor's, which is why the boarding-pagination car could
/// not fold these call sites into that helper directly).
///
/// A limit is not a filter (memory: a-limit-is-not-a-filter). Open cars
/// build past a page — in-flight + parked + landed-but-unclosed residue —
/// and the list is `ORDER BY opened_on DESC`, so a car opened days ago
/// but touched today sorts to the tail. Read with a bare `limit=`, that
/// car falls off page one and `boss receipt`/`rerail`/`channels` reported
/// a car that exists as "not found" — a false negative that grows as
/// closed cars accumulate. This pages on the response `total` via
/// [`train::list_all_pages`] (whose page-two behaviour is pinned there)
/// until every matching row is read.
pub(crate) async fn all_open_cars(http: &reqwest::Client) -> Result<Vec<Value>> {
    crate::train::list_all_pages(|offset| async move {
        api(
            http,
            reqwest::Method::GET,
            &format!(
                "/api/jobs?kind=ship-a-change&status=open&limit={}&offset={offset}",
                crate::train::PAGE_LIMIT
            ),
            None,
        )
        .await
    })
    .await
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
    force_regate: Option<String>,
    hold: Option<String>,
    now: chrono::DateTime<chrono::Utc>,
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
    // The park expectation is the one the MACHINE will judge at
    // arrival, so it never passes through `boss prove`'s own
    // warning. Say it here instead, while the flag is still being
    // typed (421b3032).
    if let Some(w) = park
        .expect
        .as_deref()
        .and_then(crate::prove::bare_number_warning)
    {
        eprintln!("{w}");
    }
    let hold = hold_guard(hold.as_deref(), &park)?;
    let http = reqwest::Client::new();
    // The concurrency bound: env override > delivery policy > compiled.
    // Fetched here, before the packet, so a bad env override refuses
    // without side effects — the policy read never fails, it falls back.
    let max = max_concurrent(&http).await?;
    let sha = resolve_sha(branch);

    // A LANDED BRANCH IS NOT GATED. Before any packet is filed or
    // reused — a refusal here costs nothing to close. See `landed_guard`.
    let landed = observe_landing(&http, branch, &sha).await;
    match landed_guard(
        branch,
        &sha,
        landed.as_ref(),
        force_regate.as_deref(),
        &park,
    ) {
        LandedGuard::Proceed => {}
        LandedGuard::Forced(note) => println!("{note}"),
        LandedGuard::Refuse(why) => bail!("{why}"),
    }

    // NOR IS A HEAD THAT IS ALREADY GREEN AND ALREADY CARRIED. One step
    // earlier than the landed guard, and the same shape: read the SoR,
    // refuse before a packet or a gate slot is spent. An unreadable list
    // is not a refusal — an unobservable signal proceeds, the way
    // `observe_landing` does. See `gated_car_guard`.
    let open_cars = all_open_cars(&http).await.unwrap_or_default();
    match gated_car_guard(branch, &sha, &open_cars, force_regate.as_deref()) {
        GatedGuard::Proceed => {}
        GatedGuard::Forced(note) => println!("{note}"),
        GatedGuard::Refuse(why) => bail!("{why}"),
    }

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
    let reuse = reusable_packet(&open, branch, &sha);

    // A REUSED PACKET MAY ALREADY BE GATING — and attaching to it is
    // neither a launch nor a refusal, so it is settled before admission.
    // This verb's own closing line used to send the operator straight
    // into the failure: run `boss gate <branch>`, then `boss gate
    // <branch> --wait` as instructed, and the second invocation reused
    // the open packet and created a SECOND Job against it. Both raced on
    // one gate-target, one died, and the survivor's green verdict was
    // recorded as `lost`. Attaching is what the advice always meant.
    if let Some(id) = reuse.as_deref()
        && !dry
        && let Some(name) = live_gate_for_packet(namespace, id)?
    {
        println!("boss gate: packet {id} is already being gated by {name}");
        if wait {
            println!("boss gate: attaching to it — a second Job would race it");
            return wait_for_verdict(&http, id, namespace, &name).await;
        }
        println!(
            "boss gate: not starting a second Job. Follow this one with \
             `boss gate {branch} --wait`, which attaches."
        );
        return Ok(());
    }

    // EVERY REFUSAL IS DECIDED HERE, BEFORE A PACKET EXISTS (fd217c65).
    // The bound, the queue cap, the legacy-workspace law and an
    // unfillable manifest all used to fire AFTER the packet was filed
    // and had to close what they had just opened — as `lost`, the only
    // terminal the gate-run protocol offers a run with no verdict, whose
    // label is "environment died". On 2026-09-08 that recorded 21
    // bounded refusals as catastrophes. `admission` answers from
    // observations alone; a Refuse from here files nothing at all.
    //
    // The count FAILS CLOSED (`running_gates` bails on an unreadable
    // cluster) and that is a plain `?` now: there is no packet to
    // orphan, and a cluster too sick to answer was never going to run
    // the gate either.
    check_placeholders(&manifest_text)?;
    let live = running_gates(namespace)?;
    // How many places are ahead of us. A packet we would REUSE that is
    // already in the line is not ahead of itself — counting it would
    // refuse a builder whose own place is what filled the last slot,
    // which is the failure mode of a cap that cannot tell a newcomer
    // from a returner.
    let ahead = places_ahead(
        &queue_order(&open, now, QUEUE_PLACE_TTL_SECS),
        reuse.as_deref(),
    );
    let queued = match admission(
        &live,
        max,
        pvc_backed_workspace(&job_document(&manifest_text)?),
        &manifest_path.display().to_string(),
        wait,
        ahead,
        QUEUE_CAP,
    ) {
        Admission::Refuse(why) => bail!("{why}"),
        Admission::Queue => true,
        Admission::Launch => false,
    };
    if !queued && !live.is_empty() {
        println!(
            "boss gate: {} gate(s) already running ({}) — workspaces are per-run, \
             verdicts stay independent; launching alongside",
            live.len(),
            live.join(", ")
        );
    }

    let mut reused = false;
    let packet = match reuse {
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
                        crate::channels::delivery_channel_for(branch).as_deref(),
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
        // This PATCH is a SECOND round-trip AFTER the packet was filed,
        // and the SoR rolls for tens of seconds on every train deploy
        // (the very thing `wait_for_verdict` defends against). An
        // unguarded `?` here used to abort `run` on a transient blip and
        // leave the packet we just opened sitting open with no runner
        // Job — an orphan. So close what we created before bailing
        // (ed7f1355); an SoR that went away IS an environment failure,
        // which is what `lost` truthfully means.
        if let Err(e) = api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(park.metadata_patch()),
        )
        .await
        .context("stamping park intent onto the gate-run")
        {
            if should_close_on_park_failure(reused, dry) {
                close_refused(&http, &packet, &format!("{e:#}")).await;
            }
            return Err(e);
        }
        println!("boss gate: park intent stamped — this branch auto-parks on green");
    }
    // Stamp the hold the same way, for the same reasons: a PATCH that
    // merges onto a created or reused packet, closing what we created
    // if the round-trip fails.
    if let Some(reason) = hold.as_deref().filter(|_| !dry) {
        if let Err(e) = api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(json!({ "hold": reason })),
        )
        .await
        .context("stamping the hold onto the gate-run")
        {
            if should_close_on_park_failure(reused, dry) {
                close_refused(&http, &packet, &format!("{e:#}")).await;
            }
            return Err(e);
        }
        println!("boss gate: hold stamped — a green here reads HELD ({reason}), not stranded");
    }
    // STALE INTENT DOES NOT SURVIVE A LANDING. A reused packet carries
    // whatever `park_*` keys its first launch stamped; on a forced
    // re-gate of a landed branch those keys are the twin's trigger
    // (610537b2), so they are removed before the runner starts. Best
    // effort: the handler refuses a landed twin on its own too.
    if reused && landed.is_some() && !dry {
        match api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(ParkIntent::clear_patch()),
        )
        .await
        {
            Ok(_) => println!(
                "boss gate: park intent cleared from the reused packet — a landed branch \
                 does not auto-park"
            ),
            Err(e) => eprintln!("boss gate: could not clear stale park intent: {e:#}"),
        }
    }

    // THE PLACE IN LINE. At the bound the packet is filed as a QUEUED
    // gate-run — an honest open packet for work that has been requested
    // and is waiting — and this process holds its place by heartbeat
    // until the line reaches it. That is the one implementation of the
    // retry loop five builders wrote by hand on 2026-09-08, with the
    // order kept by the system of record rather than by whoever polled
    // first, and with no way to outlive the session that made it.
    if queued && !dry {
        if let Err(e) = api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(queue_place_patch(now)),
        )
        .await
        .context("taking a place in the gate queue")
        {
            if should_close_on_park_failure(reused, dry) {
                close_refused(&http, &packet, &format!("{e:#}")).await;
            }
            return Err(e);
        }
        println!("{}", queued_line(&packet, branch, ahead, ahead + 1, max));
        if let Slot::Gating(name) =
            wait_for_slot(&http, &packet, branch, namespace, max, now).await?
        {
            println!("boss gate: packet {packet} was launched as {name} while it waited");
            return wait_for_verdict(&http, &packet, namespace, &name).await;
        }
        // The place is spent the moment a Job exists, and a run still
        // marked queued would be missing from the yard's gate bays.
        // Cleared BEFORE the Job is created: a failure here costs
        // nothing (the place is still held, the packet still reusable),
        // where a failure after it would hide a running gate.
        api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(queue_clear_patch()),
        )
        .await
        .context("releasing the queue place before launching")?;
    }

    let job = render_job(&manifest_text, branch, &packet, &mode)?;

    if dry {
        if queued {
            println!(
                "boss gate: DRY the node is at the bound — this would QUEUE at place {} of {}",
                ahead + 1,
                ahead + 1
            );
        }
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
            "the gate Job failed without the packet ever reporting a verdict.\n               That is NOT the same as a red gate: the run died, or it finished and could not \
             record its verdict (exit 75 — the `gate-runner: UNREPORTED verdict=…` line), \
             and the code may well have passed. The workspace was per-run and died with the \
             pod, so the surviving copy of the receipt is the pod log — `kubectl logs \
             job/<job>` (the `gate-runner: receipt` line), kept for a day after the Job \
             ends. Read it rather than re-running 40 minutes of gate on the assumption this \
             was a failure."
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

/// The verdict recorded on THIS packet, or `None` while it is silent.
///
/// Refuses, by name, a body that belongs to another packet. The poller
/// asks the SoR for one id and has always trusted whatever came back;
/// a wrong target answers instead of erroring (CLAUDE.md §Doors), so
/// the id is checked when the body carries one. A body with no id is
/// read as before — absence is not evidence of a mix-up.
///
/// Backlog 9dd9993b: under three parallel gates a builder's console
/// showed a neighbour's `failed` beside its own `green`. The poll was
/// pinned to its packet the whole time (this verb creates the packet
/// and the Job and reads both by id/name); what was missing was any
/// mark on the verdict line saying whose it was — see [`verdict_line`].
pub(crate) fn own_verdict(packet: &str, body: &Value) -> Result<Option<String>> {
    if let Some(id) = body.get("id").and_then(Value::as_str)
        && id != packet
    {
        bail!(
            "asked the system of record for gate-run {} and it answered with {} — refusing \
             to read another packet's verdict as this one's",
            &packet[..8.min(packet.len())],
            &id[..8.min(id.len())]
        );
    }
    Ok(body
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
        .map(str::to_string))
}

/// One verdict line, self-identifying: `boss gate: green  (packet
/// f451af16, fix/branch)`. Two `--wait` pollers sharing a console — the
/// normal case since gates run in parallel — must never print a line
/// the reader has to guess the owner of.
pub(crate) fn verdict_line(verdict: &str, packet: &str, body: &Value) -> String {
    let branch = body
        .pointer("/metadata/branch")
        .and_then(Value::as_str)
        .unwrap_or("<branch unrecorded>");
    format!(
        "boss gate: {verdict}  (packet {}, {branch})",
        &packet[..8.min(packet.len())]
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
        if let Some(v) = own_verdict(packet, &job)? {
            record_gated_head(http, packet, &job).await;
            println!("{}", verdict_line(&v, packet, &job));
            if v != "green" {
                bail!(
                    "gate verdict: {v}  (packet {}, {})",
                    &packet[..8.min(packet.len())],
                    job.pointer("/metadata/branch")
                        .and_then(Value::as_str)
                        .unwrap_or("<branch unrecorded>")
                );
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

/// What ended a wait in the gate queue.
enum Slot {
    /// The line reached this place and a slot is free: launch.
    Free,
    /// Another actor reused this packet and launched it while we waited.
    /// Attaching is right; a second Job would race it.
    Gating(String),
}

/// Hold a place in the gate queue until the line reaches it.
///
/// THIS IS THE LAUNCHER, and it lives here — not in the conductor's
/// cadence loop and not in a dispatcher rule — for three reasons, in
/// order of how hard they are to work around:
///
///  1. **Only this verb can create a gate Job.** It needs the Kubernetes
///     credential and the runner manifest. The conductor holds neither
///     on purpose (`infra/cluster/manifests/boss-conductor.yaml`: "RBAC:
///     NONE. The conductor holds no Kubernetes credential"), and the
///     dispatcher is no better placed. Either would mean granting a
///     resident service the right to create Jobs so it could do what the
///     process already standing here can do.
///  2. **"A slot is free" is a CLUSTER fact that no BOSS event
///     announces.** A gate Job that dies without reporting emits
///     nothing, so an event-driven rule would stall the line
///     permanently on exactly the failure the queue exists to survive. A
///     poll re-derives the truth from the cluster every time and is
///     self-healing by construction.
///  3. **A place held by a live process cannot outlive its session.**
///     That is the second half of fd217c65 as a structural property
///     rather than a promise: on 2026-09-08 a builder's session ended
///     and its detached retry loop kept gating, re-gating an
///     already-green branch into a twin car. Here the loop IS the
///     caller; when the caller dies the heartbeat stops, the place
///     expires after [`QUEUE_PLACE_TTL_SECS`], and the line behind it
///     moves.
///
/// The order is read from the system of record on every pass, so five
/// waiters agree on who is next without any of them coordinating — the
/// thing five hand-rolled retry loops could not do.
async fn wait_for_slot(
    http: &reqwest::Client,
    packet: &str,
    branch: &str,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Slot> {
    let started = std::time::Instant::now();
    let mut absent_since: Option<std::time::Instant> = None;
    let mut reported: Option<usize> = None;
    loop {
        tokio::time::sleep(QUEUE_POLL).await;
        // `now` is minted once at the CLI boundary (the no-wallclock
        // lint's rule); elapsed monotonic time carries it forward.
        let at = now
            + chrono::Duration::from_std(started.elapsed())
                .unwrap_or_else(|_| chrono::Duration::zero());
        // The heartbeat is what keeps the place. BEST-EFFORT: the TTL is
        // ten polls wide, so a blip costs nothing, and failing hard here
        // would drop a place that is genuinely still held.
        let _ = api(
            http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(json!({ QUEUE_HEARTBEAT_AT: stamp(at) })),
        )
        .await;
        // A MOMENTARY ABSENCE IS NOT A FAILURE — same tolerance as
        // `wait_for_verdict`, and for the same reason: every train
        // deploy rolls the SoR for tens of seconds, and a waiter that
        // died there would send its builder back to a retry loop.
        let open = match api(
            http,
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&status=open&limit=100",
            None,
        )
        .await
        {
            Ok(v) => {
                absent_since = None;
                rows(v)
            }
            Err(e) if is_transient(&format!("{e:#}")) => {
                let since = *absent_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() > ABSENCE_TOLERANCE {
                    bail!(
                        "the system of record has been unreachable for over {}s, which is \
                         longer than a deploy takes — giving up on the QUEUE, not on the \
                         place.\n  Gate-run {packet} still holds it until the heartbeat \
                         ages out ({QUEUE_PLACE_TTL_SECS}s); re-running `boss gate {branch} \
                         --wait` reuses that packet rather than filing a second.\n  Last \
                         error: {e:#}",
                        ABSENCE_TOLERANCE.as_secs()
                    );
                }
                eprintln!(
                    "boss gate: system of record unreachable ({}s) — a deploy rolls it \
                     briefly; still holding the place",
                    since.elapsed().as_secs()
                );
                continue;
            }
            Err(e) => return Err(e),
        };
        // Did another actor reuse this packet and launch it meanwhile?
        if let Some(name) = live_gate_for_packet(namespace, packet)? {
            return Ok(Slot::Gating(name));
        }
        let order = queue_order(&open, at, QUEUE_PLACE_TTL_SECS);
        let live = running_gates(namespace)?;
        match order.iter().position(|id| id == packet) {
            Some(pos) if may_launch(live.len(), max, pos) => {
                println!(
                    "boss gate: a slot freed after {}m — launching {branch} (packet {})",
                    started.elapsed().as_secs() / 60,
                    &packet[..8.min(packet.len())]
                );
                return Ok(Slot::Free);
            }
            Some(pos) => {
                // Only when the place MOVES, so an hour in line is a
                // handful of lines rather than a hundred.
                if reported != Some(pos) {
                    println!("{}", queued_line(packet, branch, pos, order.len(), max));
                    reported = Some(pos);
                }
            }
            None => {
                // Our place is gone: the marker was cleared, or the
                // record was unreachable past the TTL. Retake it at the
                // BACK rather than launch out of turn — jumping the line
                // is how the hand-rolled loops collided.
                eprintln!(
                    "boss gate: this packet's place in line is gone (marker cleared, or the \
                     record was unreachable past {QUEUE_PLACE_TTL_SECS}s) — retaking it at \
                     the back rather than launching out of turn"
                );
                api(
                    http,
                    reqwest::Method::PATCH,
                    &format!("/api/jobs/{packet}/metadata"),
                    Some(queue_place_patch(at)),
                )
                .await?;
                reported = None;
            }
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
            ..Default::default()
        }
    }

    fn landed_car() -> Value {
        json!({
            "id": "670087f4-0000-0000-0000-000000000000",
            "status": "closed",
            "metadata": {
                "branch": "fix/boot", "merged": "true", "merge_ref": "b641f3adcf47",
                "outcome": "merged", "train": "3a476b50-7a3a-409f-a9bc-59266e44f331",
                "boarded_head": "a56b4a9deadbeef"
            }
        })
    }

    fn landed_by_content() -> Landing {
        landing(
            &crate::merged::Verdict::Merged(crate::merged::How::ContentPresent),
            &[landed_car()],
            "fix/boot",
            "a56b4a9deadbeef",
        )
        .expect("content on main is a landing")
    }

    /// THE FRONT DOOR (610537b2). A landed branch is refused, the
    /// refusal names where it went and what to do instead.
    #[test]
    fn a_landed_branch_is_refused_and_told_to_delete_itself() {
        let l = landed_by_content();
        match landed_guard(
            "fix/boot",
            "a56b4a9deadbeef",
            Some(&l),
            None,
            &ParkIntent::default(),
        ) {
            LandedGuard::Refuse(why) => {
                assert!(why.contains("fix/boot@a56b4a9"), "{why}");
                assert!(why.contains("b641f3adcf47"), "names the merge: {why}");
                assert!(why.contains("train 3a476b50"), "names the train: {why}");
                assert!(why.contains("--delete fix/boot"), "says to delete: {why}");
                assert!(why.contains("--force-regate"), "names the door: {why}");
            }
            other => panic!("a landed branch must be refused: {other:?}"),
        }
    }

    /// The re-gate that caused the incident had a purpose (closing a dead
    /// packet); a reasoned force keeps that door, without park intent.
    #[test]
    fn a_forced_regate_proceeds_with_its_reason_but_never_with_park_intent() {
        let l = landed_by_content();
        match landed_guard(
            "fix/boot",
            "a56b4a9",
            Some(&l),
            Some("closing dead gate-run 57f49a80"),
            &ParkIntent::default(),
        ) {
            LandedGuard::Forced(note) => assert!(note.contains("57f49a80"), "{note}"),
            other => panic!("a reasoned force gates: {other:?}"),
        }
        match landed_guard("fix/boot", "a56b4a9", Some(&l), Some("why"), &park_full()) {
            LandedGuard::Refuse(why) => assert!(why.contains("twin"), "{why}"),
            other => panic!("park intent on a landed branch is a twin: {other:?}"),
        }
    }

    const TWIN_BRANCH: &str = "feat/a-human-only-step-refuses-an-agent";
    const TWIN_HEAD: &str = "cd0c4f7bdadf2306a589c922f240a7ff963f70a7";

    /// Car d08a6418 as it stood when the duplicate gate launched at
    /// 20:31:20 on 2026-09-08: green at TWIN_HEAD, aboard train #274.
    fn carried(train: Option<&str>) -> Value {
        let receipt = format!(
            "{{\"verdict\": \"green\", \"head\": \"{TWIN_HEAD}\", \"mode\": \"full\", \"fails\": []}}"
        );
        json!({
            "id": "d08a6418-e7af-484b-82bf-ab043229bfd6",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": TWIN_BRANCH, "train": train },
            "steps": [
                {"spec_slug": "gate", "title": boss_jobs::car::GATE, "status": "completed",
                 "metadata": {"receipt": receipt}},
                {"spec_slug": "review", "title": boss_jobs::car::REVIEW, "status": "ready"},
            ]
        })
    }

    /// THE SECOND FRONT DOOR (02165b1d). A head that is already green
    /// and already carried is not gated again: the refusal names the
    /// car, where it is, and the door out.
    #[test]
    fn a_head_already_green_and_already_carried_is_refused() {
        let aboard = [carried(Some("d72ecdb9-c5a1-4d16-8a00-d934fd565002"))];
        match gated_car_guard(TWIN_BRANCH, TWIN_HEAD, &aboard, None) {
            GatedGuard::Refuse(why) => {
                assert!(why.contains("car d08a6418"), "names the car: {why}");
                assert!(why.contains("aboard train d72ecdb9"), "says where: {why}");
                assert!(why.contains("nothing useful"), "{why}");
                assert!(why.contains("--force-regate"), "names the door: {why}");
            }
            other => panic!("a carried green head must be refused: {other:?}"),
        }
        // Parked rather than boarded: same refusal, different location.
        let docked = [carried(None)];
        match gated_car_guard(TWIN_BRANCH, TWIN_HEAD, &docked, None) {
            GatedGuard::Refuse(why) => assert!(why.contains("parked at the dock"), "{why}"),
            other => panic!("a parked car's green head is refused too: {other:?}"),
        }
    }

    /// `--force-regate` keeps working: a reason gates the same head
    /// anyway (unlike the landed guard, park intent is allowed — a
    /// parked car has somewhere for the fresh receipt to go).
    #[test]
    fn a_forced_regate_of_a_carried_head_still_gates() {
        let docked = [carried(None)];
        match gated_car_guard(
            TWIN_BRANCH,
            TWIN_HEAD,
            &docked,
            Some("re-running a flaked check"),
        ) {
            GatedGuard::Forced(note) => {
                assert!(note.contains("re-running a flaked check"), "{note}");
                assert!(note.contains("d08a6418"), "{note}");
            }
            other => panic!("a reasoned force gates: {other:?}"),
        }
    }

    /// A real re-gate proceeds. A moved head, no car, or a receipt that
    /// is not green are all normal — the guard fires only on the exact
    /// "nothing to gain" shape.
    #[test]
    fn a_moved_head_or_an_uncarried_branch_proceeds() {
        let aboard = [carried(Some("d72ecdb9"))];
        assert_eq!(
            gated_car_guard(
                TWIN_BRANCH,
                "9999999999999999999999999999999999999999",
                &aboard,
                None
            ),
            GatedGuard::Proceed,
            "a pushed head is a real re-gate"
        );
        assert_eq!(
            gated_car_guard(TWIN_BRANCH, TWIN_HEAD, &[], None),
            GatedGuard::Proceed,
            "no car, nothing to twin"
        );
        assert_eq!(
            gated_car_guard("feat/other", TWIN_HEAD, &aboard, None),
            GatedGuard::Proceed,
            "another branch's car does not answer"
        );
        // A spent car (past review, no train) is history, not a carrier.
        let mut spent = carried(None);
        spent["steps"][1]["status"] = json!("completed");
        assert_eq!(
            gated_car_guard(TWIN_BRANCH, TWIN_HEAD, &[spent], None),
            GatedGuard::Proceed
        );
        // A red receipt on a car is not a green to repeat.
        let mut red = carried(None);
        red["steps"][0]["metadata"]["receipt"] = json!(format!(
            "{{\"verdict\": \"failed\", \"head\": \"{TWIN_HEAD}\", \"mode\": \"full\"}}"
        ));
        assert_eq!(
            gated_car_guard(TWIN_BRANCH, TWIN_HEAD, &[red], None),
            GatedGuard::Proceed
        );
    }

    /// The FRESH receipt decides. A re-gated car carries its current
    /// receipt in `metadata.regate_receipt` while the frozen gate step
    /// still names the old head — `boss receipt`'s rule, reused here, so
    /// the guard cannot disagree with the verb operators read.
    #[test]
    fn the_regate_receipt_is_the_one_that_counts() {
        let mut rerailed = carried(None);
        rerailed["metadata"]["regate_receipt"] = json!(
            "{\"verdict\": \"green\", \"head\": \"75e4d3c59387aaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \
             \"mode\": \"full\", \"fails\": []}"
        );
        // The head the car actually stands on now: refused.
        let cars = [rerailed];
        assert!(matches!(
            gated_car_guard(
                TWIN_BRANCH,
                "75e4d3c59387aaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &cars,
                None
            ),
            GatedGuard::Refuse(_)
        ));
        // The stale gate step's head: that tree is gone, so gating it is
        // not a repeat of anything.
        assert_eq!(
            gated_car_guard(TWIN_BRANCH, TWIN_HEAD, &cars, None),
            GatedGuard::Proceed
        );
    }

    #[test]
    fn an_unlanded_branch_proceeds_even_when_forced() {
        assert_eq!(
            landed_guard("feat/new", "abc1234", None, Some("why"), &park_full()),
            LandedGuard::Proceed
        );
    }

    /// Git decides; the car names. A car that boarded THIS head answers
    /// when git cannot; a car that boarded another head is a reused
    /// branch with new, gateable commits.
    #[test]
    fn landing_reads_git_first_and_the_car_for_the_name() {
        use crate::merged::{How, Verdict};
        let l = landed_by_content();
        assert_eq!(
            l.landed_as.as_deref(),
            Some("car 670087f4 merged as b641f3adcf47 on train 3a476b50")
        );
        assert!(l.how.contains("every file"), "{}", l.how);

        // Ancestry, with no car in the SoR: still a landing, unnamed.
        let anc = landing(&Verdict::Merged(How::Ancestor), &[], "fix/boot", "a56b4a9").unwrap();
        assert_eq!(anc.landed_as, None);

        // Git could not see (stale clone) but the car boarded this head.
        let unknown = Verdict::Unknown("stale".into());
        let by_car = landing(&unknown, &[landed_car()], "fix/boot", "a56b4a9deadbeef").unwrap();
        assert!(by_car.how.contains("exact head"), "{}", by_car.how);

        // New commits on a landed branch's name: not a landing.
        assert!(landing(&Verdict::NotMerged, &[landed_car()], "fix/boot", "0000000").is_none());
        assert!(landing(&unknown, &[], "fix/boot", "a56b4a9").is_none());
    }

    #[test]
    fn clearing_stale_intent_nulls_every_park_key() {
        let p = ParkIntent::clear_patch();
        let stamped = park_full().metadata_patch();
        for k in stamped.as_object().unwrap().keys() {
            assert!(p[k].is_null(), "{k} must be nulled so the door deletes it");
        }
        // Every key a FULL intent can stamp — the four, the backlog
        // edge, and the three proof keys — must be cleared, or a
        // re-gate of a landed branch inherits a stale probe.
        let mut everything = park_full();
        everything.backlog_item = Some("7c9e376d".into());
        everything.probe = Some("true".into());
        everything.expect = Some("x".into());
        for k in everything.metadata_patch().as_object().unwrap().keys() {
            assert!(p[k].is_null(), "{k} must be nulled so the door deletes it");
        }
        assert!(p[PARK_PROOF_EVENT].is_null());
        assert_eq!(p.as_object().unwrap().len(), 8);
    }

    /// THE CAR CARRIES ITS PROBE (28ac45ab). A complete intent may add
    /// a probe + expect pair; both ride the gate-run under `park_*`
    /// keys so the auto-park handler copies them onto the car.
    ///
    /// The probe here is a `curl` against the system of record because
    /// that is a probe the forge host can RUN — this case used to be
    /// written with `kubectl`, which is the shape f9304366 measured
    /// unrunnable and the next case now refuses.
    #[test]
    fn a_probe_and_its_expectation_ride_the_park_intent_together() {
        let probe = "curl -fsS $BOSS_JOBS_URL/api/jobs/x | grep -q PARK_PROBE_OK";
        let mut p = park_full();
        p.probe = Some(probe.into());
        p.expect = Some("PARK_PROBE_OK".into());
        assert!(p.require_complete().is_ok());
        let m = p.metadata_patch();
        assert_eq!(m[PARK_PROBE], probe);
        assert_eq!(m[PARK_EXPECT], "PARK_PROBE_OK");
        assert!(m.get(PARK_PROOF_EVENT).is_none());
    }

    /// WHERE A PROBE RUNS (f9304366). A recorded probe runs on the
    /// forge host, not on the pod that wrote it. The measured shape —
    /// both cars of the auto-proof loop's first live run — is refused
    /// at gate time, on the builder's terminal, naming the tool, the
    /// host, and the two ways out.
    #[test]
    fn a_probe_needing_a_tool_the_forge_lacks_is_refused_at_gate_time() {
        let mut p = park_full();
        p.probe = Some(
            "kubectl -n boss-dev exec deploy/boss-conductor -- /usr/local/bin/boss gate --help \
             2>&1 | grep -q x && echo OK"
                .into(),
        );
        p.expect = Some("OK".into());
        let e = p.require_complete().unwrap_err().to_string();
        assert!(e.contains("kubectl"), "{e}");
        assert!(e.contains("FORGE HOST"), "{e}");
        assert!(e.contains("--park-proof-event"), "{e}");
        assert!(e.contains("host-absent-tools.txt"), "{e}");
    }

    /// The check reads COMMAND POSITION, not the whole line: a probe
    /// that merely greps for the word runs fine on the forge and is not
    /// refused. A refusal a builder cannot act on is worse than the
    /// arrival failure it replaces.
    #[test]
    fn a_probe_that_only_mentions_an_absent_tool_is_allowed() {
        let mut p = park_full();
        p.probe = Some("curl -fsS $BOSS_JOBS_URL/api/estate/nodes | grep -c kubectl".into());
        p.expect = Some("1".into());
        assert!(p.require_complete().is_ok(), "{:?}", p.require_complete());
    }

    /// An absent tool is caught wherever it sits in the pipeline, and
    /// through a path or an `env` prefix — the ways the same mistake
    /// gets written.
    #[test]
    fn the_scan_finds_a_command_anywhere_a_shell_would_run_one() {
        for probe in [
            "curl -fsS $BOSS_JOBS_URL/api/yard | kubectl apply -f -",
            "test -f x && /usr/local/bin/kubectl get pods",
            "env KUBECONFIG=/x kubectl get cm boss-conductor-env",
            "echo x $(kubectl get pods)",
        ] {
            assert_eq!(
                probe_needs_absent_tool(probe),
                Some("kubectl"),
                "not caught: {probe}"
            );
        }
        for probe in [
            "curl -fsS $BOSS_JOBS_URL/api/jobs | grep -q TOKEN",
            "git -C /home/david/boss log -1 --format=%H | grep -q abc",
            "journalctl -u boss-forge-converge -n 50 | grep -q CONVERGED",
        ] {
            assert_eq!(
                probe_needs_absent_tool(probe),
                None,
                "false refusal: {probe}"
            );
        }
    }

    /// The manifest is DATA, and the measured absence is in it. A list
    /// that lost its one measured entry would make the check silently
    /// green (CLAUDE.md §Diagnosis: a check nobody reads is a check
    /// that is not running).
    #[test]
    fn the_forge_absence_manifest_carries_the_measured_tool() {
        let tools = forge_absent_tools();
        assert!(
            tools.contains(&"kubectl"),
            "host-absent-tools.txt lost the tool f9304366 measured: {tools:?}"
        );
        for t in &tools {
            assert!(
                !t.contains('#') && !t.contains(' ') && !t.contains('/'),
                "a manifest line must be a bare tool name, got {t:?}"
            );
        }
    }

    /// A probe with no expectation is `echo hi`; an expectation with no
    /// probe is a string from nowhere. Both refused, each naming the
    /// missing half.
    #[test]
    fn half_a_probe_is_refused_naming_the_other_half() {
        let mut p = park_full();
        p.probe = Some("true".into());
        let e = p.require_complete().unwrap_err().to_string();
        assert!(e.contains("--park-expect"), "{e}");
        assert!(e.contains("echo hi"), "{e}");

        let mut p = park_full();
        p.expect = Some("x".into());
        let e = p.require_complete().unwrap_err().to_string();
        assert!(e.contains("--park-probe"), "{e}");
    }

    /// A probe rides only with a full receipt: `--park-probe` alone is
    /// the same partial intent as `--park-summary` alone.
    #[test]
    fn a_probe_without_a_receipt_is_a_partial_intent() {
        let p = ParkIntent {
            probe: Some("true".into()),
            expect: Some("x".into()),
            ..Default::default()
        };
        assert!(!p.is_empty());
        let e = p.require_complete().unwrap_err().to_string();
        assert!(e.contains("--park-summary"), "{e}");
    }

    /// An event-bound car records the event instead of a probe; the two
    /// together mean the builder did not decide.
    #[test]
    fn an_event_bound_car_records_the_event_and_never_also_a_probe() {
        let mut p = park_full();
        p.proof_event = Some("event-bound — the next yard-button cancel".into());
        assert!(p.require_complete().is_ok());
        assert_eq!(
            p.metadata_patch()[PARK_PROOF_EVENT],
            "event-bound — the next yard-button cancel"
        );

        p.probe = Some("true".into());
        p.expect = Some("x".into());
        let e = p.require_complete().unwrap_err().to_string();
        assert!(e.contains("Pick one"), "{e}");
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

    /// `--hold` marks a green as deliberately waiting. It needs a reason
    /// (the marker is the reason), and never combines with park intent
    /// (a hold keeps the green off the dock; auto-park boards it).
    #[test]
    fn a_hold_needs_a_reason_and_never_combines_with_park_intent() {
        let plain = ParkIntent::default();
        assert_eq!(hold_guard(None, &plain).unwrap(), None);
        assert_eq!(
            hold_guard(Some("  lands at the next dev-pod restart "), &plain).unwrap(),
            Some("lands at the next dev-pod restart".to_string())
        );
        let err = hold_guard(Some("   "), &plain).unwrap_err().to_string();
        assert!(err.contains("needs a reason"), "{err}");
        let err = hold_guard(Some("waiting"), &park_full())
            .unwrap_err()
            .to_string();
        assert!(err.contains("--hold and --park-* cannot combine"), "{err}");
        // A partial park intent is still park intent.
        let partial = ParkIntent {
            summary: Some("x".into()),
            ..Default::default()
        };
        assert!(hold_guard(Some("waiting"), &partial).is_err());
    }

    /// A TRANSIENT SoR BLIP ON THE PARK-INTENT PATCH MUST NOT ORPHAN
    /// THE PACKET. The park intent is a SECOND round-trip after the
    /// gate-run packet was already filed, and the SoR rolls for tens of
    /// seconds on every train deploy. If that PATCH blips, the packet we
    /// just created has no runner Job and would sit open forever — so
    /// `run` must close it, exactly as the three post-creation guards
    /// (`running_gates`, the PVC guard, `crowd_refusal`) do. That
    /// close-or-keep decision is `!reused && !dry`: close only a packet
    /// WE created this run, and never a dry run (which filed nothing).
    #[test]
    fn a_park_blip_closes_the_packet_we_created_but_not_a_reused_or_dry_one() {
        // We created the packet this run, real run → close the orphan.
        assert!(should_close_on_park_failure(false, false));
        // Reused packet: it has a life of its own; don't close it.
        assert!(!should_close_on_park_failure(true, false));
        // Dry run: nothing was ever filed to close.
        assert!(!should_close_on_park_failure(false, true));
        assert!(!should_close_on_park_failure(true, true));
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

    // ---------------------------------------------------------------
    // THE QUEUE (backlog fd217c65). Measured 2026-09-08: 24 green
    // gates, 6 red — and 21 launches REFUSED at the bound, each of
    // which filed a gate-run packet and closed it `lost` ("environment
    // died"). Nothing died. Five builders then hand-rolled five
    // five-minute retry loops; two collided on a shared script and one
    // outlived the session that made it and re-gated an already-green
    // branch. Both halves of the fix are pinned below.
    // ---------------------------------------------------------------

    /// A manifest whose /gate-target is a per-run emptyDir — the shipped
    /// shape — with the placeholders still in place.
    fn parallel_manifest() -> String {
        [
            "apiVersion: v1",
            "kind: PersistentVolumeClaim",
            "metadata: {name: gate-seed}",
            "---",
            "apiVersion: batch/v1",
            "kind: Job",
            "metadata: {generateName: gate-$GATE_NAME_HINT-}",
            "spec:",
            "  template:",
            "    spec:",
            "      containers:",
            "        - name: gate",
            "          args: [$GATE_BRANCH, $GATE_RUN_JOB_ID, $GATE_MODE]",
            "          volumeMounts:",
            "            - {name: workspace, mountPath: /gate-target}",
            "            - {name: seed, mountPath: /gate-seed}",
            "      volumes:",
            "        - name: workspace",
            "          emptyDir: {}",
            "        - name: seed",
            "          persistentVolumeClaim: {claimName: gate-seed}",
        ]
        .join("\n")
    }

    /// PART 1, THE STRUCTURAL HALF: every refusal is a value computed
    /// from observations taken BEFORE a packet is filed. There is no
    /// `Admission` arm that means "file a packet, then close it" — which
    /// is what produced 21 phantom `lost` gate-runs in one day.
    #[test]
    fn a_free_slot_admits_a_launch() {
        assert!(matches!(
            admission(
                &["gate-a".to_string()],
                3,
                false,
                "infra/gate-runner/gate-runner.yaml",
                true,
                0,
                QUEUE_CAP
            ),
            Admission::Launch
        ));
    }

    /// PART 2: THE BOUND QUEUES. A caller that will wait takes a place
    /// in line instead of being turned away to write its own retry loop.
    #[test]
    fn the_bound_queues_a_waiting_caller_instead_of_refusing() {
        let live = vec!["gate-a".to_string(), "gate-b".into(), "gate-c".into()];
        assert!(
            matches!(
                admission(&live, 3, false, "m.yaml", true, 0, QUEUE_CAP),
                Admission::Queue
            ),
            "at the bound with --wait the gate takes a place in line"
        );
    }

    /// WITHOUT `--wait` NOTHING WOULD EVER LAUNCH THE QUEUED RUN, so
    /// queueing it would file a packet that sits open forever — the
    /// orphan in different clothes. It refuses, files nothing, and names
    /// the flag that queues.
    #[test]
    fn the_bound_refuses_a_caller_that_cannot_hold_its_place() {
        let live = vec!["gate-a".to_string(), "gate-b".into(), "gate-c".into()];
        let Admission::Refuse(why) = admission(&live, 3, false, "m.yaml", false, 0, QUEUE_CAP)
        else {
            panic!("without --wait there is no process to launch the queued run")
        };
        assert!(
            why.contains("--wait"),
            "the refusal must name the flag that queues: {why}"
        );
        assert!(why.contains("gate-a"), "and still name the gates: {why}");
    }

    /// THE CAP. A queue that grows without bound is a promise the node
    /// cannot keep: at the cap the last place waits four gates, past an
    /// hour. Full refuses — files nothing — and says how long the line
    /// already is.
    #[test]
    fn a_full_queue_refuses_rather_than_growing_without_bound() {
        let live = vec!["gate-a".to_string(), "gate-b".into(), "gate-c".into()];
        let Admission::Refuse(why) =
            admission(&live, 3, false, "m.yaml", true, QUEUE_CAP, QUEUE_CAP)
        else {
            panic!("a full queue refuses")
        };
        assert!(why.contains(&QUEUE_CAP.to_string()), "name the cap: {why}");
        assert!(
            why.contains("min"),
            "a full queue must say how long the wait already is: {why}"
        );
        // One below the cap still queues.
        assert!(matches!(
            admission(&live, 3, false, "m.yaml", true, QUEUE_CAP - 1, QUEUE_CAP),
            Admission::Queue
        ));
    }

    /// THE LEGACY SHARED WORKSPACE NEVER QUEUES. Queueing it would only
    /// postpone the receipt-crossing of 2026-08-24 to when the place
    /// comes due; the old law is absolute, so it refuses outright.
    #[test]
    fn a_shared_workspace_refuses_and_is_never_queued() {
        let Admission::Refuse(why) = admission(
            &["gate-a".to_string()],
            3,
            true,
            "old-runner.yaml",
            true,
            0,
            QUEUE_CAP,
        ) else {
            panic!("a shared workspace beside a live gate refuses")
        };
        assert!(why.contains("old-runner.yaml"), "{why}");
        assert!(why.contains("2026-08-24"), "{why}");
        // Alone, the legacy shape still gates.
        assert!(matches!(
            admission(&[], 3, true, "old-runner.yaml", true, 0, QUEUE_CAP),
            Admission::Launch
        ));
    }

    fn queued_run(id: &str, at: &str, heartbeat: Option<&str>) -> Value {
        let mut md = json!({ "branch": "feat/x", QUEUED_AT: at });
        if let Some(h) = heartbeat {
            md[QUEUE_HEARTBEAT_AT] = json!(h);
        }
        json!({ "id": id, "metadata": md })
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .expect("test stamp")
            .with_timezone(&chrono::Utc)
    }

    /// THE ORDER IS THE SYSTEM OF RECORD'S, NOT EACH BUILDER'S. Oldest
    /// place first, and a run that is actually gating (no `queued_at`)
    /// is not in the line at all.
    #[test]
    fn the_queue_is_oldest_first_and_holds_only_waiting_runs() {
        let now = at("2026-09-08T20:00:00Z");
        let open = vec![
            queued_run("later", "2026-09-08T19:59:00Z", None),
            json!({ "id": "gating", "metadata": { "branch": "feat/y" } }),
            queued_run("earlier", "2026-09-08T19:58:00Z", None),
        ];
        assert_eq!(
            queue_order(&open, now, QUEUE_PLACE_TTL_SECS),
            vec!["earlier".to_string(), "later".to_string()]
        );
    }

    /// A PLACE HELD BY A DEAD SESSION EXPIRES. This is the orphan the
    /// detached retry loops made: a builder's model ran out of credits
    /// and its loop kept gating. A place in line is held by a LIVE
    /// process — it heartbeats, and when the heartbeat stops the place
    /// is skipped so the queue behind it moves.
    #[test]
    fn a_place_whose_holder_stopped_heartbeating_is_skipped() {
        let now = at("2026-09-08T20:00:00Z");
        let open = vec![
            queued_run("dead", "2026-09-08T18:00:00Z", Some("2026-09-08T18:02:00Z")),
            queued_run(
                "alive",
                "2026-09-08T19:50:00Z",
                Some("2026-09-08T19:59:40Z"),
            ),
        ];
        assert_eq!(
            queue_order(&open, now, QUEUE_PLACE_TTL_SECS),
            vec!["alive".to_string()],
            "the dead place must not hold the head of the queue forever"
        );
    }

    /// A LONG BUT LIVE WAIT KEEPS ITS PLACE. Fourth in line is over an
    /// hour; the heartbeat, not the age of the place, is what keeps it.
    #[test]
    fn a_fresh_heartbeat_keeps_an_hours_long_wait_in_line() {
        let now = at("2026-09-08T21:00:00Z");
        let open = vec![queued_run(
            "patient",
            "2026-09-08T19:00:00Z",
            Some("2026-09-08T20:59:30Z"),
        )];
        assert_eq!(
            queue_order(&open, now, QUEUE_PLACE_TTL_SECS),
            vec!["patient".to_string()]
        );
    }

    /// A stamp that does not parse is no stamp — the place is not
    /// counted rather than guessed into the head of the line.
    #[test]
    fn an_unparseable_place_is_not_in_the_queue() {
        let now = at("2026-09-08T20:00:00Z");
        let open = vec![queued_run("bad", "yesterday", None)];
        assert!(queue_order(&open, now, QUEUE_PLACE_TTL_SECS).is_empty());
    }

    /// A RETURNING BUILDER IS NOT REFUSED BY THEIR OWN PLACE. The
    /// packet a re-run would reuse is already in the line; counted
    /// against the cap it could refuse the one caller whose place is
    /// already held — the shape of the dead-session re-run this whole
    /// mechanism exists to make safe.
    #[test]
    fn a_reused_place_is_not_counted_ahead_of_itself() {
        let order = vec!["a".to_string(), "mine".to_string(), "b".to_string()];
        assert_eq!(places_ahead(&order, Some("mine")), 2);
        assert_eq!(
            places_ahead(&order, None),
            3,
            "a newcomer is behind all three"
        );
        assert_eq!(places_ahead(&order, Some("gone")), 3);
    }

    /// OLDEST FIRST, AND ONLY INTO A SLOT THAT IS ACTUALLY FREE. Second
    /// in line waits for the second free slot, so two waiters released
    /// by one finishing gate do not both launch.
    #[test]
    fn the_head_of_the_queue_launches_into_the_first_free_slot() {
        assert!(!may_launch(3, 3, 0), "no slot free");
        assert!(may_launch(2, 3, 0), "head takes the one free slot");
        assert!(!may_launch(2, 3, 1), "second in line waits its turn");
        assert!(may_launch(1, 3, 1), "two free slots release two places");
        assert!(may_launch(0, 3, 2));
        assert!(!may_launch(0, 3, 3));
    }

    /// The estimate is arithmetic on the measured median (2026-09-08:
    /// median 18.2 min over 30 runs), a gate at a time — a number the
    /// waiting builder can decide on, not a promise.
    #[test]
    fn the_estimated_wait_grows_one_gate_per_wave() {
        assert_eq!(estimated_wait_minutes(0, 3), MEDIAN_GATE_MINUTES);
        assert_eq!(estimated_wait_minutes(2, 3), MEDIAN_GATE_MINUTES);
        assert_eq!(estimated_wait_minutes(3, 3), 2 * MEDIAN_GATE_MINUTES);
        assert_eq!(
            estimated_wait_minutes(0, 0),
            MEDIAN_GATE_MINUTES,
            "no divide by zero"
        );
    }

    /// THE LINE THE BUILDER READS NAMES THE PACKET IT IS WAITING ON —
    /// the same discipline as `verdict_line`: two waiters share a
    /// console, so a line whose owner has to be guessed is no line.
    #[test]
    fn the_queued_line_names_its_packet_its_branch_and_its_place() {
        let l = queued_line("f451af16-0000-0000-0000-000000000000", "feat/x", 1, 4, 3);
        assert!(l.contains("f451af16"), "{l}");
        assert!(l.contains("feat/x"), "{l}");
        assert!(
            l.contains("2 of 4"),
            "one-based place, of the whole line: {l}"
        );
        assert!(l.contains("min"), "and what the wait costs: {l}");
    }

    /// The workspace-shape guard runs BEFORE a packet exists, which
    /// means it reads the manifest rather than the rendered Job. Same
    /// answer either way — substitution never touches a mount.
    #[test]
    fn the_workspace_shape_reads_the_same_before_and_after_substitution() {
        let m = parallel_manifest();
        let doc = job_document(&m).expect("the manifest has a Job");
        let rendered = render_job(&m, "feat/x", "packet-1", "--auto").expect("renders");
        assert_eq!(pvc_backed_workspace(&doc), pvc_backed_workspace(&rendered));
        assert!(
            !pvc_backed_workspace(&doc),
            "the shipped shape is an emptyDir"
        );
        assert!(!doc.contains("kind: PersistentVolumeClaim"));
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

    /// EVERY read verb resolves its instance through this one function,
    /// so the `127.0.0.1` trap cannot re-grow in any single verb. Pin
    /// the precedence (flag over env) and the refusal on the pure form,
    /// so no test has to mutate process env (unsafe under edition 2024,
    /// racy in parallel). `packet census` and `queue` mirror this from
    /// their own modules to document that they wire it in.
    #[test]
    fn resolve_jobs_base_prefers_flag_then_env_then_refuses() {
        assert_eq!(
            resolve_jobs_base_from(Some("http://flag:7900"), Some("http://env:7900".into()))
                .unwrap(),
            "http://flag:7900",
            "an explicit flag beats the env"
        );
        assert_eq!(
            resolve_jobs_base_from(None, Some("http://env:7900".into())).unwrap(),
            "http://env:7900",
            "the env is used when there is no flag"
        );
        for (flag, env) in [
            (None, None),
            (Some("   "), None),
            (None, Some(String::new())),
            (Some(""), Some("\t".to_string())),
        ] {
            let m = resolve_jobs_base_from(flag, env)
                .expect_err("neither a real flag nor a real env must refuse")
                .to_string();
            assert!(
                m.contains("10.20.0.34:7900"),
                "refusal must name the record: {m}"
            );
            assert!(
                m.contains("127.0.0.1:7900"),
                "refusal must warn of the trap: {m}"
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
        let b = gate_run_body(
            "feat/x",
            "abc123",
            "infra/gate-runner/gate-runner.yaml",
            None,
        );
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
        let b = gate_run_body(
            "feat/x",
            "abc123",
            "infra/gate-runner/gate-runner.yaml",
            None,
        );
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
        let mut b = gate_run_body(
            "feat/x",
            "abc123",
            "infra/gate-runner/gate-runner.yaml",
            None,
        );
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
        let b = gate_run_body("feat/x", "abc123", "infra/gate-runner/local.yaml", None);
        assert_eq!(b["metadata"]["runner"], "infra/gate-runner/local.yaml");
    }

    #[test]
    fn the_gate_run_stamps_the_delivery_channel_when_known() {
        // None (branch had no forge diff to classify) leaves it unstamped.
        let b = gate_run_body("feat/x", "abc123", "infra/gate-runner/local.yaml", None);
        assert!(b["metadata"].get("delivery_channel").is_none());
        // A known channel rides on the gate-run so the car and the
        // channel-gated delivery can read it without re-deriving.
        let d = gate_run_body(
            "feat/x",
            "abc123",
            "infra/gate-runner/local.yaml",
            Some("data"),
        );
        assert_eq!(d["metadata"]["delivery_channel"], "data");
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
    /// TWO GATES, ONE CONSOLE (backlog 9dd9993b). On 2026-09-08 a
    /// builder's `boss gate <branch> --wait` console showed `boss gate:
    /// failed` for a NEIGHBOURING gate that red-lit at the same moment
    /// (three ran in parallel), then its own `green`. The poller was
    /// pinned to its packet all along — the bare verdict line was what
    /// could not be told apart. So: the verdict is read off the packet
    /// this poller filed, a body for any other packet is refused by
    /// name, and every verdict line carries the packet and branch.
    #[test]
    fn the_poller_reads_its_own_packet_while_a_neighbour_fails_first() {
        const MINE: &str = "f451af16-0000-4000-8000-000000000001";
        const THEIRS: &str = "6de49582-0000-4000-8000-000000000002";
        let mine_silent = json!({
            "id": MINE,
            "metadata": {"branch": "fix/an-enum-field-refuses-a-value-outside-its-set"},
            "steps": [
                {"spec_slug": "gate", "status": "active"},
                {"spec_slug": "record-verdict", "status": "ready", "metadata": {}},
            ]
        });
        let theirs_failed = json!({
            "id": THEIRS,
            "metadata": {"branch": "fix/incident-post-mortem-v2"},
            "steps": [
                {"spec_slug": "record-verdict", "status": "completed",
                 "metadata": {"verdict": "failed"}},
            ]
        });
        // The neighbour red-lights first. Our packet is still silent, and
        // silence is what the poller must read — never the newest verdict
        // in the namespace.
        assert_eq!(
            own_verdict(MINE, &mine_silent).expect("our own body"),
            None,
            "a silent packet stays silent whatever a neighbour recorded"
        );
        let refused = own_verdict(MINE, &theirs_failed)
            .expect_err("a body for another packet must be refused, not read");
        let msg = format!("{refused:#}");
        assert!(
            msg.contains("f451af16") && msg.contains("6de49582"),
            "the refusal names both packets: {msg}"
        );
        // Then ours reports.
        let mine_green = json!({
            "id": MINE,
            "metadata": {"branch": "fix/an-enum-field-refuses-a-value-outside-its-set"},
            "steps": [
                {"spec_slug": "record-verdict", "status": "completed",
                 "metadata": {"verdict": "green"}},
            ]
        });
        assert_eq!(
            own_verdict(MINE, &mine_green)
                .expect("our own body")
                .as_deref(),
            Some("green")
        );
        let line = verdict_line("green", MINE, &mine_green);
        assert!(
            line.starts_with("boss gate: green"),
            "the verdict leads the line: {line}"
        );
        assert!(
            line.contains("packet f451af16")
                && line.contains("fix/an-enum-field-refuses-a-value-outside-its-set"),
            "every verdict line names its packet and branch, so two pollers sharing a \
             console cannot be confused for each other: {line}"
        );
        let red = verdict_line("failed", THEIRS, &theirs_failed);
        assert!(
            red.contains("packet 6de49582") && red.contains("fix/incident-post-mortem-v2"),
            "{red}"
        );
    }

    /// A body without an id (an older API shape) is read, not refused:
    /// the pin is against a WRONG id, and absence is not evidence.
    #[test]
    fn a_body_without_an_id_is_still_read() {
        let body = json!({
            "steps": [{"spec_slug": "record-verdict", "metadata": {"verdict": "lost"}}]
        });
        assert_eq!(
            own_verdict("f451af16-0000-4000-8000-000000000001", &body)
                .expect("no id, no refusal")
                .as_deref(),
            Some("lost")
        );
    }

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

#[cfg(test)]
mod signing_tests {
    use super::*;
    use crate::identity::Signature;

    /// A one-shot HTTP stub that hands back the request head it read.
    /// The head is where the answer lives: `x-boss-user` is what the
    /// system of record stamps into `completed_by`.
    async fn one_request(body: &'static str) -> (String, tokio::task::JoinHandle<Option<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.ok()?;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];
            loop {
                match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
            Some(String::from_utf8_lossy(&buf).into_owned())
        });
        (format!("http://{addr}"), handle)
    }

    /// Backlog 5083d6f5, at the wire: the step PUT a `boss prove`
    /// makes must arrive carrying the operator who ran it.
    #[tokio::test]
    async fn a_write_arrives_signed_as_its_caller() {
        let (base, stub) = one_request("{}").await;
        let http = reqwest::Client::new();
        api_at_signed(
            &http,
            &base,
            reqwest::Method::PUT,
            "/api/jobs/x/steps/y",
            Some(json!({"status": "completed"})),
            Signature::As("claude@algedonic.dev".into()),
        )
        .await
        .expect("the stub answers 200");
        let head = stub.await.unwrap().expect("the stub read a request");
        assert!(
            head.contains(r#""id":"claude@algedonic.dev""#),
            "the write must name its caller; head was:\n{head}"
        );
        assert!(
            !head.contains(crate::identity::CONDUCTOR),
            "an operator's write must not be signed as the train automation; head was:\n{head}"
        );
    }

    /// The loud case. A write nobody named does not go out at all —
    /// and in particular does not go out as automation, which is the
    /// whole defect.
    #[tokio::test]
    async fn an_unnamed_write_never_reaches_the_network() {
        let (base, stub) = one_request("{}").await;
        let http = reqwest::Client::new();
        let refusal = crate::identity::refusal("POST", "/api/jobs");
        let err = api_at_signed(
            &http,
            &base,
            reqwest::Method::POST,
            "/api/jobs",
            Some(json!({"kind": "backlog-item"})),
            Signature::Refused(refusal.clone()),
        )
        .await
        .expect_err("an unnamed write is refused");
        assert_eq!(err.to_string(), refusal);
        assert!(
            err.to_string().contains(crate::identity::ACTOR_ENV),
            "{err}"
        );
        // The stub finishes only once it has ACCEPTED a connection, so
        // an unfinished stub is proof nothing was sent.
        assert!(
            !stub.is_finished(),
            "a refused write must not reach the socket"
        );
        stub.abort();
    }

    /// A read attributes nothing, so it proceeds — but marked, never
    /// as an automation slug the server would read as a process.
    #[tokio::test]
    async fn an_unnamed_read_arrives_marked() {
        let (base, stub) = one_request(r#"{"data":[]}"#).await;
        let http = reqwest::Client::new();
        api_at_signed(
            &http,
            &base,
            reqwest::Method::GET,
            "/api/jobs",
            None,
            Signature::Unidentified,
        )
        .await
        .expect("a read still works");
        let head = stub.await.unwrap().expect("the stub read a request");
        assert!(
            head.contains(crate::identity::UNIDENTIFIED),
            "an unnamed read must say so; head was:\n{head}"
        );
        assert!(!head.contains(crate::identity::CONDUCTOR), "{head}");
    }
}
