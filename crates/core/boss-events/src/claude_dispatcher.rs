//! [`AgentDispatcher`] backed by `claude` CLI (Claude Code).
//!
//! Each agent run spawns `claude --print` as a subprocess with the agent's
//! system prompt, model, and budget. The message payload is piped as the
//! user prompt. Output is captured as the response, and token usage is
//! parsed from the JSON output format.
//!
//! Requires `claude` to be on $PATH and authenticated (OAuth or API key).

use async_trait::async_trait;
use boss_core::agent::{
    AgentId, AgentSpec, ClaimedMessage, Cost, Outcome, RunCompletion, RunHandle, RunId, RunStatus,
};
use boss_core::port::{AgentDispatcher, DispatchError, RunCompletions};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tracing::{error, info, warn};

/// Dispatches agent runs by invoking `claude --print` as a subprocess.
#[derive(Clone)]
pub struct ClaudeCodeDispatcher {
    running: Arc<Mutex<HashMap<RunId, RunHandle>>>,
    concurrency: Arc<Mutex<HashMap<AgentId, u32>>>,
    completions_tx: broadcast::Sender<RunCompletion>,
    /// Path to the claude binary. Defaults to "claude" (on $PATH).
    claude_bin: String,
}

impl ClaudeCodeDispatcher {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            running: Arc::new(Mutex::new(HashMap::new())),
            concurrency: Arc::new(Mutex::new(HashMap::new())),
            completions_tx: tx,
            claude_bin: std::env::var("BOSS_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()),
        }
    }

    pub fn with_binary(mut self, path: impl Into<String>) -> Self {
        self.claude_bin = path.into();
        self
    }
}

impl Default for ClaudeCodeDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentDispatcher for ClaudeCodeDispatcher {
    async fn dispatch(
        &self,
        spec: &AgentSpec,
        message: ClaimedMessage,
    ) -> Result<RunHandle, DispatchError> {
        // Enforce per-agent concurrency cap.
        {
            let mut conc = self.concurrency.lock().await;
            let current = conc.entry(spec.id.clone()).or_insert(0);
            if *current >= spec.max_concurrent_runs {
                return Err(DispatchError::CapacityExceeded(spec.id.clone()));
            }
            *current += 1;
        }

        let handle = RunHandle {
            run_id: RunId::new(),
            agent: spec.id.clone(),
            message_id: message.message.id,
            claim_id: message.claim_id,
            started_at: Utc::now(),
            status: RunStatus::Starting,
        };

        self.running
            .lock()
            .await
            .insert(handle.run_id, handle.clone());

        // Spawn the Claude subprocess asynchronously.
        let running = self.running.clone();
        let concurrency = self.concurrency.clone();
        let tx = self.completions_tx.clone();
        let run_id = handle.run_id;
        let agent = spec.id.clone();
        let system_prompt = spec.system_prompt.clone();
        let model = spec.model.clone();
        let budget_usd = spec.hourly_budget_usd_micros as f64 / 1_000_000.0;
        let payload = message.message.payload.clone();
        let claude_bin = self.claude_bin.clone();
        let mut finished_handle = handle.clone();

        tokio::spawn(async move {
            // Mark as Running.
            if let Some(h) = running.lock().await.get_mut(&run_id) {
                h.status = RunStatus::Running;
            }

            info!(
                agent = %agent,
                run_id = %run_id,
                "starting claude subprocess"
            );

            // Build the user prompt from the message payload.
            let user_prompt = serde_json::to_string_pretty(&payload).unwrap_or_default();

            // Invoke claude CLI.
            // Note: --bare is NOT used because it disables OAuth auth.
            let result = tokio::process::Command::new(&claude_bin)
                .args([
                    "--print",
                    "--output-format",
                    "json",
                    "--system-prompt",
                    &system_prompt,
                    "--model",
                    &model,
                    "--max-budget-usd",
                    &format!("{budget_usd:.2}"),
                    "--dangerously-skip-permissions",
                    "--no-session-persistence",
                    &user_prompt,
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .await;

            let outcome = match result {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);

                    if !output.status.success() {
                        error!(
                            agent = %agent,
                            status = %output.status,
                            stderr = %stderr,
                            "claude subprocess failed"
                        );
                        Outcome::Failed {
                            // It ran and then failed, so it spent what
                            // it spent; the JSON that would say how much
                            // never arrived (backlog c6e2341c).
                            cost: Cost::UNPRICED,
                            error: format!(
                                "claude exited with {}: {}",
                                output.status,
                                stderr.chars().take(500).collect::<String>()
                            ),
                        }
                    } else {
                        // Parse the JSON output to extract response and cost.
                        let (response, cost) = parse_claude_output(&stdout);
                        info!(
                            agent = %agent,
                            input_tokens = cost.input_tokens,
                            output_tokens = cost.output_tokens,
                            "claude run completed"
                        );
                        Outcome::Success { cost, response }
                    }
                }
                Err(e) => {
                    error!(agent = %agent, error = %e, "failed to spawn claude");
                    Outcome::Failed {
                        cost: Cost::ZERO,
                        error: format!("spawn error: {e}"),
                    }
                }
            };

            // Determine final status.
            finished_handle.status = match &outcome {
                Outcome::Success { .. } => RunStatus::Completed,
                Outcome::Failed { .. } => RunStatus::Failed,
                Outcome::Cancelled => RunStatus::Cancelled,
            };

            // Clean up tracking state.
            running.lock().await.remove(&run_id);
            if let Some(c) = concurrency.lock().await.get_mut(&agent) {
                *c = c.saturating_sub(1);
            }

            let _ = tx.send(RunCompletion {
                run: finished_handle,
                outcome,
                finished_at: Utc::now(),
            });
        });

        Ok(handle)
    }

