//! `jobs.run-car-probes` — a train arrived: run each boarded car's
//! recorded probe against production, by filing the ops-request the
//! forge host answers.
//!
//! THE ACTOR HALF OF CARRIED PROOF (backlog 28ac45ab). `boss gate
//! --park-probe/--park-expect` writes the probe at park time and the
//! auto-park handler copies it onto the car as `proof_probe` /
//! `proof_expect` (the input half, feat/the-car-carries-its-probe).
//! Until this rule, every recorded probe still waited for a person to
//! type `boss prove <car> --from-car`: 43 landed cars sat at
//! `proven: ready` for 2.5 days, and 12 were proven by hand in one
//! sitting. A pr-train closing `arrived` is the moment every car
//! aboard is merged AND converged, so it is the moment the probe
//! means something — and this handler files one `ops-request` per
//! probed car, `host=forge verb=run-car-probe args=[car]`.
//!
//! WHERE THE PROBE RUNS, AND WHY NOT HERE. The dispatcher's image does
//! carry a shell, so a shell-out from this handler would compile. It
//! is the wrong seat three times over: the dispatcher is the queue
//! watcher — clock, threshold, matchmaking, nothing else (David,
//! 2026-08-14) — and running a probe is an actor's work, not routing;
//! a probe that hangs would hold a JetStream consumer, and the
//! dispatcher owes the rest of the system its liveness; and the
//! vantage a probe needs (kubectl as the converge user, the converged
//! checkout, the forge journal, the SoR over the LAN) lives on the
//! forge host, not in the `boss` namespace. The forge already answers
//! ops-request packets through a reviewed verb allowlist
//! (`infra/ops/verbs.json`), so the run goes through that door:
//! `infra/forge/run-car-probe.sh` re-reads the car, refuses unless it
//! has merged and recorded a probe, runs the probe as `david` (never
//! root) with a timeout, judges it by the two rules `boss prove`
//! applies, and writes the verdict on the car — `proven` completed
//! with the recorded proof shape on green, `proof_attempt` on the car
//! and `proven` left ready otherwise.
//!
//! WHAT IT LEAVES ALONE. A car with no `proof_probe` — including an
//! EVENT-BOUND car that recorded `proof_event` instead — stays
//! hand-proven; a car already proven is not re-proven; a car that
//! already has an open run-car-probe request is not asked twice
//! (JetStream is at-least-once and the close marker is emitted from
//! three sites, so this WILL run more than once per arrival).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::car;

use super::common::{api_client, get_json, post_json};

/// The allowlisted verb (`infra/ops/verbs.json`) and the host that
/// answers it. One definition each, read by the request builder and
/// the twice-guard.
pub const VERB: &str = "run-car-probe";
pub const HOST: &str = "forge";

/// The car metadata key the probe rides under. Its ONE definition is
/// `boss_jobs::car::PROOF_PROBE`, which lands with the companion car
/// feat/the-car-carries-its-probe; this branch is gated alone against
/// main, where that symbol does not yet exist, so the literal is
/// repeated here for exactly as long as the two cars are apart.
/// COLLAPSE once both have landed: replace with `car::PROOF_PROBE`
/// and delete this constant (CLAUDE.md 9a).
const PROOF_PROBE: &str = "proof_probe";

pub struct JobsRunCarProbes {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsRunCarProbes {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// Every open Job of `kind`, paged on `total` so a car sorted past
    /// one page is still found — the same paging `boss prove` does,
    /// for the same reason (a capped page is a false negative that
    /// grows with the pipeline's age).
    async fn all_open(&self, kind: &str, rule: &str) -> Result<Vec<Value>, HandlerError> {
        const PAGE: usize = 500;
        let mut rows: Vec<Value> = Vec::new();
        loop {
            let body = get_json(
                &self.client,
                &format!(
                    "{}/api/jobs?kind={kind}&status=open&limit={PAGE}&offset={}",
                    self.base(),
                    rows.len()
                ),
                rule,
            )
            .await?;
            let total = body.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
            let page: Vec<Value> = body
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let got = page.len();
            rows.extend(page);
            if got == 0 || rows.len() >= total {
                break;
            }
        }
        Ok(rows)
    }
}

/// PURE: the id of the train that just ARRIVED, or `None` for every
/// other close this shared topic carries (the rule's `when` filters
/// them; the handler re-checks so it is correct on its own terms).
pub(crate) fn arrived_train(payload: &Value) -> Option<&str> {
    if payload.get("kind").and_then(Value::as_str) != Some("pr-train")
        || payload.get("outcome").and_then(Value::as_str) != Some("arrived")
    {
        return None;
    }
    payload.get("id").and_then(Value::as_str)
}

fn md<'a>(job: &'a Value, key: &str) -> Option<&'a str> {
    job.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// PURE: is this car's `proven` step still waiting to be filled?
fn proven_is_open(car: &Value) -> bool {
    car::find_step(car, "proven", "Proven in prod")
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|s| matches!(s, "ready" | "active"))
}

