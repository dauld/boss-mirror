//! A dead-letter lands on its packet.
//!
//! THE FAILURE THIS ANSWERS (filed `a9c498eb`, 2026-09-11). When a
//! handler fails past its redelivery budget the runner dead-letters:
//! loudly, to the log, and NOWHERE ELSE. So the packet whose step the
//! handler was supposed to complete is left exactly as it was — the step
//! sits `ready`, indistinguishable from a step whose turn has not come —
//! and because the log lives in the boss pod, a converge roll erases the
//! only record.
//!
//! Measured instance: `maintenance-sweep` packet `f8dedadf` (target
//! `empty-decisions`) opened 00:00:07Z with a rule
//! (`inspect-empty-decisions-sweep-on-step-ready`) that exists precisely
//! to complete its `Inspect` checklist. At 02:00Z the step was still
//! `ready`, the pod had rolled twice, and WHICH attempt failed and WHY
//! were no longer establishable from anything. The cost compounds: sweep
//! protocols guard their daily spawn on `NOT open_job_exists`, so one
//! sweep stuck at a non-terminal step silently retires its own cadence
//! (`cf0f5e2d`) — four sweeps accumulated that way for nine days.
//!
//! Two CLAUDE.md §Diagnosis rules name this. **A troubled packet must
//! look troubled**: a step a machine owed and did not deliver must say
//! so where the next reader is, which is the packet. **Quiet is not free**:
//! the runner holds the rule, the handler, the attempt count and the
//! error at that moment and threw all four away.
//!
//! ## What this module is
//!
//! - [`annotation_target`] — pure: which job (and step) an event's
//!   dead-letter can be landed on, or `None`.
//! - [`annotation_patch`] — pure: the `PATCH /api/jobs/{id}/metadata`
//!   body. Top-level-merge semantics, so it cannot damage the packet.
//! - [`DeadLetterSink`] — the port the runner calls. One implementation
//!   here ([`JobsApiDeadLetters`]); tests use a recording fake.
//!
//! ## Best-effort, deliberately
//!
//! The annotation is itself a jobs-API write, and the jobs API is a thing
//! that can be down — possibly the very reason the handler failed. So it
//! is **best-effort**: its failure is logged and discarded, it never
//! changes the settle decision, and it can never re-enter the redelivery
//! budget it is reporting on. CLAUDE.md: *"An arm that needs the patient
//! is not an arm"* — visibility is best-effort everywhere else in this
//! system for the same reason, and the runner must keep draining whether
//! or not it can annotate.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

/// One handler's failure, as the note records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerFailure {
    pub rule: String,
    pub handler: String,
    pub error: String,
}

impl std::fmt::Display for HandlerFailure {
    /// The same `rule/handler: error` vocabulary the runner's log line
    /// and `Settle` message use — one shape for operators to read.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}: {}", self.rule, self.handler, self.error)
    }
}

/// Why the event is dead-lettering now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadLetterClass {
    /// Every redelivery the budget allows has failed.
    BudgetExhausted,
    /// A deterministic data error: identical on every redelivery, so the
    /// runner terminates without spending the budget.
    Permanent,
}

impl DeadLetterClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BudgetExhausted => "budget-exhausted",
            Self::Permanent => "permanent",
        }
    }
}

/// Everything the runner holds at the moment it dead-letters.
#[derive(Debug, Clone)]
pub struct DeadLetterNote {
    pub topic: String,
    pub event_id: String,
    /// 1-based delivery count — the attempt that gave up.
    pub attempts: u32,
    pub class: DeadLetterClass,
    /// Every handler that failed on this event, in declaration order.
    /// ALL of them, not a reduction: a multi-handler topic
    /// (`step.done.shipment`, `step.done.production-produce`) can fail
    /// more than one, and the copy stored here is the only one that
    /// outlives the pod.
    pub failures: Vec<HandlerFailure>,
    /// Inherited from the triggering event's `_simulated`, the same read
    /// [`super::handler::dispatch`] makes — so an annotation about
    /// simulated work is simulated state.
    pub simulated: bool,
}

