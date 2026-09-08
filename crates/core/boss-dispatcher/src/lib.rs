//! boss-dispatcher: auto-assigns ready Steps to role-matched Employees.
//!
//! ## The principle
//!
//! Step assignment is a SYSTEM concern, not a simulator concern. In a
//! pre-dispatcher world the brewery-sim picked employees at Step
//! creation time and embedded them in the POST body. That made the
//! "who does the work" decision a sim implementation detail — it
//! couldn't be exercised by a real operator opening Jobs through the
//! SPA. The dispatcher closes that gap: the same auto-assignment runs
//! whether the simulator OR a human opened the Job.
//!
//! ## Subscriptions + behavior
//!
//! Subscribes to NATS topic `jobs.step.>`. For each event:
//! - If `payload.status` is `ready` (or `active` as a defensive net)
//!   AND `payload.assignee_id` is null/empty, look up active Employees
//!   matching the Step's `authority_role` (falling back to the
//!   StepType's `required_roles`) and PUT one onto the Step via
//!   `PUT /api/jobs/{job_id}/steps/{step_id}` — unless the step's
//!   metadata declares `claimable` (the protocol asked for a role
//!   queue) or `human_only` (the protocol asked for a person —
//!   c17871fe), in which case it is left unassigned for a holder of
//!   its `authority_role` to claim.
//! - Which eligible holder gets the step is chosen by a **data-selected
//!   distribution strategy** (`BOSS_DISPATCH_STRATEGY`, default `spread`),
//!   per the registries/data-over-hardcoded-paths rule — the algorithm is
//!   not baked in. `spread` (default) fans assignments across a role's
//!   holders by a stable hash of the step id, so load distributes instead
//!   of piling onto one employee; `lowest-id` is the legacy behavior (the
//!   lowest-id holder), now one selectable strategy kept for
//!   parity/debugging. Every strategy is deterministic — the same
//!   (strategy, roster, step id) always picks the same employee, so an
//!   assignment replays identically across a rebuild. Adding a new strategy
//!   is a named variant + a branch in `pick_employee`, not a fork. Future:
//!   skill matching, on-shift filtering.
//!
//! ## What the dispatcher does NOT do
//!
//! - It does not drive Steps to completion; that's the simulator's job
//!   (for sim runs) or the human operator's (for real runs).
//! - It does not gate policy; existing policy checks fire on the PUT.
//! - It does not retry indefinitely; transient errors log + move on.
//!   A future tick will re-emit the step.updated event if the dispatch
//!   never landed (or an admin can manually re-run /api/dispatcher/sweep).

pub mod cascade;
pub mod config;
pub mod dispatcher;
pub mod http;
pub mod liveness;
pub mod rules;
