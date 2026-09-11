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
//! vantage a probe needs (the converged checkout, the forge journal,
//! the SoR over the LAN) lives on the forge host, not in the `boss`
//! namespace. What the forge does NOT have is the cluster's vantage:
//! no kubectl, no kubeconfig. A probe is written on the dev pod and
//! run there, so it can be correct and unrunnable — measured on this
//! rule's first live run (f9304366), where both cars came back as an
//! exit code with empty streams. `boss gate` now refuses a probe
//! naming a tool in infra/forge/host-absent-tools.txt, and the runner
//! records `unrunnable` with the tool named rather than a bare exit
//! code. This rule does not restate either refusal; it applies the one
//! of them nothing downstream can — see [`ship_refusal`], which is
//! where that call is argued rather than described (23b2dffa).
//! The forge already answers
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

use super::common::{api_client, get_json, post_json, write_json};

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

/// What one arrival produces: requests to file, and cars this door will
/// not ship.
#[derive(Debug, Default)]
pub(crate) struct Arrival {
    /// The ops-request bodies to file, one per probed car.
    pub requests: Vec<Value>,
    /// Cars whose recorded probe this rule refuses, each with the
    /// `proof_attempt` to record on the car — `(car id, attempt)`. A
    /// refusal nobody can read is the same as not checking, so every one
    /// of these is written where the car's other proof evidence lives.
    pub refusals: Vec<(String, Value)>,
}

/// WHY THIS DOOR RE-CHECKS ONE RULE AND NOT THE OTHER (backlog
/// 23b2dffa, settled here rather than described).
///
/// `boss gate --park-probe` refuses both shapes of bad probe at park
/// time, so most cars reaching this rule were already checked. Most is
/// not all: a car's `proof_probe` can be written straight onto it with a
/// metadata PATCH (a documented door), and cars parked before the
/// gate-side refusals existed still carry whatever they carried. So the
/// question is what this rule owes a probe no gate ever saw.
///
/// THE ABSENT-TOOL RULE IS NOT RE-CHECKED, deliberately. The forge
/// runner catches it empirically: fd 9 collects every command bash could
/// not resolve, on a channel the probe's own redirections cannot reach,
/// and the attempt is stamped `unrunnable` with the tool named. A
/// measurement of the actual host beats this handler predicting it from
/// a manifest — and a prediction that is wrong (a tool installed since
/// the list was measured) would strand a car with nobody watching.
///
/// THE UNIDENTIFIED READ IS RE-CHECKED, because nothing downstream can.
/// The probe runs, policy answers a NARROWER WORLD in silence, the
/// absence assertion passes, and the runner records it as a PROOF on a
/// car that then closes (61085a9e). By the time the text reaches the
/// forge it is too late for anything but the refusal, and this is the
/// last place that can make one.
fn ship_refusal(probe: &str, expect: Option<&str>) -> Option<Value> {
    let client = boss_jobs::probe::reads_the_sor_unidentified(probe)?;
    // No `at`: this rule holds no clock (the dispatcher's time comes
    // from the clock port, which this handler does not carry), and the
    // PATCH that records the attempt is itself an audit-log event with
    // one. The same attempt re-written on a redelivered arrival is
    // idempotent by content, which is what at-least-once needs.
    Some(json!({
        "refused": boss_jobs::probe::UNIDENTIFIED_RULE,
        "probe": probe,
        "expect": expect.unwrap_or(""),
        "unrunnable": false,
        "why": format!(
            "THE PROBE WAS NOT RUN: it reads the system of record with `{client}` and no \
             identity, so it would read as operator:unidentified and be answered with a \
             NARROWER WORLD, silently. {evidence} Re-park the car with a probe that reads as \
             a named reader ({reader} /api/...), or prove it by hand.",
            evidence = boss_jobs::probe::UNIDENTIFIED_READ_EVIDENCE,
            reader = boss_jobs::probe::SOR_READER,
        ),
    }))
}

