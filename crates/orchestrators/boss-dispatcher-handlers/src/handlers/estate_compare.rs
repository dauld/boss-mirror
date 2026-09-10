//! `estate.compare` — declared vs observed; the difference is the finding.
//!
//! THE OTHER HALF OF THE ESTATE SPLIT (59ef456a). The `nodes` registry
//! says what machines we MEANT to have; `jobs.estate.observed` records
//! what a look at the cluster FOUND. Neither writes the other — if the
//! observer updated the registry, the cluster would become the source
//! of truth for its own declaration and nothing could ever be found
//! MISSING, only silently added. This handler is the comparison the
//! split exists for: it reads both sides over HTTP and records what
//! disagrees.
//!
//! EVENTED, NOT ON CADENCE — a deliberate divergence from the packet's
//! "handler on cadence" sketch, and strictly simpler: the rule fires on
//! `jobs.estate.observed`, so the observation arrives IN
//! `ctx.event_payload` and there is no `/api/events/tail` read, no
//! two-surface handler, and no second clock. The comparison inherits
//! the observation's own daily cadence; if the observer never fires,
//! the missing comparison datapoint is the signal, same as the census.
//!
//! SCOPE IS LOAD-BEARING. The observer's scope is `kubernetes-nodes`:
//! it can only see machines that joined the cluster. The registry also
//! declares machines the observer can NEVER see — the forge host and
//! boss-gcp — and counting those as "missing" would cry wolf on two
//! rows every single run. A declared row participates in this
//! comparison iff its role names a cluster node (`talos-*`) and it is
//! not retired. An observation carrying any OTHER scope is recorded
//! with its findings marked unknown-scope rather than guessed at.
//!
//! REPORT FIRST, RAISE LATER (the census's Q2 posture, unchanged): one
//! POST to `/api/estate/comparison` per observation, recording counts
//! and findings as a measured series. No packet is opened here — the
//! base rate is unknown, and a noisy raiser trains people to ignore
//! it. The raiser comes later, calibrated against this series.
//!
//! HONEST LIMITS:
//! - **Units are compared, never converted.** Both sides state memory
//!   and disk in GiB rounded to nearest — the one rule, stated once
//!   (migration 202608301905). A 1 GiB disagreement here is a real
//!   finding, not arithmetic.
//! - **Disk drift is informational.** Observed `disk_gb` is Kubernetes
//!   ephemeral-storage capacity, which is a filesystem's view, not the
//!   hardware's; it lands in `disk_informational`, not `drift`.
//! - **Disk HEADROOM is hard, on both scopes** (a520737f). Capacity
//!   drifting from the declaration is a paperwork question; a machine
//!   running out of room stops the pipeline, so one floor
//!   ([`disk_tight_finding`]) is applied to every observed machine
//!   whatever scope it arrived under. It could not fire for a cluster
//!   node until the observer started reading free space, and w-1 — the
//!   node every gate compiles on — was the one that mattered.
//!   `disk_unmeasured` is how a node that stops reporting free space
//!   stays distinguishable from one that has plenty.
//! - **A NotReady node is present, not absent.** It appears in
//!   `not_ready` and still counts as observed — a sick machine is
//!   there, and "missing" must keep meaning missing.
//! - **A failed read fails the firing.** No partial comparison is
//!   recorded; the schedule of the series makes the missing datapoint
//!   visible, and the runner logs the failure loudly.
//!
//! RETAINED WHEN THE RECORD CANNOT TAKE IT (packet 6bf34846). Both the
//! reads and the write above go to the jobs API, which is the thing
//! this stage is part of watching — CLAUDE.md §Diagnosis, *an alarm
//! that reports through its subject dies with it*. A failing firing
//! NAKs for redelivery, but that budget is 8 deliveries across a ~98
//! second backoff (`boss-nats::durable`), so an outage of any real
//! length dead-lettered the observation and the comparison was simply
//! never made — a hole in the series, and with it a hole in the
//! evidence `estate.alarm` reads back to decide whether a hard finding
//! has persisted.
//!
//! So a firing the record refuses now RETAINS the observation and
//! replays it on the next firing the record answers, oldest first
//! (`super::spool`, the Rust half of the mechanism the host observer
//! already has in `infra/estate/observe-lib.sh`). What is kept is the
//! OBSERVATION, not the comparison: during an outage the declared-nodes
//! read fails too, so there is no comparison yet to keep, and replay
//! re-runs this same code over it. The replayed comparison carries the
//! observation's ORIGINAL `observed_at`, so the series afterwards shows
//! readings that arrived late rather than readings that never were.
//!
//! `estate.alarm` needs no spool of its own, and that is a conclusion
//! from its code rather than an omission: it holds no state (`the SoR
//! is the state; the handler stays stateless`), re-derives every
//! finding from this series on each firing, and fires on every
//! comparison. Give it back the comparisons and its raise re-derives
//! itself; a raise it could not file during an outage is re-computed
//! and filed on the next comparison after it, deduped by
//! `estate_finding` the way a re-raise always is.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value as Json, json};

use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{api_client, get_json, post_json};
use super::spool::{PostFuture, Spool};

/// The cluster scope this comparator understands. The observer stamps
/// it; anything else known is routed below, and the rest is recorded
/// as unknown rather than compared wrongly.
pub(crate) const KNOWN_SCOPE: &str = "kubernetes-nodes";

/// The per-host scope (`observe-host.sh`). A host observation carries
/// ONE machine — the script reads its own /proc — so its comparison is
/// SELF-SCOPED: declared-vs-observed for exactly the ids in the
/// observation, never an absence sweep (comparing one host's POST
/// against every declared host would find all the others "missing" on
/// every firing). A host that stops posting entirely is a missing
/// datapoint in the series — `estate.alarm`'s silence sweep is what
/// notices it (a7a19a1a; it was a stated follow-up here until then).
pub(crate) const HOST_SCOPE: &str = "host";

/// The per-host unit scope (`observe-units.sh`, packet 729329c6). Like
/// HOST_SCOPE it is SELF-SCOPED — the observation names the units it
/// watched and their health; there is no declared-units registry to
/// sweep, so the comparison passes the observer's own verdicts through
/// as findings. Without this branch every five-minute unit observation
/// would dead-end as unknown_scope — the exact class the HOST_SCOPE
/// comment above records fixing (49a8d842).
pub(crate) const UNITS_SCOPE: &str = "host-units";

