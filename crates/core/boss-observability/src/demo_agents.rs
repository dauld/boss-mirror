//! Synthetic agent oversight for tenants without real
//! `boss-cybernetics` deployed yet. A playground tenant uses this
//! so `/ops` shows what agent oversight LOOKS like before
//! anyone wires their first real agent in. Tenants leaving
//! `demo_agents` absent in the obs config get the real
//! aggregator path — this code is gated behind that flag.
//!
//! THE AGENTS ARE THE TENANT'S (backlog 1c68aebc, 2026-09-23). Until
//! then this module carried one example tenant's roster as a `json!`
//! literal — hop-sourcing scouts and tap-launch copywriters in a Tier
//! 1 crate every adopter ships, describing a business most adopters
//! do not run (CLAUDE.md §10). The mechanism stays here; the roster is a
//! [`Roster`] read from the file the `[demo_agents]` block names,
//! which lives in the tenant's own directory
//! (`examples/brewery/seeds/demo_agents.toml`).
//!
//! Two surfaces:
//!
//! 1. `snapshot()` returns canned VmResult-shaped JSON for
//!    /health, /agents, /queues, /runs, /costs so the
//!    `/api/snapshot` endpoint reads as a populated dashboard
//!    instead of an empty grid.
//!
//! 2. `spawn_telemetry_loop()` pushes a periodic stream of
//!    plausible cybernetics events into the SSE hub —
//!    dispatch-requested → started → completed cycles, periodic
//!    cost.recorded pings. Operators see live agent activity
//!    without any real LLM calls.

use std::path::Path;
use std::time::Duration;

use crate::aggregator::VmResult;
use crate::sse::SseHub;
use boss_core::event::Event;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::interval;
use tracing::info;

const VM_ID: &str = "demo-vm";

/// A tenant's demo roster: the agents the synthetic dashboard shows,
/// with the invented figures that make each panel read as populated.
/// Parsed from TOML `[[agent]]` rows; unknown keys are refused so a
/// misspelt figure is an error, not a silent zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    #[serde(rename = "agent")]
    pub agents: Vec<DemoAgent>,
}