/// The packet a dead-letter can be landed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub job_id: String,
    /// The step the handler owed, when the event names one. This is the
    /// step a reader will find sitting `ready`.
    pub step_id: Option<String>,
}

/// Which packet (if any) this event's dead-letter belongs on.
///
/// TWO SHAPES, both read off the emit sites rather than guessed:
///
/// - Step events — `step.ready.*`, `step.done.*`, `step.assigned.*` —
///   carry the packet as `job_id` and the step as `step_id`
///   (`boss_jobs::http::steps`, and the contract
///   `boss-dispatcher-handlers::handlers::common::StepEvent` requires).
/// - `jobs.job.closed` carries the packet as `id` on all three of its
///   emit sites (`boss_jobs::http::steps`, `http::jobs` ×2).
///
/// Everything else the registry binds — `commerce.invoice.*`,
/// `inventory.*`, `ledger.*`, `jobs.estate.*` — names a subject that is
/// not a packet. Those return `None` and the dead-letter stays a loud log
/// line. **Reading their `id` as a job id would annotate whatever packet
/// happened to share that uuid**, which is worse than not annotating: a
/// fabricated target is a false record, and this module exists to make
/// the record true.
///
/// `job_id` is taken by name wherever it appears, because in this system
/// that key means a packet on every topic that carries it.
pub fn annotation_target(topic: &str, payload: &Value) -> Option<Target> {
    let obj = payload.as_object()?;
    let str_at = |key: &str| {
        obj.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let job_id = match str_at("job_id") {
        Some(id) => id,
        None if topic == boss_jobs::events::JOB_CLOSED => str_at("id")?,
        None => return None,
    };
    Some(Target {
        job_id,
        step_id: str_at("step_id"),
    })
}

/// The metadata key every dead-letter annotation writes.
pub const METADATA_KEY: &str = "dead_letter";

/// The `PATCH /api/jobs/{id}/metadata` body for one dead-letter.
///
/// IDEMPOTENCE — one key, overwritten, not a list. The metadata door
/// merges TOP-LEVEL keys, so writing the single [`METADATA_KEY`] means a
/// redelivery (or a restart re-presenting the row with a fresh budget)
/// leaves one annotation's worth of metadata however many times it runs.
/// That is correct rather than merely convenient:
///
/// - A redelivery is the SAME lost delivery, not a second one, and the
///   later write strictly supersedes the earlier — higher attempt count,
///   latest error.
/// - Nothing is lost by overwriting: every metadata PATCH is recorded as
///   `jobs.job.updated` in `audit_log` inside the adapter's transaction,
///   so each annotation is in the system of record even after the key
///   moves on. The packet carries the CURRENT trouble; the log carries
///   the history.
/// - An uncapped list on a repeatedly-failing rule grows the packet
///   metadata without bound, and the packet is read by humans and by the
///   yard.
///
/// The flat `rule`/`handler`/`error` trio is the first (and all but
/// always the only) failure, hoisted so a reader sees what failed without
/// digging; `failures` always carries every one.
pub fn annotation_patch(
    note: &DeadLetterNote,
    recorded_at: chrono::DateTime<chrono::Utc>,
) -> Value {
    let first = note.failures.first();
    json!({
        METADATA_KEY: {
            "rule": first.map(|f| f.rule.clone()),
            "handler": first.map(|f| f.handler.clone()),
            "error": first.map(|f| f.error.clone()),
            "failures": note.failures.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "attempts": note.attempts,
            "class": note.class.as_str(),
            "topic": note.topic,
            "event_id": note.event_id,
            "recorded_at": recorded_at.to_rfc3339(),
        }
    })
}

/// Where a dead-letter is recorded so it outlives the pod.
///
/// A port, not a concrete client, for the usual reason plus one specific
/// to this arm: the runner's tests must be able to assert what a
/// dead-letter records, and to make the recording itself fail, without a
/// jobs API.
#[async_trait]
pub trait DeadLetterSink: Send + Sync {
    /// Record `note` against `target`. Returns the failure as a string so
    /// the caller can log it; the caller MUST NOT act on it (see the
    /// module's best-effort note).
    async fn record(&self, target: &Target, note: &DeadLetterNote) -> Result<(), String>;
}

/// How long the annotation write may take before it is abandoned.
///
/// The annotation rides the rules loop, which must keep draining. A jobs
/// API that accepts the connection and never answers would otherwise
/// stall the loop on every dead-letter — the arm holding up the patient.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The production sink: `PATCH /api/jobs/{id}/metadata` on the jobs API.
pub struct JobsApiDeadLetters {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsApiDeadLetters {
    /// Construct with the jobs-api base URL (e.g. `http://127.0.0.1:7900`).
    ///
    /// The client attaches the machine token the same way
    /// `boss-dispatcher-handlers::handlers::common::api_client` does, and
    /// adds [`WRITE_TIMEOUT`], which that shared client deliberately has
    /// no opinion about — a handler's call is the work, this one is
    /// visibility and must never outlive the settle it describes.
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        boss_core::machine_token::attach(&mut headers);
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(WRITE_TIMEOUT)
            .build()
            .unwrap_or_default();
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a caller-supplied client (tests point this at a
    /// local server).
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }
}

#[async_trait]
impl DeadLetterSink for JobsApiDeadLetters {
    async fn record(&self, target: &Target, note: &DeadLetterNote) -> Result<(), String> {
        let url = format!(
            "{}/api/jobs/{}/metadata",
            self.jobs_base.trim_end_matches('/'),
            target.job_id
        );
        // The actor is the rule whose handler failed: the write is a
        // consequence of that rule's fire, and the rule-as-actor model
        // says provenance names the registry row (see super::actor).
        let rule = note
            .failures
            .first()
            .map(|f| f.rule.as_str())
            .unwrap_or("unknown");
        let resp = self
            .client
            .patch(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", super::actor::dispatcher_actor_header(rule))
            .header(
                "x-sim-origin",
                if note.simulated { "true" } else { "false" },
            )
            // `wall_now()` — a local read, not a call to the clock API:
            // this arm must owe nothing to another service. A dead-letter
            // happened at a wall instant (the failure is real even when
            // the packet is simulated), and this is the sanctioned
            // wall-stamp source outside EventStamp.
            .json(&annotation_patch(note, boss_clock_client::wall_now()))
            .send()
            .await
            .map_err(|e| format!("PATCH {url}: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("PATCH {url} returned {status}: {body}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(failures: Vec<HandlerFailure>, attempts: u32) -> DeadLetterNote {
        DeadLetterNote {
            topic: "step.ready.checklist".into(),
            event_id: "evt-1".into(),
            attempts,
            class: DeadLetterClass::BudgetExhausted,
            failures,
            simulated: false,
        }
    }

    fn failure(rule: &str, handler: &str, error: &str) -> HandlerFailure {
        HandlerFailure {
            rule: rule.into(),
            handler: handler.into(),
            error: error.into(),
        }
    }

    #[test]
    fn a_step_event_names_its_packet_and_its_step() {
        let t = annotation_target(
            "step.ready.checklist",
            &json!({"job_id": "j-1", "step_id": "s-1", "kind": "checklist"}),
        )
        .expect("a step event carries job_id");
        assert_eq!(t.job_id, "j-1");
        assert_eq!(t.step_id.as_deref(), Some("s-1"));
    }

    /// `jobs.job.closed` carries the packet as `id` on all three emit
    /// sites — the one topic where `id` IS a job id.
    #[test]
    fn a_job_closed_event_names_its_packet_as_id() {
        let t = annotation_target("jobs.job.closed", &json!({"id": "j-2", "kind": "pr-train"}))
            .expect("jobs.job.closed carries id");
        assert_eq!(t.job_id, "j-2");
        assert_eq!(t.step_id, None);
    }

    /// The honest `None`: an `id` on a non-job topic is an invoice, a
    /// node, a filing — annotating the packet that happens to share the
    /// uuid would be a fabricated record.
    #[test]
    fn a_subject_that_is_not_a_packet_has_no_target() {
        for (topic, payload) in [
            ("commerce.invoice.paid", json!({"id": "inv-1"})),
            ("inventory.item.consumed", json!({"sku": "HOPS-1"})),
            ("jobs.estate.observed", json!({"id": "node-1"})),
        ] {
            assert_eq!(
                annotation_target(topic, &payload),
                None,
                "{topic} must not produce a target"
            );
        }
    }

    #[test]
    fn an_empty_job_id_is_not_a_target() {
        assert_eq!(
            annotation_target("step.done.task", &json!({"job_id": ""})),
            None
        );
        assert_eq!(annotation_target("step.done.task", &json!([])), None);
    }

    #[test]
    fn the_patch_names_the_rule_handler_attempts_and_error() {
        let n = note(
            vec![failure(
                "inspect-empty-decisions-sweep-on-step-ready",
                "maintenance.sweep.inspect",
                "GET /api/jobs returned 503",
            )],
            8,
        );
        let p = annotation_patch(&n, chrono::Utc::now());
        let dl = &p[METADATA_KEY];
        assert_eq!(dl["rule"], "inspect-empty-decisions-sweep-on-step-ready");
        assert_eq!(dl["handler"], "maintenance.sweep.inspect");
        assert_eq!(dl["attempts"], 8);
        assert_eq!(dl["error"], "GET /api/jobs returned 503");
        assert_eq!(dl["class"], "budget-exhausted");
        assert_eq!(dl["topic"], "step.ready.checklist");
        assert_eq!(dl["event_id"], "evt-1");
        assert!(dl["recorded_at"].is_string());
    }

    /// Every failure is kept. A multi-handler topic that fails two must
    /// not store one — the stored copy is the only one that outlives the
    /// pod.
    #[test]
    fn the_patch_keeps_every_failure() {
        let n = note(
            vec![
                failure("r-a", "h.a", "503"),
                failure("r-b", "h.b", "422 bad account"),
            ],
            8,
        );
        let p = annotation_patch(&n, chrono::Utc::now());
        let failures = p[METADATA_KEY]["failures"]
            .as_array()
            .expect("failures is an array");
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0], "r-a/h.a: 503");
        assert_eq!(failures[1], "r-b/h.b: 422 bad account");
    }

    /// IDEMPOTENCE, at the shape level: the patch is exactly one
    /// top-level key, so a merge of two of them is one annotation's worth
    /// of metadata — the later one.
    #[test]
    fn two_patches_merge_to_one_annotations_worth() {
        let mut metadata = serde_json::Map::new();
        metadata.insert("channel".into(), json!("monitoring"));
        let first = annotation_patch(&note(vec![failure("r", "h", "503")], 7), chrono::Utc::now());
        let second = annotation_patch(&note(vec![failure("r", "h", "503")], 8), chrono::Utc::now());
        // The server's merge: top-level keys replace wholesale.
        for patch in [&first, &second] {
            for (k, v) in patch.as_object().expect("patch is an object") {
                metadata.insert(k.clone(), v.clone());
            }
        }
        assert_eq!(
            metadata.len(),
            2,
            "the pre-existing key plus ONE dead_letter key: {metadata:?}"
        );
        assert_eq!(metadata["channel"], "monitoring");
        assert_eq!(metadata[METADATA_KEY]["attempts"], 8);
    }
}