/// The disk floor that turns a reading into a HARD finding (49a8d842:
/// the forge host — 228G, 83% full, "THE TIGHT ONE" — could fill and
/// the comparison would keep answering unknown_scope). Free below 16
/// GiB or below 35% of capacity is `disk_tight`.
///
/// WHICH FILESYSTEM THE NUMBERS DESCRIBE — the thing that makes the
/// percentage mean one thing across two surfaces (a520737f). Both
/// `disk_gb` and `disk_free_gb` are total-and-available of ONE
/// filesystem, the one the work writes into on that machine, rounded to
/// nearest GiB by the one rule:
/// - **host scope** (`observe-host.sh`): `df -k /` — the root
///   filesystem, where the forge's docker store and CI checkouts live.
/// - **kubernetes-nodes scope** (`boss-estate-observe.yaml`): the
///   kubelet's root filesystem, "nodefs" — where the image store,
///   emptyDirs and the gate's warm target live. `disk_gb` is
///   `status.capacity.ephemeral-storage` and `disk_free_gb` is the
///   kubelet summary API's `node.fs.availableBytes`; the kubelet
///   derives the first from a statfs of the same filesystem the second
///   reports, so numerator and denominator describe one volume.
///   A cluster node's `/` is a read-only Talos squashfs and is NOT what
///   fills, which is why nodefs is the filesystem named here.
///
/// The trap this comment exists to keep shut: a volume seen from INSIDE
/// a pod is a third thing again, and reading one of those as a
/// node-level figure is how the gap this floor now closes stayed hidden.
///
/// WHY 35% (was 8%). The alarm is worth waking someone for only if it
/// arrives BEFORE the pipeline stops: the CI host's locomotive refuses
/// a run below 70 GB, the sweep keeps 100 GB, the conductor will not
/// board below 40 GB. On a 228 GB forge 8% was 18 GB — a packet that
/// would have landed hours after CI had gone red (2026-09-05: the
/// series ran 109 → 71 GB across eight trains with no finding at all).
/// 35% of 228 is 80 GB: above the locomotive's floor by the three
/// consecutive comparisons the raiser demands before it files. On a
/// 48 GB bastion 35% is under the 16 GiB minimum, which then rules —
/// the same reading as before for hosts that do less.
const DISK_TIGHT_FLOOR_GB: i64 = 16;
const DISK_TIGHT_FLOOR_PCT: i64 = 35;

/// The floor applied to one observed node — ONE definition, read by
/// both scopes (CLAUDE.md §9a: the floor is a fact, and a fact that
/// lives twice drifts). `None` when the node is fine OR when it carries
/// no usable pair of numbers; an unmeasured node is not a clean one, and
/// [`compare`] records that separately as `disk_unmeasured`.
fn disk_tight_finding(node: &Json) -> Option<Json> {
    let id = node.get("id").and_then(Json::as_str)?;
    let free = node.get("disk_free_gb").and_then(Json::as_i64)?;
    let total = node.get("disk_gb").and_then(Json::as_i64)?;
    (total > 0 && (free < DISK_TIGHT_FLOOR_GB || free * 100 < total * DISK_TIGHT_FLOOR_PCT))
        .then(|| json!({ "id": id, "free_gb": free, "disk_gb": total }))
}

