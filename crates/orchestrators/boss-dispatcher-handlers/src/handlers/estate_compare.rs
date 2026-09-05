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
//! - **A NotReady node is present, not absent.** It appears in
//!   `not_ready` and still counts as observed — a sick machine is
//!   there, and "missing" must keep meaning missing.
//! - **A failed read fails the firing.** No partial comparison is
//!   recorded; the schedule of the series makes the missing datapoint
//!   visible, and the runner logs the failure loudly.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value as Json, json};

use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{api_client, get_json, post_json};

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

/// The disk floor that turns a host reading into a HARD finding
/// (49a8d842: the forge host — 228G, 83% full, "THE TIGHT ONE" — could
/// fill and the comparison would keep answering unknown_scope). Free
/// below 16 GiB or below 35% of capacity is `disk_tight`.
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
        if let (Some(free), Some(total)) = (
            node.get("disk_free_gb").and_then(Json::as_i64),
            node.get("disk_gb").and_then(Json::as_i64),
        ) && total > 0
            && (free < DISK_TIGHT_FLOOR_GB || free * 100 < total * DISK_TIGHT_FLOOR_PCT)
        {
            disk_tight.push(json!({ "id": id, "free_gb": free, "disk_gb": total }));
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
    let mut not_ready: Vec<Json> = Vec::new();

    let participating: Vec<&Json> = declared.iter().filter(|d| participates(d)).collect();

    for node in &observed {
        let Some(id) = observed_id(node) else {
            continue;
        };
        if node.get("ready").and_then(Json::as_bool) == Some(false) {
            not_ready.push(json!(id));
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
        },
        "findings": {
            "observed_not_declared": observed_not_declared,
            "declared_not_observed": declared_not_observed,
            "drift": drift,
            "disk_informational": disk_informational,
            "not_ready": not_ready,
        },
    })
}

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

pub struct EstateCompare {
    client: reqwest::Client,
    jobs_base: String,
}

impl EstateCompare {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
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
        let scope = observation
            .get("scope")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        let envelope = |body: Json| {
            let mut obj = body;
            if let Some(o) = obj.as_object_mut() {
                o.insert("scope".into(), json!(scope));
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

        let body = if scope == KNOWN_SCOPE {
            let nodes = get_json(
                &self.client,
                &format!("{}/api/estate/nodes", self.base()),
                rule,
            )
            .await?;
            let declared: Vec<Json> = nodes
                .get("data")
                .and_then(Json::as_array)
                .cloned()
                .ok_or_else(|| {
                    HandlerError::Downstream(
                        "GET /api/estate/nodes: response carries no data array".into(),
                    )
                })?;
            envelope(compare(&declared, observation))
        } else if scope == HOST_SCOPE {
            // Self-scoped: one host posting its own /proc (49a8d842 —
            // until this branch, every host observation dead-ended as
            // unknown_scope and boss-gcp's 48G disk could fill with the
            // comparison still answering shrug).
            let nodes = get_json(
                &self.client,
                &format!("{}/api/estate/nodes", self.base()),
                rule,
            )
            .await?;
            let declared: Vec<Json> = nodes
                .get("data")
                .and_then(Json::as_array)
                .cloned()
                .ok_or_else(|| {
                    HandlerError::Downstream(
                        "GET /api/estate/nodes: response carries no data array".into(),
                    )
                })?;
            envelope(compare_host(&declared, observation))
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
            &self.client,
            &format!("{}/api/estate/comparison", self.base()),
            &body,
            rule,
        )
        .await
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
        json!({ "scope": "host-units", "observer": "boss-estate-observe-units",
                "nodes": [{ "id": "boss-gcp", "healthy": true, "units": units }] })
    }

    #[test]
    fn an_unhealthy_unit_is_the_finding() {
        // The quiet-conductor class: boss-train.service dead while a
        // CI-green train sat unmerged for two hours with no signal.
        let body = compare_units(&units_obs(json!([
            {"unit":"boss-train.service","load_state":"loaded","active_state":"inactive",
             "sub_state":"dead","result":"success","exec_main_status":0,"healthy":false,
             "journal":"Sep 02 08:15:00 boss-gcp systemd[1]: Stopped boss-train."},
            {"unit":"forgejo.service","load_state":"loaded","active_state":"active",
             "sub_state":"running","result":"success","exec_main_status":0,"healthy":true},
        ])));
        assert_eq!(body["counts"]["units"], 2);
        assert_eq!(body["counts"]["units_unhealthy"], 1);
        let finding = &body["findings"]["units_unhealthy"][0];
        assert_eq!(finding["host"], "boss-gcp");
        assert_eq!(finding["unit"], "boss-train.service");
        assert_eq!(finding["active_state"], "inactive");
        // The journal excerpt stays on the OBSERVATION row — copying
        // ~20 lines into every comparison would double the evidence's
        // storage without doubling the evidence.
        assert!(finding.get("journal").is_none());
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
}
