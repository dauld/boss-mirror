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

/// The one scope this comparator understands. The observer stamps it;
/// anything else is recorded as unknown rather than compared wrongly.
const KNOWN_SCOPE: &str = "kubernetes-nodes";

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
}
