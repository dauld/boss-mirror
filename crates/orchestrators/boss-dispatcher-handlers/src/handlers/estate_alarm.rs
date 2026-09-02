//! `estate.alarm` — the raiser the comparison series was recorded for.
//!
//! Post-mortem #2's blind spot, twice: on 2026-08-27 the cp-2/cp-3
//! kubelets flapped, boss served 500s for ~25 minutes, six CronJob
//! runs died — and it was found HOURS later, by hand, because every
//! noticing mechanism BOSS has keys off packets and a kubelet is not
//! a packet (a5adfb99). The estate loop already measures: observers
//! post, `estate.compare` records findings as a series on
//! `jobs.estate.compared` — "the series the eventual raiser will be
//! calibrated on (59ef456a, report first, raise later)". This is that
//! raiser: when a HARD finding persists across consecutive
//! comparisons, it becomes an urgent packet the overdue/watchlist
//! machinery can finally see. The algedonic channel BOSS is named
//! for, wired to the cluster itself.
//!
//! CALIBRATION, so the alarm is worth trusting:
//! - HARD findings only — `not_ready` (a declared node that is sick)
//!   and `declared_not_observed` (a declared node that is GONE).
//!   `observed_not_declared` is a paperwork gap and `drift` is config
//!   — real, but not 03:00-urgent, and an alarm that cries over
//!   paperwork trains operators to ignore it.
//! - PERSISTENCE over [`PERSIST_N`] consecutive same-scope
//!   comparisons, read back from the recorded series (the SoR is the
//!   state; the handler stays stateless). One flapped reading is
//!   weather; N in a row is a condition.
//! - DEDUP against open packets carrying the same `estate_finding`
//!   key: a persisting condition is ONE packet, not one per firing.
//!
//! A no-op, not an error, when findings are absent, not yet
//! persistent, or already raised.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{api_client, get_json, post_json};

/// Consecutive same-scope comparisons a hard finding must survive to
/// raise. Three: at the tightened 15-minute observer cadence that is
/// ~30-45 minutes of a node being gone or NotReady — slower than a
/// pager, far faster than "David asked hours later", and immune to a
/// single flapped reading.
const PERSIST_N: usize = 3;

pub struct EstateAlarm {
    client: reqwest::Client,
    jobs_base: String,
}

impl EstateAlarm {
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

/// The HARD finding keys of one comparison payload:
/// `not_ready:<id>` / `gone:<id>`. Ids arrive both bare
/// (`not_ready` pushes strings) and wrapped (`{"id": ...}`), so both
/// are read; anything else is ignored rather than guessed at.
fn hard_finding_keys(comparison: &Value) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let findings = comparison.get("findings");
    let mut collect = |field: &str, prefix: &str| {
        for v in findings
            .and_then(|f| f.get(field))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = v
                .as_str()
                .or_else(|| v.get("id").and_then(Value::as_str))
                .unwrap_or("");
            if !id.is_empty() {
                keys.insert(format!("{prefix}:{id}"));
            }
        }
    };
    collect("not_ready", "not_ready");
    collect("declared_not_observed", "gone");
    // The host scope's disk floor (49a8d842): a machine below the
    // headroom a full gate needs is as hard as a sick node.
    collect("disk_tight", "disk_tight");
    keys
}

/// The keys present in EVERY one of the `n` most recent same-scope
/// comparisons — the persistence test, pure. `comparisons` arrives
/// newest-first (the API's order); fewer than `n` same-scope rows
/// means not enough evidence, so nothing persists.
fn persistent_keys(comparisons: &[Value], scope: &str, n: usize) -> BTreeSet<String> {
    let same_scope: Vec<&Value> = comparisons
        .iter()
        .filter(|c| c.get("scope").and_then(Value::as_str) == Some(scope))
        .take(n)
        .collect();
    if same_scope.len() < n {
        return BTreeSet::new();
    }
    let mut iter = same_scope.iter();
    let mut keys = iter
        .next()
        .map(|c| hard_finding_keys(c))
        .unwrap_or_default();
    for c in iter {
        let these = hard_finding_keys(c);
        keys = keys.intersection(&these).cloned().collect();
    }
    keys
}

