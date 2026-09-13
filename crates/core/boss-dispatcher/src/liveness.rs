//! Live readiness of the dispatcher's two durable JetStream consumers — the
//! signal a `/api/dispatcher/health` 200 cannot give you.
//!
//! `boss-dispatcher` spawns both consumer loops detached: if either's message
//! stream ends — a NATS blip, or JetStream not ready at cold start under a
//! launcher with NO per-service restart (the docker `services-launcher.sh`
//! uses `wait -n`, unlike systemd which restarts a dead unit) — the loop just
//! logs and ends while the PROCESS stays up and health-green. With the
//! assignment consumer dead, ready Steps are never assigned, the workforce has
//! nothing to pull, and every Job stalls `open`: the exact "thousands of jobs
//! in flight, nothing reaching a terminal state" failure.
//!
//! This handle makes that visible at `/api/dispatcher/readyz` so an operator —
//! and the brewery sim's pre-Go readiness gate — can confirm both consumers are
//! actually bound and draining before trusting the stack with work.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// Shared, lock-free liveness for the dispatcher's consumers. One instance is
/// created in `main`, cloned into each consumer loop (which marks it), and
/// cloned into the HTTP surface (which reads it for `/readyz`).
#[derive(Default)]
pub struct DispatcherLiveness {
    /// `dispatcher.rs::run_loop`'s `dispatcher-steps` consumer is bound + draining.
    assigning: AtomicBool,
    /// Step events the assignment loop has handled since bind (monotonic).
    assignment_events: AtomicU64,
    /// The rules runner's `dispatcher-rules` consumer is bound + draining.
    rules_running: AtomicBool,
    /// Side-effect events the rules runner has handled since bind (monotonic).
    rules_events: AtomicU64,
    /// The schedule runner's clock-stream loop is live (consuming ticks).
    /// Unlike the two JetStream consumers this is an SSE loop, but it's the
    /// same liveness question: a process that's health-200 but whose
    /// schedule loop died fires zero clock-driven rules (payroll, tax,
    /// sweeps stop). `true` only matters when the registry HAS schedule
    /// rules — with none, the runner never starts and this stays false.
    schedule_running: AtomicBool,
    /// Sim-days the schedule runner has fired since start (monotonic).
    schedule_events: AtomicU64,
    /// Wall-clock unix seconds of the most recently handled event (0 = none yet).
    last_event_unix: AtomicI64,
    /// Dead-letters since start (monotonic): handler failures past their
    /// budget, or permanent. Counted HERE — a local read — because the
    /// durable record (an annotation on the packet) needs the jobs API,
    /// which is exactly what a dead-letter often reports broken
    /// (8834804a). Six topics carry no packet at all; for those and for
    /// a failed annotation write, this number is the only record that
    /// outlives the log line.
    dead_letters: AtomicU64,
    /// Of those, how many left NO durable record: no packet to annotate,
    /// no sink configured, or the annotation write failed.
    dead_letters_unrecorded: AtomicU64,
    /// Wall-clock unix seconds of the most recent dead-letter (0 = none).
    last_dead_letter_unix: AtomicI64,
    /// Wall-clock unix seconds of the most recent UNRECORDED dead-letter
    /// (0 = none). Its own stamp, because the estate chain's finding
    /// (`estate.compare`, 8834804a) is "an unrecorded one happened
    /// recently" judged against the observation's stamp — and a
    /// recorded dead-letter moving `last_dead_letter_unix` would keep a
    /// stale unrecorded one looking fresh.
    last_unrecorded_dead_letter_unix: AtomicI64,
}

impl DispatcherLiveness {
    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Called by `run_loop` once its durable consumer is bound.
    pub fn mark_assigning(&self) {
        self.assigning.store(true, Ordering::Relaxed);
    }
    /// Called by `run_loop` when its message stream ends (consumer dead).
    pub fn mark_assign_stopped(&self) {
        self.assigning.store(false, Ordering::Relaxed);
    }
    /// Called by `run_loop` per handled step event.
    pub fn record_assignment(&self) {
        self.assignment_events.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix
            .store(Self::now_unix(), Ordering::Relaxed);
    }

    /// Called by the rules runner once its durable consumer is bound.
    pub fn mark_rules_running(&self) {
        self.rules_running.store(true, Ordering::Relaxed);
    }
    /// Called by the rules runner when its message stream ends.
    pub fn mark_rules_stopped(&self) {
        self.rules_running.store(false, Ordering::Relaxed);
    }
    /// Called by the rules runner per handled side-effect event.
    pub fn record_rules(&self) {
        self.rules_events.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix
            .store(Self::now_unix(), Ordering::Relaxed);
    }