    async fn running(&self) -> Result<Vec<RunHandle>, DispatchError> {
        Ok(self.running.lock().await.values().cloned().collect())
    }

    async fn cancel(&self, run_id: &RunId) -> Result<(), DispatchError> {
        let mut running = self.running.lock().await;
        let handle = running
            .remove(run_id)
            .ok_or(DispatchError::UnknownRun(*run_id))?;
        if let Some(c) = self.concurrency.lock().await.get_mut(&handle.agent) {
            *c = c.saturating_sub(1);
        }
        let mut h = handle;
        h.status = RunStatus::Cancelled;
        let _ = self.completions_tx.send(RunCompletion {
            run: h,
            outcome: Outcome::Cancelled,
            finished_at: Utc::now(),
        });
        Ok(())
    }

    async fn completions(&self) -> Result<Box<dyn RunCompletions>, DispatchError> {
        Ok(Box::new(BroadcastCompletions {
            rx: self.completions_tx.subscribe(),
        }))
    }
}

/// Parse Claude CLI JSON output to extract the response text and token usage.
///
/// Claude `--output-format json` produces:
/// ```json
/// {
///   "type": "result",
///   "result": "response text...",
///   "total_cost_usd": 0.029,
///   "usage": { "input_tokens": 10, "output_tokens": 253, ... }
/// }
/// ```
fn parse_claude_output(stdout: &str) -> (serde_json::Value, Cost) {
    let parsed: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => {
            warn!("could not parse claude JSON output, treating as plain text");
            // The run happened and spent something; we just cannot read
            // what. UNPRICED, not ZERO (backlog c6e2341c).
            return (serde_json::json!({ "text": stdout }), Cost::UNPRICED);
        }
    };

    let response = if let Some(result) = parsed.get("result") {
        serde_json::json!({ "text": result })
    } else {
        parsed.clone()
    };

    // Token counts are nested under "usage".
    let usage = parsed.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Total cost is at the top level — and when it is absent, the
    // run is unpriced, not free (backlog c6e2341c).
    let usd_micros = parsed
        .get("total_cost_usd")
        .and_then(|v| v.as_f64())
        .map(|usd| (usd * 1_000_000.0) as u64);

    (
        response,
        Cost {
            input_tokens,
            output_tokens,
            usd_micros,
        },
    )
}

struct BroadcastCompletions {
    rx: broadcast::Receiver<RunCompletion>,
}

#[async_trait]
impl RunCompletions for BroadcastCompletions {
    async fn next(&mut self) -> Option<RunCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(c) => return Some(c),
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_output_without_a_total_cost_is_unpriced_not_free() {
        // backlog c6e2341c. `total_cost_usd` is absent whenever the CLI
        // was not told to price the run; reading that as $0.00 put a
        // measurement's authority on a number nobody reported.
        let (_, cost) = parse_claude_output(
            r#"{"result":"hi","usage":{"input_tokens":10,"output_tokens":253}}"#,
        );
        assert_eq!(cost.input_tokens, 10);
        assert_eq!(cost.output_tokens, 253);
        assert_eq!(cost.usd_micros, None, "unpriced is not free");

        let (_, priced) = parse_claude_output(
            r#"{"result":"hi","total_cost_usd":0.029,"usage":{"input_tokens":10,"output_tokens":253}}"#,
        );
        assert_eq!(priced.usd_micros, Some(29_000));

        // Output we cannot parse came from a run that DID happen.
        let (_, unreadable) = parse_claude_output("not json at all");
        assert_eq!(unreadable.usd_micros, None);
    }
}