/// PURE: the ops-request bodies to file for one arrival — one per car
/// aboard `train_id` that recorded a probe, still has `proven` open,
/// and has no run-car-probe request already open. Everything the
/// forge script needs rides in metadata: `host`/`verb`/`args` for the
/// ops-runner, `car`/`branch`/`train` for a reader.
///
/// A car whose recorded probe this door refuses ([`ship_refusal`]) is
/// not shipped, and comes back in `refusals` so the reason lands on the
/// car instead of vanishing.
pub(crate) fn probe_requests(
    train_id: &str,
    cars: &[Value],
    open_requests: &[Value],
    rule_name: &str,
    event_id: &str,
    topic: &str,
) -> Arrival {
    let already_asked = |car_id: &str| {
        open_requests
            .iter()
            .any(|r| md(r, "verb") == Some(VERB) && md(r, "car") == Some(car_id))
    };
    let mut refusals: Vec<(String, Value)> = Vec::new();
    let requests = cars
        .iter()
        .filter(|c| md(c, "train") == Some(train_id))
        .filter(|c| md(c, PROOF_PROBE).is_some())
        .filter(|c| proven_is_open(c))
        .filter_map(|c| {
            let id = c.get("id").and_then(Value::as_str)?;
            if already_asked(id) {
                return None;
            }
            if let Some(probe) = md(c, PROOF_PROBE)
                && let Some(attempt) = ship_refusal(probe, md(c, car::PROOF_EXPECT))
            {
                refusals.push((id.to_string(), attempt));
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
        .collect();
    Arrival { requests, refusals }
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
        let arrival = probe_requests(
            train_id,
            &cars,
            &open_requests,
            &ctx.rule_name,
            &ctx.triggering_event_id,
            &ctx.triggering_topic,
        );
        // The refusals go first, and they are recorded rather than
        // logged: a car this rule will not probe must LOOK like one, on
        // the car, beside the attempts the forge runner writes — a
        // refusal only a journal knows about is indistinguishable from
        // a rule that never ran (CLAUDE.md §Diagnosis).
        for (car_id, attempt) in &arrival.refusals {
            write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{car_id}/metadata", self.base()),
                &json!({"proof_attempt": attempt}),
                &ctx.rule_name,
            )
            .await?;
        }
        for body in &arrival.requests {
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

    /// A car carrying a GOOD probe: the named reader the forge puts on
    /// the probe's PATH. This fixture used to be
    /// `curl -s http://sor/api/yard | grep -c x` — exactly the shape
    /// 61085a9e measured, an unidentified read answered with a narrower
    /// world — so the fixture was itself an example of the defect.
    fn probed() -> Value {
        json!({"proof_probe": "boss-sor-read /api/yard/status | grep -q dock_depth",
               "proof_expect": "dock_depth"})
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
        assert_eq!(got.requests.len(), 1);
        let r = &got.requests[0];
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
            .requests
            .iter()
            .map(|r| r["metadata"]["car"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["fresh"], "{ids:?}");
    }

    /// THE RULE WITH NO BACKSTOP DOWNSTREAM (backlog 23b2dffa). The
    /// forge runner catches a missing TOOL empirically — fd 9 collects
    /// what bash could not resolve — so a probe naming `kubectl` comes
    /// back as `unrunnable` with the tool named, which is better than
    /// any prediction this handler could make. Nothing downstream
    /// catches an UNIDENTIFIED READ: the probe runs, policy answers a
    /// narrower world, the absence assertion passes, and the runner
    /// records a green proof of nothing (61085a9e). So this door checks
    /// that one rule, on the car's own recorded text, and refuses to
    /// ship it — because a car can carry a probe no gate ever saw (a
    /// metadata PATCH is a door, and cars parked before the gate-side
    /// refusal existed still carry theirs).
    #[test]
    fn a_car_whose_probe_reads_the_sor_unidentified_is_refused_not_shipped() {
        let bad = json!({
            "proof_probe": "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=gate-run | grep -q c0ffee",
            "proof_expect": "c0ffee",
        });
        let cars = [car("c1", "t1", bad, "ready")];
        let got = probe_requests("t1", &cars, &[], "r", "ev", "jobs.job.closed");
        assert!(
            got.requests.is_empty(),
            "a probe that reads unidentified must not be shipped to the forge: {:?}",
            got.requests
        );
        assert_eq!(got.refusals.len(), 1);
        let (id, attempt) = &got.refusals[0];
        assert_eq!(id, "c1");
        assert_eq!(attempt["refused"], boss_jobs::probe::UNIDENTIFIED_RULE);
        assert!(
            attempt["why"]
                .as_str()
                .unwrap_or_default()
                .contains("unidentified"),
            "the attempt must say what was wrong: {attempt}"
        );
        assert_eq!(
            attempt["probe"],
            "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=gate-run | grep -q c0ffee"
        );
    }

    /// And the absent-tool rule is deliberately NOT re-checked here: the
    /// runner measures the actual host, which is strictly better than
    /// this handler predicting it from a manifest. A `kubectl` probe is
    /// still shipped, and comes back `unrunnable` naming the tool.
    #[test]
    fn a_probe_naming_a_tool_the_forge_lacks_is_still_shipped_for_the_runner_to_measure() {
        let cars = [car(
            "c1",
            "t1",
            json!({"proof_probe": "kubectl -n boss get deploy | grep -q boss-jobs",
                   "proof_expect": "boss-jobs"}),
            "ready",
        )];
        let got = probe_requests("t1", &cars, &[], "r", "ev", "jobs.job.closed");
        assert_eq!(got.requests.len(), 1);
        assert!(got.refusals.is_empty());
    }
}