    /// Called by the schedule runner once its clock-stream loop is live.
    pub fn mark_schedule_running(&self) {
        self.schedule_running.store(true, Ordering::Relaxed);
    }
    /// Called by the schedule runner when its clock stream ends.
    pub fn mark_schedule_stopped(&self) {
        self.schedule_running.store(false, Ordering::Relaxed);
    }
    /// Called by the schedule runner per sim-day it fires.
    /// One dead-letter happened; `recorded` says whether a durable record
    /// (the packet annotation) landed for it.
    pub fn record_dead_letter(&self, recorded: bool) {
        let now = Self::now_unix();
        self.dead_letters.fetch_add(1, Ordering::Relaxed);
        if !recorded {
            self.dead_letters_unrecorded.fetch_add(1, Ordering::Relaxed);
            self.last_unrecorded_dead_letter_unix
                .store(now, Ordering::Relaxed);
        }
        self.last_dead_letter_unix.store(now, Ordering::Relaxed);
    }

    pub fn record_schedule(&self) {
        self.schedule_events.fetch_add(1, Ordering::Relaxed);
        self.last_event_unix
            .store(Self::now_unix(), Ordering::Relaxed);
    }

    /// JSON body for `/api/dispatcher/readyz`. `ready` is true only when BOTH
    /// durable consumers are bound — the precondition for a Job to flow
    /// step-ready → assigned → worked → side-effects → closed.
    pub fn snapshot(&self) -> serde_json::Value {
        let assigning = self.assigning.load(Ordering::Relaxed);
        let rules_running = self.rules_running.load(Ordering::Relaxed);
        // `ready` stays gated on the two JetStream consumers only — the
        // schedule runner is optional (it never starts when the registry
        // has no schedule rules), so gating readiness on it would falsely
        // mark a schedule-free deployment not-ready. Its liveness is
        // reported as informational fields instead.
        serde_json::json!({
            "ready": assigning && rules_running,
            "assigning": assigning,
            "assignment_events": self.assignment_events.load(Ordering::Relaxed),
            "rules_running": rules_running,
            "rules_events": self.rules_events.load(Ordering::Relaxed),
            "schedule_running": self.schedule_running.load(Ordering::Relaxed),
            "schedule_events": self.schedule_events.load(Ordering::Relaxed),
            "last_event_unix": self.last_event_unix.load(Ordering::Relaxed),
            "dead_letters": self.dead_letters.load(Ordering::Relaxed),
            "dead_letters_unrecorded": self.dead_letters_unrecorded.load(Ordering::Relaxed),
            "last_dead_letter_unix": self.last_dead_letter_unix.load(Ordering::Relaxed),
            "last_unrecorded_dead_letter_unix": self.last_unrecorded_dead_letter_unix.load(Ordering::Relaxed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The finding the estate chain raises on (8834804a) is "an
    /// unrecorded dead-letter happened RECENTLY", judged against the
    /// observation's own stamp. `last_dead_letter_unix` cannot carry
    /// that: a recorded dead-letter after an unrecorded one would move
    /// it and keep an old unrecorded one looking fresh. So the
    /// unrecorded ones get a stamp of their own, which a recorded one
    /// leaves alone.
    #[test]
    fn an_unrecorded_dead_letter_has_its_own_stamp_that_a_recorded_one_does_not_move() {
        let live = DispatcherLiveness::default();
        let snap = live.snapshot();
        assert_eq!(snap["last_unrecorded_dead_letter_unix"], 0, "{snap}");

        live.record_dead_letter(true);
        let snap = live.snapshot();
        assert!(
            snap["last_dead_letter_unix"].as_i64().unwrap_or(0) > 0,
            "{snap}"
        );
        assert_eq!(
            snap["last_unrecorded_dead_letter_unix"], 0,
            "a recorded dead-letter is not an unrecorded one: {snap}"
        );

        live.record_dead_letter(false);
        let snap = live.snapshot();
        let unrecorded_at = snap["last_unrecorded_dead_letter_unix"]
            .as_i64()
            .unwrap_or(0);
        assert!(unrecorded_at > 0, "{snap}");
        assert_eq!(snap["dead_letters"], 2, "{snap}");
        assert_eq!(snap["dead_letters_unrecorded"], 1, "{snap}");

        // Push the stamp into the past and record a RECORDED one: the
        // unrecorded stamp must not follow it.
        live.last_unrecorded_dead_letter_unix
            .store(unrecorded_at - 3600, Ordering::Relaxed);
        live.record_dead_letter(true);
        let snap = live.snapshot();
        assert_eq!(
            snap["last_unrecorded_dead_letter_unix"],
            unrecorded_at - 3600,
            "{snap}"
        );
    }
}