/// One synthetic agent. The first six fields are the shape
/// boss-cybernetics answers on `/agents`; the rest feed the queues,
/// runs and costs panels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoAgent {
    pub id: String,
    pub display_name: String,
    pub system_prompt: String,
    pub model: String,
    pub hourly_budget_usd_micros: u64,
    pub max_concurrent_runs: u32,
    #[serde(default)]
    pub queue_depth: u32,
    /// A run shown as in flight, if any.
    #[serde(default)]
    pub run: Option<DemoRun>,
    pub cost_hour: DemoCost,
    pub cost_day: DemoCost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoRun {
    pub id: String,
    /// How long before the snapshot the run is shown as started.
    pub started_seconds_ago: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoCost {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub usd_micros: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RosterError {
    #[error("reading demo roster {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("demo roster is not valid: {0}")]
    Toml(#[from] toml::de::Error),
    /// The telemetry loop cycles the roster, so an empty one has
    /// nothing to cycle and the dashboard nothing to show.
    #[error("demo roster declares no [[agent]] rows")]
    Empty,
    #[error("demo roster declares agent id `{0}` twice")]
    DuplicateId(String),
}

impl Roster {
    /// Parse and validate a roster from its TOML text.
    pub fn parse(text: &str) -> Result<Self, RosterError> {
        let roster: Roster = toml::from_str(text)?;
        roster.validate()?;
        Ok(roster)
    }

    /// Read the roster the `[demo_agents]` block names.
    pub fn load(path: &Path) -> Result<Self, RosterError> {
        let text = std::fs::read_to_string(path).map_err(|source| RosterError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text)
    }

    fn validate(&self) -> Result<(), RosterError> {
        if self.agents.is_empty() {
            return Err(RosterError::Empty);
        }
        let mut seen = std::collections::HashSet::new();
        self.agents
            .iter()
            .find(|a| !seen.insert(a.id.as_str()))
            .map_or(Ok(()), |a| Err(RosterError::DuplicateId(a.id.clone())))
    }

    fn agent_specs(&self) -> Value {
        Value::Array(
            self.agents
                .iter()
                .map(|a| {
                    json!({
                        "id": a.id,
                        "display_name": a.display_name,
                        "system_prompt": a.system_prompt,
                        "model": a.model,
                        "hourly_budget_usd_micros": a.hourly_budget_usd_micros,
                        "max_concurrent_runs": a.max_concurrent_runs,
                    })
                })
                .collect(),
        )
    }

    fn queues(&self) -> Value {
        Value::Array(
            self.agents
                .iter()
                .map(|a| json!({ "agent": a.id, "depth": a.queue_depth }))
                .collect(),
        )
    }

    fn runs(&self, now: DateTime<Utc>) -> Value {
        Value::Array(
            self.agents
                .iter()
                .filter_map(|a| a.run.as_ref().map(|r| (a, r)))
                .map(|(a, r)| {
                    let started = now - chrono::Duration::seconds(r.started_seconds_ago);
                    json!({ "id": r.id, "agent": a.id, "started_at": started.to_rfc3339() })
                })
                .collect(),
        )
    }

    /// Every agent's hour window, then every agent's day window — the
    /// order the dashboard has always received.
    fn costs(&self) -> Value {
        let window = |name: &str, pick: fn(&DemoAgent) -> &DemoCost| {
            self.agents
                .iter()
                .map(|a| json!({ "agent": a.id, "cost": pick(a), "window": name }))
                .collect::<Vec<_>>()
        };
        let mut rows = window("hour", |a| &a.cost_hour);
        rows.extend(window("day", |a| &a.cost_day));
        Value::Array(rows)
    }
}

/// Build the five VmResult arrays the SPA expects. Each carries
/// a single demo VM with status 200; the body matches what
/// boss-cybernetics would return for a real VM.
///
/// `demo_mode: true` is included at the top level so the SPA's
/// `/ops` view can render an honest banner distinguishing the
/// synthetic surface from a real cybernetics deployment. Without
/// it, a first-time visitor would see "4 agents · $X/hr token
/// spend" and reasonably conclude the tenant is running real LLM
/// workers.
pub fn snapshot(roster: &Roster) -> serde_json::Value {
    snapshot_at(roster, Utc::now())
}

/// [`snapshot`] at a given instant — pure, so it can be tested.
pub fn snapshot_at(roster: &Roster, now: DateTime<Utc>) -> serde_json::Value {
    json!({
        "demo_mode": true,
        "health": [vm_result(json!({
            "vm_id": VM_ID,
            "status": "healthy",
            "timestamp": now.to_rfc3339(),
        }))],
        "agents": [vm_result(roster.agent_specs())],
        "queues": [vm_result(roster.queues())],
        "runs": [vm_result(roster.runs(now))],
        "costs": [vm_result(roster.costs())],
    })
}

fn vm_result(body: Value) -> Value {
    serde_json::to_value(VmResult {
        vm_id: VM_ID.to_string(),
        status: 200,
        body,
        error: None,
    })
    .unwrap_or(Value::Null)
}

/// Spawn a tokio task that pushes a periodic stream of synthetic
/// cybernetics telemetry events into the SSE hub. The cycle walks
/// dispatch.requested → dispatch.started → dispatch.completed
/// across the roster's agents, interleaved with cost.recorded
/// pings. Cancels cleanly when the watch flips.
pub fn spawn_telemetry_loop(
    hub: SseHub,
    roster: Roster,
    tick_seconds: u64,
    cancel: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        info!(
            tick_seconds,
            agents = roster.agents.len(),
            "demo-agents telemetry loop starting"
        );
        let mut tick = interval(Duration::from_secs(tick_seconds.max(1)));
        // Skip the first immediate tick so the first batch lands
        // `tick_seconds` after startup, not at startup.
        tick.tick().await;
        let mut counter: u64 = 0;
        let mut cancel = cancel;
        loop {
            tokio::select! {
                _ = cancel.changed() => {
                    info!("demo-agents telemetry loop stopping");
                    break;
                }
                _ = tick.tick() => {
                    // `Roster::load` refuses an empty roster, so the
                    // modulus is never zero; `get` keeps it panic-free
                    // for a roster built some other way.
                    let Some(demo) = roster
                        .agents
                        .get((counter as usize) % roster.agents.len().max(1))
                    else {
                        break;
                    };
                    let agent = demo.id.as_str();
                    let phase = counter % 3;
                    let kind = match phase {
                        0 => "cybernetics.dispatch.requested",
                        1 => "cybernetics.dispatch.started",
                        _ => "cybernetics.dispatch.completed",
                    };
                    let run_id = format!(
                        "run-{}-{:04}",
                        agent.split('-').next_back().unwrap_or("?"),
                        counter / 3,
                    );
                    let payload = match phase {
                        0 => json!({
                            "agent": agent,
                            "run_id": run_id,
                            "trigger_topic": "step.done.handoff",
                        }),
                        1 => json!({
                            "agent": agent,
                            "run_id": run_id,
                            "model": demo.model,
                        }),
                        _ => json!({
                            "agent": agent,
                            "run_id": run_id,
                            "duration_ms": 1_400 + (counter % 800),
                            "input_tokens": 4_200 + (counter * 31 % 4_000),
                            "output_tokens": 600 + (counter * 17 % 800),
                            "usd_micros": 8_400 + (counter * 47 % 16_000),
                        }),
                    };
                    // Synthetic demo loop — wall-clock is the
                    // intended source here. These events never land
                    // in audit_log; they only feed the /ops
                    // dashboard's live ticker on tenants without a
                    // real boss-cybernetics deployment.
                    hub.publish(Event::new(
                        "boss-cybernetics/demo-vm",
                        kind,
                        payload,
                        Utc::now(),
                    ));

                    // After every completed phase, sneak in a
                    // cost.recorded ping so the cost panel ticks
                    // as well.
                    if phase == 2 {
                        let cost = json!({
                            "agent": agent,
                            "run_id": run_id,
                            "input_tokens": 4_200,
                            "output_tokens": 720,
                            "usd_micros": 8_400,
                        });
                        hub.publish(Event::new(
                            "boss-cybernetics/demo-vm",
                            "cybernetics.cost.recorded",
                            cost,
                            Utc::now(),
                        ));
                    }

                    counter = counter.wrapping_add(1);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A roster unlike the example tenant's: the mechanism must render
    /// whatever tenant it is handed.
    const LAB: &str = r#"
[[agent]]
id = "agent-sample-triage"
display_name = "Sample triage"
system_prompt = "Route incoming samples to a bench."
model = "claude-haiku-4-5"
hourly_budget_usd_micros = 100_000
max_concurrent_runs = 1
queue_depth = 3
run = { id = "run-triage-007", started_seconds_ago = 30 }
cost_hour = { input_tokens = 10, output_tokens = 20, usd_micros = 30 }
cost_day = { input_tokens = 100, output_tokens = 200, usd_micros = 300 }

[[agent]]
id = "agent-plate-reader"
display_name = "Plate reader"
system_prompt = "Summarise plate reads."
model = "claude-sonnet-4-6"
hourly_budget_usd_micros = 200_000
max_concurrent_runs = 2
cost_hour = { input_tokens = 1, output_tokens = 2, usd_micros = 3 }
cost_day = { input_tokens = 4, output_tokens = 5, usd_micros = 6 }
"#;

    fn body<'a>(snap: &'a Value, panel: &str) -> &'a Value {
        &snap[panel][0]["body"]
    }

    #[test]
    fn the_snapshot_is_rendered_from_the_roster_it_is_handed() {
        let roster = Roster::parse(LAB).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-09-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let snap = snapshot_at(&roster, now);

        assert_eq!(snap["demo_mode"], json!(true));
        assert_eq!(
            body(&snap, "agents")[0],
            json!({
                "id": "agent-sample-triage",
                "display_name": "Sample triage",
                "system_prompt": "Route incoming samples to a bench.",
                "model": "claude-haiku-4-5",
                "hourly_budget_usd_micros": 100_000,
                "max_concurrent_runs": 1,
            })
        );
        assert_eq!(
            body(&snap, "queues"),
            &json!([
                { "agent": "agent-sample-triage", "depth": 3 },
                { "agent": "agent-plate-reader", "depth": 0 },
            ])
        );
        // Only an agent with a `run` is shown in flight, dated from `now`.
        assert_eq!(
            body(&snap, "runs"),
            &json!([{
                "id": "run-triage-007",
                "agent": "agent-sample-triage",
                "started_at": "2026-09-23T11:59:30+00:00",
            }])
        );
        let windows: Vec<(&str, &str)> = body(&snap, "costs")
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["agent"].as_str().unwrap(), c["window"].as_str().unwrap()))
            .collect();
        assert_eq!(
            windows,
            [
                ("agent-sample-triage", "hour"),
                ("agent-plate-reader", "hour"),
                ("agent-sample-triage", "day"),
                ("agent-plate-reader", "day"),
            ]
        );
        assert_eq!(
            body(&snap, "costs")[2]["cost"],
            json!({ "input_tokens": 100, "output_tokens": 200, "usd_micros": 300 })
        );
    }

    #[test]
    fn an_empty_roster_is_refused() {
        assert!(matches!(
            Roster::parse("agent = []").unwrap_err(),
            RosterError::Empty
        ));
    }

    #[test]
    fn a_duplicate_agent_id_is_refused_by_name() {
        let twice = format!("{LAB}\n{}", &LAB[LAB.find("[[agent]]").unwrap()..]);
        let err = Roster::parse(&twice).unwrap_err();
        assert!(
            matches!(&err, RosterError::DuplicateId(id) if id == "agent-sample-triage"),
            "{err}"
        );
    }

    #[test]
    fn a_misspelt_figure_is_refused_not_zeroed() {
        let typo = LAB.replacen("queue_depth", "queue_dept", 1);
        let err = Roster::parse(&typo).unwrap_err();
        assert!(err.to_string().contains("queue_dept"), "{err}");
    }

    #[test]
    fn a_missing_roster_file_names_its_path() {
        let err = Roster::load(Path::new("/nonexistent/demo_agents.toml")).unwrap_err();
        assert!(
            err.to_string().contains("/nonexistent/demo_agents.toml"),
            "{err}"
        );
    }
}