/// PURE: the ops-request bodies to file for one arrival — one per car
/// aboard `train_id` that recorded a probe, still has `proven` open,
/// and has no run-car-probe request already open. Everything the
/// forge script needs rides in metadata: `host`/`verb`/`args` for the
/// ops-runner, `car`/`branch`/`train` for a reader.
pub(crate) fn probe_requests(
    train_id: &str,
    cars: &[Value],
    open_requests: &[Value],
    rule_name: &str,
    event_id: &str,
    topic: &str,
) -> Vec<Value> {
    let already_asked = |car_id: &str| {
        open_requests
            .iter()
            .any(|r| md(r, "verb") == Some(VERB) && md(r, "car") == Some(car_id))
    };
    cars.iter()
        .filter(|c| md(c, "train") == Some(train_id))
        .filter(|c| md(c, PROOF_PROBE).is_some())
        .filter(|c| proven_is_open(c))
        .filter_map(|c| {
            let id = c.get("id").and_then(Value::as_str)?;
            if already_asked(id) {
                return None;
            }
            let title = c.get("title").and_then(Value::as_str).unwrap_or("");
            Some(json!({
                "kind": "ops-request",
                "title": format!("run the recorded probe for {title}"),
                // The CAR is the subject: what this packet is about, and
                // the cheap handle for anyone asking "was this car's
                // probe run?"
                "subject": {"subject_kind": "custom", "id": id},
                "owner_id": format!("rule:{rule_name}"),
                "priority": "standard",
                "status": "open",
                "tags": ["dispatcher-spawned"],
                "metadata": {
                    "host": HOST,
                    "verb": VERB,
                    "args": [id],
                    "car": id,
                    "branch": md(c, "branch").unwrap_or(""),
                    "train": train_id,
                    "spawned_by_rule": rule_name,
                    "triggered_by_event_id": event_id,
                    "triggered_by_topic": topic,
                },
            }))
        })
        .collect()
}

#[async_trait]
impl Handler for JobsRunCarProbes {
    fn name(&self) -> &'static str {
        "jobs.run-car-probes"
    }

    async fn invoke(
        &self,
        _args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let Some(train_id) = arrived_train(&ctx.event_payload) else {
            return Ok(());
        };
        let cars = self.all_open("ship-a-change", &ctx.rule_name).await?;
        let open_requests = self.all_open("ops-request", &ctx.rule_name).await?;
        let bodies = probe_requests(
            train_id,
            &cars,
            &open_requests,
            &ctx.rule_name,
            &ctx.triggering_event_id,
            &ctx.triggering_topic,
        );
        for body in &bodies {
            post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                body,
                &ctx.rule_name,
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn car(id: &str, train: &str, extra: Value, proven: &str) -> Value {
        let mut m = json!({"branch": format!("feat/{id}"), "merged": "true", "train": train});
        if let (Some(dst), Some(src)) = (m.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        json!({
            "id": id, "kind": "ship-a-change", "status": "open",
            "title": format!("Car {id}"),
            "metadata": m,
            "steps": [{"spec_slug": "proven", "title": "Proven in prod", "status": proven}]
        })
    }

    fn probed() -> Value {
        json!({"proof_probe": "curl -s http://sor/api/yard | grep -c x", "proof_expect": "1"})
    }

    /// THE TRIGGER: only a pr-train closing `arrived` names a train.
    #[test]
    fn only_an_arrived_train_is_an_arrival() {
        let closed = |kind: &str, outcome: Value| json!({"id": "t1", "kind": kind, "outcome": outcome, "closed_on": "2026-09-08"});
        assert_eq!(
            arrived_train(&closed("pr-train", json!("arrived"))),
            Some("t1")
        );
        assert_eq!(arrived_train(&closed("pr-train", json!("cancelled"))), None);
        assert_eq!(arrived_train(&closed("pr-train", Value::Null)), None);
        assert_eq!(
            arrived_train(&closed("ship-a-change", json!("merged"))),
            None
        );
    }

    /// A boarded, probed car with `proven` open gets exactly one
    /// request, shaped for the ops-runner (host/verb/args) and for a
    /// reader (car/branch/train), with the car as its subject.
    #[test]
    fn a_probed_car_aboard_the_train_gets_one_bounded_request() {
        let cars = [car("c1", "t1", probed(), "ready")];
        let got = probe_requests(
            "t1",
            &cars,
            &[],
            "run-car-probes-on-train-arrived",
            "ev",
            "jobs.job.closed",
        );
        assert_eq!(got.len(), 1);
        let r = &got[0];
        assert_eq!(r["kind"], "ops-request");
        assert_eq!(r["subject"]["id"], "c1");
        assert_eq!(r["metadata"]["host"], "forge");
        assert_eq!(r["metadata"]["verb"], "run-car-probe");
        assert_eq!(r["metadata"]["args"], json!(["c1"]));
        assert_eq!(r["metadata"]["car"], "c1");
        assert_eq!(r["metadata"]["branch"], "feat/c1");
        assert_eq!(r["metadata"]["train"], "t1");
        assert_eq!(
            r["metadata"]["spawned_by_rule"],
            "run-car-probes-on-train-arrived"
        );
        assert!(r["title"].as_str().unwrap().contains("Car c1"));
    }

    /// Everything that must NOT be asked: another train's car, an
    /// event-bound car, a car with no proof intent at all, a car
    /// already proven, and a car already asked (at-least-once
    /// delivery).
    #[test]
    fn unprobed_foreign_proven_and_already_asked_cars_are_left_alone() {
        let cars = [
            car("other", "t2", probed(), "ready"),
            car(
                "event",
                "t1",
                json!({"proof_event": "event-bound — a stalled train"}),
                "ready",
            ),
            car("bare", "t1", json!({}), "ready"),
            car("done", "t1", probed(), "completed"),
            car("asked", "t1", probed(), "ready"),
            car("fresh", "t1", probed(), "active"),
        ];
        let open = [json!({
            "id": "r1", "kind": "ops-request", "status": "open",
            "metadata": {"host": "forge", "verb": "run-car-probe", "car": "asked"}
        })];
        let got = probe_requests("t1", &cars, &open, "r", "ev", "jobs.job.closed");
        let ids: Vec<&str> = got
            .iter()
            .map(|r| r["metadata"]["car"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["fresh"], "{ids:?}");
    }
}