/// `estate_finding` keys already carried by an open packet — the dedup
/// set, pure over the jobs listing.
fn already_raised(open_jobs: &[Value]) -> BTreeSet<String> {
    open_jobs
        .iter()
        .filter_map(|j| {
            j.get("metadata")?
                .get("estate_finding")?
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

/// The urgent packet one persistent finding becomes.
fn alarm_body(key: &str, scope: &str, evidence: &str) -> Value {
    json!({
        "kind": "backlog-item",
        "title": format!("ESTATE ALARM: {key} persisted {PERSIST_N} consecutive comparisons"),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": "emp-david",
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": {
            "area": "estate",
            "estate_finding": key,
            "detail": format!(
                "Raised by estate.alarm (a5adfb99, the raiser 59ef456a's series was \
                 recorded for): the finding `{key}` appeared in {PERSIST_N} consecutive \
                 `{scope}` comparisons — a condition, not a flap. The 2026-08-27 class \
                 (kubelet flap, 25min of 500s, found hours later by hand) now files \
                 itself while it is happening. Evidence: {evidence}. The observation \
                 and comparison series at /api/estate/observations and \
                 /api/estate/comparisons carry the full readings."
            ),
        },
    })
}

#[async_trait]
impl Handler for EstateAlarm {
    fn name(&self) -> &'static str {
        "estate.alarm"
    }

    async fn invoke(
        &self,
        _args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let comparison = &ctx.event_payload;
        let scope = comparison
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Cheap rejects before any fetch: an unscoped payload cannot be
        // compared to its own series, and a clean comparison raises
        // nothing.
        if scope.is_empty() || hard_finding_keys(comparison).is_empty() {
            return Ok(());
        }

        // The recorded series IS the state (the handler keeps none):
        // enough recent rows to find PERSIST_N of this scope even when
        // another scope's rows interleave.
        let recent = get_json(
            &self.client,
            &format!("{}/api/estate/comparisons?limit=20", self.base()),
            &ctx.rule_name,
        )
        .await?;
        let rows: Vec<Value> = recent
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Rows are event envelopes; the comparison rides in `payload`
        // (recorded verbatim by the dumb door). Fall back to the row
        // itself so a flattened future shape keeps working.
        let payloads: Vec<Value> = rows
            .iter()
            .map(|r| r.get("payload").cloned().unwrap_or_else(|| r.clone()))
            .collect();
        let persistent = persistent_keys(&payloads, scope, PERSIST_N);
        if persistent.is_empty() {
            return Ok(());
        }

        let open = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind=backlog-item&status=open&limit=200",
                self.base()
            ),
            &ctx.rule_name,
        )
        .await?;
        let open_rows: Vec<Value> = open
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let raised = already_raised(&open_rows);

        let evidence = format!(
            "triggering event {} on topic {}",
            ctx.triggering_event_id, ctx.triggering_topic
        );
        for key in persistent.difference(&raised) {
            post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &alarm_body(key, scope, &evidence),
                &ctx.rule_name,
            )
            .await?;
            tracing::info!(finding = %key, scope, "estate.alarm raised a packet");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comparison(scope: &str, not_ready: &[&str], gone: &[&str]) -> Value {
        json!({
            "scope": scope,
            "findings": {
                "not_ready": not_ready,
                "declared_not_observed": gone.iter().map(|g| json!({"id": g})).collect::<Vec<_>>(),
                "observed_not_declared": [{"id": "paperwork-only"}],
                "drift": [{"id": "w-1", "fields": {}}],
            }
        })
    }

    #[test]
    fn only_hard_findings_key_and_both_id_shapes_are_read() {
        let keys = hard_finding_keys(&comparison("kubernetes-nodes", &["cp-2"], &["w-9"]));
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec!["gone:w-9".to_string(), "not_ready:cp-2".to_string()],
            "paperwork (observed_not_declared) and drift must not alarm"
        );
    }

    #[test]
    fn a_finding_must_survive_every_one_of_the_last_n() {
        let c = |nr: &[&str]| comparison("kubernetes-nodes", nr, &[]);
        // Newest-first: present, present, present — persists.
        let steady = [c(&["cp-2"]), c(&["cp-2"]), c(&["cp-2"])];
        assert!(persistent_keys(&steady, "kubernetes-nodes", 3).contains("not_ready:cp-2"));
        // A flap (missing in the middle reading) does not.
        let flap = [c(&["cp-2"]), c(&[]), c(&["cp-2"])];
        assert!(persistent_keys(&flap, "kubernetes-nodes", 3).is_empty());
    }

    #[test]
    fn too_few_same_scope_rows_is_not_enough_evidence() {
        let c = comparison("kubernetes-nodes", &["cp-2"], &[]);
        let other = comparison("forge-host", &["cp-2"], &[]);
        // Two matching + one other scope: only 2 of scope — no alarm.
        let rows = [c.clone(), other, c.clone()];
        assert!(persistent_keys(&rows, "kubernetes-nodes", 3).is_empty());
    }

    #[test]
    fn an_open_packet_with_the_key_suppresses_a_second() {
        let open = [json!({"metadata": {"estate_finding": "not_ready:cp-2"}})];
        assert!(already_raised(&open).contains("not_ready:cp-2"));
    }

    #[test]
    fn the_alarm_packet_is_urgent_and_carries_the_key() {
        let b = alarm_body("not_ready:cp-2", "kubernetes-nodes", "evt");
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["metadata"]["estate_finding"], "not_ready:cp-2");
        assert!(b["title"].as_str().unwrap().contains("not_ready:cp-2"));
    }
}