/// The self-scoped host comparison, pure: for each observed host that
/// is also declared, drift on the identity fields + the disk floor;
/// `not_ready` passes through; an observed host nobody declared is the
/// same w-1 class as the cluster scope.
pub(crate) fn compare_host(declared: &[Json], observation: &Json) -> Json {
    let observed: Vec<&Json> = observation
        .get("nodes")
        .and_then(Json::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    let mut observed_not_declared: Vec<Json> = Vec::new();
    let mut drift: Vec<Json> = Vec::new();
    let mut disk_tight: Vec<Json> = Vec::new();
    let mut not_ready: Vec<Json> = Vec::new();

    for node in &observed {
        let Some(id) = node.get("id").and_then(Json::as_str) else {
            continue;
        };
        if node.get("ready").and_then(Json::as_bool) == Some(false) {
            not_ready.push(json!(id));
        }
        if let Some(finding) = disk_tight_finding(node) {
            disk_tight.push(finding);
        }
        let dec = declared
            .iter()
            .find(|d| d.get("id").and_then(Json::as_str) == Some(id));
        let Some(dec) = dec else {
            observed_not_declared.push(json!({
                "id": id,
                "address": node.get("address"),
                "cpu": node.get("cpu"),
                "memory_gb": node.get("memory_gb"),
            }));
            continue;
        };
        let mut fields = serde_json::Map::new();
        for key in ["cpu", "memory_gb"] {
            let d = dec.get(key).cloned().unwrap_or(Json::Null);
            let o = node.get(key).cloned().unwrap_or(Json::Null);
            if !d.is_null() && d != o {
                fields.insert(key.into(), json!({ "declared": d, "observed": o }));
            }
        }
        if !fields.is_empty() {
            drift.push(json!({ "id": id, "fields": fields }));
        }
    }

    json!({
        // The series identity: a self-scoped comparison is one host's
        // reading, and the raiser keys persistence per (scope, host).
        // Without this stamp a CLEAN comparison is anonymous, so two
        // hosts interleaving one scope erase each other's persistence.
        "host": observed.first().and_then(|n| n.get("id")).cloned().unwrap_or(Json::Null),
        "counts": {
            "observed": observed.len(),
            "observed_not_declared": observed_not_declared.len(),
            "drift": drift.len(),
            "disk_tight": disk_tight.len(),
        },
        "findings": {
            "observed_not_declared": observed_not_declared,
            "drift": drift,
            "disk_tight": disk_tight,
            "not_ready": not_ready,
        },
    })
}

/// A declared row participates in the kubernetes-nodes comparison iff
/// its role names a cluster node. conductor/forge roles never
/// participate — the observer cannot see them, so their absence is a
/// fact about the instrument, not the estate.
fn participates(declared: &Json) -> bool {
    let retired = declared
        .get("retired")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let role = declared.get("role").and_then(Json::as_str).unwrap_or("");
    !retired && role.starts_with("talos-")
}

/// The comparison, pure: declared registry rows vs one observation.
/// Returns the full findings payload minus the envelope fields the
/// handler adds (scope, observed_at, observer).
pub(crate) fn compare(declared: &[Json], observation: &Json) -> Json {
    let observed: Vec<&Json> = observation
        .get("nodes")
        .and_then(Json::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    let observed_id = |n: &Json| n.get("id").and_then(Json::as_str).map(str::to_string);

    let mut observed_not_declared: Vec<Json> = Vec::new();
    let mut declared_not_observed: Vec<Json> = Vec::new();
    let mut drift: Vec<Json> = Vec::new();
    let mut disk_informational: Vec<Json> = Vec::new();
    let mut disk_tight: Vec<Json> = Vec::new();
    let mut disk_unmeasured: Vec<Json> = Vec::new();
    let mut not_ready: Vec<Json> = Vec::new();

    let participating: Vec<&Json> = declared.iter().filter(|d| participates(d)).collect();

    for node in &observed {
        let Some(id) = observed_id(node) else {
            continue;
        };
        if node.get("ready").and_then(Json::as_bool) == Some(false) {
            not_ready.push(json!(id));
        }
        // The floor, before the declared lookup — the same order the
        // host scope uses, and for the same reason: a machine nobody
        // declared is still a machine that can fill, and w-1 (the node
        // that compiles this repository) was undeclared for five days.
        if let Some(finding) = disk_tight_finding(node) {
            disk_tight.push(finding);
        } else if node.get("disk_free_gb").and_then(Json::as_i64).is_none()
            || node.get("disk_gb").and_then(Json::as_i64).is_none()
        {
            // No free-space reading at all — the state this whole scope
            // was in until a520737f, in which `disk_tight` cannot fire
            // and a filling node is indistinguishable from a healthy
            // one. The observer's kubelet read is best-effort so the
            // rest of the observation survives losing it; this is where
            // losing it becomes visible. Informational, not hard —
            // `estate.alarm` does not read this key.
            disk_unmeasured.push(json!(id));
        }
        let Some(dec) = participating
            .iter()
            .find(|d| d.get("id").and_then(Json::as_str) == Some(id.as_str()))
        else {
            // The w-1 class: a machine nobody declared. The expensive
            // one — the node that compiles the repo was invisible for
            // five days.
            observed_not_declared.push(json!({
                "id": id,
                "address": node.get("address"),
                "cpu": node.get("cpu"),
                "memory_gb": node.get("memory_gb"),
                "purpose": node.get("purpose"),
            }));
            continue;
        };

        // Same machine on both sides: compare what the registry
        // declares against what the observer measured, field by field.
        let mut fields = serde_json::Map::new();
        for key in ["cpu", "memory_gb", "address"] {
            let d = dec.get(key).cloned().unwrap_or(Json::Null);
            let o = node.get(key).cloned().unwrap_or(Json::Null);
            if d != o {
                fields.insert(key.into(), json!({ "declared": d, "observed": o }));
            }
        }
        if !fields.is_empty() {
            drift.push(json!({ "id": id, "fields": fields }));
        }
        // Disk is informational: observed disk is ephemeral-storage,
        // a filesystem's view. Reported only when both sides claim one.
        let (dd, od) = (dec.get("disk_gb"), node.get("disk_gb"));
        if let (Some(dd), Some(od)) = (dd, od)
            && !dd.is_null()
            && !od.is_null()
            && dd != od
        {
            disk_informational.push(json!({ "id": id, "declared": dd, "observed": od }));
        }
    }

    for dec in &participating {
        let Some(id) = dec.get("id").and_then(Json::as_str) else {
            continue;
        };
        if !observed
            .iter()
            .any(|n| n.get("id").and_then(Json::as_str) == Some(id))
        {
            // Declared, not seen: a machine that died, was removed, or
            // never joined. The observer CAN see this class — that is
            // what the scope filter above guarantees.
            declared_not_observed.push(json!({
                "id": id,
                "role": dec.get("role"),
                "address": dec.get("address"),
            }));
        }
    }

    json!({
        "counts": {
            "observed": observed.len(),
            "participating_declared": participating.len(),
            "observed_not_declared": observed_not_declared.len(),
            "declared_not_observed": declared_not_observed.len(),
            "drift": drift.len(),
            "disk_tight": disk_tight.len(),
            "disk_unmeasured": disk_unmeasured.len(),
        },
        "findings": {
            "observed_not_declared": observed_not_declared,
            "declared_not_observed": declared_not_observed,
            "drift": drift,
            "disk_informational": disk_informational,
            "disk_tight": disk_tight,
            "disk_unmeasured": disk_unmeasured,
            "not_ready": not_ready,
        },
    })
}

/// Units retired BY DESIGN that a per-host observer still watches, so a
/// stale observation reports them unhealthy on every ~5-minute firing.
/// Keyed `(host, unit)` and matched host-scoped: a retired unit's
/// inevitable "inactive" is suppressed on ITS host only, while the same
/// unit name on any OTHER host still surfaces.
///
/// `boss-gcp` / `boss-train.service`: the conductor moved into the
/// cluster on 2026-09-04 (feat/conductor-cutover), and the boss-gcp node
/// registry `notes` say "boss-train.service here is retired by design."
/// boss-gcp's `observe-units.sh` still lists it in the default UNITS
/// set, so every unit observation flags it — a persistent false alarm
/// that masks real findings.
///
/// INTERIM. The durable fix is boss-gcp's per-host `observe-units`
/// config dropping the unit from what it watches; that needs boss-gcp to
/// converge, which it does not today. Remove this entry once it does.
const RETIRED_UNITS: &[(&str, &str)] = &[("boss-gcp", "boss-train.service")];

/// The self-scoped unit comparison, pure: every observed unit whose
/// observer did not stamp `healthy: true` is a finding. Deliberately
/// no recomputation from the raw states — the observer derived health
/// with the journal in hand, and a comparator that second-guesses its
/// instrument is a second instrument. A row without a healthy flag
/// counts as unhealthy: a malformed instrument must surface, not pass.
///
/// The journal excerpt is NOT copied into the finding — it rides the
/// observation row this comparison was computed from, and the
/// comparisons series is what the eventual raiser gets calibrated on
/// (report first, raise later), so it carries names and counts, not
/// twenty lines of log per unit per five minutes.
///
/// KNOWN-RETIRED units are suppressed by `RETIRED_UNITS` below — a unit
/// an observer still watches after it was retired by design reports
/// unhealthy forever, and that inevitability is noise, not a finding
/// (CLAUDE.md's "a check nobody reads" hazard: constant false alarms
/// train people to ignore the surface).
pub(crate) fn compare_units(observation: &Json) -> Json {
    let nodes: Vec<&Json> = observation
        .get("nodes")
        .and_then(Json::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    let mut units = 0usize;
    let mut units_unhealthy: Vec<Json> = Vec::new();

    for node in &nodes {
        let host = node.get("id").and_then(Json::as_str).unwrap_or("");
        for unit in node
            .get("units")
            .and_then(Json::as_array)
            .map(|a| a.iter())
            .into_iter()
            .flatten()
        {
            units += 1;
            if unit.get("healthy").and_then(Json::as_bool) == Some(true) {
                continue;
            }
            // Suppress a KNOWN-RETIRED unit on ITS host: an observer still
            // watching a by-design-dead unit reports it inactive forever,
            // and that inevitability is noise, not a finding. Host-scoped
            // — the same unit name on any other host still surfaces — and
            // it stays counted in `units`, only kept out of the findings.
            let unit_name = unit.get("unit").and_then(Json::as_str).unwrap_or("");
            if RETIRED_UNITS
                .iter()
                .any(|&(h, u)| h == host && u == unit_name)
            {
                continue;
            }
            units_unhealthy.push(json!({
                "host": host,
                "unit": unit.get("unit"),
                "load_state": unit.get("load_state"),
                "active_state": unit.get("active_state"),
                "sub_state": unit.get("sub_state"),
                "result": unit.get("result"),
                "exec_main_status": unit.get("exec_main_status"),
            }));
        }
    }

    json!({
        // Same series stamp as compare_host, same reason: the raiser
        // must tell this host's five-minute series from its neighbor's.
        "host": nodes.first().and_then(|n| n.get("id")).cloned().unwrap_or(Json::Null),
        "counts": {
            "hosts": nodes.len(),
            "units": units,
            "units_unhealthy": units_unhealthy.len(),
        },
        "findings": {
            "units_unhealthy": units_unhealthy,
        },
    })
}

/// The stage name, and therefore the subdirectory the retained
/// observations wait in under the one estate spool.
const STAGE: &str = "estate.compare";

/// Compare one observation and record the result — the whole of this
/// handler's work, as a free function so a REPLAYED observation goes
/// through exactly the same path a fresh one does. There is no
/// second, replay-shaped code path to drift from this one.
///
/// Both reads and the write go to the system of record, so any of them
/// failing is the same condition: the record cannot take this
/// observation right now.
async fn compare_and_record(
    client: &reqwest::Client,
    base: &str,
    rule: &str,
    observation: &Json,
) -> Result<(), HandlerError> {
    let scope = observation
        .get("scope")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let envelope = |body: Json| {
        let mut obj = body;
        if let Some(o) = obj.as_object_mut() {
            o.insert("scope".into(), json!(scope));
            // The observation's OWN stamp, never the clock: a
            // comparison replayed after an outage must say when the
            // reading was taken, so the series shows a gap that filled
            // in late rather than one that never happened.
            o.insert(
                "observed_at".into(),
                observation
                    .get("observed_at")
                    .cloned()
                    .unwrap_or(Json::Null),
            );
            o.insert(
                "observer".into(),
                observation.get("observer").cloned().unwrap_or(Json::Null),
            );
        }
        obj
    };

    let declared = async {
        let nodes = get_json(client, &format!("{base}/api/estate/nodes"), rule).await?;
        nodes
            .get("data")
            .and_then(Json::as_array)
            .cloned()
            .ok_or_else(|| {
                HandlerError::Downstream(
                    "GET /api/estate/nodes: response carries no data array".into(),
                )
            })
    };

    let body = if scope == KNOWN_SCOPE {
        envelope(compare(&declared.await?, observation))
    } else if scope == HOST_SCOPE {
        // Self-scoped: one host posting its own /proc (49a8d842 —
        // until this branch, every host observation dead-ended as
        // unknown_scope and boss-gcp's 48G disk could fill with the
        // comparison still answering shrug).
        envelope(compare_host(&declared.await?, observation))
    } else if scope == UNITS_SCOPE {
        // Self-scoped like HOST_SCOPE, and simpler: no registry
        // read — the observation itself carries both what was
        // watched and what the observer concluded about it.
        envelope(compare_units(observation))
    } else {
        // An observation from an instrument this comparator does
        // not understand. Guessing which declared rows it should
        // have seen would manufacture findings; saying so is the
        // honest record.
        envelope(json!({
            "counts": {},
            "findings": { "unknown_scope": scope },
        }))
    };

    post_json(
        client,
        &format!("{base}/api/estate/comparison"),
        &body,
        rule,
    )
    .await
}

pub struct EstateCompare {
    client: reqwest::Client,
    jobs_base: String,
    spool: Spool,
}

impl EstateCompare {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Self::with_spool(jobs_base, Spool::for_stage(STAGE))
    }

    /// The same handler with an explicit spool — the seam a test binds
    /// so its retained observations go somewhere it owns instead of the
    /// host's one estate spool.
    pub(crate) fn with_spool(jobs_base: impl Into<String>, spool: Spool) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            spool,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// A post function bound to this handler's client and record, for
    /// [`Spool::replay`] to drive over the retained observations.
    fn replayer(&self, rule: &str) -> impl FnMut(Json) -> PostFuture + use<> {
        let client = self.client.clone();
        let base = self.base().to_string();
        let rule = rule.to_string();
        move |observation: Json| {
            let (client, base, rule) = (client.clone(), base.clone(), rule.clone());
            Box::pin(async move {
                compare_and_record(&client, &base, &rule, &observation)
                    .await
                    .map_err(|e| e.to_string())
            })
        }
    }
}

#[async_trait]
impl Handler for EstateCompare {
    fn name(&self) -> &'static str {
        "estate.compare"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = &ctx.rule_name;
        let observation = &ctx.event_payload;

        // The record could not take this observation. RETAIN it, so
        // the outage shows up afterwards as a comparison that arrived
        // late instead of one that never existed at all — the file is
        // named by the observation's own `observed_at`, so a NAK'd
        // redelivery of the same observation lands on itself rather
        // than piling up. Returning the error keeps the redelivery
        // budget as the fast path; the spool is what is left when that
        // budget runs out (~98s) and the outage does not.
        if let Err(e) = compare_and_record(&self.client, self.base(), rule, observation).await {
            let observed_at = observation
                .get("observed_at")
                .and_then(Json::as_str)
                .unwrap_or_default();
            match self.spool.put(observed_at, observation) {
                Ok(()) => tracing::warn!(
                    spool = %self.spool.dir().display(),
                    waiting = self.spool.waiting(),
                    observed_at,
                    error = %e,
                    "estate.compare: the system of record could not take this observation — retained for replay"
                ),
                // A spool that cannot write is the pre-fix behaviour,
                // not a new failure: say so and let the error stand.
                Err(io) => tracing::error!(
                    spool = %self.spool.dir().display(),
                    error = %io,
                    "estate.compare: could not retain the observation — this reading is lost"
                ),
            }
            return Err(e);
        }

        // The record answered, so anything retained during an outage
        // can go in now — oldest first, stopping at the first refusal
        // with the rest kept.
        let replayed = self.spool.replay(self.replayer(rule)).await;
        if let Some(stopped) = replayed.stopped {
            return Err(HandlerError::Downstream(format!(
                "estate.compare: this observation was recorded, but the replay of {} retained one(s) \
                 stopped after {} with {} still waiting in {}: {stopped}",
                replayed.posted + replayed.waiting,
                replayed.posted,
                replayed.waiting,
                self.spool.dir().display(),
            )));
        }
        if replayed.posted > 0 {
            tracing::info!(
                replayed = replayed.posted,
                "estate.compare: replayed retained observations — the gap in the series is filled, stamped when the readings were taken"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared_fixture() -> Vec<Json> {
        vec![
            json!({"id":"cp-1","role":"talos-control-plane","cpu":8,"memory_gb":15,"address":"10.20.0.11","disk_gb":200,"retired":false}),
            json!({"id":"w-2","role":"talos-worker","cpu":4,"memory_gb":15,"address":"10.20.0.16","disk_gb":110,"retired":false}),
            json!({"id":"boss-gcp","role":"conductor","cpu":4,"memory_gb":15,"address":"34.45.110.40","disk_gb":48,"retired":false}),
            json!({"id":"forge","role":"forge","cpu":16,"memory_gb":30,"address":"10.20.0.15","disk_gb":437,"retired":false}),
        ]
    }

    fn observed(nodes: Json) -> Json {
        json!({"observed_at":"2026-08-30T10:20:00Z","observer":"boss-estate-observe","scope":"kubernetes-nodes","nodes":nodes})
    }

    #[test]
    fn out_of_scope_rows_are_never_missing() {
        // The cry-wolf trap: boss-gcp and the forge are declared but
        // can never appear in a kubernetes-nodes observation.
        let out = compare(
            &declared_fixture(),
            &observed(json!([
                {"id":"cp-1","cpu":8,"memory_gb":15,"address":"10.20.0.11","ready":true},
                {"id":"w-2","cpu":4,"memory_gb":15,"address":"10.20.0.16","ready":true},
            ])),
        );
        assert_eq!(out["counts"]["declared_not_observed"], 0);
        assert_eq!(out["counts"]["observed_not_declared"], 0);
        assert_eq!(out["counts"]["drift"], 0);
        assert_eq!(out["counts"]["participating_declared"], 2);
    }

    #[test]
    fn an_undeclared_machine_is_the_finding() {
        // The w-1 class: in the cluster, in no registry.
        let out = compare(
            &declared_fixture(),
            &observed(json!([
                {"id":"cp-1","cpu":8,"memory_gb":15,"address":"10.20.0.11","ready":true},
                {"id":"w-2","cpu":4,"memory_gb":15,"address":"10.20.0.16","ready":true},
                {"id":"w-1","cpu":32,"memory_gb":63,"address":"10.20.0.14","purpose":"build","ready":true},
            ])),
        );
        assert_eq!(out["counts"]["observed_not_declared"], 1);
        assert_eq!(out["findings"]["observed_not_declared"][0]["id"], "w-1");
    }

    #[test]
    fn a_declared_cluster_node_that_vanished_is_missing() {
        let out = compare(
            &declared_fixture(),
            &observed(json!([
                {"id":"cp-1","cpu":8,"memory_gb":15,"address":"10.20.0.11","ready":true},
            ])),
        );
        assert_eq!(out["counts"]["declared_not_observed"], 1);
        assert_eq!(out["findings"]["declared_not_observed"][0]["id"], "w-2");
    }

    #[test]
    fn a_retired_row_does_not_participate() {
        let mut declared = declared_fixture();
        declared.push(json!({"id":"w-9","role":"talos-worker","cpu":8,"memory_gb":15,"address":"10.20.0.99","retired":true}));
        let out = compare(
            &declared,
            &observed(json!([
                {"id":"cp-1","cpu":8,"memory_gb":15,"address":"10.20.0.11","ready":true},
                {"id":"w-2","cpu":4,"memory_gb":15,"address":"10.20.0.16","ready":true},
            ])),
        );
        assert_eq!(out["counts"]["declared_not_observed"], 0);
    }

    #[test]
    fn equal_rounded_gib_is_not_drift_and_one_off_is() {
        // Both sides state GiB rounded to nearest by the one rule;
        // equal values must not drift, a 1-GiB difference must.
        let out = compare(
            &declared_fixture(),
            &observed(json!([
                {"id":"cp-1","cpu":8,"memory_gb":15,"address":"10.20.0.11","ready":true},
                {"id":"w-2","cpu":4,"memory_gb":16,"address":"10.20.0.16","ready":true},
            ])),
        );
        assert_eq!(out["counts"]["drift"], 1);
        assert_eq!(out["findings"]["drift"][0]["id"], "w-2");
        assert_eq!(
            out["findings"]["drift"][0]["fields"]["memory_gb"]["declared"],
            15
        );
    }

    #[test]
    fn disk_difference_is_informational_not_drift() {
        let out = compare(
            &declared_fixture(),
            &observed(json!([
                {"id":"cp-1","cpu":8,"memory_gb":15,"address":"10.20.0.11","disk_gb":199,"ready":true},
                {"id":"w-2","cpu":4,"memory_gb":15,"address":"10.20.0.16","disk_gb":110,"ready":true},
            ])),
        );
        assert_eq!(out["counts"]["drift"], 0);
        assert_eq!(out["findings"]["disk_informational"][0]["id"], "cp-1");
    }

    #[test]
    fn a_not_ready_node_is_present_not_missing() {
        let out = compare(
            &declared_fixture(),
            &observed(json!([
                {"id":"cp-1","cpu":8,"memory_gb":15,"address":"10.20.0.11","ready":true},
                {"id":"w-2","cpu":4,"memory_gb":15,"address":"10.20.0.16","ready":false},
            ])),
        );
        assert_eq!(out["counts"]["declared_not_observed"], 0);
        assert_eq!(out["findings"]["not_ready"][0], "w-2");
    }

    #[test]
    fn nothing_declared_means_everything_observed_is_undeclared() {
        let declared = vec![json!({"id":"boss-gcp","role":"conductor","retired":false})];
        let out = compare(
            &declared,
            &observed(json!([
                {"id":"cp-1","cpu":8,"memory_gb":15,"address":"10.20.0.11","ready":true},
            ])),
        );
        assert_eq!(out["counts"]["participating_declared"], 0);
        assert_eq!(out["counts"]["observed_not_declared"], 1);
    }

    // ----- the cluster scope's disk floor (a520737f) -----

    fn cluster_obs(id: &str, total: i64, free: Option<i64>) -> Json {
        let mut node = json!({
            "id": id, "cpu": 32, "memory_gb": 125, "address": "10.20.0.21",
            "disk_gb": total, "ready": true });
        if let Some(free) = free {
            node["disk_free_gb"] = json!(free);
        }
        observed(json!([node]))
    }

    #[test]
    fn a_cluster_node_below_the_floor_is_disk_tight() {
        // w-1 is THE BUILD NODE: every gate's warm target lives on its
        // nodefs and every gate Job prefers it by affinity. Measured
        // 2026-09-10 the kubernetes-nodes observation carried
        // `disk_gb: 929` and NO free figure, so `disk_tight` could never
        // fire for it — the build node could fill and nothing would say
        // so (a520737f).
        let declared = vec![
            json!({"id":"w-1","role":"talos-worker","cpu":32,"memory_gb":125,
            "address":"10.20.0.21","disk_gb":929,"retired":false}),
        ];
        // 390 GiB free of 929 is 42% — the reading on the day the gap
        // was found, and correctly not tight.
        let fine = compare(&declared, &cluster_obs("w-1", 929, Some(390)));
        assert_eq!(fine["counts"]["disk_tight"], 0);
        // 300 of 929 is 32%: above the 16 GiB floor, under 35%, so a
        // packet lands while the gate can still run.
        let tight = compare(&declared, &cluster_obs("w-1", 929, Some(300)));
        assert_eq!(tight["counts"]["disk_tight"], 1);
        assert_eq!(tight["findings"]["disk_tight"][0]["id"], "w-1");
        assert_eq!(tight["findings"]["disk_tight"][0]["free_gb"], 300);
        assert_eq!(tight["findings"]["disk_tight"][0]["disk_gb"], 929);
    }

    #[test]
    fn the_floor_reads_the_same_on_both_surfaces() {
        // ONE definition of the floor, not two (CLAUDE.md §9a). The host
        // scope's 71-of-228 reading (the forge mid-build on 2026-09-05)
        // and the same ratio on a cluster node must both be tight, and
        // the same pair the other side of 35% must both be clean.
        let dec_host = vec![json!({"id":"h","role":"forge"})];
        let dec_node = vec![json!({"id":"h","role":"talos-worker"})];
        for (free, want) in [(71, 1), (80, 0)] {
            assert_eq!(
                compare_host(&dec_host, &host_obs("h", free, 228))["counts"]["disk_tight"],
                want,
                "host scope, {free} of 228"
            );
            assert_eq!(
                compare(&dec_node, &cluster_obs("h", 228, Some(free)))["counts"]["disk_tight"],
                want,
                "cluster scope, {free} of 228"
            );
        }
    }

    #[test]
    fn a_node_with_no_free_reading_is_unmeasured_not_clean() {
        // The kubelet stats read is best-effort BY DESIGN: losing it must
        // not cost the whole observation, because declared_not_observed,
        // not_ready and the alarm's silence sweep all ride the same POST.
        // So the blindness has to show somewhere, or the instrument can
        // go dark and read exactly like a healthy estate — which IS the
        // class this packet reports. Informational, not hard:
        // `estate.alarm` raises on `disk_tight`, `not_ready`,
        // `declared_not_observed` and `units_unhealthy`, so nothing new
        // wakes anyone.
        let declared = vec![json!({"id":"w-1","role":"talos-worker","retired":false})];
        let blind = compare(&declared, &cluster_obs("w-1", 929, None));
        assert_eq!(blind["counts"]["disk_tight"], 0);
        assert_eq!(blind["counts"]["disk_unmeasured"], 1);
        assert_eq!(blind["findings"]["disk_unmeasured"][0], "w-1");
        let seeing = compare(&declared, &cluster_obs("w-1", 929, Some(390)));
        assert_eq!(seeing["counts"]["disk_unmeasured"], 0);
    }

    #[test]
    fn an_undeclared_node_that_is_tight_still_surfaces() {
        // w-1 was undeclared for five days. A machine nobody declared is
        // still a machine that can fill, so the floor is tested before
        // the declared lookup, exactly as the host scope tests it.
        let out = compare(&[], &cluster_obs("w-9", 929, Some(10)));
        assert_eq!(out["counts"]["observed_not_declared"], 1);
        assert_eq!(out["findings"]["disk_tight"][0]["id"], "w-9");
    }

    // ----- the self-scoped host comparison (49a8d842) -----

    fn host_obs(id: &str, free: i64, total: i64) -> Json {
        json!({ "scope": "host", "nodes": [{
            "id": id, "cpu": 8, "memory_gb": 32,
            "disk_gb": total, "disk_free_gb": free, "ready": true }] })
    }

    #[test]
    fn a_host_below_the_floor_is_disk_tight_and_above_is_not() {
        let declared =
            vec![json!({"id": "boss-gcp-1", "role": "conductor", "cpu": 8, "memory_gb": 32})];
        // 12G free of 47: below the 16G floor.
        let tight = compare_host(&declared, &host_obs("boss-gcp-1", 12, 47));
        assert_eq!(tight["findings"]["disk_tight"][0]["id"], "boss-gcp-1");
        // 95G free of 228: above both floors (16G and 35% = 80G).
        let fine = compare_host(&declared, &host_obs("forge-host", 95, 228));
        assert_eq!(fine["findings"]["disk_tight"].as_array().unwrap().len(), 0);
        // 71G free of 228 (the forge at 16:57 on 2026-09-05, mid-build,
        // with the locomotive's 70G refusal one train away) is above the
        // GB floor but under 35% — tight, and a packet lands before CI
        // goes red instead of hours after.
        let pct = compare_host(&declared, &host_obs("forge-host", 71, 228));
        assert_eq!(pct["findings"]["disk_tight"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_self_scoped_comparison_names_its_host() {
        // The raiser keys persistence per (scope, host). Without this
        // stamp a CLEAN comparison carries no identity at all, so two
        // hosts interleaving one scope erase each other's persistence
        // and a host finding can never survive N consecutive rows.
        let body = compare_host(&[], &host_obs("boss-gcp-1", 30, 47));
        assert_eq!(body["host"], "boss-gcp-1");
        let units = compare_units(&units_obs(json!([])));
        assert_eq!(units["host"], "boss-gcp");
    }

    #[test]
    fn a_single_host_post_never_reports_other_declared_hosts_missing() {
        // The false-fire this scope exists to avoid: one host's POST
        // must not find every OTHER declared machine absent.
        let declared = vec![
            json!({"id": "boss-gcp-1", "role": "conductor"}),
            json!({"id": "forge-host", "role": "forge"}),
        ];
        let body = compare_host(&declared, &host_obs("boss-gcp-1", 30, 47));
        assert!(body["findings"].get("declared_not_observed").is_none());
        assert_eq!(
            body["findings"]["observed_not_declared"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn an_undeclared_host_is_the_w1_class() {
        let body = compare_host(&[], &host_obs("mystery-box", 30, 47));
        assert_eq!(
            body["findings"]["observed_not_declared"][0]["id"],
            "mystery-box"
        );
    }

    // ----- the self-scoped unit comparison (729329c6) -----

    fn units_obs(units: Json) -> Json {
        units_obs_on("boss-gcp", units)
    }

    fn units_obs_on(host: &str, units: Json) -> Json {
        json!({ "scope": "host-units", "observer": "boss-estate-observe-units",
                "nodes": [{ "id": host, "healthy": true, "units": units }] })
    }

    #[test]
    fn an_unhealthy_unit_is_the_finding() {
        // The class the comparison exists for: a live unit dead while a
        // CI-green train sat unmerged for two hours with no signal. (The
        // original incident was boss-train.service, now retired by design
        // on boss-gcp and suppressed — see the RETIRED_UNITS tests below;
        // any non-retired unit still surfaces exactly like this.)
        let body = compare_units(&units_obs(json!([
            {"unit":"boss-jobs-api.service","load_state":"loaded","active_state":"inactive",
             "sub_state":"dead","result":"success","exec_main_status":0,"healthy":false,
             "journal":"Sep 02 08:15:00 boss-gcp systemd[1]: Stopped boss-jobs-api."},
            {"unit":"forgejo.service","load_state":"loaded","active_state":"active",
             "sub_state":"running","result":"success","exec_main_status":0,"healthy":true},
        ])));
        assert_eq!(body["counts"]["units"], 2);
        assert_eq!(body["counts"]["units_unhealthy"], 1);
        let finding = &body["findings"]["units_unhealthy"][0];
        assert_eq!(finding["host"], "boss-gcp");
        assert_eq!(finding["unit"], "boss-jobs-api.service");
        assert_eq!(finding["active_state"], "inactive");
        // The journal excerpt stays on the OBSERVATION row — copying
        // ~20 lines into every comparison would double the evidence's
        // storage without doubling the evidence.
        assert!(finding.get("journal").is_none());
    }

    #[test]
    fn a_retired_unit_on_its_host_is_not_a_finding() {
        // boss-gcp/boss-train.service: retired by design at the
        // 2026-09-04 conductor cutover, but boss-gcp's observer still
        // watches it and reports it inactive every ~5 minutes. The
        // comparison must NOT turn that inevitability into a finding —
        // but it stays counted as an observed unit (it was observed).
        let body = compare_units(&units_obs(json!([
            {"unit":"boss-train.service","load_state":"loaded","active_state":"inactive",
             "sub_state":"dead","result":"success","exec_main_status":0,"healthy":false},
            {"unit":"forgejo.service","load_state":"loaded","active_state":"active",
             "sub_state":"running","result":"success","exec_main_status":0,"healthy":true},
        ])));
        assert_eq!(body["counts"]["units"], 2);
        assert_eq!(body["counts"]["units_unhealthy"], 0);
        assert_eq!(
            body["findings"]["units_unhealthy"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn a_retired_unit_name_on_another_host_still_surfaces() {
        // The suppression is (host, unit)-scoped: boss-train.service is
        // retired only on boss-gcp. The SAME unit name unhealthy on any
        // other host is a real finding and must surface.
        let body = compare_units(&units_obs_on(
            "cp-1",
            json!([
                {"unit":"boss-train.service","load_state":"loaded","active_state":"inactive",
                 "sub_state":"dead","result":"success","exec_main_status":0,"healthy":false},
            ]),
        ));
        assert_eq!(body["counts"]["units_unhealthy"], 1);
        let finding = &body["findings"]["units_unhealthy"][0];
        assert_eq!(finding["host"], "cp-1");
        assert_eq!(finding["unit"], "boss-train.service");
    }

    #[test]
    fn a_non_retired_unhealthy_unit_still_surfaces_on_boss_gcp() {
        // The suppression touches ONLY the listed (host, unit) pairs:
        // any other unhealthy unit on boss-gcp is still a finding.
        let body = compare_units(&units_obs(json!([
            {"unit":"boss-dispatcher.service","load_state":"loaded","active_state":"failed",
             "sub_state":"failed","result":"exit-code","exec_main_status":1,"healthy":false},
            {"unit":"boss-train.service","load_state":"loaded","active_state":"inactive",
             "sub_state":"dead","result":"success","exec_main_status":0,"healthy":false},
        ])));
        assert_eq!(body["counts"]["units"], 2);
        assert_eq!(body["counts"]["units_unhealthy"], 1);
        let finding = &body["findings"]["units_unhealthy"][0];
        assert_eq!(finding["unit"], "boss-dispatcher.service");
    }

    #[test]
    fn an_all_healthy_post_reports_no_findings() {
        let body = compare_units(&units_obs(json!([
            {"unit":"boss-train.service","load_state":"loaded","active_state":"active",
             "sub_state":"running","result":"success","exec_main_status":0,"healthy":true},
        ])));
        assert_eq!(body["counts"]["units"], 1);
        assert_eq!(body["counts"]["units_unhealthy"], 0);
        assert_eq!(
            body["findings"]["units_unhealthy"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn a_unit_row_without_a_healthy_flag_is_unhealthy_not_invisible() {
        // A malformed row is a broken instrument, and a broken
        // instrument must surface as a finding, not pass as health.
        let body = compare_units(&units_obs(json!([
            {"unit":"forgejo.service","active_state":"active"},
        ])));
        assert_eq!(body["counts"]["units_unhealthy"], 1);
        assert_eq!(
            body["findings"]["units_unhealthy"][0]["unit"],
            "forgejo.service"
        );
    }

    // -----------------------------------------------------------------
    // Retain and replay, at the handler (packet 6bf34846). The unit
    // tests in `super::spool` pin the mechanism; these pin that THIS
    // stage uses it — measured at the consuming layer, against a stub
    // system of record that can be taken down and brought back.
    // -----------------------------------------------------------------

    /// A stub system of record: serves the declared-nodes read and the
    /// comparison write, refuses everything while `down`, and keeps
    /// every comparison it accepted.
    #[derive(Clone)]
    struct StubRecord {
        down: Arc<std::sync::atomic::AtomicBool>,
        recorded: Arc<std::sync::Mutex<Vec<Json>>>,
    }

    impl StubRecord {
        async fn start() -> (Self, String) {
            use axum::extract::State;
            use axum::response::IntoResponse;
            use axum::routing::{get, post};
            use axum::{Json as AxJson, Router};
            let me = Self {
                down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                recorded: Arc::new(std::sync::Mutex::new(Vec::new())),
            };
            fn refuse() -> axum::response::Response {
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "the system of record is down",
                )
                    .into_response()
            }
            let app = Router::new()
                .route(
                    "/api/estate/nodes",
                    get(|State(s): State<StubRecord>| async move {
                        if s.is_down() {
                            return refuse();
                        }
                        AxJson(json!({ "data": declared_fixture() })).into_response()
                    }),
                )
                .route(
                    "/api/estate/comparison",
                    post(
                        |State(s): State<StubRecord>, AxJson(body): AxJson<Json>| async move {
                            if s.is_down() {
                                return refuse();
                            }
                            s.recorded.lock().expect("recorded lock").push(body);
                            axum::http::StatusCode::ACCEPTED.into_response()
                        },
                    ),
                )
                .with_state(me.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            (me, format!("http://{addr}"))
        }

        fn is_down(&self) -> bool {
            self.down.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn set_down(&self, down: bool) {
            self.down.store(down, std::sync::atomic::Ordering::SeqCst);
        }
        fn stamps(&self) -> Vec<String> {
            self.recorded
                .lock()
                .expect("recorded lock")
                .iter()
                .map(|c| c["observed_at"].as_str().unwrap_or_default().to_string())
                .collect()
        }
    }

    fn firing(observation: Json) -> InvocationContext {
        InvocationContext {
            rule_name: "estate-compare-on-observation".into(),
            triggering_event_id: "evt-test".into(),
            triggering_topic: "jobs.estate.observed".into(),
            event_payload: observation,
        }
    }

    fn host_observation(at: &str) -> Json {
        json!({
            "scope": HOST_SCOPE,
            "observed_at": at,
            "observer": "automation:estate-observer-host",
            "nodes": [{"id":"forge","cpu":16,"memory_gb":30,"address":"10.20.0.15","disk_gb":437,"disk_free_gb":300,"ready":true}],
        })
    }

    struct SpoolDir(std::path::PathBuf);
    impl SpoolDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "boss-estate-compare-test-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for SpoolDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn an_observation_the_record_could_not_compare_is_retained_not_lost() {
        let (record, base) = StubRecord::start().await;
        let dir = SpoolDir::new("retain");
        let handler = EstateCompare::with_spool(&base, Spool::at(&dir.0, 10));

        record.set_down(true);
        let out = handler
            .invoke(&[], &firing(host_observation("2026-09-05T08:00:00Z")))
            .await;

        assert!(
            out.is_err(),
            "a firing the record refused must still fail, so the redelivery budget is spent first"
        );
        assert_eq!(
            handler.spool.waiting(),
            1,
            "the observation was dropped — the comparison series keeps a hole nobody can tell from a timer that never fired"
        );
        assert!(record.stamps().is_empty());
    }

    #[tokio::test]
    async fn the_next_firing_replays_the_gap_oldest_first_with_original_stamps() {
        let (record, base) = StubRecord::start().await;
        let dir = SpoolDir::new("replay");
        let handler = EstateCompare::with_spool(&base, Spool::at(&dir.0, 10));

        // Three observations arrive while the record is down. Each
        // fails and is retained.
        record.set_down(true);
        for at in [
            "2026-09-05T08:00:00Z",
            "2026-09-05T08:15:00Z",
            "2026-09-05T08:30:00Z",
        ] {
            assert!(
                handler
                    .invoke(&[], &firing(host_observation(at)))
                    .await
                    .is_err(),
                "a firing during the outage must fail"
            );
        }
        assert_eq!(handler.spool.waiting(), 3);

        // The record comes back and the next observation fires.
        record.set_down(false);
        handler
            .invoke(&[], &firing(host_observation("2026-09-05T08:45:00Z")))
            .await
            .expect("the firing the record answered");

        assert_eq!(
            record.stamps(),
            vec![
                // The live one is recorded first — it is what the
                // firing was for; the retained ones follow, oldest
                // first.
                "2026-09-05T08:45:00Z",
                "2026-09-05T08:00:00Z",
                "2026-09-05T08:15:00Z",
                "2026-09-05T08:30:00Z",
            ],
            "the gap was not filled in the order the readings were taken"
        );
        assert_eq!(
            handler.spool.waiting(),
            0,
            "the spool did not drain once the record answered"
        );
    }

    #[tokio::test]
    async fn a_redelivery_of_the_same_observation_retains_it_once() {
        // The runner NAKs a failed firing and JetStream redelivers it
        // up to MAX_DELIVER times. Eight redeliveries of one reading
        // must not become eight retained readings.
        let (record, base) = StubRecord::start().await;
        let dir = SpoolDir::new("redeliver");
        let handler = EstateCompare::with_spool(&base, Spool::at(&dir.0, 10));

        record.set_down(true);
        let ctx = firing(host_observation("2026-09-05T08:00:00Z"));
        for _ in 0..8 {
            let _ = handler.invoke(&[], &ctx).await;
        }
        assert_eq!(handler.spool.waiting(), 1);
    }
}
