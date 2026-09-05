//! Job Kind Registry — see docs/architecture-decisions.md §Jobs,
//! Workflows, Steps.
//!
//! Every Job is an instance of a Workflow. This module defines the
//! registry shape (`WorkflowSpec`), the port every adapter implements
//! (`WorkflowRegistry`), and the in-memory + postgres adapters.
//!
//! The registry is append-only: every edit to a kind creates a new
//! `(kind, version+1)` row. Only one row per `kind` has
//! `status = Active` at a time, enforced by a partial unique index
//! on the Postgres side and by an invariant in the in-memory adapter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::step_registry::{Completion, StepRegistry};
use async_trait::async_trait;
use boss_core::job::{JobId, Step, StepId, StepStatus, Subject};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowStatus {
    Draft,
    Active,
    Retired,
}

impl WorkflowStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Retired => "retired",
        }
    }
}

impl std::str::FromStr for WorkflowStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "draft" => Ok(Self::Draft),
            "active" => Ok(Self::Active),
            "retired" => Ok(Self::Retired),
            other => Err(format!("unknown job kind status: {other}")),
        }
    }
}

/// One step in a Workflow: a typed unit of work plus the predicate
/// that decides when it becomes eligible to run.
///
/// `title` is a stable kebab-case slug, unique within the Workflow.
/// Predicates reference it as `steps.<title>.done` /
/// `steps.<title>.metadata.<field>`; the implicit step DAG is
/// recovered from which titles each `ready_when` mentions. The
/// human-facing label comes from `title_template` (or a humanized
/// `title` when that's blank).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StepSpec {
    /// Stable slug + predicate identifier, unique within the
    /// Workflow. Kebab-case (`mash-in`, `cfo-approval`).
    pub title: String,
    /// StepType slug from the StepType registry.
    pub kind: String,
    /// Predicate over the workflow's state — `true` when this step
    /// is eligible to advance from `pending` to `ready`. Evaluated
    /// by the shared `boss-expr` DSL against `(subject, job
    /// metadata, prior step states)`. `"true"` marks a trigger
    /// step that fires at Job open. Vocabulary:
    /// docs/architecture-decisions.md §Jobs, Workflows, Steps.
    pub ready_when: String,
    /// Marks this step as an outcome. Reaching `Completed` on a
    /// terminal step closes the Job and stamps the outcome label.
    /// Multiple terminals per Workflow is normal (success /
    /// rejection / abandonment paths all terminate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<Terminal>,
    /// Human display label; `{subject.<field>}` + `{day…}` tokens
    /// expand at materialization. Blank → humanized `title`.
    #[serde(default)]
    pub title_template: String,
    /// Role codes that must each stamp the materialized step in its
    /// current shape before completion (the sign-off contract). The marker
    /// `"@authority_role"` resolves to this step's `authority_role`
    /// at materialization.
    #[serde(default)]
    pub sign_offs_required: Vec<String>,
    /// The weakest stamp this step accepts. Protocol data: raising a
    /// step to `presence` is a Workflow edit, not a deploy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assurance_required: Option<boss_core::job::Assurance>,
    /// How long this step's work actually takes, in hours. Executors
    /// pacing a step (the sim workforce's duration-gated completion)
    /// prefer this over the StepType kind's `typical_duration_hours` —
    /// a `task`-kind fermentation ferments for the 168h its Workflow
    /// says, not the kind's one-workday default. `None` means "the
    /// kind's typical duration", so every existing spec is unchanged.
    /// Protocol data, not a code path (§9): correcting a duration is a
    /// Workflow edit, not a deploy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_hours: Option<f64>,
    /// The split of `duration_hours` into its two real meanings
    /// (d64fe2d2, David's Q2: "we can split those two times").
    ///
    /// `labor_hours` is what a PERSON spends — the input to per-person
    /// capacity, where the Q3 norm caps one person at 8 labor-hours a
    /// day. `wall_clock_hours` is what the CALENDAR spends —
    /// fermentation holds 168h of wall clock and ~0 of labor; an ACH
    /// window is days of calendar and minutes of attention.
    ///
    /// Executors pace by the wall-clock leg (`wall_clock_hours`, then
    /// `duration_hours`, then the kind's typical) and meter capacity
    /// by `labor_hours` ONLY where it is authored — an unauthored spec
    /// meters nothing, so every existing Workflow is unchanged (the
    /// Q3 rider: realism is a configuration expectation reviewed at
    /// protocol-authoring time, not a sweep invariant). Protocol data,
    /// not a code path (§9): correcting either is a Workflow edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labor_hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_clock_hours: Option<f64>,
    /// Step-authored completion-contract fields (inline
    /// authoring) — validated in union with the kind bundle's fields,
    /// so vocabulary that isn't shared needs no registry row.
    #[serde(default)]
    pub fields: Vec<boss_core::job::StepField>,
    #[serde(default)]
    pub authority_role: Option<String>,
    /// Leave this step UNASSIGNED for whoever holds `authority_role`
    /// to claim, instead of the dispatcher handing it to one person.
    ///
    /// The dispatcher's role-assignment loop resolves a role-gated
    /// step to a single eligible employee and assigns it. Where a role
    /// has one holder that is not routing, it is a permanent
    /// nomination: every `platform-admin` step in the system is
    /// assigned to the same person, and nobody else — human or agent —
    /// can pick one up even when they hold the role. Measured
    /// 2026-08-18: all six open design reviews assigned to `emp-david`,
    /// none claimable, which is why answered reviews still read as his
    /// queue.
    ///
    /// CLAIMABILITY IS NOT AUTHORITY, which is what makes this safe.
    /// `authority_role` still decides who MAY claim and complete; this
    /// only decides whether the packet arrives pre-nominated or waits
    /// in a role queue. Nothing here widens what an actor is permitted
    /// to do, and policy is still enforced at the claim.
    ///
    /// Protocol data, not a code path (§9): making a step claimable is
    /// a Workflow edit, not a deploy. `None` means today's behaviour,
    /// so every existing spec is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimable: Option<bool>,
    #[serde(default)]
    pub metadata_defaults: serde_json::Value,
}

/// An outcome marker on a terminal step. Reaching `Completed` on a
/// step that carries one closes the Job with this outcome label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminal {
    /// User-facing outcome: "succeeded" | "rejected" | "abandoned"
    /// | "cancelled" + domain-specific ("paid", "shipped",
    /// "skipped", "resold").
    pub outcome: String,
}

/// A cross-Job trigger declaration. When a Job of the parent
/// Workflow transitions to `closed`, the runtime spawns one new Job
/// per entry in `on_complete_create`.
///
/// A rule like "completing a wholesale-order Job creates an invoice
/// Job" lives as a row on the parent Workflow, the same way the step
/// graph does — not as a branch in core code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobTrigger {
    /// Workflow slug to spawn. Must resolve to an active Workflow
    /// row when the trigger fires; an unresolvable kind logs a
    /// warning and the trigger is skipped (not fatal).
    pub kind: String,
    /// Where the new Job's subject comes from:
    ///
    /// - `"same"` (default) — reuse the closing Job's subject
    ///   verbatim. Common for chained workflows that share an
    ///   anchor (a sale Job and its invoice both reference the
    ///   same customer).
    /// - `"metadata:<key>"` — read the closing Job's
    ///   `metadata.<key>` as a string id; the new Job's
    ///   subject_kind comes from the spawned Workflow's first
    ///   `subject_kinds` entry.
    #[serde(default = "default_subject_source")]
    pub subject_source: String,
    /// Static metadata to seed onto the new Job. Values may use
    /// `{closing.metadata.<key>}` placeholders; the runtime expands
    /// them at trigger time. Empty object = no seed.
    #[serde(default)]
    pub metadata_seed: serde_json::Value,
}

fn default_subject_source() -> String {
    "same".to_string()
}

/// A full registry row. All fields serialize directly to the
/// `workflows` JSONB columns with the same names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSpec {
    pub kind: String,
    pub version: i32,
    pub status: WorkflowStatus,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub category: String,
    pub subject_kinds: Vec<String>,
    /// The flat set of steps. The DAG is implicit in the steps'
    /// `ready_when` predicates — no separate tier / edge structure.
    pub steps: Vec<StepSpec>,
    #[serde(default)]
    pub metadata_schema: serde_json::Value,
    #[serde(default)]
    pub entitlements: serde_json::Value,
    /// Workflow-level display/routing hints — distinct from the
    /// per-Job metadata and from `metadata_schema` (which describes
    /// per-Job fields). The first key is `surfaces`: which
    /// operational pages a Workflow appears on. Same
    /// `serde_json::Value` round-trip shape as `metadata_schema` /
    /// `entitlements`; serializes to the `metadata` JSONB column.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Workflows to spawn when a Job of this kind closes. The
    /// runtime that fires triggers reads this list off the row;
    /// data shape is shipped here, the runtime hook lands in a
    /// follow-up. Empty list = no triggers (the common case).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_complete_create: Vec<JobTrigger>,
    pub owning_team: String,
    #[serde(default)]
    pub authoring_job_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl WorkflowSpec {
    /// Build a system-owned active v1 row for seeding.
    pub fn platform_seed(
        kind: impl Into<String>,
        label: impl Into<String>,
        category: impl Into<String>,
        subject_kinds: Vec<String>,
        steps: Vec<StepSpec>,
    ) -> Self {
        Self {
            kind: kind.into(),
            version: 1,
            status: WorkflowStatus::Active,
            label: label.into(),
            description: None,
            category: category.into(),
            subject_kinds,
            steps,
            metadata_schema: serde_json::json!({}),
            entitlements: serde_json::json!({}),
            metadata: serde_json::json!({}),
            on_complete_create: Vec::new(),
            owning_team: "platform".to_string(),
            authoring_job_id: None,
            created_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Platform kinds — code-resident Workflows that the bootstrap
// reconciler upserts into the live registry on every
// boss-jobs-api start. The single platform kind today is
// `workflow-design`, the meta-kind that authors every other
// kind in the registry. See docs/architecture-decisions.md
// §Jobs, Workflows, Steps (Workflows bootstrap through Jobs).
// ---------------------------------------------------------------------------

/// Build the canonical `workflow-design` WorkflowSpec.
///
/// Step graph (tier-major, default edges between adjacent tiers):
/// 0. `task`              — Author spec
/// 1. `task`              — Validate (lint via `validate_all`)
/// 2. `sign-off`          — Approve (authority_role = `workflow-approver`)
/// 3. `workflow-publish`  — Publish (writes to registry, emits
///    `jobs.kind.published`)
///
/// Subject discriminator is `custom`, with `custom_kind =
/// "workflow"` per Q3 of the design doc — reuses the existing
/// CustomSubject support without forcing a new Subject variant
/// in `boss-core`.
#[cfg(test)]
fn workflow_design_spec() -> WorkflowSpec {
    let steps = vec![
        StepSpec {
            title: "author".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Author WorkflowSpec".into(),
            ..Default::default()
        },
        StepSpec {
            title: "validate".into(),
            kind: "task".into(),
            ready_when: "steps.author.done".into(),
            title_template: "Validate spec".into(),
            // The next step is a SIGN-OFF, and required metadata is
            // checked at COMPLETION — so the constraint that guarantees
            // the approver has something to read belongs HERE, not
            // there. On the sign-off it would refuse only after the
            // human had already opened an empty screen. Phase 4 of the
            // viability lint refuses the alternative.
            fields: vec![boss_core::job::StepField {
                name: "sign_off_context".into(),
                field_type: "string".into(),
                required: true,
                filled_by: boss_core::job::FilledBy::Executor,
                item_keys: Vec::new(),
            }],
            ..Default::default()
        },
        StepSpec {
            title: "approve".into(),
            kind: "sign-off".into(),
            ready_when: "steps.validate.done".into(),
            title_template: "Approve spec".into(),
            // Approval authority is `workflow-approver` — an operational-
            // leadership capability granted (via tenant policy) to the
            // C-suite/COO/dept-heads who own the `workflows` authoring
            // surface, plus platform-admin (core policy default). NOT
            // platform-admin alone: authoring a work-type is the
            // operational leaders' job, not solely the deploy operator's.
            sign_offs_required: vec!["workflow-approver".into()],
            assurance_required: None,
            authority_role: Some("workflow-approver".into()),
            metadata_defaults: serde_json::json!({ "authority_role": "workflow-approver" }),
            // 2026-08-31, cdfe2e1a: the decision must LEAVE a record
            // (workflow_lint Phase 5) — required at completion, on the
            // step itself, unlike `sign_off_context` above which
            // guards arrival on the predecessor. This function is the
            // frozen conversion reference for the bundle-faithfulness
            // test, so a deliberate post-conversion evolution rides
            // through BOTH — the double edit is the price of keeping
            // the transcription guard byte-exact, and it is also load-
            // bearing: bootstrap republishes bundle drift, so a bundle
            // left behind would have regressed the live v3 back to
            // field-less on the next boot.
            fields: vec![boss_core::job::StepField {
                name: "decision".into(),
                field_type: "pending|approved|rejected|changes-requested".into(),
                required: true,
                filled_by: boss_core::job::FilledBy::Executor,
                item_keys: Vec::new(),
            }],
            ..Default::default()
        },
        StepSpec {
            title: "publish".into(),
            kind: "workflow-publish".into(),
            ready_when: "steps.approve.done".into(),
            title_template: "Publish to registry".into(),
            terminal: Some(Terminal {
                outcome: "published".into(),
            }),
            ..Default::default()
        },
    ];

    let mut spec = WorkflowSpec::platform_seed(
        "workflow-design",
        "Design a Workflow",
        "platform",
        vec!["custom".into()],
        steps,
    );
    // Q7: the responsible human for platform meta-work. The approve
    // step's authority (`workflow-approver`) is a policy CAPABILITY,
    // not an employees.role value, so the step-authority fallback
    // can't resolve it — name the operator-baseline role explicitly.
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec.description = Some(
        "Meta-kind: every Workflow in the registry is authored by a Job of this kind. \
         The terminal `workflow-publish` step writes the spec into the registry and \
         emits `jobs.kind.published` into audit_log. See \
         docs/architecture-decisions.md (Jobs, Workflows, Steps)."
            .to_string(),
    );
    spec
}

/// Build the canonical `ship-a-change` WorkflowSpec.
///
/// Shipping is work, so it is a Job — the same argument feedback got.
/// What it buys here is different, though: a Job gives a change an
/// owner, a recorded decision about its boundary, and a place in the
/// same throughput view as everything else the team does. `/system/flow`
/// counts these against a cadence target without a second mechanism.
///
/// The Subject is a `custom` Subject whose id is the branch name, the
/// shape feedback uses for a route and design-doc-review uses for a
/// path. "What shipped on this branch" then answers from Subject
/// history rather than from a report someone writes.
///
/// ## Why `scope` comes first, and is gated on a person
///
/// The problem this kind exists to solve is that a PR's boundary gets
/// decided at the END, when whoever is working is tired and everything
/// is already entangled. The branch that added this spec is the
/// evidence: one PR carrying a guest sign-in, a dispatcher fix, a
/// ledger determinism fix and two new surfaces, because nothing ever
/// asked where it should have been cut.
///
/// So the first step is a human declaring what this change contains
/// and what it deliberately leaves out, BEFORE the work. `excludes` is
/// required for exactly that reason: naming what you are not doing is
/// the act that keeps a change small, and a field nobody has to fill
/// in would be filled in never. It is the split point, made a state
/// transition instead of a judgement call.
///
/// Step graph:
///  -1. `opened`  — someone started a change
///   0. `scope`   — declare the boundary (human-gated)
///   1. `build`   — the change, with the test that fails without it
///   2. `gate`    — everything green, and observed working
///   3. `review`  — opened for review, url recorded
///   999. `merged`/`abandoned` — outcomes
// Kept only as the fidelity test's expected value — see
// `the_platform_bundle_matches_the_specs_it_replaced`. Out of
// `platform_workflows()`, so the lib build has no caller: the kind
// now lives in infra/platform/workflows.toml and an operator edit
// to it survives a boot, which is the whole point of the move.
#[cfg(test)]
fn ship_a_change_spec() -> WorkflowSpec {
    let steps = vec![
        StepSpec {
            title: "opened".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            title_template: "Change started".into(),
            metadata_defaults: serde_json::json!({
                "trigger_kind": "operator",
                "trigger_name": "operator-starts-a-change",
            }),
            ..Default::default()
        },
        StepSpec {
            title: "scope".into(),
            kind: "task".into(),
            ready_when: "steps.opened.done".into(),
            title_template: "Declare the boundary".into(),
            // Human-gated for the same reason triage is: `task` carries
            // no required role, and an ungated ready step gets
            // role-matched and completed by the simulated workforce. A
            // scope nobody chose is worse than no scope step, because
            // the audit trail then says someone decided.
            authority_role: Some("platform-admin".into()),
            fields: vec![
                boss_core::job::StepField {
                    name: "summary".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                // Required, deliberately. See the doc comment: the
                // sentence that keeps a change small is the one about
                // what it is not doing, and an optional field for it
                // would be skipped every time under exactly the
                // conditions that need it.
                boss_core::job::StepField {
                    name: "excludes".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
            ],
            ..Default::default()
        },
        StepSpec {
            title: "build".into(),
            kind: "task".into(),
            ready_when: "steps.scope.done".into(),
            title_template: "Build it".into(),
            authority_role: Some("platform-admin".into()),
            fields: vec![
                // The test that fails without the change. Named rather
                // than checkboxed: "tests pass" is true of a change
                // with no test, and a name is something a reviewer can
                // go read.
                boss_core::job::StepField {
                    name: "test".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
            ],
            ..Default::default()
        },
        StepSpec {
            title: "gate".into(),
            kind: "task".into(),
            ready_when: "steps.build.done".into(),
            title_template: "Green, and observed working".into(),
            authority_role: Some("platform-admin".into()),
            fields: vec![
                boss_core::job::StepField {
                    name: "gates".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                // How the change was seen working on a running system
                // — or why there is nothing to observe. Required
                // because "the tests passed" and "the operator can use
                // it" came apart repeatedly: a deploy that copied a
                // stale binary, a fix reported from a green suite while
                // the running service still had the bug.
                boss_core::job::StepField {
                    name: "verified".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                // The gate's OWN account of the run, not the author's.
                //
                // `gates` and `verified` are prose, so "the gate was
                // green" has always been something the protocol takes
                // on trust. On 2026-08-17 a car asserted
                // `infra/gate.sh --auto green` while its crate did not
                // compile, and the train it boarded reddened twice
                // (742d1faa). Two more reds the same day were the
                // subtler version: the gate really did pass, on a
                // laptop, in a shape CI does not run — one suite that
                // had never seen `FORGEJO_ACTIONS`, one that had never
                // run against a clean tree whose HEAD is its own trunk.
                // Neither prose field would have shown that, because
                // the author did not know it either.
                //
                // `infra/gate.sh` now writes a receipt (see
                // `write_receipt`) recording the mode, the commit,
                // whether the tree was dirty, the host, whether any CI
                // marker was set, the free space, and every check with
                // its result. Paste it here.
                //
                // This is EVIDENCE, NOT ENFORCEMENT — nothing stops
                // someone typing a fiction into a string field, and
                // pretending otherwise would be the same trust the
                // prose fields already misplace. What it changes is
                // that the honest answer is now the easy one, and that
                // the fact which keeps catching us — WHERE the gate ran
                // — is written down by something other than the person
                // making the claim.
                boss_core::job::StepField {
                    name: "receipt".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                // What the change LOOKS like, for a car that changes a
                // rendered surface — a screenshot path, or what was
                // rendered and looked at.
                //
                // Optional, because most cars change no surface and a
                // required field would be answered "n/a" into
                // meaninglessness. It exists because the protocol is a
                // better place to carry this than any one actor's
                // notes: on 2026-08-15 a UI change was "fixed" twice in
                // the wrong file and both diffs compiled, typechecked
                // and read plausibly. The tooling to render it was
                // already in the repo — `apps/web/playwright.mocked
                // .config.ts`, chromium installed — and one screenshot
                // named the mistake in a minute. Asking the question on
                // the step is what makes that habit belong to whoever
                // holds the car rather than to whoever happened to
                // learn it.
                boss_core::job::StepField {
                    name: "rendered".into(),
                    field_type: "string".into(),
                    required: false,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
            ],
            ..Default::default()
        },
        StepSpec {
            title: "review".into(),
            kind: "task".into(),
            ready_when: "steps.gate.done".into(),
            title_template: "Open for review".into(),
            authority_role: Some("platform-admin".into()),
            fields: vec![boss_core::job::StepField {
                name: "pr_url".into(),
                field_type: "string".into(),
                required: true,
                filled_by: boss_core::job::FilledBy::Executor,
                item_keys: Vec::new(),
            }],
            ..Default::default()
        },
        // Merged is not done. David, 2026-08-19, after a day of
        // "changes are done that are not visible in my UI experience":
        // *"Since I am using 'prod', I should have the definitive view
        // and proof that the change is fully deployed, which should be
        // the happy path terminal outcome."* The step's contract is
        // proof AT THE CONSUMING LAYER of the deployed system — a
        // browser check (infra/uxprobe) for anything with a surface,
        // endpoint/log evidence for anything without one. `verified`
        // is required at done so the proof is on the record, and
        // `method` names which kind of proof it was.
        //
        // Same trigger the terminal used to fire on: the conductor (or
        // a person) observed the merge. The terminal below now waits
        // for the proof instead.
        StepSpec {
            title: "proven".into(),
            kind: "task".into(),
            ready_when: "steps.review.done AND job.metadata.merged = \"true\"".into(),
            title_template: "Proven in prod".into(),
            authority_role: Some("platform-admin".into()),
            fields: vec![
                boss_core::job::StepField {
                    name: "verified".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                boss_core::job::StepField {
                    name: "method".into(),
                    field_type: "browser|api|log".into(),
                    required: false,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
            ],
            ..Default::default()
        },
        StepSpec {
            title: "merged".into(),
            kind: "outcome".into(),
            // The happy terminal fires on the PROOF, not the merge —
            // the outcome value stays "merged" so every consumer of
            // the close marker (the feedback obligation above all)
            // keeps matching, and now fires only once the change is
            // verified where the operator actually lives. The
            // conductor's own bookkeeping is untouched: it still
            // completes `review` and sets the merge marker, and the
            // packet then waits at `proven` instead of closing.
            ready_when: "steps.proven.done".into(),
            title_template: "Merged".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "completed" }),
            terminal: Some(Terminal {
                outcome: "merged".into(),
            }),
            ..Default::default()
        },
        // A change that gets abandoned is a real outcome, and the
        // cadence view should tell it apart from one still in flight.
        //
        // Gated on an explicit `job.metadata.abandoned` marker, NOT on
        // "scope is done". The first version used the latter and it
        // closed the very first Job filed against this Workflow: an
        // ungated terminal that is ready is a terminal the dispatcher
        // completes, so `complete-marker-on-step-ready` fired the
        // instant scope finished, skipped build/gate/review, and shut
        // the Job as abandoned seconds after it opened.
        //
        // An always-ready escape hatch is indistinguishable from "this
        // Job is finished". Abandoning has to be an act someone
        // performs, which is what the marker makes it.
        StepSpec {
            title: "abandoned".into(),
            kind: "outcome".into(),
            // BOTH halves are load-bearing. `steps.scope.done` is the
            // DAG edge — the viability lint rejects a step no trigger
            // can reach, and gating on metadata alone left this one
            // orphaned. The marker is what stops it being ready by
            // default, which is what let the dispatcher close a Job
            // the moment its scope was declared.
            ready_when: "steps.scope.done AND job.metadata.abandoned = \"true\"".into(),
            title_template: "Abandoned".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "aborted" }),
            terminal: Some(Terminal {
                outcome: "abandoned".into(),
            }),
            ..Default::default()
        },
    ];

    let mut spec = WorkflowSpec::platform_seed(
        "ship-a-change",
        "Ship a change",
        "platform",
        vec!["custom".into()],
        steps,
    );
    // Same owner as the other platform meta-kinds — and what puts
    // these Jobs on `/system/flow`, which selects by owner_role rather
    // than by a list of kinds.
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec.description = Some(
        "One change, from declaring its boundary to merging it. The Subject is a `custom` \
         Subject whose id is the branch, so \"what shipped here\" is a Subject-history \
         question. The `scope` step is the point of the kind: it asks a person what the \
         change contains and what it deliberately excludes BEFORE the work, which is the \
         only moment that decision keeps a PR small. Counted on /system/flow, so a cadence \
         target needs no second mechanism. Name the feedback packet this change answers in \
         `metadata.backlog_item` — a declared job edge, ref-checked at the write, and the \
         link the dispatcher follows on merge to complete that packet's open branch and \
         tell its filer. Use `metadata.backlog_text` only when the referent is not a Job \
         on this instance (legacy, or a request that arrived as prose); it is free text \
         and nothing follows it."
            .to_string(),
    );
    spec
}

/// Build the canonical `regenerate-deployment` WorkflowSpec.
///
/// A regen drops the database and rebuilds it: schema, seed, six
/// months of backfilled history, then live. It is the highest-stakes
/// routine operation the platform has — it destroys data on purpose,
/// takes hours, and every step of it lived in one person's head and a
/// shell script until now.
///
/// The Subject is the DEPLOYMENT (`custom`, id `/deployment/<name>`),
/// so "what regens has this box had, and why" is a Subject-history
/// question rather than something reconstructed from journald.
///
/// Every required field here is a thing that actually went wrong on
/// 2026-08-07, which is the only defensible reason to make a person
/// fill something in:
///
/// - `artifacts` exists because five separate stale-binary incidents
///   happened that day — a library built instead of a binary, a
///   binary that survived two build attempts, one copied over itself,
///   seventeen seed binaries that would have written the old schema
///   into the new database, and one whose mtime was NEWER than the
///   source change it was missing. Verifying artifacts is the step
///   everyone skips because the build said "Finished".
/// - `reset.destroying` is required because "clean start" should be a
///   decision recorded with the numbers, not a shrug. Say what is
///   being destroyed before destroying it.
/// - `backfill.went_live` is required because the transition is the
///   load-bearing claim of the whole design, and the temptation is to
///   assert it from a clean compile. It is confirmed by warp reaching
///   1.0 on a running clock or it is not confirmed.
/// - `verify.checks` is required because a sweep can pass vacuously —
///   the ledger replay-check went green that day on data that no
///   longer contained the case it had been failing on.
///
/// Step graph:
///  -1. `requested` — someone asked for a regen
///   0. `scope`     — why, and what it destroys (human-gated)
///   1. `build`     — release artifacts
///   2. `artifacts` — prove the built thing is the deployed thing
///   3. `deploy`    — install, including the non-service binaries
///   4. `reset`     — the destructive step (human-gated)
///   5. `backfill`  — synthetic past, then the go-live transition
///   6. `verify`    — schema, registry, invariants
///   999. `complete` / `abandoned`
// Kept only as the fidelity test's expected value — see
// `the_platform_bundle_matches_the_specs_it_replaced`. Out of
// `platform_workflows()`, so the lib build has no caller.
#[cfg(test)]
fn regenerate_deployment_spec() -> WorkflowSpec {
    /// A step in the chain, gated on its predecessor, carrying one
    /// required record of what was done.
    fn linked(title: &str, after: &str, label: &str, field: &str, human: bool) -> StepSpec {
        StepSpec {
            title: title.into(),
            kind: "task".into(),
            ready_when: format!("steps.{after}.done"),
            title_template: label.into(),
            authority_role: if human {
                Some("platform-admin".into())
            } else {
                None
            },
            fields: vec![boss_core::job::StepField {
                name: field.into(),
                field_type: "string".into(),
                required: true,
                filled_by: boss_core::job::FilledBy::Executor,
                item_keys: Vec::new(),
            }],
            ..Default::default()
        }
    }

    let steps = vec![
        StepSpec {
            title: "requested".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            title_template: "Regen requested".into(),
            metadata_defaults: serde_json::json!({
                "trigger_kind": "operator",
                "trigger_name": "operator-requests-a-regen",
            }),
            ..Default::default()
        },
        // Human-gated: a regen is destructive and nobody should be
        // able to start one without saying why.
        StepSpec {
            title: "scope".into(),
            kind: "task".into(),
            ready_when: "steps.requested.done".into(),
            title_template: "Why, and what it destroys".into(),
            authority_role: Some("platform-admin".into()),
            fields: vec![
                boss_core::job::StepField {
                    name: "reason".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                boss_core::job::StepField {
                    name: "destroying".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
            ],
            ..Default::default()
        },
        linked(
            "build",
            "scope",
            "Build release artifacts",
            "source_ref",
            false,
        ),
        linked(
            "artifacts",
            "build",
            "Prove the built thing is the deployed thing",
            "verified",
            false,
        ),
        linked("deploy", "artifacts", "Install", "deployed", false),
        // The destructive one. Gated on a person even though every
        // step around it can be automated: the whole Workflow exists
        // so this moment is a recorded decision.
        linked("reset", "deploy", "Drop and reseed", "baseline", true),
        linked(
            "backfill",
            "reset",
            "Backfill, then go live",
            "went_live",
            false,
        ),
        linked(
            "verify",
            "backfill",
            "Verify the new world",
            "checks",
            false,
        ),
        StepSpec {
            title: "complete".into(),
            kind: "outcome".into(),
            ready_when: "steps.verify.done".into(),
            title_template: "Regen complete".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "completed" }),
            terminal: Some(Terminal {
                outcome: "regenerated".into(),
            }),
            ..Default::default()
        },
        // A regen that fails partway is the normal bad case and leaves
        // a deployment in a known-broken state, so it needs an
        // outcome. Same gate as ship-a-change, for the same reason:
        // an ungated terminal that is ready gets completed by the
        // dispatcher, which would close a regen as abandoned the
        // moment its scope was declared.
        StepSpec {
            title: "abandoned".into(),
            kind: "outcome".into(),
            // BOTH halves are load-bearing. `steps.scope.done` is the
            // DAG edge — the viability lint rejects a step no trigger
            // can reach, and gating on metadata alone left this one
            // orphaned. The marker is what stops it being ready by
            // default, which is what let the dispatcher close a Job
            // the moment its scope was declared.
            ready_when: "steps.scope.done AND job.metadata.abandoned = \"true\"".into(),
            title_template: "Abandoned".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "aborted" }),
            terminal: Some(Terminal {
                outcome: "abandoned".into(),
            }),
            ..Default::default()
        },
    ];

    let mut spec = WorkflowSpec::platform_seed(
        "regenerate-deployment",
        "Regenerate a deployment",
        "platform",
        vec!["custom".into()],
        steps,
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec.description = Some(
        "Drop a deployment's database and rebuild it: schema, seed, backfilled history, \
         then live. The Subject is the deployment, so \"what regens has this box had, and \
         why\" answers from Subject history. Two steps are gated on a person — declaring \
         why it is happening, and the destructive reset itself — and the rest record what \
         was done at each stage. Note the bootstrap gap: a deployment cannot run the \
         Workflow that regenerates it, so the Job lives wherever the operator is."
            .to_string(),
    );
    spec
}

/// Build the canonical `backlog-item` WorkflowSpec.
///
/// Engineering backlog, modelled as work rather than as a markdown
/// file. TODO.md carried 41 open items across eight sections when this
/// was written, some more than a month old, and the file cannot tell
/// you which of them are still true.
///
/// The Subject is the AREA the item touches — a `custom` Subject whose
/// id is a crate or surface path (`/crate/boss-ledger`,
/// `/surface/cockpit`). That makes "what is outstanding against the
/// ledger" a Subject-history question rather than a grep.
///
/// ## Why this is not `user-feedback` with a different name
///
/// The shape is close — both fork on a disposition — but two of the
/// routes here do not exist there, and they are the ones a backlog
/// needs most:
///
/// - `stale`: the claim is no longer true, and nobody did it on
///   purpose. Triaging this file found C1 ("event_facts and
///   search_index have no refresh path") dead — two timers now
///   refresh both, shipped by work that never referenced the item.
///   Feedback does not rot this way; an outside report is about
///   something that happened. An internal claim about the codebase
///   decays every time the codebase moves, which is daily.
/// - `verify`: the claim needs re-measuring before anyone acts. The
///   same triage hit an item whose RATIONALE was stale ("masked
///   because the gate only diffs journal lines" — it diffs facts now)
///   while its DEFECT stood unverified. Those are different states
///   and collapsing them is how a backlog becomes fiction.
///
/// ## Why `evidence` is required at triage
///
/// Because the failure mode of a backlog is not neglect, it is
/// confident wrong answers read off the file. Both findings above
/// came from checking the claim against the running system — one
/// item died, one survived, and reading either off its own text
/// would have got it backwards. You cannot route an item here
/// without saying what you actually checked.
///
/// Step graph:
///  -1. `filed`   — an item entered the backlog
///   0. `triage`  — measure the claim, choose a route (human-gated)
///   1..n         — one branch per route
///   999. `closed`
#[cfg(test)]
fn backlog_item_spec() -> WorkflowSpec {
    const DISPOSITIONS: &str = "verify|design|build|duplicate|stale|decline";

    /// A branch that leaves the Job open for someone to do the work.
    /// Authority-gated for the same reason triage is: `task` declares
    /// no required roles, so an ungated ready step gets role-matched
    /// and completed by the simulated workforce.
    fn branch(title: &str, label: &str, disposition: &str) -> StepSpec {
        StepSpec {
            title: title.into(),
            kind: "task".into(),
            ready_when: format!(
                "steps.triage.done AND steps.triage.metadata.disposition = \"{disposition}\""
            ),
            title_template: label.into(),
            authority_role: Some("platform-admin".into()),
            ..Default::default()
        }
    }

    fn closing_branch(
        title: &str,
        label: &str,
        disposition: &str,
        outcome_kind: &str,
        outcome: &str,
    ) -> StepSpec {
        StepSpec {
            title: title.into(),
            kind: "outcome".into(),
            ready_when: format!(
                "steps.triage.done AND steps.triage.metadata.disposition = \"{disposition}\""
            ),
            title_template: label.into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": outcome_kind }),
            terminal: Some(Terminal {
                outcome: outcome.into(),
            }),
            ..Default::default()
        }
    }

    let steps = vec![
        StepSpec {
            title: "filed".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            title_template: "Filed to the backlog".into(),
            metadata_defaults: serde_json::json!({
                "trigger_kind": "operator",
                "trigger_name": "item-enters-the-backlog",
            }),
            ..Default::default()
        },
        StepSpec {
            title: "triage".into(),
            kind: "task".into(),
            ready_when: "steps.filed.done".into(),
            title_template: "Measure the claim, choose a route".into(),
            authority_role: Some("platform-admin".into()),
            fields: vec![
                boss_core::job::StepField {
                    name: "disposition".into(),
                    field_type: DISPOSITIONS.into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                // What was checked, and what it showed. Required: see
                // the doc comment. An item routed without a
                // measurement is an opinion about code that may have
                // moved since the item was written.
                boss_core::job::StepField {
                    name: "evidence".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                // The decider's brief, written at routing time: what
                // they must read (markdown) and the answer the triager
                // would give. Optional — only the design route needs
                // them; the decide step's surface says when absent.
                boss_core::job::StepField {
                    name: "context_md".into(),
                    field_type: "string".into(),
                    required: false,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
                boss_core::job::StepField {
                    name: "proposed".into(),
                    field_type: "string".into(),
                    required: false,
                    filled_by: boss_core::job::FilledBy::Executor,
                    item_keys: Vec::new(),
                },
            ],
            ..Default::default()
        },
        branch("measure", "Re-measure the claim", "verify"),
        // answer-question, not a bare task: the decision surface —
        // question, the asker's context, a proposed answer, verdict —
        // the same step user-feedback's design route uses. A task here
        // reached David's queue as a title and two empty boxes
        // (2026-09-05, three items).
        StepSpec {
            kind: "answer-question".into(),
            ..branch("design-review", "Decide the design", "design")
        },
        branch("build", "Build the change", "build"),
        closing_branch(
            "duplicate",
            "Closed as a duplicate",
            "duplicate",
            "withdrawn",
            "duplicate",
        ),
        // Not "completed" — nobody completed it. The world moved and
        // the claim stopped being true, which is worth being able to
        // count separately from work anyone chose to do.
        closing_branch(
            "stale",
            "Closed — the claim no longer holds",
            "stale",
            "withdrawn",
            "stale",
        ),
        closing_branch(
            "declined",
            "Closed without action",
            "decline",
            "aborted",
            "declined",
        ),
        StepSpec {
            title: "closed".into(),
            kind: "outcome".into(),
            ready_when: "steps.measure.done OR steps.design-review.done \
                         OR steps.build.done"
                .into(),
            title_template: "Backlog item closed".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "completed" }),
            terminal: Some(Terminal {
                outcome: "completed".into(),
            }),
            ..Default::default()
        },
    ];

    let mut spec = WorkflowSpec::platform_seed(
        "backlog-item",
        "Backlog item",
        "platform",
        vec!["custom".into()],
        steps,
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec.description = Some(
        "One piece of engineering backlog, modelled as work rather than a line in a \
         markdown file. The Subject is the area it touches, so \"what is outstanding \
         against the ledger\" answers from Subject history. Triage requires the evidence \
         behind the routing decision, because the failure mode of a backlog is not \
         neglect but confident wrong answers read off its own text — an internal claim \
         about the codebase decays every time the codebase moves. `stale` exists for the \
         items that die without anyone doing them, and `verify` for the ones whose claim \
         needs re-measuring before anyone acts."
            .to_string(),
    );
    spec
}

/// Every Workflow that ships baked into the platform binary. Read by
/// `boss-jobs-api`'s startup reconciler — `kind_registry
/// .bootstrap_reconcile(&platform_workflows())` runs on every boot,
/// inserting missing rows / refreshing drifted bootstrap-owned
/// rows / preserving operator-edited rows.
///
/// Today: `workflow-design` + `design-doc-review`. Future platform
/// kinds (a `step-plugin-design` meta-kind, perhaps a
/// `policy-rule-design` one) land here as additional entries —
/// never as TOML-loader exceptions, never as direct
/// `INSERT INTO workflows` SQL.
/// The path to the platform Workflow bundle, resolved from this crate.
pub fn platform_bundle_path() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/platform/workflows.toml"
    )
}

/// EVERY kind a deployment has: the Rust roster PLUS the bundle.
///
/// `platform_workflows()` alone stopped being that answer the moment
/// kinds began moving to infra/platform/workflows.toml, and the gap is
/// silent in exactly the wrong way — a test that seeds only the roster
/// still compiles, still runs, and gets `unknown or inactive job kind`
/// at the first create, so the assertion it was written for never
/// executes. Four suites failed that way the day `ship-a-change`
/// converted; each had been passing while testing nothing.
///
/// Anything standing up a registry that is meant to resemble a
/// deployment wants this, not the roster.
pub fn seedable_platform_workflows() -> Vec<WorkflowSpec> {
    let mut all = platform_workflows();
    all.extend(
        crate::seed_loader::load_workflows(platform_bundle_path())
            .expect("the platform Workflow bundle parses"),
    );
    all
}

pub fn platform_workflows() -> Vec<WorkflowSpec> {
    vec![
        maintenance_spec(
            "maintenance-backup",
            "Nightly backup",
            "The 03:00 backup run — configs, Postgres dump, kanidm state.",
        ),
        maintenance_spec(
            "maintenance-audit-integrity",
            "Audit-log integrity check",
            "The 03:00 chain scan + event-kind drift guard.",
        ),
        maintenance_spec(
            "maintenance-ledger-replay",
            "Ledger replay check",
            "The 03:30 rooted-at-audit-log replay comparison.",
        ),
        design_doc_review_spec(),
        // `workflow-design`, `regenerate-deployment`, `backlog-item`,
        // `ship-a-change`, `user-feedback` and `pr-train`
        // are NOT missing — they moved to infra/platform/workflows.toml
        // and are supplied by `boss-platform-workflow-seed`
        // (protocols-as-data, step 1: the kinds with no traffic first,
        // so a wrong loader can hurt nothing).
        //
        // Leaving this list is the point of the exercise. A kind here
        // is a protocol that cannot be changed without a deploy, and
        // one bootstrap_reconcile will rewrite if an operator edits it.
        // A kind in the bundle is data: the seed inserts it once and
        // nothing ever overwrites it.
        //
        // THE BUNDLE IS WHAT SAYS WHICH, and it has to be read on the
        // main this lands on rather than the one the branch was cut
        // from. This car spent a round narrowed to `regenerate-deployment`
        // alone, because at its base commit the bundle held one
        // workflow. Train #45 added the other two an hour later, so
        // measuring against the branch point would have left
        // `workflow-design` and `backlog-item` in BOTH places — and a
        // kind in both places is the worse failure, not the safe one:
        // bootstrap_reconcile republishes the code version over the
        // bundle's on every boot, which is precisely what moving them
        // out was for.
        //
        // The three builders survive below under #[cfg(test)] as the
        // fidelity test's expected value — the proof the bundle says
        // exactly what the code used to. They are deleted for real
        // once that test has watched a release go by.
    ]
}

/// One maintenance kind per chore (internal-forge.md Q6): the systemd
/// timer stays the EXECUTOR; the Job is the visibility layer. The
/// timer's unit ensures the open Job exists at start
/// (`boss-maintenance-wrap.sh`) and completes `run` on success via
/// `boss-step.sh` — which is also why it is one KIND per chore:
/// boss-step's contract is "the single open Job of a workflow".
/// Failure completes nothing, so the Job stays OPEN — visible on the
/// fleet and the canvas until a later successful run (or a human)
/// closes it. A failed backup is an algedonic signal, not a journal
/// line.
///
/// Deliberately NOT spawned by the dispatcher's schedule runner: it
/// fires on SIM-day boundaries, and at warp a "daily" rule fires
/// every couple of wall-minutes — maintenance is wall-clock work.
fn maintenance_spec(kind: &str, label: &str, description: &str) -> WorkflowSpec {
    let steps = vec![
        StepSpec {
            title: "scheduled".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            title_template: "Timer fired".into(),
            metadata_defaults: serde_json::json!({
                "trigger_kind": "periodic",
                "trigger_name": "systemd-timer",
            }),
            ..Default::default()
        },
        StepSpec {
            title: "run".into(),
            kind: "task".into(),
            ready_when: "steps.scheduled.done".into(),
            title_template: "Run to completion".into(),
            // Gated so the simulated workforce cannot role-match and
            // "complete" real maintenance (the ship-a-change scope
            // comment's hazard); the timer's boss-step call presents
            // the automation actor with this role.
            authority_role: Some("platform-admin".into()),
            fields: vec![boss_core::job::StepField {
                name: "result".into(),
                field_type: "string".into(),
                required: true,
                filled_by: boss_core::job::FilledBy::Executor,
                item_keys: Vec::new(),
            }],
            ..Default::default()
        },
        StepSpec {
            title: "completed".into(),
            kind: "outcome".into(),
            ready_when: "steps.run.done AND steps.run.metadata.result = \"ok\"".into(),
            title_template: "Maintenance completed".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "completed" }),
            terminal: Some(Terminal {
                outcome: "completed".into(),
            }),
            ..Default::default()
        },
        // A run that died records how (boss-step.sh from ExecStopPost:
        // the service result and exit status) and lands here, instead
        // of sitting open looking like a run in progress until a later
        // run closed it "ok" (2026-09-05, twice in one afternoon).
        StepSpec {
            title: "failed".into(),
            kind: "outcome".into(),
            ready_when: "steps.run.done AND steps.run.metadata.result != \"ok\"".into(),
            title_template: "Maintenance failed".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "aborted" }),
            terminal: Some(Terminal {
                outcome: "failed".into(),
            }),
            ..Default::default()
        },
    ];
    let mut spec =
        WorkflowSpec::platform_seed(kind, label, description, vec!["custom".into()], steps);
    // Owner + /system/flow membership: maintenance is the department's
    // own labor, so it appears with the other platform kinds.
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec
}

/// Which `user-feedback` step a triage disposition opens, and whether
/// reaching it ends the Job on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackBranch {
    /// The step's `spec_slug` — the stable machine-facing identifier
    /// (`investigate`, `design-review`, …), which is what a caller
    /// addressing the step across the HTTP surface matches on.
    pub slug: String,
    /// `true` for `duplicate` / `declined`: the branch IS a declared
    /// terminal, so triage closes the Job outright and there is no
    /// open step for anything downstream to complete.
    pub terminal: bool,
}

/// Which branch step the `user-feedback` triage disposition
/// `disposition` opened — the fork's routing table, answered from the
/// shipped spec rather than from a second list.
///
/// Every branch's eligibility is `steps.triage.done AND
/// steps.triage.metadata.disposition = "<value>"`, so the mapping
/// disposition → step already exists in the Workflow; this reads it
/// back out instead of restating it. A branch renamed in
/// `user_feedback_spec` moves this answer with it, which is the point
/// (CLAUDE.md §9a — the fact stays in one place instead of drifting
/// against a hardcoded table).
///
/// `None` for a disposition no branch claims — an unknown value, or
/// one the viability lint has yet to catch.
pub fn feedback_branch_for_disposition(disposition: &str) -> Option<FeedbackBranch> {
    let needle = format!("steps.triage.metadata.disposition = \"{disposition}\"");
    // READS THE BUNDLE, because that is where the protocol lives now.
    // The §9a point is unchanged — this still derives the mapping from
    // the protocol rather than restating it — but the source moved from
    // a Rust function to `infra/platform/workflows.toml` (e332a320).
    // Parsed once: the bundle is a build-time artefact of the tree, not
    // something that changes under a running process.
    static SPEC: std::sync::OnceLock<Option<WorkflowSpec>> = std::sync::OnceLock::new();
    let spec = SPEC.get_or_init(|| {
        crate::seed_loader::load_workflows(platform_bundle_path())
            .ok()?
            .into_iter()
            .find(|w| w.kind == "user-feedback")
    });
    spec.as_ref()?
        .steps
        .iter()
        .find(|s| s.ready_when.contains(&needle))
        .map(|s| FeedbackBranch {
            slug: s.title.clone(),
            terminal: s.terminal.is_some(),
        })
}

/// Build the canonical `design-doc-review` WorkflowSpec.
///
/// The "system models its own development" workflow: a design doc
/// under `docs/design/` becomes the Subject of one of these Jobs; the
/// review-design step (custom Step UX plugin) gates completion on
/// every open question being addressed.
///
/// Step graph:
/// -1. `trigger`        — author opened design doc for review
///  0. `review-design`  — plugin-rendered: lists open questions,
///                        captures a resolution per question,
///                        blocks completion until all questions
///                        have a resolution recorded. Resolutions
///                        flow into the existing
///                        `/api/design/pending-decisions` rows;
///                        `/api/design/flush-jobs` extracts them
///                        to ADRs.
///  999. `outcome`      — review complete; decisions captured
fn design_doc_review_spec() -> WorkflowSpec {
    let steps = vec![
        StepSpec {
            title: "open".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            title_template: "Design doc opened for review".into(),
            metadata_defaults: serde_json::json!({
                "trigger_kind": "operator",
                "trigger_name": "design-doc-author-opens-review",
            }),
            ..Default::default()
        },
        StepSpec {
            title: "review".into(),
            kind: "review-design".into(),
            ready_when: "steps.open.done".into(),
            title_template: "Review design doc — address open questions".into(),
            authority_role: Some("platform-admin".into()),
            // doc_path is stamped AT MATERIALIZATION from the Job's
            // subject (the doc path IS the subject id) — atomically,
            // in the same write that creates the step. It must never
            // rely on a follow-up PUT: a second write loses
            // read-overlay-write races against dispatcher assignment
            // and workforce completion, and terminal-metadata
            // immutability then seals the empty value (the
            // 2026-07-14 "doc_path is empty" incident).
            metadata_defaults: serde_json::json!({
                "doc_path": "{subject.id}",
                "resolutions": [],
            }),
            ..Default::default()
        },
        StepSpec {
            title: "reviewed".into(),
            kind: "outcome".into(),
            ready_when: "steps.review.done".into(),
            title_template: "Design reviewed — decisions captured".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "completed" }),
            terminal: Some(Terminal {
                outcome: "completed".into(),
            }),
            ..Default::default()
        },
    ];

    let mut spec = WorkflowSpec::platform_seed(
        "design-doc-review",
        "Review a design doc",
        "platform",
        vec!["custom".into()],
        steps,
    );
    // Q7: same platform-admin ownership as workflow-design — the
    // review step's authority is platform-admin already, but the
    // explicit owner_role keeps both meta-kinds resolvable even if
    // step shapes change.
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec.description = Some(
        "Meta-kind: every design doc under docs/design/ gets reviewed via a Job of this kind. \
         The `review-design` step uses a custom Step UX plugin that reads the doc's open \
         questions (parsed by boss-docs-api from `### Qn:` headings) and gates completion \
         until each one has a recorded resolution. Resolutions land in \
         `/api/design/pending-decisions`; subsequent `/api/design/flush-jobs` writes them \
         into the source doc's Decision-history section (each release, settled material \
         folds into `docs/architecture-decisions.md` and the source doc is deleted). \
         Replaces the in-app decision-tracker surface retired on 2026-05-03."
            .to_string(),
    );
    spec
}

// ---------------------------------------------------------------------------
// Materialization — turn a spec + subject into concrete Steps
// ---------------------------------------------------------------------------

/// Flatten a Subject into `{subject.<field>}` lookup pairs for title
/// template expansion. A Subject is uniformly `(kind, id)`, so this
/// exposes `{subject.id}` and `{subject.kind}` for every kind.
fn subject_fields(subject: &Subject) -> Vec<(&'static str, &str)> {
    // Subject is uniformly (kind, id). Templates use
    // `{subject.id}` and `{subject.kind}`.
    vec![("id", subject.id.as_str()), ("kind", subject.kind.as_str())]
}

/// Substitute the step-title token alphabet: `{subject.<field>}` and
/// `{metadata.<field>}`, the same two namespaces `expand_metadata`
/// exposes to `metadata_defaults`. A title is the sentence telling a
/// human what this step is asking of them, so it gets the same per-Job
/// context the step's metadata already had.
///
/// Scalar metadata only (string/number/bool), matching
/// `expand_metadata`: an object or array has no sensible rendering in
/// a title, and stringifying one would produce a worse label than
/// leaving the token alone.
///
/// An unknown token is left LITERAL rather than blanked, which is the
/// same choice `{day…}` makes without a date anchor: a visible
/// `{metadata.typo}` in the UI is a bug report, whereas silently
/// emitting "Inspect: " reads as finished work.
fn expand_title(template: &str, subject: &Subject, job_metadata: &serde_json::Value) -> String {
    let mut out = template.to_string();
    for (field, value) in subject_fields(subject) {
        out = out.replace(&format!("{{subject.{field}}}"), value);
    }
    if let Some(map) = job_metadata.as_object() {
        for (key, value) in map {
            let rendered = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            out = out.replace(&format!("{{metadata.{key}}}"), &rendered);
        }
    }
    out
}

/// Recursive `{subject.<field>}` + `{day...}` substitution over
/// a JSON metadata-defaults blob. Walks every string leaf in the
/// value (object keys + array elements + nested objects). Keys
/// are not substituted — the placeholder lives in values.
///
/// Supported tokens:
///   - `{subject.<field>}` — per-Job subject context (every call)
///   - `{day}` — the Job's open day, when `date_anchor` is `Some`
///   - `{day_minus_<N>}` — open day minus N calendar days
///   - `{day_plus_<N>}` — open day plus N calendar days
///
/// Date substitution is opt-in via `date_anchor`. When `None`,
/// `{day...}` tokens stay as literal placeholders — that keeps
/// the live-API path (which has no sim day) honest about
/// "this template requires a date anchor" instead of silently
/// stamping `Utc::now()`. The brewery's biweekly-payroll Workflow
/// uses this for `period_start = "{day_minus_13}"` so each
/// run's period derives from the sim day instead of a hardcoded
/// 2026-01-* literal (which left the labor-absorption credit
/// to 6100 Payroll un-offset by the payroll DR within the same
/// FY and broke period close).
fn expand_metadata(
    template: &serde_json::Value,
    subject: &Subject,
    job_metadata: &serde_json::Value,
    date_anchor: Option<chrono::NaiveDate>,
) -> serde_json::Value {
    use serde_json::Value;
    let fields = subject_fields(subject);
    // Scalar Job-metadata fields, exposed as `{metadata.<field>}` so a spawned
    // Job can parameterize its steps' metadata_defaults — e.g. the reorder rule
    // passes the triggering `part_sku` into the restock's PO line so each
    // restock buys one ingredient instead of a fixed full-catalog bundle.
    let meta_fields: Vec<(String, String)> = job_metadata
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| match v {
                    Value::String(s) => Some((k.clone(), s.clone())),
                    Value::Number(n) => Some((k.clone(), n.to_string())),
                    Value::Bool(b) => Some((k.clone(), b.to_string())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    fn substitute_day(out: &mut String, day: chrono::NaiveDate) {
        // Cheap-and-clean exact replace for {day}, then offset
        // tokens via a small regex-free scan. The token alphabet
        // is `{day_minus_<digits>}` / `{day_plus_<digits>}` —
        // anything else is left alone.
        *out = out.replace("{day}", &day.format("%Y-%m-%d").to_string());
        for prefix in ["{day_minus_", "{day_plus_"] {
            while let Some(start) = out.find(prefix) {
                let after = start + prefix.len();
                let Some(end_rel) = out[after..].find('}') else {
                    break;
                };
                let end = after + end_rel;
                let digits = &out[after..end];
                let Ok(n) = digits.parse::<i64>() else {
                    // Malformed token — replace with itself to avoid an
                    // infinite loop, then bail.
                    let token = &out[start..=end].to_string();
                    *out = out.replacen(token, &format!("__{token}__"), 1);
                    break;
                };
                let offset = if prefix.starts_with("{day_minus_") {
                    -n
                } else {
                    n
                };
                let stamped = (day + chrono::Duration::days(offset))
                    .format("%Y-%m-%d")
                    .to_string();
                let token = &out[start..=end].to_string();
                *out = out.replacen(token, &stamped, 1);
            }
        }
    }
    /// A default written as EXACTLY `{metadata.<key>}` whose Job value
    /// is an array or an object is substituted whole, not stringified.
    ///
    /// Without this, structured Job metadata cannot reach a step at
    /// all: `meta_fields` above keeps only String/Number/Bool, so an
    /// array is not in the substitution table, the token matches
    /// nothing, and the step is materialized carrying the literal text
    /// `{metadata.questions}`. Silent, and it looks like the spawn rule
    /// failed to bind.
    ///
    /// That is why a design review opens blank (defect 6f40b23f). The
    /// review plugin renders `step.metadata.questions` when the packet
    /// carries its own work, and falls back to fetching the doc from a
    /// service the cluster does not route. The questions could not ride
    /// into the step even when the spawn bound them onto the Job.
    ///
    /// SCALARS ARE DELIBERATELY UNTOUCHED. They already substitute
    /// correctly, and whole-value substitution would change their TYPE
    /// — `{metadata.count}` returns String("5") today and would start
    /// returning Number(5), which `check_metadata_defaults_values`
    /// type-checks against the StepType's declared field. Narrowing
    /// this to the case that is currently broken keeps every existing
    /// template byte-identical.
    fn whole_value<'a>(s: &str, job_meta: &'a serde_json::Value) -> Option<&'a serde_json::Value> {
        let key = s.strip_prefix("{metadata.")?.strip_suffix('}')?;
        // One token and nothing else — `"{metadata.a}{metadata.b}"` is
        // string concatenation and has no whole-value reading.
        if key.contains('{') || key.contains('}') {
            return None;
        }
        match job_meta.get(key) {
            Some(v @ (Value::Array(_) | Value::Object(_))) => Some(v),
            _ => None,
        }
    }

    fn walk(
        v: &serde_json::Value,
        fields: &[(&'static str, &str)],
        meta_fields: &[(String, String)],
        job_meta: &serde_json::Value,
        date_anchor: Option<chrono::NaiveDate>,
    ) -> serde_json::Value {
        match v {
            Value::String(s) => {
                if let Some(whole) = whole_value(s, job_meta) {
                    return whole.clone();
                }
                let mut out = s.clone();
                for (field, value) in fields {
                    let token = format!("{{subject.{field}}}");
                    out = out.replace(&token, value);
                }
                for (field, value) in meta_fields {
                    let token = format!("{{metadata.{field}}}");
                    out = out.replace(&token, value);
                }
                if let Some(day) = date_anchor {
                    substitute_day(&mut out, day);
                }
                Value::String(out)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|x| walk(x, fields, meta_fields, job_meta, date_anchor))
                    .collect(),
            ),
            Value::Object(map) => {
                let mut out = serde_json::Map::with_capacity(map.len());
                for (k, v) in map {
                    out.insert(
                        k.clone(),
                        walk(v, fields, meta_fields, job_meta, date_anchor),
                    );
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }
    walk(template, &fields, &meta_fields, job_metadata, date_anchor)
}

/// Turn a `WorkflowSpec` + concrete `Subject` + fresh `JobId` into
/// ready-to-insert `Step` rows. Step IDs come from the
/// caller-provided closure so deterministic callers (sim) can use a
/// monotonic counter while runtime paths use `StepId::new()`.
///
/// Eager materialization (Workflow v2): **every** step is created at
/// Job open. Each step's `blocked_by` is the set of upstream steps
/// its `ready_when` predicate references; its initial status is
/// decided by [`reevaluate`] against the open-time state (no step
/// done yet) — trigger / subject-gated predicates flip straight to
/// `Ready`, the rest stay `Pending`. `sort_order` is the step's
/// index in `spec.steps`, which [`reevaluate`] relies on to pair a
/// live `Step` back to its `StepSpec`.
///
/// `{day}` tokens in `metadata_defaults` stay literal — call
/// [`materialize_steps_at`] when the open day is known so the
/// payroll / period-end family of fields gets sim-derived dates.
pub fn materialize_steps<F>(
    spec: &WorkflowSpec,
    subject: &Subject,
    job_id: JobId,
    job_metadata: &serde_json::Value,
    step_id_fn: F,
) -> Vec<Step>
where
    F: FnMut() -> StepId,
{
    materialize_steps_at(spec, subject, job_id, job_metadata, step_id_fn, None, None)
}

/// Same as [`materialize_steps`] but expands `{day}`,
/// `{day_minus_<N>}`, `{day_plus_<N>}` tokens in step
/// `metadata_defaults` against `date_anchor`. The sim engine passes
/// `Some(day)` so each Job's metadata reflects the sim's clock; live
/// API paths pass `Some(state.clock.now().await.now.date_naive())`.
pub fn materialize_steps_at<F>(
    spec: &WorkflowSpec,
    subject: &Subject,
    job_id: JobId,
    job_metadata: &serde_json::Value,
    mut step_id_fn: F,
    date_anchor: Option<chrono::NaiveDate>,
    step_registry: Option<&StepRegistry>,
) -> Vec<Step>
where
    F: FnMut() -> StepId,
{
    // Allocate one StepId per spec step, in order, and remember each
    // step's slug → id so a predicate's `steps.<slug>` references
    // resolve to concrete StepIds for the denormalized `blocked_by`
    // edge list the SPA renders the DAG from.
    let ids: Vec<StepId> = spec.steps.iter().map(|_| step_id_fn()).collect();
    let slug_to_id: std::collections::HashMap<&str, StepId> = spec
        .steps
        .iter()
        .zip(&ids)
        .map(|(s, id)| (s.title.as_str(), *id))
        .collect();

    let mut steps: Vec<Step> = Vec::with_capacity(spec.steps.len());
    for (idx, (spec_step, id)) in spec.steps.iter().zip(&ids).enumerate() {
        let title = if spec_step.title_template.is_empty() {
            humanize_slug(&spec_step.title)
        } else {
            expand_title(&spec_step.title_template, subject, job_metadata)
        };
        let merged = merge_metadata(&spec_step.metadata_defaults, spec_step);
        // Per-Job context flows through `{subject.<field>}` + `{day…}`
        // substitution so side-effect-bound steps emit per-Job-distinct
        // facts instead of identical deterministic defaults.
        let metadata = expand_metadata(&merged, subject, job_metadata, date_anchor);
        let blocked_by: Vec<StepId> = predicate_step_refs(&spec_step.ready_when)
            .iter()
            .filter_map(|slug| slug_to_id.get(slug.as_str()).copied())
            .collect();
        steps.push(Step {
            id: *id,
            job_id,
            kind: spec_step.kind.clone(),
            title,
            spec_slug: Some(spec_step.title.clone()),
            // The protocol's requirement travels onto the packet at
            // materialisation, like every other step property — so an
            // in-flight step keeps the assurance its Workflow version
            // declared even if the protocol is later edited.
            assurance_required: spec_step.assurance_required,
            assignee_id: None,
            status: StepStatus::Pending,
            sort_order: idx as i32,
            blocked_by,
            sign_offs_required: spec_step
                .sign_offs_required
                .iter()
                .filter_map(|r| {
                    if r == "@authority_role" {
                        spec_step.authority_role.clone()
                    } else {
                        Some(r.clone())
                    }
                })
                .collect(),
            sign_offs: Vec::new(),
            fields: spec_step.fields.clone(),
            completed_on: None,
            metadata,
            notes: None,
            // Snapshot is taken on INSERT in postgres::add_step.
            step_plugin_version: 0,
            embedded_job: None,
        });
    }

    // Trigger provenance: a trigger describes a job-creation condition
    // and has no work of its own, so it is terminal the instant the Job
    // exists — the firing trigger is `Completed`, its alternatives
    // `Skipped` (the branch not taken). Resolved BEFORE the readiness
    // pass so a downstream `steps.<trigger>.done` predicate sees the
    // fired trigger as `.done` and promotes in the same call. Skipped
    // when no registry is supplied: the pure helper path then leaves
    // the trigger `Ready` for a marker handler to complete, while the
    // create path resolves it here.
    if let Some(registry) = step_registry {
        resolve_triggers(registry, &mut steps, job_metadata, date_anchor);
    }
    // Open-time readiness pass: flip the subject-gated steps to Ready
    // (or provably-N/A steps to Skipped).
    reevaluate(spec, &mut steps, subject, job_metadata);
    steps
}

/// The admission half of the completion contract: every `(step, field)`
/// a filer still owes on a freshly materialized step graph.
///
/// A field declared `filled_by = "filer"` (registry data on the
/// Workflow row) is one the work is not doable without — a design
/// review's `markdown`, the thing under review. Required-at-done would
/// detonate its absence on the EXECUTOR mid-work, the party least able
/// to fix it; this check moves the refusal to admission, where the
/// filer is still on the line. Completion validation is unchanged —
/// the field stays required-at-done too, this just catches it first.
///
/// "Missing" is: no key, an explicit `null`, or a whole-string
/// unexpanded `{metadata.<key>}` token. The third is the binding
/// idiom's honest failure — a spec binds a filer field with
/// `metadata_defaults = { markdown = "{metadata.markdown}" }`, and
/// [`expand_metadata`] deliberately leaves an unmatched token literal
/// as a visible bug report. Admission is where that report becomes a
/// refusal instead of riding to the reviewer as prose. Prose that
/// merely contains a token is a value; only an exact single-token
/// string reads as unexpanded.
///
/// Steps are named by `spec_slug` (the stable machine-facing id the
/// filer authored against), falling back to `title` for steps born
/// outside a spec.
///
/// A field that declares `item_keys` is checked for SHAPE as well as
/// presence: the value must be an array, and every element an object
/// carrying each named key as a non-empty string. A miss is reported
/// as `field[i].key` so the 422 names the element, not just the field.
/// Presence and shape are the same refusal: a `questions` array whose
/// elements have no `title` renders as "nothing to review", which is
/// exactly what an absent `questions` renders as.
pub fn missing_filer_fields(steps: &[Step]) -> Vec<(String, String)> {
    steps
        .iter()
        .flat_map(|step| {
            step.fields
                .iter()
                .filter(|f| f.required && f.filled_by == boss_core::job::FilledBy::Filer)
                .flat_map(|f| filer_field_misses(f, step.metadata.get(&f.name)))
                .map(|field| {
                    (
                        step.spec_slug.clone().unwrap_or_else(|| step.title.clone()),
                        field,
                    )
                })
        })
        .collect()
}

/// Every miss on one filer field: the bare name when the value is
/// absent, else one `name[i].key` per element key the shape lacks.
fn filer_field_misses(
    field: &boss_core::job::StepField,
    value: Option<&serde_json::Value>,
) -> Vec<String> {
    if filer_value_missing(value) {
        return vec![field.name.clone()];
    }
    if field.item_keys.is_empty() {
        return Vec::new();
    }
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return vec![format!("{} (not an array)", field.name)];
    };
    items
        .iter()
        .enumerate()
        .flat_map(|(i, item)| {
            field
                .item_keys
                .iter()
                .filter(move |key| {
                    item.get(key.as_str())
                        .and_then(|v| v.as_str())
                        .is_none_or(|s| s.trim().is_empty())
                })
                .map(move |key| format!("{}[{i}].{key}", field.name))
        })
        .collect()
}

fn filer_value_missing(value: Option<&serde_json::Value>) -> bool {
    match value {
        None | Some(serde_json::Value::Null) => true,
        Some(v) => is_unexpanded_metadata_token(v),
    }
}

/// True iff the value is a string that is EXACTLY one `{metadata.<key>}`
/// token — the literal a `metadata_defaults` binding leaves behind when
/// the Job's metadata had nothing to bind. Mirrors `whole_value`'s key
/// extraction: one token, nothing else, no nested braces.
fn is_unexpanded_metadata_token(v: &serde_json::Value) -> bool {
    v.as_str().is_some_and(|s| {
        s.strip_prefix("{metadata.")
            .and_then(|rest| rest.strip_suffix('}'))
            .is_some_and(|key| !key.is_empty() && !key.contains(['{', '}']))
    })
}

/// Resolve `auto-on-materialize` (trigger) steps to their terminal
/// status at Job open: the firing trigger becomes `Completed`, every
/// alternative becomes `Skipped`. A trigger names a job-creation
/// condition and carries no completion logic of its own, so it is
/// terminal the instant the Job exists — never transient Ready work an
/// executor or the dispatcher's marker handler picks up. This is the
/// honest form of "every trigger fires regardless of spawn cause": only
/// the trigger that actually fired is recorded as `Completed`.
///
/// Which trigger fired is read from the Job's `metadata.trigger_name`
/// (stamped by the `jobs.spawn` rule that opened the Job, or absent for
/// operator-opened Jobs): the trigger step whose own
/// `metadata.trigger_name` matches it fires. With no provenance — an
/// operator-opened Job, a sole trigger, or a name that matches none —
/// the first trigger fires (the compat fallback). Downstream predicates
/// that fan in from multiple triggers use `steps.a.done OR steps.b.done`
/// and `.done` is `Completed`-only, so a `Skipped` alternative correctly
/// does not satisfy them.
///
/// Trigger steps are identified by the `AutoOnMaterialize` completion
/// property, never the kind name (no-step-kind-match). Returns the
/// indices whose status changed, for the caller's `step.created`
/// emission.
fn resolve_triggers(
    registry: &StepRegistry,
    steps: &mut [Step],
    job_metadata: &serde_json::Value,
    date_anchor: Option<chrono::NaiveDate>,
) -> Vec<usize> {
    let trigger_idxs: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            registry
                .get(&s.kind)
                .is_some_and(|st| st.completion == Completion::AutoOnMaterialize)
        })
        .map(|(i, _)| i)
        .collect();
    if trigger_idxs.is_empty() {
        return Vec::new();
    }

    let job_trigger = job_metadata.get("trigger_name").and_then(|v| v.as_str());
    let firing = job_trigger
        .and_then(|name| {
            trigger_idxs.iter().copied().find(|&i| {
                steps[i]
                    .metadata
                    .get("trigger_name")
                    .and_then(|v| v.as_str())
                    == Some(name)
            })
        })
        .or_else(|| trigger_idxs.first().copied());

    let mut changed = Vec::new();
    for &i in &trigger_idxs {
        let next = if Some(i) == firing {
            StepStatus::Completed
        } else {
            StepStatus::Skipped
        };
        if steps[i].status != next {
            steps[i].status = next;
            if next == StepStatus::Completed {
                steps[i].completed_on = date_anchor;
            }
            changed.push(i);
        }
    }
    changed
}

/// Humanize a kebab-case slug into a display title: `mash-in` →
/// `Mash in`. Used when a `StepSpec` declares no `title_template`.
fn humanize_slug(slug: &str) -> String {
    let mut s = slug.replace('-', " ");
    if let Some(first) = s.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    s
}

/// The upstream step slugs a predicate reads — its `steps.<slug>.…`
/// paths — sorted + deduped. Powers the denormalized `blocked_by`
/// edge list and the satisfiability check in [`reevaluate`]. An
/// unparseable predicate contributes no refs; the viability lint is
/// what rejects those at author / publish / boot time.
pub fn predicate_step_refs(ready_when: &str) -> Vec<String> {
    let Ok(expr) = boss_expr::parse(ready_when) else {
        return Vec::new();
    };
    let mut slugs: Vec<String> = boss_expr::references(&expr)
        .into_iter()
        .filter(|path| path.len() >= 2 && path[0] == "steps")
        .map(|path| path[1].clone())
        .collect();
    slugs.sort();
    slugs.dedup();
    slugs
}

/// Does this predicate reference mutable Job state (`job.metadata.*`)?
/// Step terminality proves nothing about such a predicate — the
/// metadata can be written later (a marker like `merged = "true"`
/// arrives at merge time, long after every referenced step is done) —
/// so `reevaluate` must never infer Skipped from it (aa9980c8: four
/// ship-a-change Jobs closed with their Merged outcome skipped 134ms
/// after boarding).
pub fn predicate_refs_job_metadata(ready_when: &str) -> bool {
    let Ok(expr) = boss_expr::parse(ready_when) else {
        return false;
    };
    boss_expr::references(&expr)
        .into_iter()
        .any(|path| path.first().map(|s| s == "job").unwrap_or(false))
}

/// Pair each SPEC step with the JOB step that carries its slug.
/// `pairing[i]` is the index into `steps` for `spec.steps[i]`, or
/// `None` when the packet has no step for that spec step.
///
/// WHY THIS EXISTS. Predicates are not stored on the step row (there is
/// no `ready_when` column), so advancement has to re-associate spec
/// steps with job steps on every pass. That association used to be the
/// INDEX, which made the step list's shape load-bearing: one step
/// appended to a live job — and `POST /api/jobs/{id}/steps` is a public
/// route — misaligned every pair after it. The old guard called that
/// FROZEN and evaluated nothing, correctly, because the alternative was
/// worse: `build_context` keys the context by spec slug while reading
/// the positionally-paired step, so a misaligned job answers
/// `steps.triage.done` with a DIFFERENT step's status. Design review
/// 32a4e70d froze exactly this way on 2026-08-13 and surfaced only as
/// "I finished it and it is still there".
///
/// The step row has carried `spec_slug` since; this is the durable fix
/// the old guard named and deferred — pairing by name, extra steps
/// simply ignored.
///
/// IDENTITY WHEN SLUGS ARE ABSENT. A step materialized before the
/// column exists has `spec_slug: None`, and for those packets the index
/// is the only association there is. Falling back wholesale (rather than
/// per step) keeps such a packet behaving exactly as it does today
/// instead of half-pairing it, which would be a new failure mode.
///
/// SAFETY PROPERTY, AND THE FIRST TEST: when every slug is present and
/// matches the spec in order, this returns the identity — so every
/// healthy packet is paired precisely as before.
fn pair_steps(spec: &WorkflowSpec, steps: &[Step]) -> Vec<Option<usize>> {
    let all_slugged = !steps.is_empty() && steps.iter().all(|s| s.spec_slug.is_some());
    if !all_slugged {
        return (0..spec.steps.len())
            .map(|i| (i < steps.len()).then_some(i))
            .collect();
    }
    let mut by_slug: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (j, s) in steps.iter().enumerate() {
        if let Some(slug) = s.spec_slug.as_deref() {
            // First occurrence wins, so a duplicated slug is resolved
            // deterministically rather than by iteration order.
            by_slug.entry(slug).or_insert(j);
        }
    }
    spec.steps
        .iter()
        .map(|ss| by_slug.get(ss.title.as_str()).copied())
        .collect()
}

/// Build the predicate-evaluation payload for a Job's current state:
/// `{ subject, job: { metadata }, steps: { <slug>: { done, metadata } } }`.
/// `steps` is keyed by each `StepSpec.title` slug and read through
/// `pairing`, so the value under a slug is that slug's step — see
/// [`pair_steps`] for why reading it positionally was a defect.
///
/// A spec step the packet does not have is OMITTED rather than faked.
/// `eval_ready_when` already treats a missing reference as unknown,
/// which is the honest answer; inserting `done: false` would assert
/// that a step nobody materialized is incomplete.
fn build_context(
    spec: &WorkflowSpec,
    steps: &[Step],
    pairing: &[Option<usize>],
    subject: &Subject,
    job_metadata: &serde_json::Value,
) -> serde_json::Value {
    let mut steps_obj = serde_json::Map::new();
    for (i, spec_step) in spec.steps.iter().enumerate() {
        let Some(step) = pairing.get(i).copied().flatten().and_then(|j| steps.get(j)) else {
            continue;
        };
        steps_obj.insert(
            spec_step.title.clone(),
            serde_json::json!({
                "done": step.status == StepStatus::Completed,
                "metadata": step.metadata,
            }),
        );
    }
    serde_json::json!({
        "subject": serde_json::to_value(subject).unwrap_or(serde_json::Value::Null),
        "job": { "metadata": job_metadata },
        "steps": serde_json::Value::Object(steps_obj),
    })
}

/// Evaluate a `ready_when` predicate against a context payload.
/// `None` on a parse error, an eval error (e.g. a referenced metadata
/// field the upstream step hasn't set yet), or a non-boolean result.
/// Workflow predicates are pure boolean / field / comparison
/// expressions, so no helper functions are registered.
fn eval_ready_when(ready_when: &str, payload: &serde_json::Value) -> Option<bool> {
    let expr = boss_expr::parse(ready_when).ok()?;
    let ctx = boss_expr::Context {
        payload,
        helpers: &boss_expr::NoHelpers,
    };
    boss_expr::eval(&expr, &ctx).ok()?.as_bool()
}

/// True iff every step that step `idx`'s predicate references has
/// reached a terminal state (`Completed` / `Skipped`) — i.e. no
/// future change can flip its `ready_when`. Unknown refs (which the
/// lint forbids) count as terminal so a stray reference can't wedge a
/// step `Pending` forever.
fn refs_all_terminal(
    spec: &WorkflowSpec,
    steps: &[Step],
    pairing: &[Option<usize>],
    idx: usize,
) -> bool {
    let Some(spec_step) = spec.steps.get(idx) else {
        return true;
    };
    predicate_step_refs(&spec_step.ready_when)
        .iter()
        .all(|slug| {
            // Resolve the slug to its SPEC position, then through the
            // pairing to the job step. Going straight from spec index to
            // `steps[index]` was the same positional assumption
            // `build_context` made — see [`pair_steps`].
            spec.steps
                .iter()
                .position(|s| &s.title == slug)
                .and_then(|i| pairing.get(i).copied().flatten())
                .and_then(|j| steps.get(j))
                .map(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped))
                .unwrap_or(true)
        })
}

/// Re-evaluate every `Pending` step against the current Job state,
/// promoting `Pending → Ready` when its `ready_when` is satisfied, or
/// `Pending → Skipped` when it is provably unsatisfiable (every step
/// it references is terminal and the predicate still won't hold).
/// Returns the indices whose status changed so the caller can emit
/// the matching `step.updated` events.
///
/// The single readiness engine shared by the live API (boss-jobs
/// http) and the simulator (which builds audit_log directly). Steps
/// must be in spec order (`sort_order == index`); the materializer
/// guarantees that. Iterates to a fixpoint so a `Skipped` cascade —
/// one skip making a downstream predicate's refs all-terminal —
/// settles in a single call. A spec/steps length mismatch (only
/// possible if a Workflow was republished mid-flight with a different
/// step count) is treated as "leave everything as-is."
/// Has this job's step list diverged from the spec it was admitted
/// under? Exposed so a caller with the job id in hand can report WHICH
/// job, which [`reevaluate`] cannot.
///
/// THIS NO LONGER MEANS FROZEN. It used to: pairing was positional, so
/// any length mismatch stopped advancement permanently. Since steps
/// pair by slug ([`pair_steps`]) a diverged job keeps moving, and this
/// reports a shape worth looking at rather than a death certificate.
///
/// Divergence is now "a spec step has no row on this job" — the case
/// that genuinely cannot advance — OR a plain count mismatch, which
/// catches extra rows the spec does not describe. A packet with extra
/// steps still advances; it is simply carrying something unexplained.
pub fn steps_diverged_from_spec(spec: &WorkflowSpec, steps: &[Step]) -> bool {
    let pairing = pair_steps(spec, steps);
    pairing.iter().any(Option::is_none) || spec.steps.len() != steps.len()
}

pub fn reevaluate(
    spec: &WorkflowSpec,
    steps: &mut [Step],
    subject: &Subject,
    job_metadata: &serde_json::Value,
) -> Vec<usize> {
    let mut changed = Vec::new();
    let pairing = pair_steps(spec, steps);

    // A DIVERGED SHAPE IS NO LONGER FATAL — it is reported.
    //
    // This used to bail outright, freezing the job forever, because
    // pairing was positional and one appended step misaligned every
    // pair after it. `POST /api/jobs/{id}/steps` is a public route, so
    // any protocol that added a step to a live job froze it that way,
    // and it did: design review 32a4e70d, 2026-08-14, sat with its
    // review completed and its terminal pending, and the only symptom
    // was "I finished it and it is still there".
    //
    // Pairing by slug removes the misalignment, so extra job steps are
    // simply ignored and the job keeps moving. It stays LOUD because a
    // shape that does not match its protocol is still worth knowing
    // about: a spec step with no job step can never advance, and this
    // is the only place that can see it.
    let unpaired: Vec<&str> = spec
        .steps
        .iter()
        .zip(&pairing)
        .filter(|(_, p)| p.is_none())
        .map(|(s, _)| s.title.as_str())
        .collect();
    if !unpaired.is_empty() || steps.len() != spec.steps.len() {
        tracing::warn!(
            spec_steps = spec.steps.len(),
            job_steps = steps.len(),
            unpaired = ?unpaired,
            "job steps diverged from its workflow spec — pairing by slug and \
             continuing; steps listed as unpaired have no row on this job and \
             cannot advance"
        );
    }

    loop {
        let ctx = build_context(spec, steps, &pairing, subject, job_metadata);
        let mut moved = false;
        for (i, spec_step) in spec.steps.iter().enumerate() {
            let Some(j) = pairing.get(i).copied().flatten() else {
                continue;
            };
            if steps[j].status != StepStatus::Pending {
                continue;
            }
            let next = match eval_ready_when(&spec_step.ready_when, &ctx) {
                Some(true) => Some(StepStatus::Ready),
                Some(false) | None => (refs_all_terminal(spec, steps, &pairing, i)
                    && !predicate_refs_job_metadata(&spec_step.ready_when))
                .then_some(StepStatus::Skipped),
            };
            if let Some(status) = next {
                steps[j].status = status;
                // JOB indices, not spec indices: callers persist
                // `steps[i]` and emit its `step.updated`.
                changed.push(j);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    changed
}

/// If the step has an `authority_role`, surface it in metadata so the
/// sign-off gate in `boss-jobs::http::update_step` can enforce it.
fn merge_metadata(defaults: &serde_json::Value, step: &StepSpec) -> serde_json::Value {
    let mut merged = match defaults {
        serde_json::Value::Object(_) => defaults.clone(),
        _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    if let (Some(role), serde_json::Value::Object(m)) = (&step.authority_role, &mut merged) {
        m.insert(
            "authority_role".to_string(),
            serde_json::Value::String(role.clone()),
        );
    }
    // Surfaced the same way `authority_role` is, because the
    // dispatcher reads the materialized STEP, never the spec — it is
    // reacting to an event and has no workflow row in hand.
    if let (Some(claimable), serde_json::Value::Object(m)) = (step.claimable, &mut merged) {
        m.insert("claimable".to_string(), serde_json::Value::Bool(claimable));
    }
    merged
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("job kind not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid spec: {0}")]
    Invalid(String),
    /// The spec failed the viability lint and so may not occupy the
    /// ACTIVE slot. Carries every problem so the HTTP surface can
    /// hand the editor the same `{step, reason, message}` list
    /// `POST /api/workflows/_validate` returns. Distinct from
    /// `Invalid` because it maps to 422 (semantically well-formed,
    /// operationally unrunnable), not 400.
    #[error("workflow is not viable: {}", .0.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "))]
    Unviable(Vec<crate::workflow_lint::WorkflowLintError>),
    #[error("storage error: {0}")]
    Storage(String),
}

// ---------------------------------------------------------------------------
// Port
// ---------------------------------------------------------------------------

#[async_trait]
pub trait WorkflowRegistry: Send + Sync {
    /// Return the currently-active spec for `kind`, or NotFound if
    /// no active row exists.
    async fn get_active(&self, kind: &str) -> Result<WorkflowSpec, WorkflowError>;

    /// Return a specific historical version. Version 0 is reserved
    /// as "latest active."
    async fn get_version(&self, kind: &str, version: i32) -> Result<WorkflowSpec, WorkflowError>;

    /// List every active spec, optionally filtered by category.
    async fn list_active(&self, category: Option<&str>)
    -> Result<Vec<WorkflowSpec>, WorkflowError>;

    /// Every version of a single kind, oldest first. Includes drafts
    /// and retired rows.
    async fn list_versions(&self, kind: &str) -> Result<Vec<WorkflowSpec>, WorkflowError>;

    /// Append a new version with `status = Draft`. If no prior rows
    /// exist, the new row is version 1; otherwise it's max(version)+1.
    /// Returns the stored spec (with its assigned version + created_at).
    ///
    /// Every write method takes `actor` + `now` because under 3P a
    /// registry write IS a network configuration change
    /// (protocol-policy-publish.md, Constraints): the adapter builds
    /// the corresponding event via `events::workflow_registry_event`
    /// and records it atomically with the row — the caller supplies
    /// the who (session actor, or a named automation) and the when
    /// (clock-routed, never wallclock in production paths).
    async fn create_draft(
        &self,
        spec: WorkflowSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError>;

    /// Flip the latest draft row of this kind to active and any
    /// previous active row to retired. Transactional; records
    /// `jobs.kind.published` (payload = the promoted spec) with the
    /// row flips.
    async fn publish(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError>;

    /// Flip the active row of this kind to retired. Idempotent if
    /// already retired — and silent then too: only a write that
    /// touched a row records `jobs.kind.retired` (payload = the
    /// retired spec).
    async fn retire(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), WorkflowError>;

    /// Delete a DRAFT row outright. A draft admitted nothing, so it is
    /// pre-history and may be removed; an active or retired version IS
    /// history and refuses with `Conflict` (ebd7bb70 — the publish
    /// guard demanded "resolve that draft first" while no verb or
    /// route could, and the actual resolution was a raw psql DELETE
    /// the classifier rightly blocks). Records
    /// `jobs.kind.draft_discarded` iff a row was removed; a missing
    /// row is `NotFound` so a typo cannot read as success.
    async fn discard_draft(
        &self,
        kind: &str,
        version: i32,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), WorkflowError>;

    /// Look up the active spec and materialize its step DAG against
    /// the given subject. Default impl fetches + delegates to the
    /// pure `materialize_steps` helper; adapters can override if a
    /// smarter path is available.
    ///
    /// The closure must be `Send` so the returned future is `Send`
    /// across the async trait boundary. Callers that need non-`Send`
    /// counters (e.g. the sim) call `materialize_steps` directly on
    /// a spec they fetched separately.
    async fn materialize_steps_for_kind(
        &self,
        kind: &str,
        subject: &Subject,
        job_id: JobId,
        job_metadata: &serde_json::Value,
        step_ids: &mut (dyn FnMut() -> StepId + Send),
    ) -> Result<Vec<Step>, WorkflowError> {
        let spec = self.get_active(kind).await?;
        Ok(materialize_steps(
            &spec,
            subject,
            job_id,
            job_metadata,
            step_ids,
        ))
    }

    /// Single-shot create + publish, used by the `workflow-publish`
    /// StepType's dispatch path inside `boss-jobs-api::update_step`.
    /// The meta-Job that authored the spec passes its own id as
    /// `authoring_job_id`; the row stamps `created_by =
    /// "job-{authoring_job_id}"` so the bootstrap reconciler
    /// preserves the row going forward (`created_by != 'bootstrap'`
    /// → operator-owned).
    ///
    /// Semantics, in one transaction:
    /// - Compute `next_version = max(version) + 1`, or 1 if no
    ///   prior rows for this kind.
    /// - INSERT the spec at that version with `status='active'`.
    /// - UPDATE any previously-active row of the same kind to
    ///   `status='retired'`.
    ///
    /// Returns the published spec with `version` + `created_at`
    /// reflecting the durable row. Records `jobs.kind.published`
    /// (payload = the promoted spec) in the same transaction — the
    /// step-update path that dispatches here no longer emits its own
    /// copy. See `docs/architecture-decisions.md` §Jobs, Workflows,
    /// Steps.
    async fn publish_authored(
        &self,
        spec: WorkflowSpec,
        authoring_job_id: JobId,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError>;

    /// Reconcile the active rows in the registry against a set of
    /// platform-supplied defaults. For each default:
    ///
    /// - No row exists for `kind` → insert as version 1, active,
    ///   `created_by = 'bootstrap'`. Counted as `inserted`.
    /// - Active row exists, `created_by = 'bootstrap'`, body
    ///   drifted from the default (label / category /
    ///   subject_kinds / steps / metadata_schema /
    ///   entitlements / metadata / on_complete_create / owning_team) → upsert in
    ///   place, restamping `created_by = 'bootstrap'`. Counted as
    ///   `refreshed`. The version + created_at are preserved so a
    ///   bootstrap fixup never reads as a publish event.
    /// - Active row exists, `created_by != 'bootstrap'` → preserve
    ///   untouched. Counted as `preserved`.
    /// - Active row exists, `created_by = 'bootstrap'`, no drift →
    ///   no write. Counted as `unchanged`.
    ///
    /// Same semantic shape as `boss_policy_client::PolicyRepository::
    /// bootstrap_reconcile`. Used by
    /// `boss-jobs-api`'s startup loop to upsert `platform_workflows()`
    /// — including `workflow-design`, the meta-kind that owns the
    /// design / review / publish workflow for every other kind in
    /// the registry.
    ///
    /// Records `jobs.kind.published` per inserted/republished row;
    /// preserved and unchanged rows write nothing and record
    /// nothing (rows_affected > 0 means event).
    async fn bootstrap_reconcile(
        &self,
        defaults: &[WorkflowSpec],
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<KindReconcileStats, WorkflowError>;
}

/// Result of a `bootstrap_reconcile` call. Counts each branch so
/// the service log records how much drift was healed on this boot.
/// Same shape as `boss_policy_client::ReconcileStats`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindReconcileStats {
    /// New rows inserted as `created_by = 'bootstrap'`.
    pub inserted: usize,
    /// Bootstrap-owned rows whose body drifted from the default and
    /// were republished as a NEW version. Never an in-place rewrite:
    /// a Job pins the version it opened under, so mutating a live
    /// version's body re-points every in-flight Job at a spec it never
    /// agreed to.
    pub republished: usize,
    /// Operator-edited rows left untouched
    /// (`created_by != 'bootstrap'`).
    pub preserved: usize,
    /// Bootstrap-owned rows already matching the default — no
    /// write.
    pub unchanged: usize,
    /// Defaults refused by the viability lint — never written, never
    /// activated. A shipped seed landing here is a code bug (the
    /// `every_shipped_platform_seed_is_viable` test exists to keep
    /// this at zero); the reconcile still completes for the rest so
    /// one bad default can't strand the whole platform set.
    pub rejected: usize,
}

/// Helper: returns true iff every body field that bootstrap
/// reconcile cares about matches between `existing` and `default`.
/// Fields excluded from the comparison: `kind` (key), `version`
/// (preserved), `status` (always `Active` post-reconcile),
/// `created_at` (preserved), `description` (cosmetic — change in
/// description alone shouldn't trigger a refresh write),
/// `authoring_job_id` (the bootstrap-side default doesn't carry
/// one).
fn kind_body_matches(existing: &WorkflowSpec, default: &WorkflowSpec) -> bool {
    existing.label == default.label
        && existing.category == default.category
        && existing.subject_kinds == default.subject_kinds
        && existing.steps == default.steps
        && existing.metadata_schema == default.metadata_schema
        && existing.entitlements == default.entitlements
        && existing.metadata == default.metadata
        && existing.on_complete_create == default.on_complete_create
        && existing.owning_team == default.owning_team
}

// ---------------------------------------------------------------------------
// In-memory adapter
// ---------------------------------------------------------------------------

/// Mutex-backed in-memory registry. Every async fn resolves immediately;
/// safe to call from either a tokio or a non-tokio context.
pub struct InMemoryWorkflows {
    rows: Arc<Mutex<HashMap<(String, i32), WorkflowSpec>>>,
    /// Tracks which rows came from a bootstrap reconcile. Mirrors
    /// the `created_by = 'bootstrap'` discriminator the postgres
    /// adapter uses, so reconcile semantics match across adapters
    /// in tests.
    bootstrap_owned: Arc<Mutex<std::collections::HashSet<(String, i32)>>>,
    /// What the Pg adapter records into `event_outbox` inside the
    /// row transaction, this adapter collects here — same events at
    /// the same write points, so tests assert the event contract
    /// through the port without a database.
    recorded: Arc<Mutex<Vec<boss_core::event::Event>>>,
}

impl Default for InMemoryWorkflows {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryWorkflows {
    pub fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(HashMap::new())),
            bootstrap_owned: Arc::new(Mutex::new(std::collections::HashSet::new())),
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every event a write method recorded, in write order — the
    /// in-memory stand-in for `SELECT ... FROM event_outbox`.
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().unwrap().clone()
    }

    fn record(&self, event: boss_core::event::Event) {
        self.recorded.lock().unwrap().push(event);
    }

    /// Seed helper for tests + bootstrap. Inserts a row as-is without
    /// versioning logic — used for the seed migration. The seeded
    /// row is NOT marked bootstrap-owned (use `seed_bootstrap` for
    /// that semantic).
    pub fn seed(&self, spec: WorkflowSpec) -> Result<(), WorkflowError> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&(spec.kind.clone(), spec.version)) {
            return Err(WorkflowError::Conflict(format!(
                "row already exists: {}@{}",
                spec.kind, spec.version
            )));
        }
        rows.insert((spec.kind.clone(), spec.version), spec);
        Ok(())
    }

    fn snapshot(&self) -> Vec<WorkflowSpec> {
        let rows = self.rows.lock().unwrap();
        rows.values().cloned().collect()
    }

    fn max_version(&self, kind: &str) -> Option<i32> {
        let rows = self.rows.lock().unwrap();
        rows.keys()
            .filter(|(k, _)| k == kind)
            .map(|(_, v)| *v)
            .max()
    }
}

#[async_trait]
impl WorkflowRegistry for InMemoryWorkflows {
    async fn get_active(&self, kind: &str) -> Result<WorkflowSpec, WorkflowError> {
        let rows = self.snapshot();
        rows.into_iter()
            .find(|r| r.kind == kind && r.status == WorkflowStatus::Active)
            .ok_or_else(|| WorkflowError::NotFound(format!("no active kind: {kind}")))
    }

    async fn get_version(&self, kind: &str, version: i32) -> Result<WorkflowSpec, WorkflowError> {
        let rows = self.rows.lock().unwrap();
        rows.get(&(kind.to_string(), version))
            .cloned()
            .ok_or_else(|| WorkflowError::NotFound(format!("{kind}@v{version}")))
    }

    async fn list_active(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<WorkflowSpec>, WorkflowError> {
        let mut rows: Vec<WorkflowSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.status == WorkflowStatus::Active)
            .filter(|r| category.is_none_or(|c| r.category == c))
            .collect();
        rows.sort_by(|a, b| a.kind.cmp(&b.kind));
        Ok(rows)
    }

    async fn list_versions(&self, kind: &str) -> Result<Vec<WorkflowSpec>, WorkflowError> {
        let mut rows: Vec<WorkflowSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.kind == kind)
            .collect();
        rows.sort_by_key(|r| r.version);
        Ok(rows)
    }

    async fn create_draft(
        &self,
        mut spec: WorkflowSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        let next = self.max_version(&spec.kind).unwrap_or(0) + 1;
        spec.version = next;
        spec.status = WorkflowStatus::Draft;
        spec.created_at = now;
        let mut rows = self.rows.lock().unwrap();
        rows.insert((spec.kind.clone(), spec.version), spec.clone());
        drop(rows);
        self.record(crate::events::workflow_registry_event(
            crate::events::WORKFLOW_DRAFT_SAVED,
            actor,
            &spec,
        ));
        Ok(spec)
    }

    async fn publish(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        let mut rows = self.rows.lock().unwrap();

        // Find the latest draft for this kind.
        let latest_draft = rows
            .values()
            .filter(|r| r.kind == kind && r.status == WorkflowStatus::Draft)
            .max_by_key(|r| r.version)
            .cloned()
            .ok_or_else(|| {
                WorkflowError::NotFound(format!("no draft to publish for kind: {kind}"))
            })?;

        // The publish gate — refuse before any row flips.
        crate::workflow_lint::gate_active(&latest_draft).map_err(WorkflowError::Unviable)?;

        // Demote any currently-active row for this kind.
        for ((k, _), row) in rows.iter_mut() {
            if k == kind && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
            }
        }

        // Promote the draft.
        let key = (latest_draft.kind.clone(), latest_draft.version);
        let row = rows.get_mut(&key).unwrap();
        row.status = WorkflowStatus::Active;
        let promoted = row.clone();
        drop(rows);
        self.record(crate::events::workflow_registry_event(
            crate::events::WORKFLOW_PUBLISHED,
            actor,
            &promoted,
        ));
        Ok(promoted)
    }

    async fn retire(
        &self,
        kind: &str,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<(), WorkflowError> {
        let mut rows = self.rows.lock().unwrap();
        let any_active = rows
            .values()
            .any(|r| r.kind == kind && r.status == WorkflowStatus::Active);
        if !any_active {
            // Idempotent — nothing to do, so nothing to record.
            return Ok(());
        }
        let mut retired: Option<WorkflowSpec> = None;
        for ((k, _), row) in rows.iter_mut() {
            if k == kind && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
                retired = Some(row.clone());
            }
        }
        drop(rows);
        if let Some(spec) = retired {
            self.record(crate::events::workflow_registry_event(
                crate::events::WORKFLOW_RETIRED,
                actor,
                &spec,
            ));
        }
        Ok(())
    }

    async fn discard_draft(
        &self,
        kind: &str,
        version: i32,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<(), WorkflowError> {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.get(&(kind.to_string(), version)) else {
            return Err(WorkflowError::NotFound(format!("{kind} v{version}")));
        };
        if row.status != WorkflowStatus::Draft {
            return Err(WorkflowError::Conflict(format!(
                "{kind} v{version} is {:?}, not a draft — an active or retired                  version is history; only a draft (which admitted nothing) can                  be discarded",
                row.status
            )));
        }
        let spec = rows.remove(&(kind.to_string(), version)).expect("checked");
        drop(rows);
        self.record(crate::events::workflow_registry_event(
            crate::events::WORKFLOW_DRAFT_DISCARDED,
            actor,
            &spec,
        ));
        Ok(())
    }

    async fn publish_authored(
        &self,
        mut spec: WorkflowSpec,
        authoring_job_id: JobId,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        // Same gate as `publish` — this path writes an active row
        // with no draft ever existing, so it needs its own check.
        crate::workflow_lint::gate_active(&spec).map_err(WorkflowError::Unviable)?;

        let next = self.max_version(&spec.kind).unwrap_or(0) + 1;
        spec.version = next;
        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        spec.authoring_job_id = Some(*authoring_job_id.inner().as_uuid());

        let key = (spec.kind.clone(), spec.version);
        let mut rows = self.rows.lock().unwrap();
        // Retire any currently-active row of the same kind.
        for ((k, _), row) in rows.iter_mut() {
            if k == &spec.kind && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
            }
        }
        rows.insert(key.clone(), spec.clone());
        drop(rows);

        // The new row was published via a Job — operator-owned, NOT
        // bootstrap. Make sure the discriminator reflects that so
        // future reconciles preserve it.
        let mut owned = self.bootstrap_owned.lock().unwrap();
        owned.remove(&key);
        drop(owned);

        self.record(crate::events::workflow_registry_event(
            crate::events::WORKFLOW_PUBLISHED,
            actor,
            &spec,
        ));
        Ok(spec)
    }

    async fn bootstrap_reconcile(
        &self,
        defaults: &[WorkflowSpec],
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<KindReconcileStats, WorkflowError> {
        let mut stats = KindReconcileStats::default();
        // Inserted/republished rows record `jobs.kind.published`;
        // preserved/unchanged rows touched nothing and record
        // nothing. Collected here and pushed after the row locks
        // drop.
        let mut events: Vec<boss_core::event::Event> = Vec::new();
        let step_types = crate::step_registry::StepRegistry::v1();
        let mut rows = self.rows.lock().unwrap();
        let mut owned = self.bootstrap_owned.lock().unwrap();

        for default in defaults {
            // Platform seeding sets rows ACTIVE, so it answers to the
            // publish gate too. An unviable default is refused here
            // rather than seeded and quarantined at the next boot.
            if let Err(problems) = crate::workflow_lint::gate_active_with(default, &step_types) {
                tracing::error!(
                    kind = %default.kind,
                    problems = %problems.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("; "),
                    "platform Workflow default fails the viability lint — refusing to seed it"
                );
                stats.rejected += 1;
                continue;
            }
            // Find the active row for this kind, if any.
            let active_key: Option<(String, i32)> = rows
                .iter()
                .find(|((k, _), r)| k == &default.kind && r.status == WorkflowStatus::Active)
                .map(|((k, v), _)| (k.clone(), *v));

            match active_key {
                None => {
                    // No active row — insert as v1, active,
                    // bootstrap-owned. Force version=1 + status=Active
                    // even if the default carried different values, so
                    // a malformed default can't sneak a draft into the
                    // active slot.
                    let mut spec = default.clone();
                    spec.version = 1;
                    spec.status = WorkflowStatus::Active;
                    let key = (spec.kind.clone(), spec.version);
                    rows.insert(key.clone(), spec.clone());
                    owned.insert(key);
                    events.push(crate::events::workflow_registry_event(
                        crate::events::WORKFLOW_PUBLISHED,
                        actor,
                        &spec,
                    ));
                    stats.inserted += 1;
                }
                Some(key) => {
                    let existing = rows.get(&key).expect("just located it").clone();
                    if owned.contains(&key) {
                        if !kind_body_matches(&existing, default) {
                            // Publish a NEW version and retire the old
                            // one. This used to rewrite the body in
                            // place and keep the version, on the theory
                            // that a fixup should not look like a
                            // publish — but a Job pins the version it
                            // opened under, so an in-place rewrite
                            // silently re-points every in-flight Job at
                            // a spec it never agreed to. Two feedback
                            // Jobs were stranded exactly that way: their
                            // steps stayed the old shape while closure
                            // came to depend on branch steps they never
                            // had.
                            let next = rows
                                .keys()
                                .filter(|(k, _)| k == &default.kind)
                                .map(|(_, v)| *v)
                                .max()
                                .unwrap_or(0)
                                + 1;
                            let mut retired = existing.clone();
                            retired.status = WorkflowStatus::Retired;
                            rows.insert(key.clone(), retired);

                            let mut published = default.clone();
                            published.version = next;
                            published.status = WorkflowStatus::Active;
                            let new_key = (published.kind.clone(), next);
                            rows.insert(new_key.clone(), published.clone());
                            owned.insert(new_key);
                            events.push(crate::events::workflow_registry_event(
                                crate::events::WORKFLOW_PUBLISHED,
                                actor,
                                &published,
                            ));
                            stats.republished += 1;
                        } else {
                            stats.unchanged += 1;
                        }
                    } else {
                        // Operator-owned — preserve untouched.
                        stats.preserved += 1;
                    }
                }
            }
        }
        drop(rows);
        drop(owned);
        for event in events {
            self.record(event);
        }

        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// Postgres adapter
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::PgPool;

    pub struct PgWorkflows {
        pool: PgPool,
    }

    impl PgWorkflows {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    #[derive(sqlx::FromRow)]
    struct Row {
        kind: String,
        version: i32,
        status: String,
        label: String,
        description: Option<String>,
        category: String,
        subject_kinds: serde_json::Value,
        steps: serde_json::Value,
        metadata_schema: serde_json::Value,
        entitlements: serde_json::Value,
        metadata: serde_json::Value,
        on_complete_create: serde_json::Value,
        owning_team: String,
        authoring_job_id: Option<Uuid>,
        created_at: DateTime<Utc>,
    }

    fn row_to_spec(r: Row) -> Result<WorkflowSpec, WorkflowError> {
        let subject_kinds: Vec<String> = serde_json::from_value(r.subject_kinds)
            .map_err(|e| WorkflowError::Storage(format!("subject_kinds decode: {e}")))?;
        let steps: Vec<StepSpec> = serde_json::from_value(r.steps)
            .map_err(|e| WorkflowError::Storage(format!("steps decode: {e}")))?;
        let on_complete_create: Vec<JobTrigger> = serde_json::from_value(r.on_complete_create)
            .map_err(|e| WorkflowError::Storage(format!("on_complete_create decode: {e}")))?;
        let status = r
            .status
            .parse::<WorkflowStatus>()
            .map_err(WorkflowError::Storage)?;
        Ok(WorkflowSpec {
            kind: r.kind,
            version: r.version,
            status,
            label: r.label,
            description: r.description,
            category: r.category,
            subject_kinds,
            steps,
            metadata_schema: r.metadata_schema,
            entitlements: r.entitlements,
            metadata: r.metadata,
            on_complete_create,
            owning_team: r.owning_team,
            authoring_job_id: r.authoring_job_id,
            created_at: r.created_at,
        })
    }

    #[async_trait]
    impl WorkflowRegistry for PgWorkflows {
        async fn get_active(&self, kind: &str) -> Result<WorkflowSpec, WorkflowError> {
            let row: Option<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM workflows
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| WorkflowError::NotFound(format!("no active kind: {kind}")))
        }

        async fn get_version(
            &self,
            kind: &str,
            version: i32,
        ) -> Result<WorkflowSpec, WorkflowError> {
            let row: Option<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM workflows
                 WHERE kind = $1 AND version = $2",
            )
            .bind(kind)
            .bind(version)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| WorkflowError::NotFound(format!("{kind}@v{version}")))
        }

        async fn list_active(
            &self,
            category: Option<&str>,
        ) -> Result<Vec<WorkflowSpec>, WorkflowError> {
            let rows: Vec<Row> = match category {
                Some(c) => {
                    sqlx::query_as(
                        "SELECT kind, version, status, label, description, category,
                            subject_kinds, steps, metadata_schema, entitlements, metadata,
                            on_complete_create, owning_team, authoring_job_id, created_at
                     FROM workflows
                     WHERE status = 'active' AND category = $1
                     ORDER BY kind",
                    )
                    .bind(c)
                    .fetch_all(&self.pool)
                    .await
                }
                None => {
                    sqlx::query_as(
                        "SELECT kind, version, status, label, description, category,
                            subject_kinds, steps, metadata_schema, entitlements, metadata,
                            on_complete_create, owning_team, authoring_job_id, created_at
                     FROM workflows
                     WHERE status = 'active'
                     ORDER BY kind",
                    )
                    .fetch_all(&self.pool)
                    .await
                }
            }
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn list_versions(&self, kind: &str) -> Result<Vec<WorkflowSpec>, WorkflowError> {
            let rows: Vec<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM workflows
                 WHERE kind = $1
                 ORDER BY version",
            )
            .bind(kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn create_draft(
            &self,
            mut spec: WorkflowSpec,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<WorkflowSpec, WorkflowError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            // Next version = max(version) + 1, or 1 if no rows. `MAX(...)`
            // returns a single row with a NULL column when the filter
            // matches nothing, so decode as `Option<i32>`.
            let max: (Option<i32>,) =
                sqlx::query_as("SELECT MAX(version) FROM workflows WHERE kind = $1")
                    .bind(&spec.kind)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            let next = max.0.map(|v| v + 1).unwrap_or(1);

            spec.version = next;
            spec.status = WorkflowStatus::Draft;
            spec.created_at = now;

            let subject_kinds_json = serde_json::to_value(&spec.subject_kinds)
                .map_err(|e| WorkflowError::Invalid(e.to_string()))?;
            let steps_json = serde_json::to_value(&spec.steps)
                .map_err(|e| WorkflowError::Invalid(e.to_string()))?;
            let on_complete_create_json = serde_json::to_value(&spec.on_complete_create)
                .map_err(|e| WorkflowError::Invalid(e.to_string()))?;

            // `created_by` is the discriminator bootstrap_reconcile uses:
            // it republishes the code default over any row that reads
            // 'bootstrap', and preserves anything else. The column
            // defaults to 'bootstrap', and this INSERT used to omit it —
            // so a workflow published through the API was stamped as if
            // the platform seed had written it, and the next boot
            // silently republished the code default over the operator's
            // edit.
            //
            // That is not hypothetical. Two protocol edits on 2026-08-14
            // — a `satisfied` terminal added to `user-feedback` v8 and to
            // `ship-a-change` v4 — were gone by the next day, both kinds
            // bumped one version with the terminal absent, and nobody
            // noticed for a day. `approval`, edited the same way but
            // absent from platform_workflows(), kept its edits, which is
            // what isolated the cause (68331085).
            //
            // Stamping the ACTOR makes the write mean what it says: a
            // person or agent published this, so reconcile preserves it.
            // Same posture `publish_authored` already had with
            // `job-<id>`; this closes the gap for the plain path.
            let created_by = actor.to_string();
            sqlx::query(
                "INSERT INTO workflows
                    (kind, version, status, label, description, category,
                     subject_kinds, steps, metadata_schema, entitlements, metadata,
                     on_complete_create, owning_team, authoring_job_id, created_by,
                     created_at)
                 VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
            )
            .bind(&spec.kind)
            .bind(spec.version)
            .bind(&spec.label)
            .bind(&spec.description)
            .bind(&spec.category)
            .bind(&subject_kinds_json)
            .bind(&steps_json)
            .bind(&spec.metadata_schema)
            .bind(&spec.entitlements)
            .bind(&spec.metadata)
            .bind(&on_complete_create_json)
            .bind(&spec.owning_team)
            .bind(spec.authoring_job_id)
            .bind(&created_by)
            .bind(spec.created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            let event = crate::events::workflow_registry_event(
                crate::events::WORKFLOW_DRAFT_SAVED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(WorkflowError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            Ok(spec)
        }

        async fn publish(
            &self,
            kind: &str,
            actor: &boss_core::actor::ActorId,
            _now: DateTime<Utc>,
        ) -> Result<WorkflowSpec, WorkflowError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            // Pick the latest draft — the full row, not just the
            // version: the event payload is the promoted spec, and
            // it must come from data read INSIDE this transaction
            // (a post-commit re-fetch could observe a concurrent
            // writer and record a spec the flip never produced).
            let draft: Option<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM workflows
                 WHERE kind = $1 AND status = 'draft'
                 ORDER BY version DESC LIMIT 1",
            )
            .bind(kind)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            let mut promoted = draft
                .map(row_to_spec)
                .transpose()?
                .ok_or_else(|| WorkflowError::NotFound(format!("no draft to publish: {kind}")))?;

            // The publish gate, inside the transaction and against
            // the row we are about to promote — not against whatever
            // a caller happened to hand us.
            crate::workflow_lint::gate_active(&promoted).map_err(WorkflowError::Unviable)?;

            // Retire any currently-active row.
            sqlx::query(
                "UPDATE workflows SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            // Promote the draft.
            sqlx::query(
                "UPDATE workflows SET status = 'active'
                 WHERE kind = $1 AND version = $2",
            )
            .bind(kind)
            .bind(promoted.version)
            .execute(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            promoted.status = WorkflowStatus::Active;

            let event = crate::events::workflow_registry_event(
                crate::events::WORKFLOW_PUBLISHED,
                actor,
                &promoted,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(WorkflowError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            Ok(promoted)
        }

        async fn retire(
            &self,
            kind: &str,
            actor: &boss_core::actor::ActorId,
            _now: DateTime<Utc>,
        ) -> Result<(), WorkflowError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            // Read the active row first — the event payload is the
            // retired spec, and the idempotent no-op path (nothing
            // active) must record nothing. At most one row can be
            // active per kind (partial unique index).
            let active: Option<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM workflows
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            let Some(row) = active else {
                // Idempotent — nothing to do, so nothing to record.
                return Ok(());
            };
            let mut spec = row_to_spec(row)?;

            sqlx::query(
                "UPDATE workflows SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            spec.status = WorkflowStatus::Retired;

            let event = crate::events::workflow_registry_event(
                crate::events::WORKFLOW_RETIRED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(WorkflowError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            Ok(())
        }

        async fn discard_draft(
            &self,
            kind: &str,
            version: i32,
            actor: &boss_core::actor::ActorId,
            _now: DateTime<Utc>,
        ) -> Result<(), WorkflowError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            // Read first: the refusal must say what the row IS (a typo
            // must read as NotFound, history as Conflict — never as a
            // silent no-op), and the discard event's payload is the
            // spec being removed.
            let found: Option<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM workflows
                 WHERE kind = $1 AND version = $2",
            )
            .bind(kind)
            .bind(version)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            let Some(row) = found else {
                return Err(WorkflowError::NotFound(format!("{kind} v{version}")));
            };
            let spec = row_to_spec(row)?;
            if spec.status != WorkflowStatus::Draft {
                return Err(WorkflowError::Conflict(format!(
                    "{kind} v{version} is {:?}, not a draft — an active or retired \
                     version is history; only a draft (which admitted nothing) can \
                     be discarded",
                    spec.status
                )));
            }

            // The one DELETE the append-only registry permits: a draft
            // admitted nothing, so removing it rewrites no packet's
            // history — and the discard itself goes on the record in
            // the same transaction.
            sqlx::query(
                "DELETE FROM workflows WHERE kind = $1 AND version = $2 AND status = 'draft'",
            )
            .bind(kind)
            .bind(version)
            .execute(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            let event = crate::events::workflow_registry_event(
                crate::events::WORKFLOW_DRAFT_DISCARDED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(WorkflowError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            Ok(())
        }

        async fn publish_authored(
            &self,
            mut spec: WorkflowSpec,
            authoring_job_id: JobId,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<WorkflowSpec, WorkflowError> {
            // Same gate as `publish` — refuse before opening the
            // transaction, since nothing here can rescue an unviable
            // spec.
            crate::workflow_lint::gate_active(&spec).map_err(WorkflowError::Unviable)?;

            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            // Compute next version inside the transaction so a
            // concurrent publish can't race us into a duplicate.
            let max: (Option<i32>,) =
                sqlx::query_as("SELECT MAX(version) FROM workflows WHERE kind = $1")
                    .bind(&spec.kind)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| WorkflowError::Storage(e.to_string()))?;
            let next = max.0.map(|v| v + 1).unwrap_or(1);

            spec.version = next;
            spec.status = WorkflowStatus::Active;
            spec.created_at = now;
            spec.authoring_job_id = Some(*authoring_job_id.inner().as_uuid());

            // Retire any existing active row of this kind first —
            // the partial unique index demands at most one Active.
            sqlx::query(
                "UPDATE workflows SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(&spec.kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            let subject_kinds_json = serde_json::to_value(&spec.subject_kinds)
                .map_err(|e| WorkflowError::Invalid(e.to_string()))?;
            let steps_json = serde_json::to_value(&spec.steps)
                .map_err(|e| WorkflowError::Invalid(e.to_string()))?;
            let on_complete_create_json = serde_json::to_value(&spec.on_complete_create)
                .map_err(|e| WorkflowError::Invalid(e.to_string()))?;

            // Stamp created_by = "job-<authoring_job_id>" so the
            // bootstrap reconciler preserves this row (operator-
            // owned). Same shape any future reconcile decision uses.
            let created_by = format!("job-{}", authoring_job_id);

            sqlx::query(
                "INSERT INTO workflows
                    (kind, version, status, label, description, category,
                     subject_kinds, steps, metadata_schema, entitlements, metadata,
                     on_complete_create, owning_team, authoring_job_id,
                     created_by, created_at)
                 VALUES ($1, $2, 'active', $3, $4, $5, $6, $7, $8, $9, $10, $11,
                         $12, $13, $14, $15)",
            )
            .bind(&spec.kind)
            .bind(spec.version)
            .bind(&spec.label)
            .bind(&spec.description)
            .bind(&spec.category)
            .bind(&subject_kinds_json)
            .bind(&steps_json)
            .bind(&spec.metadata_schema)
            .bind(&spec.entitlements)
            .bind(&spec.metadata)
            .bind(&on_complete_create_json)
            .bind(&spec.owning_team)
            .bind(spec.authoring_job_id)
            .bind(&created_by)
            .bind(spec.created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            let event = crate::events::workflow_registry_event(
                crate::events::WORKFLOW_PUBLISHED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(WorkflowError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

            Ok(spec)
        }

        async fn bootstrap_reconcile(
            &self,
            defaults: &[WorkflowSpec],
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<KindReconcileStats, WorkflowError> {
            // Dedicated row shape that includes the bootstrap
            // discriminator alongside the body. The non-reconcile
            // read paths above use `Row` (which doesn't carry
            // `created_by`) so reconcile is the only caller that
            // needs this struct.
            #[derive(sqlx::FromRow)]
            struct ReconcileRow {
                kind: String,
                version: i32,
                status: String,
                label: String,
                description: Option<String>,
                category: String,
                subject_kinds: serde_json::Value,
                steps: serde_json::Value,
                metadata_schema: serde_json::Value,
                entitlements: serde_json::Value,
                metadata: serde_json::Value,
                on_complete_create: serde_json::Value,
                owning_team: String,
                authoring_job_id: Option<Uuid>,
                created_at: DateTime<Utc>,
                created_by: String,
            }

            let mut stats = KindReconcileStats::default();
            let step_types = crate::step_registry::StepRegistry::v1();

            for default in defaults {
                // Platform seeding sets rows ACTIVE, so it answers to
                // the publish gate too. An unviable default is refused
                // here rather than seeded and quarantined at the next
                // boot.
                if let Err(problems) = crate::workflow_lint::gate_active_with(default, &step_types)
                {
                    tracing::error!(
                        kind = %default.kind,
                        problems = %problems.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("; "),
                        "platform Workflow default fails the viability lint — refusing to seed it"
                    );
                    stats.rejected += 1;
                    continue;
                }

                let row: Option<ReconcileRow> = sqlx::query_as(
                    "SELECT kind, version, status, label, description, category,
                            subject_kinds, steps, metadata_schema, entitlements, metadata,
                            on_complete_create, owning_team, authoring_job_id, created_at,
                            created_by
                     FROM workflows
                     WHERE kind = $1 AND status = 'active'",
                )
                .bind(&default.kind)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                match row {
                    None => {
                        // Insert as v1, active, bootstrap-owned —
                        // in a transaction so the published event
                        // records with the row it describes.
                        let subject_kinds_json = serde_json::to_value(&default.subject_kinds)
                            .map_err(|e| WorkflowError::Invalid(e.to_string()))?;
                        let steps_json = serde_json::to_value(&default.steps)
                            .map_err(|e| WorkflowError::Invalid(e.to_string()))?;
                        let on_complete_create_json =
                            serde_json::to_value(&default.on_complete_create)
                                .map_err(|e| WorkflowError::Invalid(e.to_string()))?;

                        let mut inserted = default.clone();
                        inserted.version = 1;
                        inserted.status = WorkflowStatus::Active;
                        inserted.created_at = now;

                        let mut tx = self
                            .pool
                            .begin()
                            .await
                            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                        sqlx::query(
                            "INSERT INTO workflows
                                (kind, version, status, label, description, category,
                                 subject_kinds, steps, metadata_schema, entitlements, metadata,
                                 on_complete_create, owning_team, authoring_job_id,
                                 created_by, created_at)
                             VALUES ($1, 1, 'active', $2, $3, $4, $5, $6, $7, $8, $9,
                                     $10, $11, $12, 'bootstrap', $13)",
                        )
                        .bind(&default.kind)
                        .bind(&default.label)
                        .bind(&default.description)
                        .bind(&default.category)
                        .bind(&subject_kinds_json)
                        .bind(&steps_json)
                        .bind(&default.metadata_schema)
                        .bind(&default.entitlements)
                        .bind(&default.metadata)
                        .bind(&on_complete_create_json)
                        .bind(&default.owning_team)
                        .bind(default.authoring_job_id)
                        .bind(now)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                        let event = crate::events::workflow_registry_event(
                            crate::events::WORKFLOW_PUBLISHED,
                            actor,
                            &inserted,
                        );
                        boss_events::outbox::record_event_in_tx(&mut tx, &event)
                            .await
                            .map_err(WorkflowError::Storage)?;

                        tx.commit()
                            .await
                            .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                        stats.inserted += 1;
                    }
                    Some(reconcile_row) => {
                        let version = reconcile_row.version;
                        let created_by = reconcile_row.created_by.clone();
                        let body = Row {
                            kind: reconcile_row.kind,
                            version: reconcile_row.version,
                            status: reconcile_row.status,
                            label: reconcile_row.label,
                            description: reconcile_row.description,
                            category: reconcile_row.category,
                            subject_kinds: reconcile_row.subject_kinds,
                            steps: reconcile_row.steps,
                            metadata_schema: reconcile_row.metadata_schema,
                            entitlements: reconcile_row.entitlements,
                            metadata: reconcile_row.metadata,
                            on_complete_create: reconcile_row.on_complete_create,
                            owning_team: reconcile_row.owning_team,
                            authoring_job_id: reconcile_row.authoring_job_id,
                            created_at: reconcile_row.created_at,
                        };
                        let existing = row_to_spec(body)?;
                        if created_by == "bootstrap" {
                            if !kind_body_matches(&existing, default) {
                                // Publish a NEW version and retire the
                                // live one. This used to UPDATE the body
                                // in place and keep the version, so a Job
                                // pinned to that version silently began
                                // resolving a spec it never agreed to —
                                // which stranded in-flight Jobs whose
                                // materialized steps no longer matched
                                // the predicates closure now depended on.
                                let subject_kinds_json =
                                    serde_json::to_value(&default.subject_kinds)
                                        .map_err(|e| WorkflowError::Invalid(e.to_string()))?;
                                let steps_json = serde_json::to_value(&default.steps)
                                    .map_err(|e| WorkflowError::Invalid(e.to_string()))?;
                                let on_complete_create_json =
                                    serde_json::to_value(&default.on_complete_create)
                                        .map_err(|e| WorkflowError::Invalid(e.to_string()))?;

                                let mut tx = self
                                    .pool
                                    .begin()
                                    .await
                                    .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                                // Retire first: `workflows_one_active_per_kind`
                                // is a unique index over active rows, so the
                                // two cannot both be active even mid-transaction.
                                sqlx::query(
                                    "UPDATE workflows SET status = 'retired'
                                     WHERE kind = $1 AND version = $2",
                                )
                                .bind(&default.kind)
                                .bind(version)
                                .execute(&mut *tx)
                                .await
                                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                                let next: i32 = sqlx::query_scalar(
                                    "SELECT COALESCE(MAX(version), 0) + 1
                                     FROM workflows WHERE kind = $1",
                                )
                                .bind(&default.kind)
                                .fetch_one(&mut *tx)
                                .await
                                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                                sqlx::query(
                                    "INSERT INTO workflows
                                        (kind, version, status, label, description, category,
                                         subject_kinds, steps, metadata_schema, entitlements,
                                         metadata, on_complete_create, owning_team,
                                         authoring_job_id, created_by, created_at)
                                     VALUES ($1, $2, 'active', $3, $4, $5, $6, $7, $8, $9,
                                             $10, $11, $12, NULL, 'bootstrap', $13)",
                                )
                                .bind(&default.kind)
                                .bind(next)
                                .bind(&default.label)
                                .bind(&default.description)
                                .bind(&default.category)
                                .bind(&subject_kinds_json)
                                .bind(&steps_json)
                                .bind(&default.metadata_schema)
                                .bind(&default.entitlements)
                                .bind(&default.metadata)
                                .bind(&on_complete_create_json)
                                .bind(&default.owning_team)
                                .bind(now)
                                .execute(&mut *tx)
                                .await
                                .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                                let mut published = default.clone();
                                published.version = next;
                                published.status = WorkflowStatus::Active;
                                published.created_at = now;
                                published.authoring_job_id = None;
                                let event = crate::events::workflow_registry_event(
                                    crate::events::WORKFLOW_PUBLISHED,
                                    actor,
                                    &published,
                                );
                                boss_events::outbox::record_event_in_tx(&mut tx, &event)
                                    .await
                                    .map_err(WorkflowError::Storage)?;

                                tx.commit()
                                    .await
                                    .map_err(|e| WorkflowError::Storage(e.to_string()))?;

                                stats.republished += 1;
                            } else {
                                stats.unchanged += 1;
                            }
                        } else {
                            stats.preserved += 1;
                        }
                    }
                }
            }

            Ok(stats)
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgWorkflows;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {

    /// The platform bundle says exactly what the code used to say.
    ///
    /// This is protocols-as-data Q4 made mechanical. David's answer:
    /// "moving `user-feedback` v10 from code to bundle must produce a
    /// row identical to the live v10 — not v11. If the loader publishes
    /// a new version instead of recognising the existing one, every
    /// in-flight packet keeps its old spec and the board grows a second
    /// lineage."
    ///
    /// It lives here rather than in tests/ because the builder it
    /// compares against is private and no longer reachable from
    /// `platform_workflows()` — it was removed from that list when the
    /// kind moved to the bundle, and it survives only as this test's
    /// expected value. Deleting it is safe once a release has gone by
    /// with this green; until then it is the proof.
    ///
    /// The length assertion is the load-bearing half, and it has
    /// already earned its keep: it went red the first time three kinds
    /// were removed from `platform_workflows()` against a bundle
    /// holding one. A kind that leaves the code without arriving in
    /// the bundle exists in neither place on a fresh deployment, and
    /// nothing else in the tree would have said so.
    /// The bundle's own kinds pass the same viability lint the code
    /// kinds do.
    ///
    /// `validate_all` is what proves a protocol can FINISH — that
    /// every terminal is reachable and no step is orphaned. Rust
    /// kinds get it via `platform_workflows_passes_validate_all`;
    /// until now nothing pointed it at the bundle, so a protocol
    /// authored as data had strictly less checking than one authored
    /// as a literal. That gap is the wrong incentive to leave in
    /// place while the whole direction of travel is protocols
    /// becoming data.
    #[test]
    fn the_bundle_is_as_viable_as_the_code() {
        use crate::step_registry::StepRegistry;
        use crate::workflow_lint::validate_all;
        let bundled = crate::seed_loader::load_workflows(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/platform/workflows.toml"
        ))
        .expect("the platform bundle parses");
        let registry = StepRegistry::v1();
        let findings = validate_all(&bundled, &registry);
        assert!(
            findings.is_empty(),
            "the platform bundle has viability findings: {findings:#?}"
        );
    }

    #[test]
    fn the_platform_bundle_matches_the_specs_it_replaced() {
        let bundle_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/platform/workflows.toml"
        );
        let bundled =
            crate::seed_loader::load_workflows(bundle_path).expect("the platform bundle parses");
        let expected = [
            workflow_design_spec(),
            regenerate_deployment_spec(),
            backlog_item_spec(),
            ship_a_change_spec(),
        ];
        // Every CONVERTED kind must still be here. This is the
        // load-bearing half and it has earned its keep: it went red
        // the first time three kinds were removed from
        // `platform_workflows()` against a bundle holding one. A kind
        // that leaves the code without arriving in the bundle exists
        // in neither place on a fresh deployment.
        //
        // Not a length equality any more, because the bundle is now
        // also where NEW platform kinds are authored — `design-doc` was
        // never a Rust literal and has no spec to be faithful to.
        // Requiring len() to match would mean every new protocol
        // authored as data had to be added to a list of things
        // converted FROM code, which is backwards: the whole direction
        // of travel is that a new protocol never touches Rust at all.
        for want in &expected {
            assert!(
                bundled.iter().any(|b| b.kind == want.kind),
                "`{}` was converted to the bundle and has gone missing from it",
                want.kind
            );
        }
        // …and nothing in the bundle may SHADOW a kind the code still
        // seeds. That is the other direction of the same guarantee: a
        // kind in both places gets its bundle row republished from
        // code by bootstrap_reconcile on every boot.
        for b in &bundled {
            assert!(
                !platform_workflows().iter().any(|p| p.kind == b.kind),
                "`{}` is in BOTH the bundle and platform_workflows() — bootstrap_reconcile \
                 will republish the code version over the bundle's on every boot",
                b.kind
            );
        }
        for want in &expected {
            let got = bundled
                .iter()
                .find(|b| b.kind == want.kind)
                .unwrap_or_else(|| {
                    panic!("`{}` left the code but is not in the bundle", want.kind)
                });
            assert_eq!(got.label, want.label, "{}: label", want.kind);
            assert_eq!(got.category, want.category, "{}: category", want.kind);
            assert_eq!(
                got.subject_kinds, want.subject_kinds,
                "{}: subject_kinds",
                want.kind
            );
            assert_eq!(
                got.owning_team, want.owning_team,
                "{}: owning_team",
                want.kind
            );

            // THE STEPS ARE THE PROTOCOL, AND THEY WERE NOT CHECKED.
            //
            // Until 2026-08-26 this test compared four scalars and
            // stopped. The bundle's own header claimed it "asserts each
            // row equals the spec it replaces, field for field", and
            // that was simply not true: step titles, `ready_when`
            // predicates, fields, sign-offs, terminals and metadata
            // defaults were all unchecked. A conversion could rewrite
            // the entire flow and still pass, because label and
            // category matched.
            //
            // That gap matters most exactly where the conversion order
            // says the risk is highest. `ship-a-change` carries 255
            // packets; a mistranscribed predicate there — `steps.gate
            // .done` where the spec says something stricter — would
            // survive this test AND the viability lint, which only asks
            // whether the flow is reachable, not whether it is the same
            // flow. Every future car would then run a protocol nobody
            // chose.
            //
            // Compared as JSON rather than by PartialEq so the failure
            // names the offending step and shows both sides, which is
            // what a transcription error needs in order to be fixable.
            let got_steps = serde_json::to_value(&got.steps).expect("bundle steps serialize");
            let want_steps = serde_json::to_value(&want.steps).expect("spec steps serialize");
            assert_eq!(
                got_steps, want_steps,
                "{}: the bundle's steps differ from the spec they replaced. A conversion must \
                 change WHERE a protocol lives, never WHAT it says.",
                want.kind
            );
        }
    }
    use super::*;

    #[test]
    fn expand_metadata_substitutes_subject_fields_in_string_leaves() {
        let subject = Subject::new("account", "acc-bigseed-0042");
        let template = serde_json::json!({
            "account_id": "{subject.id}",
            "amount_cents": 420000,
            "currency": "USD",
            "line_items": [
                { "amount_cents": 240000, "description": "Pale Ale" },
                { "amount_cents": 180000, "description": "IPA" }
            ]
        });
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), None);
        assert_eq!(
            expanded.get("account_id").and_then(|v| v.as_str()),
            Some("acc-bigseed-0042"),
            "subject.id should substitute into account_id"
        );
        // Non-template strings pass through verbatim.
        assert_eq!(
            expanded.get("currency").and_then(|v| v.as_str()),
            Some("USD")
        );
        // Non-string leaves pass through.
        assert_eq!(
            expanded.get("amount_cents").and_then(|v| v.as_i64()),
            Some(420000)
        );
        // Recursion through arrays + nested objects works.
        let lines = expanded
            .get("line_items")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].get("description").and_then(|v| v.as_str()),
            Some("Pale Ale")
        );
    }

    #[test]
    fn missing_filer_fields_names_the_step_and_field() {
        use boss_core::job::{FilledBy, StepField};
        // A review-shaped step: two filer fields, one executor field.
        // The filer supplied `title`, forgot `markdown`, and the
        // executor's `resolutions` is legitimately absent at create.
        let mut step = Step::new(JobId::new(), "review-design", "Answer the questions", 0);
        step.spec_slug = Some("review".into());
        step.fields = vec![
            StepField {
                name: "title".into(),
                field_type: "string".into(),
                required: true,
                filled_by: FilledBy::Filer,
                item_keys: Vec::new(),
            },
            StepField {
                name: "markdown".into(),
                field_type: "string".into(),
                required: true,
                filled_by: FilledBy::Filer,
                item_keys: Vec::new(),
            },
            StepField {
                name: "resolutions".into(),
                field_type: "array".into(),
                required: true,
                filled_by: FilledBy::Executor,
                item_keys: Vec::new(),
            },
        ];
        step.metadata = serde_json::json!({ "title": "Packet loss" });

        let missing = missing_filer_fields(std::slice::from_ref(&step));
        assert_eq!(
            missing,
            vec![("review".to_string(), "markdown".to_string())],
            "only the absent FILER field is named; executor fields stay create-legal"
        );

        // Supplied → clean.
        step.metadata = serde_json::json!({ "title": "Packet loss", "markdown": "# doc" });
        assert!(missing_filer_fields(std::slice::from_ref(&step)).is_empty());
    }

    #[test]
    fn missing_filer_fields_reads_null_and_unexpanded_tokens_as_missing() {
        use boss_core::job::{FilledBy, StepField};
        let mut step = Step::new(JobId::new(), "review-design", "Answer the questions", 0);
        step.spec_slug = Some("review".into());
        step.fields = vec![StepField {
            name: "markdown".into(),
            field_type: "string".into(),
            required: true,
            filled_by: FilledBy::Filer,
            item_keys: Vec::new(),
        }];

        // An explicit null is not a value.
        step.metadata = serde_json::json!({ "markdown": null });
        assert_eq!(missing_filer_fields(std::slice::from_ref(&step)).len(), 1);

        // The binding idiom is `metadata_defaults = { markdown =
        // "{metadata.markdown}" }`, and expand_metadata leaves an
        // unmatched token LITERAL (its "visible bug report" contract).
        // At admission that literal is the report: the filer never
        // supplied the value, so it reads as missing rather than
        // riding to the reviewer as prose.
        step.metadata = serde_json::json!({ "markdown": "{metadata.markdown}" });
        assert_eq!(missing_filer_fields(std::slice::from_ref(&step)).len(), 1);

        // Real prose that merely CONTAINS a token is a value, not a
        // leftover binding — only a whole-string single token reads
        // as unexpanded.
        step.metadata =
            serde_json::json!({ "markdown": "Tokens like {metadata.x} expand at open." });
        assert!(missing_filer_fields(std::slice::from_ref(&step)).is_empty());
    }

    #[test]
    fn missing_filer_fields_checks_the_shape_of_a_structured_field() {
        use boss_core::job::{FilledBy, StepField};
        // The design-doc review: `questions` is a filer field whose
        // elements must each carry anchor, title and proposal. Eight
        // packets on 2026-09-04/05 were admitted with no `title` on
        // any element and rendered "nothing to review" — the same
        // outcome as no questions at all, so it is the same refusal.
        let mut step = Step::new(JobId::new(), "review-design", "Answer the questions", 0);
        step.spec_slug = Some("review".into());
        step.fields = vec![StepField {
            name: "questions".into(),
            field_type: "array".into(),
            required: true,
            filled_by: FilledBy::Filer,
            item_keys: vec!["anchor".into(), "title".into(), "proposal".into()],
        }];

        // A title-less element is named by index and key.
        step.metadata = serde_json::json!({ "questions": [
            { "anchor": "Q1", "title": "first brick?", "proposal": "the cheap one" },
            { "anchor": "Q2", "proposal": "ship it" },
            { "anchor": "Q3", "title": "  ", "proposal": "ship it" },
        ]});
        assert_eq!(
            missing_filer_fields(std::slice::from_ref(&step)),
            vec![
                ("review".to_string(), "questions[1].title".to_string()),
                ("review".to_string(), "questions[2].title".to_string()),
            ],
            "each element missing a declared key is named; blank counts as missing"
        );

        // Not an array at all — prose where the tracker wants data —
        // is the original 2026-09-02 defect (questions written into
        // `detail`) and is refused by name.
        step.metadata = serde_json::json!({ "questions": "Q1: first brick? ship the cheap one" });
        assert_eq!(
            missing_filer_fields(std::slice::from_ref(&step)),
            vec![("review".to_string(), "questions (not an array)".to_string())]
        );

        // Absent stays the bare field name — presence first.
        step.metadata = serde_json::json!({});
        assert_eq!(
            missing_filer_fields(std::slice::from_ref(&step)),
            vec![("review".to_string(), "questions".to_string())]
        );

        // Well-shaped, and an EMPTY array, both admit: an empty array
        // is the filer's explicit "no open questions" (boss design
        // --no-questions writes exactly that), not an omission.
        step.metadata = serde_json::json!({ "questions": [
            { "anchor": "Q1", "title": "first brick?", "proposal": "the cheap one" },
        ]});
        assert!(missing_filer_fields(std::slice::from_ref(&step)).is_empty());
        step.metadata = serde_json::json!({ "questions": [] });
        assert!(missing_filer_fields(std::slice::from_ref(&step)).is_empty());
    }

    #[test]
    fn missing_filer_fields_ignores_optional_filer_fields() {
        use boss_core::job::{FilledBy, StepField};
        let mut step = Step::new(JobId::new(), "review-design", "Answer the questions", 0);
        step.fields = vec![StepField {
            name: "doc_path".into(),
            field_type: "string".into(),
            required: false,
            filled_by: FilledBy::Filer,
            item_keys: Vec::new(),
        }];
        assert!(
            missing_filer_fields(std::slice::from_ref(&step)).is_empty(),
            "an optional filer field is advisory; only required ones gate admission"
        );
    }

    #[test]
    fn expand_metadata_handles_compound_template_strings() {
        // The bridge handlers accept `"PO-{subject.id}"` as the
        // po_id default — substitution must work mid-string, not
        // only when the entire value is a single token.
        let subject = Subject::new("vendor", "vnd-malt-001");
        let template = serde_json::json!({ "po_id": "PO-{subject.id}" });
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), None);
        assert_eq!(
            expanded.get("po_id").and_then(|v| v.as_str()),
            Some("PO-vnd-malt-001")
        );
    }

    #[test]
    fn expand_metadata_carries_a_structured_job_value_whole_into_a_step() {
        // Defect 6f40b23f: a design review opened blank because the
        // question set could not reach the step. The plugin renders
        // `step.metadata.questions` when a packet carries its own
        // work; the spawn bound the questions onto the JOB, and this
        // expansion dropped them, because only scalars were in the
        // substitution table.
        let subject = Subject::new("custom", "docs/design/packet-loss.md");
        let job_meta = serde_json::json!({
            "questions": [
                {"anchor": "Q1", "title": "How do we detect a dropped packet?"},
                {"anchor": "Q2", "title": "Who is told, and how fast?"},
            ],
        });
        let template = serde_json::json!({ "questions": "{metadata.questions}" });
        let expanded = expand_metadata(&template, &subject, &job_meta, None);
        let got = expanded.get("questions").expect("questions present");
        assert!(
            got.is_array(),
            "the step must receive the ARRAY, not the literal token — it was {got:?}"
        );
        assert_eq!(got.as_array().map(Vec::len), Some(2));
        assert_eq!(
            got[1].get("anchor").and_then(|v| v.as_str()),
            Some("Q2"),
            "carried whole, not flattened or re-ordered"
        );
    }

    #[test]
    fn expand_metadata_carries_a_structured_value_that_is_an_object_too() {
        let subject = Subject::new("custom", "x");
        let job_meta = serde_json::json!({ "doc": {"title": "Packet loss", "words": 900} });
        let expanded = expand_metadata(
            &serde_json::json!({ "doc": "{metadata.doc}" }),
            &subject,
            &job_meta,
            None,
        );
        assert_eq!(
            expanded
                .get("doc")
                .and_then(|d| d.get("title"))
                .and_then(|v| v.as_str()),
            Some("Packet loss")
        );
    }

    #[test]
    fn expand_metadata_still_stringifies_scalars_exactly_as_before() {
        // The narrowing that keeps this change safe. A scalar token
        // must keep returning a STRING: `check_metadata_defaults_values`
        // type-checks the result against the StepType's declared field,
        // and every existing template was authored against this
        // behaviour.
        let subject = Subject::new("custom", "x");
        let job_meta = serde_json::json!({ "part_sku": "malt-2row", "count": 5 });
        let expanded = expand_metadata(
            &serde_json::json!({ "sku": "{metadata.part_sku}", "n": "{metadata.count}" }),
            &subject,
            &job_meta,
            None,
        );
        assert_eq!(
            expanded.get("sku").and_then(|v| v.as_str()),
            Some("malt-2row")
        );
        assert_eq!(
            expanded.get("n").and_then(|v| v.as_str()),
            Some("5"),
            "a number token stays a string — changing this would retype every existing default"
        );
    }

    #[test]
    fn expand_metadata_does_not_take_a_whole_value_for_a_concatenation() {
        // Two tokens in one string is string-building; there is no
        // whole-value reading of it, so it must fall through to
        // substitution (and an array, absent from the scalar table,
        // simply leaves its token alone rather than corrupting it).
        let subject = Subject::new("custom", "x");
        let job_meta = serde_json::json!({ "a": [1, 2], "b": "tail" });
        let expanded = expand_metadata(
            &serde_json::json!({ "joined": "{metadata.a}{metadata.b}" }),
            &subject,
            &job_meta,
            None,
        );
        assert_eq!(
            expanded.get("joined").and_then(|v| v.as_str()),
            Some("{metadata.a}tail"),
            "concatenation stays a string"
        );
    }

    #[test]
    fn expand_metadata_leaves_a_structured_token_alone_when_the_job_has_no_such_key() {
        let subject = Subject::new("custom", "x");
        let expanded = expand_metadata(
            &serde_json::json!({ "questions": "{metadata.questions}" }),
            &subject,
            &serde_json::json!({}),
            None,
        );
        assert_eq!(
            expanded.get("questions").and_then(|v| v.as_str()),
            Some("{metadata.questions}"),
            "an unbound token passes through verbatim, as every other unknown token does"
        );
    }

    #[test]
    fn expand_metadata_leaves_unknown_tokens_alone() {
        // No `{subject.X}` token = no rewrite. Unknown tokens
        // (e.g. `{job.id}` — a future syntax) pass through
        // verbatim so no false substitutions sneak in.
        let subject = Subject::new("account", "acc-1");
        let template = serde_json::json!({
            "literal": "literal value with no token",
            "future": "something {job.id} that we don't expand yet",
        });
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), None);
        assert_eq!(
            expanded.get("literal").and_then(|v| v.as_str()),
            Some("literal value with no token")
        );
        assert_eq!(
            expanded.get("future").and_then(|v| v.as_str()),
            Some("something {job.id} that we don't expand yet")
        );
    }

    #[test]
    fn expand_metadata_substitutes_day_tokens_when_anchor_given() {
        let subject = Subject::new("location", "loc-brewery-brewhouse");
        let template = serde_json::json!({
            "period_start": "{day_minus_13}",
            "period_end": "{day}",
            "run_date": "{day_plus_1}",
        });
        let day = chrono::NaiveDate::from_ymd_opt(2025, 4, 14).unwrap();
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), Some(day));
        assert_eq!(expanded["period_start"], "2025-04-01");
        assert_eq!(expanded["period_end"], "2025-04-14");
        assert_eq!(expanded["run_date"], "2025-04-15");
    }

    #[test]
    fn expand_metadata_leaves_day_tokens_literal_when_anchor_none() {
        // Live-API callers without a clock get literal pass-through.
        // The downstream handler will then error on the malformed
        // date — a loud failure beats a silent wallclock stamp.
        let subject = Subject::new("location", "loc-1");
        let template = serde_json::json!({
            "period_start": "{day_minus_13}",
        });
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), None);
        assert_eq!(expanded["period_start"], "{day_minus_13}");
    }

    #[test]
    fn job_trigger_round_trips_through_json_with_defaults() {
        // Empty `on_complete_create` is the common case and must
        // not bloat the wire/DB shape — `skip_serializing_if` keeps
        // it absent from output rather than serializing `[]`.
        let mut spec = WorkflowSpec::platform_seed(
            "wholesale-order",
            "Wholesale Order",
            "sales",
            vec!["account".into()],
            Vec::new(),
        );
        let empty_json = serde_json::to_value(&spec).unwrap();
        assert!(empty_json.get("on_complete_create").is_none());

        // With a real trigger: a wholesale-order Job spawning an
        // invoice Job on the same Subject. Round-trips through JSON
        // including the default subject_source.
        spec.on_complete_create = vec![JobTrigger {
            kind: "invoice".to_string(),
            subject_source: "same".to_string(),
            metadata_seed: serde_json::json!({"source_kind": "wholesale-order"}),
        }];
        let with_trigger = serde_json::to_value(&spec).unwrap();
        assert_eq!(with_trigger["on_complete_create"][0]["kind"], "invoice");
        assert_eq!(
            with_trigger["on_complete_create"][0]["subject_source"],
            "same"
        );

        // Missing subject_source decodes to "same" via the default.
        let json_without_source = serde_json::json!({
            "kind": "invoice",
            "metadata_seed": {}
        });
        let trigger: JobTrigger = serde_json::from_value(json_without_source).unwrap();
        assert_eq!(trigger.subject_source, "same");
    }

    /// A minimal VIABLE spec — one trigger flowing into one terminal.
    /// Every write path that sets a row ACTIVE runs the viability
    /// gate, so a step-less fixture would be refused (and described
    /// work no Job could ever finish, which is why the gate exists).
    fn seed_spec(kind: &str) -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            kind,
            format!("Test {kind}"),
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "start".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "finish".into(),
                    kind: "task".into(),
                    ready_when: "steps.start.done".into(),
                    terminal: Some(Terminal {
                        outcome: "done".into(),
                    }),
                    ..Default::default()
                },
            ],
        )
    }

    /// Shared write-path actor: every registry write records an
    /// event, and the event needs a who. Tests are exempt from the
    /// no-wallclock lint, so `Utc::now()` rides along at call sites.
    fn test_actor() -> boss_core::actor::ActorId {
        boss_core::actor::ActorId::Human("emp-test".into())
    }

    #[tokio::test]
    async fn create_draft_assigns_next_version() {
        let reg = InMemoryWorkflows::new();
        let v1 = reg
            .create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(v1.status, WorkflowStatus::Draft);

        let v2 = reg
            .create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(v2.status, WorkflowStatus::Draft);
    }

    #[tokio::test]
    async fn publish_promotes_draft_and_retires_previous_active() {
        let reg = InMemoryWorkflows::new();

        // v1 drafted and published.
        reg.create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        let active_v1 = reg
            .publish("repair", &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(active_v1.version, 1);
        assert_eq!(active_v1.status, WorkflowStatus::Active);

        // v2 drafted and published.
        reg.create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        let active_v2 = reg
            .publish("repair", &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(active_v2.version, 2);
        assert_eq!(active_v2.status, WorkflowStatus::Active);

        // v1 now retired; only v2 is active.
        let v1 = reg.get_version("repair", 1).await.unwrap();
        assert_eq!(v1.status, WorkflowStatus::Retired);
        let current = reg.get_active("repair").await.unwrap();
        assert_eq!(current.version, 2);
    }

    #[tokio::test]
    async fn retire_flips_active_to_retired() {
        let reg = InMemoryWorkflows::new();
        reg.create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.publish("repair", &test_actor(), Utc::now())
            .await
            .unwrap();

        reg.retire("repair", &test_actor(), Utc::now())
            .await
            .unwrap();
        let err = reg.get_active("repair").await.unwrap_err();
        match err {
            WorkflowError::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    /// ebd7bb70: the publish guard's "resolve that draft first" now has
    /// a resolution. A draft admitted nothing and may be removed; the
    /// removal goes on the record; history refuses; a typo is NotFound.
    #[tokio::test]
    async fn a_draft_can_be_discarded_and_history_cannot() {
        let reg = InMemoryWorkflows::new();
        let d = reg
            .create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.discard_draft("repair", d.version, &test_actor(), Utc::now())
            .await
            .unwrap();
        // Gone: publishing now finds no draft.
        match reg.publish("repair", &test_actor(), Utc::now()).await {
            Err(WorkflowError::NotFound(_)) => {}
            other => panic!("draft should be gone, got {other:?}"),
        }
        // The discard is on the record.
        assert!(
            reg.recorded_events()
                .iter()
                .any(|e| e.kind == crate::events::WORKFLOW_DRAFT_DISCARDED),
            "discard must record jobs.kind.draft_discarded"
        );
        // History refuses: publish a fresh draft, then try to discard it.
        let d2 = reg
            .create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.publish("repair", &test_actor(), Utc::now())
            .await
            .unwrap();
        match reg
            .discard_draft("repair", d2.version, &test_actor(), Utc::now())
            .await
        {
            Err(WorkflowError::Conflict(m)) => assert!(m.contains("history"), "{m}"),
            other => panic!("active must refuse discard, got {other:?}"),
        }
        // A version that never existed is NotFound, not a silent success.
        match reg
            .discard_draft("repair", 99, &test_actor(), Utc::now())
            .await
        {
            Err(WorkflowError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retire_is_idempotent() {
        let reg = InMemoryWorkflows::new();
        // Retiring a kind with no active row is a no-op, not an error.
        reg.retire("never-existed", &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.retire("never-existed", &test_actor(), Utc::now())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn publish_without_draft_returns_not_found() {
        let reg = InMemoryWorkflows::new();
        let err = reg
            .publish("repair", &test_actor(), Utc::now())
            .await
            .unwrap_err();
        match err {
            WorkflowError::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_active_filters_by_category() {
        let reg = InMemoryWorkflows::new();
        let mut refurb = seed_spec("refurb");
        refurb.category = "refurb".into();
        reg.seed(WorkflowSpec {
            status: WorkflowStatus::Active,
            ..refurb
        })
        .unwrap();

        let mut sale = seed_spec("sale");
        sale.category = "sales".into();
        reg.seed(WorkflowSpec {
            status: WorkflowStatus::Active,
            ..sale
        })
        .unwrap();

        let all = reg.list_active(None).await.unwrap();
        assert_eq!(all.len(), 2);

        let sales_only = reg.list_active(Some("sales")).await.unwrap();
        assert_eq!(sales_only.len(), 1);
        assert_eq!(sales_only[0].kind, "sale");
    }

    #[test]
    fn materialize_expands_title_template_and_sets_blocked_by() {
        // v2: the DAG is implicit in `ready_when`. `intake` is the
        // trigger; `diagnosis` + `parts-pull` both depend on it
        // (recovered into `blocked_by` from their predicates) and are
        // terminals so the spec is viable.
        let spec = WorkflowSpec::platform_seed(
            "repair",
            "Repair",
            "service",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "intake".into(),
                    kind: "intake".into(),
                    ready_when: "true".into(),
                    title_template: "Intake {subject.id}".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "diagnosis".into(),
                    kind: "diagnosis".into(),
                    ready_when: "steps.intake.done".into(),
                    title_template: "Diagnose".into(),
                    terminal: Some(Terminal {
                        outcome: "diagnosed".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "parts-pull".into(),
                    kind: "parts-pull".into(),
                    ready_when: "steps.intake.done".into(),
                    title_template: "Parts check".into(),
                    terminal: Some(Terminal {
                        outcome: "pulled".into(),
                    }),
                    ..Default::default()
                },
            ],
        );

        let subject = Subject::new("asset", "SYS-42");
        let job_id = JobId::new();
        let job_metadata = serde_json::Value::Object(Default::default());

        let mut counter = 0u32;
        let steps = materialize_steps(&spec, &subject, job_id, &job_metadata, || {
            counter += 1;
            let raw = Uuid::from_u128(counter as u128);
            StepId::from_uuid(raw)
        });

        assert_eq!(steps.len(), 3);

        // Trigger intake: title expanded, no deps, Ready at open.
        assert_eq!(steps[0].kind, "intake");
        assert_eq!(steps[0].title, "Intake SYS-42");
        assert!(steps[0].blocked_by.is_empty());
        assert_eq!(steps[0].status, StepStatus::Ready);

        // Dependents: both blocked_by the trigger step, Pending at open.
        assert_eq!(steps[1].kind, "diagnosis");
        assert_eq!(steps[1].blocked_by, vec![steps[0].id]);
        assert_eq!(steps[1].status, StepStatus::Pending);
        assert_eq!(steps[2].kind, "parts-pull");
        assert_eq!(steps[2].blocked_by, vec![steps[0].id]);

        // job_id threaded through.
        for s in &steps {
            assert_eq!(s.job_id, job_id);
        }
    }

    /// The feedback flow collects exactly one thing per DECIDING step
    /// — a disposition — so every step must close on its own defaults
    /// plus that.
    ///
    /// There are two deciding steps since v10: triage routes on what
    /// the report says, and `investigate` re-routes on what the
    /// investigation found. Both are human-gated `task` steps whose
    /// declared fields the generic step surface renders, so both are
    /// collectable. A required field on any OTHER step would not be.
    ///
    /// Both halves matter. A required field anywhere else is a step
    /// nothing in this flow can satisfy, because no surface asks for
    /// it: the chrome control posts a message and the board sends a
    /// status flip. And triage must genuinely require the disposition,
    /// or routing becomes optional and the fork decorative.
    ///
    /// Regression this came from: triage shipped as `acknowledgment`,
    /// which requires `document_title`. Validators run at `completed`,
    /// so the Job materialized cleanly and looked healthy in the
    /// The `user-feedback` protocol, read from the bundle it now lives
    /// in (e332a320). These tests used to call a Rust `user_feedback_spec()`;
    /// reading the bundle is strictly stronger, because the bundle is
    /// what a deployment actually seeds — a test against the Rust copy
    /// could pass while the shipped protocol differed.
    fn user_feedback_spec() -> WorkflowSpec {
        crate::seed_loader::load_workflows(platform_bundle_path())
            .expect("the platform Workflow bundle parses")
            .into_iter()
            .find(|w| w.kind == "user-feedback")
            .expect("the bundle carries user-feedback")
    }

    /// waiting column; the only symptom was a 400 the first time a
    /// human tried to triage real feedback.
    #[test]
    fn user_feedback_collects_only_the_triage_disposition() {
        let spec = user_feedback_spec();
        let registry = crate::step_registry::StepRegistry::v1();
        let subject = Subject::new("custom", "/system");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut i = 0u32;
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, || {
            i += 1;
            StepId::from_uuid(Uuid::from_u128(i as u128))
        });
        assert!(!steps.is_empty(), "user-feedback materialized no steps");

        let mut operator_supplied: Vec<String> = Vec::new();

        for s in &steps {
            let mut metadata = s.metadata.clone();

            // Stand in for what the operator types. A pipe-shaped
            // field_type is an enum, so the first value is a legal
            // answer; anything else required here would be a field no
            // surface in this flow collects.
            for f in s.fields.iter().filter(|f| f.required) {
                operator_supplied.push(format!("{}.{}", s.title, f.name));
                let sample = f.field_type.split('|').next().unwrap_or("x");
                if let serde_json::Value::Object(m) = &mut metadata {
                    m.insert(
                        f.name.clone(),
                        serde_json::Value::String(sample.to_string()),
                    );
                }
            }
            // …and what the KIND's own surface collects. A StepType's
            // required fields are the kind's completion contract, and
            // the kind's surface (plugin or platform) is what asks for
            // them — v11's `answer-question` design-review collects
            // verdict + answer through its own form, exactly like the
            // approval Workflow's decide step. Only spec-declared
            // fields were simulated before, which made any kind with
            // its own required fields read as "can never complete".
            if let Some(st) = registry.get(&s.kind) {
                for f in st.fields.iter().filter(|f| f.required) {
                    let sample = f.field_type.split('|').next().unwrap_or("x");
                    if let serde_json::Value::Object(m) = &mut metadata
                        && !m.contains_key(f.name)
                    {
                        m.insert(
                            f.name.to_string(),
                            serde_json::Value::String(sample.to_string()),
                        );
                    }
                }
            }

            let result = registry
                .validate_metadata(&s.kind, &metadata)
                .and_then(|()| {
                    crate::step_registry::StepRegistry::validate_authored_fields(
                        &s.fields, &metadata,
                    )
                });
            if let Err(errors) = result {
                panic!(
                    "step `{}` (kind `{}`) can never be completed as materialized — {}",
                    s.title,
                    s.kind,
                    errors
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
        }

        assert_eq!(
            operator_supplied,
            vec![
                "Triage feedback.disposition".to_string(),
                "Reproduce and investigate.disposition".to_string(),
            ],
            "the feedback flow collects a disposition at each deciding step and nothing \
             else; anything else here is a step no surface can complete"
        );
    }

    /// The routing table triage actually implements. A shipped change
    /// answers a feedback packet by completing the step its
    /// disposition opened, so "which step did `build` open" has to be
    /// an answerable question rather than something a caller guesses
    /// from the label.
    #[test]
    fn each_disposition_names_the_branch_it_opened() {
        for (disposition, slug, terminal) in [
            ("reproduce", "investigate", false),
            ("design", "design-review", false),
            ("build", "build", false),
            ("needs-info", "needs-info", false),
            ("duplicate", "duplicate", true),
            ("decline", "declined", true),
        ] {
            let branch = feedback_branch_for_disposition(disposition)
                .unwrap_or_else(|| panic!("`{disposition}` opens no branch"));
            assert_eq!(
                branch.slug, slug,
                "`{disposition}` routed to the wrong step"
            );
            assert_eq!(
                branch.terminal, terminal,
                "`{disposition}` disagrees about whether its branch ends the Job"
            );
        }
    }

    /// EVERY value of the disposition enum resolves — the same
    /// property the viability lint proves about successors, asserted
    /// against the lookup callers use. A disposition with no branch
    /// here is an item that routes nowhere.
    #[test]
    fn every_declared_disposition_resolves_to_a_branch() {
        let spec = user_feedback_spec();
        let triage = spec
            .steps
            .iter()
            .find(|s| s.title == "triage")
            .expect("triage step present");
        let field = triage
            .fields
            .iter()
            .find(|f| f.name == "disposition")
            .expect("triage collects a disposition");
        for value in field.field_type.split('|') {
            assert!(
                feedback_branch_for_disposition(value).is_some(),
                "disposition `{value}` is offered to triagers but opens no branch"
            );
        }
    }

    /// An unknown disposition is `None`, not a wrong answer. A caller
    /// completing "whatever branch this opened" must be able to tell
    /// "nothing" from "something".
    #[test]
    fn an_unknown_disposition_opens_no_branch() {
        assert_eq!(feedback_branch_for_disposition("wontfix"), None);
        assert_eq!(feedback_branch_for_disposition(""), None);
    }

    #[test]
    fn materialize_surfaces_claimable_and_leaves_authority_intact() {
        // The dispatcher reads the materialized STEP, never the spec —
        // it reacts to an event with no workflow row in hand. So
        // `claimable` has to ride into step metadata the same way
        // `authority_role` does, or the flag is invisible where it is
        // consulted.
        //
        // The second assertion is the one that matters for security:
        // making a step claimable must not disturb its authority.
        // Claimability decides who the packet WAITS for; authority
        // decides who may act. Widening the first while silently
        // clearing the second would turn a queue into an open door.
        let spec = WorkflowSpec::platform_seed(
            "review",
            "Review",
            "governance",
            vec!["custom".into()],
            vec![StepSpec {
                title: "decide".into(),
                kind: "sign-off".into(),
                ready_when: "true".into(),
                authority_role: Some("platform-admin".into()),
                claimable: Some(true),
                terminal: Some(Terminal {
                    outcome: "decided".into(),
                }),
                ..Default::default()
            }],
        );
        let subject = Subject::new("custom", "docs/design/x.md");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut i = 0u32;
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, || {
            i += 1;
            StepId::from_uuid(Uuid::from_u128(i as u128))
        });
        let md = &steps[0].metadata;
        assert_eq!(
            md.get("claimable").and_then(|v| v.as_bool()),
            Some(true),
            "the dispatcher cannot honour a flag it cannot see"
        );
        assert_eq!(
            md.get("authority_role").and_then(|v| v.as_str()),
            Some("platform-admin"),
            "claimable must not disturb authority — a role queue is \
             still gated on the role"
        );
    }

    #[test]
    fn a_step_says_nothing_about_claimability_by_default() {
        // Absent means today's behaviour. Every existing Workflow row
        // deserialises with `claimable: None` and must materialize
        // exactly as it does now, or this change silently unassigns
        // the whole system.
        let spec = WorkflowSpec::platform_seed(
            "review",
            "Review",
            "governance",
            vec!["custom".into()],
            vec![StepSpec {
                title: "decide".into(),
                kind: "sign-off".into(),
                ready_when: "true".into(),
                authority_role: Some("platform-admin".into()),
                terminal: Some(Terminal {
                    outcome: "decided".into(),
                }),
                ..Default::default()
            }],
        );
        let subject = Subject::new("custom", "x");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut i = 0u32;
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, || {
            i += 1;
            StepId::from_uuid(Uuid::from_u128(i as u128))
        });
        assert!(
            steps[0].metadata.get("claimable").is_none(),
            "an unset flag must not appear in step metadata at all — the \
             dispatcher defaults it to false, and writing it explicitly \
             would make every step look deliberately non-claimable"
        );
    }

    #[test]
    fn materialize_surfaces_authority_role_into_metadata() {
        // Single-step Workflow: the one step is both the trigger and
        // the terminal (viable). authority_role must be surfaced into
        // step metadata so the sign-off gate can enforce it.
        let spec = WorkflowSpec::platform_seed(
            "cert",
            "Certification",
            "service",
            vec!["asset".into()],
            vec![StepSpec {
                title: "annual".into(),
                kind: "sign-off".into(),
                ready_when: "true".into(),
                title_template: "Annual".into(),
                sign_offs_required: vec!["platform-admin".into()],
                assurance_required: None,
                authority_role: Some("qa-lead".into()),
                terminal: Some(Terminal {
                    outcome: "certified".into(),
                }),
                ..Default::default()
            }],
        );
        let subject = Subject::new("asset", "SYS-1");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut i = 0u32;
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, || {
            i += 1;
            StepId::from_uuid(Uuid::from_u128(i as u128))
        });
        assert_eq!(
            steps[0]
                .metadata
                .get("authority_role")
                .and_then(|v| v.as_str()),
            Some("qa-lead"),
            "authority_role must land in step metadata so the sign-off gate picks it up"
        );
        assert_eq!(
            steps[0].sign_offs_required,
            vec!["platform-admin".to_string()]
        );
    }

    #[tokio::test]
    async fn list_versions_returns_oldest_first() {
        let reg = InMemoryWorkflows::new();
        reg.create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.publish("repair", &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.publish("repair", &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();

        let versions = reg.list_versions("repair").await.unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].version, 1);
        assert_eq!(versions[0].status, WorkflowStatus::Retired);
        assert_eq!(versions[1].version, 2);
        assert_eq!(versions[1].status, WorkflowStatus::Active);
        assert_eq!(versions[2].version, 3);
        assert_eq!(versions[2].status, WorkflowStatus::Draft);
    }

    // ----- v2 conditional skip: predicate-driven Skipped status -----
    //
    // v1 modeled "this tier doesn't apply to this subject" by omitting
    // the tier from materialization (`skip_when` on TierSpec). v2 makes
    // every step materialize eagerly and uses `ready_when` + the
    // re-evaluator: a branch not taken becomes `Skipped` once its
    // referenced steps are all terminal. These tests pin that behavior.

    /// A fork Workflow: `decide` (trigger) routes to either `ship` or
    /// `scrap` based on `decide`'s `outcome` metadata. Both branches
    /// are terminals, so the spec is viable.
    fn fork_spec() -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            "fork-job",
            "Fork Job",
            "test",
            vec!["account".into()],
            vec![
                StepSpec {
                    title: "decide".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "ship".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.outcome = \"approved\"".into(),
                    terminal: Some(Terminal {
                        outcome: "shipped".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "scrap".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.outcome = \"rejected\"".into(),
                    terminal: Some(Terminal {
                        outcome: "scrapped".into(),
                    }),
                    ..Default::default()
                },
            ],
        )
    }

    #[test]
    fn eager_materialization_creates_every_step() {
        // v2: every step materializes at Job open (no tier omission).
        // The trigger is Ready; the branches are Pending (their
        // predicates can't resolve until `decide` completes).
        let spec = fork_spec();
        let subject = Subject::new("account", "a-1");
        let job_metadata = serde_json::Value::Object(Default::default());
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);
        assert_eq!(steps.len(), 3, "all steps materialize eagerly");
        assert_eq!(steps[0].status, StepStatus::Ready, "trigger is Ready");
        assert_eq!(steps[1].status, StepStatus::Pending, "ship awaits decide");
        assert_eq!(steps[2].status, StepStatus::Pending, "scrap awaits decide");
        // blocked_by edges recovered from the predicates: both
        // branches depend on `decide`.
        assert_eq!(steps[1].blocked_by, vec![steps[0].id]);
        assert_eq!(steps[2].blocked_by, vec![steps[0].id]);
    }

    #[test]
    fn reevaluate_skips_branch_not_taken() {
        // After `decide` completes with outcome=approved, the
        // re-evaluator promotes `ship` to Ready and, because `scrap`'s
        // predicate is now provably false (its only ref `decide` is
        // terminal), marks `scrap` Skipped.
        let spec = fork_spec();
        let subject = Subject::new("account", "a-1");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut steps =
            materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);

        // Complete `decide` with the approved outcome.
        steps[0].status = StepStatus::Completed;
        steps[0].metadata = serde_json::json!({ "outcome": "approved" });

        let changed = reevaluate(&spec, &mut steps, &subject, &job_metadata);
        // Both branches changed status (ship → Ready, scrap → Skipped).
        assert!(changed.contains(&1), "ship should change: {changed:?}");
        assert!(changed.contains(&2), "scrap should change: {changed:?}");
        assert_eq!(
            steps[1].status,
            StepStatus::Ready,
            "approved branch is Ready"
        );
        assert_eq!(
            steps[2].status,
            StepStatus::Skipped,
            "branch not taken is Skipped"
        );
    }

    #[test]
    fn reevaluate_promotes_linear_dependent_to_ready() {
        // A simple chain: trigger → work(terminal). The dependent
        // sits Pending at open, then `reevaluate` promotes it to Ready
        // once the trigger completes. Returns the promoted index.
        let spec = WorkflowSpec::platform_seed(
            "chain",
            "Chain",
            "test",
            vec!["account".into()],
            vec![
                StepSpec {
                    title: "trigger".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "work".into(),
                    kind: "task".into(),
                    ready_when: "steps.trigger.done".into(),
                    terminal: Some(Terminal {
                        outcome: "done".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let subject = Subject::new("account", "a-1");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut steps =
            materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);
        assert_eq!(steps[1].status, StepStatus::Pending);
        // The dependent's blocked_by points at the trigger.
        assert_eq!(steps[1].blocked_by, vec![steps[0].id]);

        steps[0].status = StepStatus::Completed;
        let changed = reevaluate(&spec, &mut steps, &subject, &job_metadata);
        assert_eq!(changed, vec![1]);
        assert_eq!(steps[1].status, StepStatus::Ready);
    }

    /// A two-step chain used by the pairing tests below.
    fn pairing_spec() -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            "chain",
            "Chain",
            "test",
            vec!["account".into()],
            vec![
                StepSpec {
                    title: "trigger".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "work".into(),
                    kind: "task".into(),
                    ready_when: "steps.trigger.done".into(),
                    ..Default::default()
                },
            ],
        )
    }

    /// THE SAFETY PROPERTY, AND THE REASON THIS CHANGE IS SHIPPABLE.
    ///
    /// Every healthy packet — slugs present, matching the spec, in
    /// order — must pair EXACTLY as index-pairing did. If this holds,
    /// the change is inert for all existing work and only alters the
    /// packets that were previously frozen.
    #[test]
    fn a_healthy_packet_pairs_to_the_identity() {
        let spec = pairing_spec();
        let subject = Subject::new("account", "a-1");
        let md = serde_json::Value::Object(Default::default());
        let steps = materialize_steps(&spec, &subject, JobId::new(), &md, StepId::new);

        assert_eq!(
            pair_steps(&spec, &steps),
            vec![Some(0), Some(1)],
            "a materialized packet must pair positionally, or this change is \
             not inert for existing work"
        );
        assert!(!steps_diverged_from_spec(&spec, &steps));
    }

    /// THE DEFECT THIS FIXES. A step appended to a live job used to
    /// misalign every pair after it, so `reevaluate` bailed and the job
    /// never advanced again — design review 32a4e70d, which surfaced
    /// only as "I finished it and it is still there".
    #[test]
    fn an_extra_step_no_longer_freezes_the_job() {
        let spec = pairing_spec();
        let subject = Subject::new("account", "a-1");
        let md = serde_json::Value::Object(Default::default());
        let mut steps = materialize_steps(&spec, &subject, JobId::new(), &md, StepId::new);

        // Someone POSTs a step onto the live job, between the two.
        let mut extra = steps[1].clone();
        extra.id = StepId::new();
        extra.spec_slug = Some("an-appended-step".into());
        extra.status = StepStatus::Pending;
        steps.insert(1, extra);

        steps[0].status = StepStatus::Completed;
        let changed = reevaluate(&spec, &mut steps, &subject, &md);

        assert_eq!(
            steps[2].status,
            StepStatus::Ready,
            "the real `work` step must still promote; before slug pairing this \
             job was frozen forever"
        );
        assert_eq!(
            changed,
            vec![2],
            "the returned indices must be JOB indices — callers persist steps[i]"
        );
    }

    /// AND THE WORSE HALF: misalignment did not only stall a job, it
    /// made the predicate context answer about the wrong step, because
    /// `build_context` keys by spec slug and used to read positionally.
    #[test]
    fn a_reordered_packet_answers_about_the_right_step() {
        let spec = pairing_spec();
        let subject = Subject::new("account", "a-1");
        let md = serde_json::Value::Object(Default::default());
        let mut steps = materialize_steps(&spec, &subject, JobId::new(), &md, StepId::new);
        steps.swap(0, 1);

        // `trigger` is complete; `work` is not. Positionally these are
        // now the other way round, so an index-paired context would
        // report steps.trigger.done from the `work` row.
        for s in steps.iter_mut() {
            if s.spec_slug.as_deref() == Some("trigger") {
                s.status = StepStatus::Completed;
            }
        }
        reevaluate(&spec, &mut steps, &subject, &md);

        let work = steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some("work"))
            .expect("work step");
        assert_eq!(
            work.status,
            StepStatus::Ready,
            "steps.trigger.done must read the trigger row, whatever position \
             it occupies"
        );
    }

    /// A packet materialized before `spec_slug` existed has only its
    /// index. Falling back wholesale keeps those behaving exactly as
    /// they do today rather than half-pairing them, which would be a
    /// new failure mode rather than a fix.
    #[test]
    fn a_packet_without_slugs_still_pairs_by_index() {
        let spec = pairing_spec();
        let subject = Subject::new("account", "a-1");
        let md = serde_json::Value::Object(Default::default());
        let mut steps = materialize_steps(&spec, &subject, JobId::new(), &md, StepId::new);
        for s in steps.iter_mut() {
            s.spec_slug = None;
        }
        assert_eq!(pair_steps(&spec, &steps), vec![Some(0), Some(1)]);

        steps[0].status = StepStatus::Completed;
        reevaluate(&spec, &mut steps, &subject, &md);
        assert_eq!(steps[1].status, StepStatus::Ready);
    }

    #[test]
    fn reevaluate_never_skips_a_metadata_gated_outcome() {
        // aa9980c8: four ship-a-change Jobs closed with their Merged
        // outcome SKIPPED 134ms after boarding. The outcome's gate —
        // `steps.review.done AND job.metadata.merged = "true"` — was
        // false the instant review completed, and with `review`
        // terminal the re-evaluator inferred "provably unsatisfiable".
        // A predicate referencing job.metadata is NOT provable from
        // step terminality: the marker arrives later (the conductor
        // writes it at actual merge time). The step must stay Pending
        // — awaiting data — and promote once the marker lands.
        let spec = WorkflowSpec::platform_seed(
            "mini-ship",
            "Mini ship",
            "test",
            vec!["account".into()],
            vec![
                StepSpec {
                    title: "review".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "merged".into(),
                    kind: "outcome".into(),
                    ready_when: "steps.review.done AND job.metadata.merged = \"true\"".into(),
                    terminal: Some(Terminal {
                        outcome: "merged".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let subject = Subject::new("account", "a-1");
        let no_marker = serde_json::Value::Object(Default::default());
        let mut steps = materialize_steps(&spec, &subject, JobId::new(), &no_marker, StepId::new);

        steps[0].status = StepStatus::Completed;
        let changed = reevaluate(&spec, &mut steps, &subject, &no_marker);
        assert!(
            changed.is_empty(),
            "no promotion and no skip before the marker: {changed:?}"
        );
        assert_eq!(
            steps[1].status,
            StepStatus::Pending,
            "metadata-gated outcome awaits its marker"
        );

        // The marker lands (the metadata write at actual merge time).
        let marker = serde_json::json!({ "merged": "true" });
        let changed = reevaluate(&spec, &mut steps, &subject, &marker);
        assert_eq!(changed, vec![1], "marker write promotes the outcome");
        assert_eq!(steps[1].status, StepStatus::Ready);
    }

    // -----------------------------------------------------------
    // Trigger provenance — complete the firing trigger, skip the rest
    // -----------------------------------------------------------

    /// Mirrors `ingredient-restock`: two `auto-on-materialize` triggers
    /// (threshold + forecast) fanning into one downstream task gated on
    /// either via `steps.a.done OR steps.b.done`.
    fn two_trigger_spec() -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            "restock-2trig",
            "Restock (two triggers)",
            "test",
            vec!["vendor".into()],
            vec![
                StepSpec {
                    title: "trigger".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    metadata_defaults: serde_json::json!({
                        "trigger_name": "inventory-reorder-threshold"
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "reorder-check".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    metadata_defaults: serde_json::json!({
                        "trigger_name": "demand-forecast-reorder"
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "audit-stock".into(),
                    kind: "task".into(),
                    ready_when: "steps.trigger.done OR steps.reorder-check.done".into(),
                    ..Default::default()
                },
            ],
        )
    }

    #[test]
    fn materialize_preserves_the_spec_slug_beside_the_rendered_title() {
        // Machine callers (boss-step.sh, the deploy scripts) address a
        // step by its spec slug — "trigger", "audit-stock" — while
        // `title_template` renders display text. Materialisation used
        // to discard the slug, so every self-closing step call no-op'd
        // silently behind `|| true` (backlog 6c6b9e06). The slug and
        // the title are two facts; a step must carry both.
        let spec = two_trigger_spec();
        let subject = Subject::new("vendor", "v-1");
        let steps = materialize_steps_at(
            &spec,
            &subject,
            JobId::new(),
            &serde_json::Value::Object(Default::default()),
            StepId::new,
            None,
            Some(&StepRegistry::v1()),
        );
        for (spec_step, step) in spec.steps.iter().zip(&steps) {
            assert_eq!(
                step.spec_slug.as_deref(),
                Some(spec_step.title.as_str()),
                "the materialised step keeps the slug its spec declared"
            );
        }
    }

    #[test]
    fn materialize_completes_matching_trigger_skips_alternative() {
        // Provenance says the threshold trigger fired: it is born
        // Completed, the forecast alternative born Skipped, and the
        // downstream task promotes to Ready off the fired trigger — in
        // one materialization pass.
        let spec = two_trigger_spec();
        let subject = Subject::new("vendor", "v-1");
        let reg = StepRegistry::v1();
        let job_metadata = serde_json::json!({ "trigger_name": "inventory-reorder-threshold" });
        let steps = materialize_steps_at(
            &spec,
            &subject,
            JobId::new(),
            &job_metadata,
            StepId::new,
            None,
            Some(&reg),
        );
        assert_eq!(
            steps[0].status,
            StepStatus::Completed,
            "the fired (threshold) trigger is Completed"
        );
        assert_eq!(
            steps[1].status,
            StepStatus::Skipped,
            "the alternative (forecast) trigger is the branch not taken"
        );
        assert_eq!(
            steps[2].status,
            StepStatus::Ready,
            "downstream task is Ready off the fired trigger's .done"
        );
    }

    #[test]
    fn materialize_readies_an_absent_optional_flag_gate() {
        // 7b756357: a `NOT job.metadata.x` gate over an ABSENT flag
        // READIES (optional-flag-defaults-off), where the eval error
        // used to leave it pending forever and dead-letter its marker. A
        // `job.metadata.go = "true"` gate stays PENDING until go is set —
        // the job-metadata guard keeps it waiting rather than skipping.
        let spec = WorkflowSpec::platform_seed(
            "absent-gate",
            "Absent gate",
            "test",
            vec!["custom".into()],
            vec![
                StepSpec {
                    title: "opened".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    title_template: "Opened".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "act".into(),
                    kind: "task".into(),
                    ready_when: "NOT job.metadata.blocked".into(),
                    title_template: "Act".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "gated".into(),
                    kind: "task".into(),
                    ready_when: "job.metadata.go = \"true\"".into(),
                    title_template: "Gated".into(),
                    ..Default::default()
                },
            ],
        );
        let subject = Subject::new("custom", "c-1");
        let reg = StepRegistry::v1();
        let steps = materialize_steps_at(
            &spec,
            &subject,
            JobId::new(),
            &serde_json::Value::Object(Default::default()),
            StepId::new,
            None,
            Some(&reg),
        );
        assert_eq!(
            steps[0].status,
            StepStatus::Completed,
            "the lone trigger is born completed"
        );
        assert_eq!(
            steps[1].status,
            StepStatus::Ready,
            "NOT job.metadata.blocked over an absent flag readies (was pending-forever before 7b756357)"
        );
        assert_eq!(
            steps[2].status,
            StepStatus::Pending,
            "job.metadata.go = \"true\" over an absent flag stays pending, not skipped"
        );
    }

    #[test]
    fn materialize_with_no_provenance_fires_first_trigger() {
        // An operator-opened Job carries no `trigger_name`: the first
        // trigger fires (compat), the rest Skip — never all of them.
        let spec = two_trigger_spec();
        let subject = Subject::new("vendor", "v-1");
        let reg = StepRegistry::v1();
        let job_metadata = serde_json::Value::Object(Default::default());
        let steps = materialize_steps_at(
            &spec,
            &subject,
            JobId::new(),
            &job_metadata,
            StepId::new,
            None,
            Some(&reg),
        );
        assert_eq!(
            steps[0].status,
            StepStatus::Completed,
            "first trigger fires"
        );
        assert_eq!(steps[1].status, StepStatus::Skipped, "the rest Skip");
        assert_eq!(steps[2].status, StepStatus::Ready);
    }

    #[test]
    fn materialize_single_trigger_is_born_completed() {
        // The common case: one trigger → it is born Completed at open,
        // never transient Ready work.
        let spec = WorkflowSpec::platform_seed(
            "single-trig",
            "Single trigger",
            "test",
            vec!["account".into()],
            vec![
                StepSpec {
                    title: "trigger".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "work".into(),
                    kind: "task".into(),
                    ready_when: "steps.trigger.done".into(),
                    ..Default::default()
                },
            ],
        );
        let subject = Subject::new("account", "a-1");
        let reg = StepRegistry::v1();
        let job_metadata = serde_json::Value::Object(Default::default());
        let steps = materialize_steps_at(
            &spec,
            &subject,
            JobId::new(),
            &job_metadata,
            StepId::new,
            None,
            Some(&reg),
        );
        assert_eq!(
            steps[0].status,
            StepStatus::Completed,
            "sole trigger is born Completed"
        );
        assert_eq!(
            steps[1].status,
            StepStatus::Ready,
            "downstream promotes off it"
        );
    }

    #[test]
    fn materialize_without_registry_leaves_triggers_ready() {
        // The pure helper path (no registry) keeps the historical shape:
        // triggers materialize Ready. The live/sim create path always
        // passes the registry; this guards the no-arg `materialize_steps`
        // callers (tests, the dead trait default) from a behavior change.
        let spec = two_trigger_spec();
        let subject = Subject::new("vendor", "v-1");
        let job_metadata = serde_json::json!({ "trigger_name": "inventory-reorder-threshold" });
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);
        assert_eq!(steps[0].status, StepStatus::Ready);
        assert_eq!(steps[1].status, StepStatus::Ready);
    }

    // -----------------------------------------------------------
    // bootstrap_reconcile — InMemoryWorkflows
    // -----------------------------------------------------------

    /// Viable by construction — reconcile activates rows, so it runs
    /// the same gate publish does.
    fn reconcile_spec(kind: &str, label: &str) -> WorkflowSpec {
        let mut spec = seed_spec(kind);
        spec.label = label.to_string();
        spec.category = "platform".into();
        spec.subject_kinds = vec!["account".into()];
        spec
    }

    #[tokio::test]
    async fn bootstrap_reconcile_inserts_missing_kinds() {
        let registry = InMemoryWorkflows::new();
        let defaults = vec![reconcile_spec("workflow-design", "Design a Workflow")];
        let stats = registry
            .bootstrap_reconcile(&defaults, &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(stats.inserted, 1);
        assert_eq!(stats.republished, 0);
        assert_eq!(stats.preserved, 0);
        assert_eq!(stats.unchanged, 0);
        let live = registry.get_active("workflow-design").await.unwrap();
        assert_eq!(live.label, "Design a Workflow");
        assert_eq!(live.version, 1);
        assert_eq!(live.status, WorkflowStatus::Active);
    }

    #[tokio::test]
    async fn bootstrap_reconcile_republishes_drift_as_a_new_version() {
        let registry = InMemoryWorkflows::new();
        // Seed a stale bootstrap row.
        let stale = reconcile_spec("workflow-design", "Old Label");
        registry
            .bootstrap_reconcile(&[stale], &test_actor(), Utc::now())
            .await
            .expect("seed bootstrap");
        // Defaults now carry the corrected label.
        let updated = reconcile_spec("workflow-design", "Design a Workflow");
        let stats = registry
            .bootstrap_reconcile(&[updated], &test_actor(), Utc::now())
            .await
            .expect("republish");
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.republished, 1);
        assert_eq!(stats.preserved, 0);
        assert_eq!(stats.unchanged, 0);

        let live = registry.get_active("workflow-design").await.unwrap();
        assert_eq!(live.label, "Design a Workflow", "drift should self-heal");
        // A changed body is a NEW version, not a rewrite of the old
        // one. This used to assert the opposite, and that is precisely
        // what defeated the version pin: Jobs opened under v1 kept
        // pointing at v1 while v1's body changed underneath them.
        assert_eq!(live.version, 2, "a changed body publishes a version");

        // The old version must still be readable, or a Job pinned to
        // it has nothing to resolve.
        let pinned = registry
            .get_version("workflow-design", 1)
            .await
            .expect("v1 still resolvable");
        assert_eq!(pinned.label, "Old Label", "v1 keeps the body it had");
        assert_eq!(pinned.status, WorkflowStatus::Retired);
    }

    /// Reconcile runs on EVERY boot. If an unchanged default still
    /// published, a restart loop would mint versions forever.
    #[tokio::test]
    async fn bootstrap_reconcile_republishes_only_on_a_real_change() {
        let registry = InMemoryWorkflows::new();
        let spec = reconcile_spec("workflow-design", "Design a Workflow");
        registry
            .bootstrap_reconcile(std::slice::from_ref(&spec), &test_actor(), Utc::now())
            .await
            .expect("seed");
        for _ in 0..3 {
            let stats = registry
                .bootstrap_reconcile(std::slice::from_ref(&spec), &test_actor(), Utc::now())
                .await
                .expect("reboot");
            assert_eq!(stats.republished, 0);
            assert_eq!(stats.unchanged, 1);
        }
        assert_eq!(
            registry
                .get_active("workflow-design")
                .await
                .unwrap()
                .version,
            1,
            "three boots with no change must not mint versions"
        );
    }

    #[tokio::test]
    async fn bootstrap_reconcile_preserves_operator_edits() {
        let registry = InMemoryWorkflows::new();
        // Operator-owned row: inserted via `seed`, NOT via reconcile,
        // so it lands without bootstrap ownership tracking.
        let mut operator_spec = reconcile_spec("workflow-design", "Operator Label");
        operator_spec.version = 1;
        operator_spec.status = WorkflowStatus::Active;
        registry.seed(operator_spec).expect("seed operator row");

        let updated = reconcile_spec("workflow-design", "Default Label");
        let stats = registry
            .bootstrap_reconcile(&[updated], &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.republished, 0);
        assert_eq!(stats.preserved, 1);
        assert_eq!(stats.unchanged, 0);
        let live = registry.get_active("workflow-design").await.unwrap();
        assert_eq!(
            live.label, "Operator Label",
            "operator edits must survive reconcile"
        );
    }

    #[tokio::test]
    async fn bootstrap_reconcile_no_op_when_already_matching() {
        let registry = InMemoryWorkflows::new();
        let spec = reconcile_spec("workflow-design", "Design a Workflow");
        registry
            .bootstrap_reconcile(std::slice::from_ref(&spec), &test_actor(), Utc::now())
            .await
            .expect("seed");
        let stats = registry
            .bootstrap_reconcile(&[spec], &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.republished, 0);
        assert_eq!(stats.preserved, 0);
        assert_eq!(stats.unchanged, 1);
    }

    // -----------------------------------------------------------
    // publish_authored — InMemoryWorkflows
    // -----------------------------------------------------------

    #[tokio::test]
    async fn publish_authored_creates_v1_when_no_prior_rows() {
        let registry = InMemoryWorkflows::new();
        let spec = reconcile_spec("morning-brew", "Morning Brew");
        let job_id = JobId::new();

        let published = registry
            .publish_authored(spec, job_id, &test_actor(), Utc::now())
            .await
            .expect("publish");

        assert_eq!(published.kind, "morning-brew");
        assert_eq!(published.version, 1);
        assert_eq!(published.status, WorkflowStatus::Active);
        assert_eq!(
            published.authoring_job_id.expect("authoring set"),
            *job_id.inner().as_uuid(),
            "authoring_job_id must round-trip through publish"
        );

        let live = registry.get_active("morning-brew").await.unwrap();
        assert_eq!(live.version, 1);
    }

    #[tokio::test]
    async fn publish_authored_supersedes_prior_active_row() {
        let registry = InMemoryWorkflows::new();
        let job_a = JobId::new();
        let job_b = JobId::new();

        registry
            .publish_authored(
                reconcile_spec("morning-brew", "v1 label"),
                job_a,
                &test_actor(),
                Utc::now(),
            )
            .await
            .expect("first publish");
        let updated = registry
            .publish_authored(
                reconcile_spec("morning-brew", "v2 label"),
                job_b,
                &test_actor(),
                Utc::now(),
            )
            .await
            .expect("supersede");

        assert_eq!(updated.version, 2, "supersede must bump version");
        assert_eq!(updated.label, "v2 label");

        let active = registry.get_active("morning-brew").await.unwrap();
        assert_eq!(active.version, 2);
        assert_eq!(active.label, "v2 label");

        let v1 = registry.get_version("morning-brew", 1).await.unwrap();
        assert_eq!(v1.status, WorkflowStatus::Retired);
    }

    // -----------------------------------------------------------
    // platform_workflows — the code-resident Workflows reconciled into
    // the live registry on every boss-jobs-api start.
    // -----------------------------------------------------------

    #[test]
    fn pr_train_steps_close_on_evidence_not_on_readiness() {
        // pr-train moved to the bundle (e332a320), so read it from
        // where it now lives — which is also what a deployment seeds.
        let kinds = seedable_platform_workflows();
        let train = kinds
            .iter()
            .find(|k| k.kind == "pr-train")
            .expect("pr-train present in the platform bundle");

        // Every evidence step — one that carries required fields for
        // the conductor to fill — is authority-gated: an ungated ready
        // step gets role-matched and completed by the simulated
        // workforce, and a train whose steps the sim closes records
        // fiction. Property-based, not kind-named (ADR-0021): "has
        // evidence fields, is not a terminal" IS the conductor-closed
        // set, whatever kinds those steps declare.
        let evidence_steps: Vec<_> = train
            .steps
            .iter()
            .filter(|s| s.terminal.is_none() && !s.fields.is_empty())
            .collect();
        assert!(
            evidence_steps.len() >= 6,
            "collect/assemble/pr/ci/merged/deployed all carry evidence fields"
        );
        for s in &evidence_steps {
            assert_eq!(
                s.authority_role.as_deref(),
                Some("platform-admin"),
                "evidence step `{}` must be closed by the conductor or a person, \
                 never the sim workforce",
                s.title
            );
        }

        // `merged` closes on evidence, not readiness: it must NOT be a
        // terminal — the dispatcher's complete-on-ready rule fires
        // ready terminals, which is exactly how ship-a-change Jobs
        // came to read "merged" while their PR was still open — and it
        // must demand the observed merge commit.
        let merged = train
            .steps
            .iter()
            .find(|s| s.title == "merged")
            .expect("merged step present");
        assert!(
            merged.terminal.is_none(),
            "a terminal `merged` would be auto-completed the moment it became ready"
        );
        assert!(
            merged
                .fields
                .iter()
                .any(|f| f.name == "merge_ref" && f.required),
            "the merge commit is the evidence; without it the step is a claim"
        );

        // The empty-window bail-out needs the conductor's explicit
        // marker — collect.done alone would cancel every train the
        // moment it collected.
        let cancelled = train
            .steps
            .iter()
            .find(|s| s.title == "cancelled")
            .expect("cancelled outcome present");
        assert!(
            cancelled.ready_when.contains("job.metadata.empty"),
            "cancelled must be marker-gated; ready_when = {:?}",
            cancelled.ready_when
        );
    }

    #[test]
    fn ship_a_change_merged_waits_for_the_actual_merge() {
        // The dispatcher completes any ready terminal, so an outcome
        // gated on `steps.review.done` alone fires when review
        // completes — Job 606b40fb read "merged" while its PR was
        // still open. The merge-evidence gate now lives on `proven`
        // (the terminal fires on the PROOF, David 2026-08-19:
        // "merged" must mean visible in prod, not landed on main),
        // so this guard follows it: the chain terminal ← proven ←
        // merge marker must be unbroken, or the dispatcher closes
        // Jobs on review completion again.
        // READ FROM THE BUNDLE, because that is where the protocol
        // lives now. Repointed rather than deleted when ship-a-change
        // converted to data: the guarantee is about the PROTOCOL, not
        // about which file holds it, and a conversion that quietly
        // dropped this assertion would have removed the guard while
        // the flow it guards kept running.
        let kinds = crate::seed_loader::load_workflows(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/platform/workflows.toml"
        ))
        .expect("the platform bundle parses");
        let ship = kinds
            .iter()
            .find(|k| k.kind == "ship-a-change")
            .expect("ship-a-change present in the bundle");
        let proven = ship
            .steps
            .iter()
            .find(|s| s.title == "proven")
            .expect("proven step present");
        assert!(
            proven.ready_when.contains("job.metadata.merged"),
            "proven must wait for merge evidence; ready_when = {:?}",
            proven.ready_when
        );
        let merged = ship
            .steps
            .iter()
            .find(|s| s.title == "merged")
            .expect("merged outcome present");
        assert!(
            merged.ready_when.contains("steps.proven.done"),
            "the terminal must wait for the prod proof; ready_when = {:?}",
            merged.ready_when
        );
        // The proof itself cannot evaporate into a completed step.
        assert!(
            proven
                .fields
                .iter()
                .any(|f| f.name == "verified" && f.required),
            "proven must require the `verified` evidence at done"
        );
    }

    #[test]
    fn platform_workflows_carries_the_shipped_meta_kinds() {
        let kinds = platform_workflows();
        // The roster and the seed change together — that is
        // `a-registry-seed-and-its-roster-test-change-together` in the
        // register, and this assertion is the half that enforces it.
        // The count only ever goes DOWN now: protocols-as-data.md's
        // direction of travel is that a kind leaves this roster for
        // infra/platform/workflows.toml and never comes back, and a
        // new protocol never touches Rust at all.
        assert_eq!(
            kinds.len(),
            4,
            "ships design-doc-review + the three maintenance kinds (backup / \
             audit-integrity / ledger-replay — internal-forge Q6). \
             workflow-design, regenerate-deployment, backlog-item, \
             ship-a-change, and now user-feedback and pr-train (e332a320) \
             are in the bundle, not here."
        );
        // NO TENANT NOUNS IN CORE. David, 2026-08-16: "We don't want
        // brewery nouns in core no matter what. But most nouns should
        // be data anyway, so that was its own class of problem."
        //
        // Two problems were tangled in protocols-as-data Q5 and this
        // asserts the one that survives. The FORM problem — protocols
        // as Rust literals — is what the bundle fixes. The LAYER
        // problem — a tenant's vocabulary living in Tier 1 — is never
        // acceptable and was held only by vigilance: the roster count
        // above says "seven", not "seven PLATFORM kinds", so swapping
        // one for `morning-brew` would keep it green.
        //
        // `tier-import-audit.sh` cannot see this. It checks crate
        // DEPENDENCIES, and a brewery noun hardcoded in a core crate
        // adds no dependency at all — which is exactly how this class
        // hides.
        //
        // The list is the brewery and used-device-shop vocabularies
        // that have historically appeared here. Deliberately a
        // denylist rather than a shape rule: "is this noun
        // tenant-specific" is a judgement, and encoding a bad guess as
        // a lint would block legitimate platform kinds. A tenant kind
        // that is not on this list still gets caught by the count
        // assertion above, which cannot be satisfied without a
        // deliberate edit.
        for tenant_noun in [
            "sale",
            "morning-brew",
            "morning-brew-ipa",
            "morning-brew-stout",
            "wholesale-keg-order",
            "direct-shop-order",
            "ingredient-restock",
            "refurb-used",
            "refurb-device",
        ] {
            assert!(
                !kinds.iter().any(|k| k.kind == tenant_noun),
                "`{tenant_noun}` is a TENANT noun and must not ship in \
                 platform_workflows() — tenant kinds seed from examples/<tenant>/, \
                 and a brewery vocabulary in Tier 1 is the leak CLAUDE.md §10 names. \
                 tier-import-audit.sh cannot catch this: a hardcoded noun adds no \
                 crate dependency."
            );
        }

        for bundled_kind in ["workflow-design", "regenerate-deployment", "backlog-item"] {
            assert!(
                !kinds.iter().any(|k| k.kind == bundled_kind),
                "`{bundled_kind}` is supplied by the bundle; a kind in BOTH places is \
                 worse than a kind in neither — bootstrap_reconcile republishes the \
                 code version over the bundle's on every boot"
            );
        }

        // The boundary declaration is the whole point of the kind, so
        // it gets pinned rather than left to the generic viability
        // lint. `excludes` optional would make the step completable
        // without saying what the change leaves out, which is the one
        // sentence that keeps a change small.
        // ship-a-change moved to the bundle, so the assertions follow
        // it there rather than lapsing — the same treatment
        // regenerate-deployment got below. A guarantee that stops being
        // checked because its subject changed file is a guarantee that
        // was never really held.
        let bundled_kinds = crate::seed_loader::load_workflows(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/platform/workflows.toml"
        ))
        .expect("the platform bundle parses");
        let ship = bundled_kinds
            .iter()
            .find(|k| k.kind == "ship-a-change")
            .expect("ship-a-change present in the bundle");
        let scope = ship
            .steps
            .iter()
            .find(|s| s.title == "scope")
            .expect("scope step present");
        assert_eq!(
            scope.authority_role.as_deref(),
            Some("platform-admin"),
            "an ungated scope step gets role-matched and completed by the simulated \
             workforce, and the audit trail then says a person chose the boundary"
        );
        for field in ["summary", "excludes"] {
            let f = scope
                .fields
                .iter()
                .find(|f| f.name == field)
                .unwrap_or_else(|| panic!("scope declares `{field}`"));
            assert!(f.required, "`{field}` is required at done");
        }

        // The step this Workflow exists for. Five stale-binary
        // incidents in one day is what "verify the artifacts" is
        // guarding, and an optional field would be skipped under
        // exactly the conditions that produce them.
        //
        // `regenerate-deployment` moved to the bundle, so the
        // assertions follow it there rather than lapsing. A guarantee
        // that stops being checked because its subject moved house is
        // a guarantee that was deleted quietly.
        let bundled = crate::seed_loader::load_workflows(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/platform/workflows.toml"
        ))
        .expect("the platform bundle parses");
        let regen = bundled
            .iter()
            .find(|k| k.kind == "regenerate-deployment")
            .expect("regenerate-deployment present in the bundle");
        let artifacts = regen
            .steps
            .iter()
            .find(|s| s.title == "artifacts")
            .expect("artifacts step present");
        assert!(
            artifacts
                .fields
                .iter()
                .any(|f| f.name == "verified" && f.required),
            "`verified` must be required at done"
        );
        // The two moments that must not happen without a person: the
        // decision to regenerate, and the destruction itself.
        for gated in ["scope", "reset"] {
            let step = regen
                .steps
                .iter()
                .find(|s| s.title == gated)
                .unwrap_or_else(|| panic!("`{gated}` step present"));
            assert_eq!(
                step.authority_role.as_deref(),
                Some("platform-admin"),
                "`{gated}` must wait for a person — an ungated step gets role-matched \
                 and completed by the simulated workforce"
            );
        }

        // No terminal may be ready on nothing but a prior step.
        //
        // The viability lint proves a terminal is REACHABLE; it cannot
        // say whether reaching it is deliberate. `ship-a-change` had
        // an `abandoned` step gated only on `steps.scope.done`, which
        // passed the lint and then closed the first Job ever filed
        // against the Workflow: an ungated terminal that is ready is
        // one the dispatcher completes, so `complete-marker-on-step-
        // ready` fired the instant scope finished, skipped
        // build/gate/review, and shut the Job seconds after it opened.
        //
        // The rule: an escape-hatch terminal needs a condition a
        // PERSON supplies. A disposition on a fork step counts; so
        // does a job-metadata marker. Bare step-completion does not.
        for wf in &kinds {
            for step in wf.steps.iter().filter(|s| s.terminal.is_some()) {
                let rw = &step.ready_when;
                // Terminals that conclude real work are fine gated on
                // the steps that did it. This targets the ones whose
                // whole purpose is to bail out.
                if !matches!(
                    step.title.as_str(),
                    "abandoned" | "declined" | "duplicate" | "stale"
                ) {
                    continue;
                }
                assert!(
                    rw.contains("metadata"),
                    "{}/{}: an escape-hatch terminal ready on step state alone gets \
                     auto-completed the moment it becomes ready — it needs a marker a \
                     person sets. ready_when = {rw:?}",
                    wf.kind,
                    step.title
                );
            }
        }

        // The two routes that justify this being its own Workflow
        // rather than user-feedback wearing a different label. Both
        // came out of triaging TODO.md: one item was dead because the
        // world moved (`stale`), another had a stale rationale but a
        // live defect (`verify`). Drop either and a backlog rots into
        // fiction with nowhere to record why.
        // Follows the kind into the bundle, like the regenerate-deployment
        // assertions above. A guarantee that lapses because its subject
        // moved house is a guarantee that was deleted quietly.
        let backlog = bundled
            .iter()
            .find(|k| k.kind == "backlog-item")
            .expect("backlog-item present in the bundle");
        let triage = backlog
            .steps
            .iter()
            .find(|s| s.title == "triage")
            .expect("triage step present");
        let disposition = triage
            .fields
            .iter()
            .find(|f| f.name == "disposition")
            .expect("disposition field");
        for route in ["stale", "verify"] {
            assert!(
                disposition.field_type.split('|').any(|v| v == route),
                "`{route}` must be a disposition — it is why this is not user-feedback"
            );
        }
        // Evidence is what separates triage from filing: every route
        // here is a claim about code that may have moved since the
        // item was written.
        assert!(
            triage
                .fields
                .iter()
                .any(|f| f.name == "evidence" && f.required),
            "`evidence` must be required at done"
        );

        let design = bundled
            .iter()
            .find(|k| k.kind == "workflow-design")
            .expect("workflow-design present in the bundle");
        assert_eq!(design.version, 1);
        assert_eq!(design.status, WorkflowStatus::Active);
        assert_eq!(design.subject_kinds, vec!["custom".to_string()]);
        assert_eq!(design.owning_team, "platform");

        let review = kinds
            .iter()
            .find(|k| k.kind == "design-doc-review")
            .expect("design-doc-review present");
        assert_eq!(review.version, 1);
        assert_eq!(review.status, WorkflowStatus::Active);

        // Feedback is a Job like any other work: its Subject is the
        // surface it is about, which is what makes "what have people
        // said about this page" answerable from Subject history.
        // From the bundle now (e332a320) — the roster no longer carries
        // it, and the bundle is what a deployment seeds.
        let seeded = seedable_platform_workflows();
        let feedback = seeded
            .iter()
            .find(|k| k.kind == "user-feedback")
            .expect("user-feedback present in the platform bundle");
        assert_eq!(feedback.version, 1);
        assert_eq!(feedback.status, WorkflowStatus::Active);
        assert_eq!(feedback.subject_kinds, vec!["custom".to_string()]);
        // submitted -> triage -> one branch per disposition -> a
        // terminal. Asserting the fork rather than a step count: what
        // matters is that triage HAS somewhere to route to, since a
        // triage step with a single successor is a checkbox, which is
        // what this spec shipped as first.
        let triage = feedback
            .steps
            .iter()
            .find(|s| s.title == "triage")
            .expect("triage step present");
        let disposition = triage
            .fields
            .iter()
            .find(|f| f.name == "disposition")
            .expect("triage declares a disposition field");
        assert!(
            disposition.required,
            "triage must not be completable without choosing a route"
        );
        let values: Vec<&str> = disposition.field_type.split('|').collect();
        assert!(
            values.len() >= 2,
            "a fork needs at least two dispositions, got {values:?}"
        );
        // Every declared disposition has a step gated on it. The
        // viability lint proves this too; asserting it here names the
        // offending value instead of failing a whole-registry check.
        for v in &values {
            let needle = format!("disposition = \"{v}\"");
            assert!(
                feedback
                    .steps
                    .iter()
                    .any(|s| s.ready_when.contains(&needle)),
                "disposition `{v}` has no successor — a Job routed there would strand"
            );
        }
        assert!(
            feedback.steps.iter().any(|s| s.terminal.is_some()),
            "must have a terminal step or feedback Jobs never close"
        );
    }

    #[test]
    fn design_doc_review_materializes_doc_path_from_the_subject() {
        // The review-design step's doc_path must be stamped AT
        // materialization, atomically, from the Job's subject (the doc
        // path IS the subject id). The pre-fix seed defaulted it to ""
        // and left a second SPA PUT to fill it in — which lost a
        // read-overlay-write race against dispatcher assignment and
        // the sim workforce's completion, and terminal-metadata
        // immutability then sealed the empty value forever
        // ("Failed to load doc: step.metadata.doc_path is empty",
        // 2026-07-14).
        let kinds = platform_workflows();
        let review = kinds
            .iter()
            .find(|k| k.kind == "design-doc-review")
            .expect("design-doc-review present");

        let subject = Subject::new("custom", "docs/design/transactional-audit-log.md");
        let job_id = JobId::new();
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut counter = 0u32;
        let steps = materialize_steps(review, &subject, job_id, &job_metadata, || {
            counter += 1;
            StepId::from_uuid(Uuid::from_u128(counter as u128))
        });

        // Spec order: open → review → reviewed (positional, matching
        // the sibling platform-kind step tests).
        let review_step = &steps[1];
        assert_eq!(review_step.kind, "review-design");
        assert_eq!(
            review_step
                .metadata
                .get("doc_path")
                .and_then(|v| v.as_str()),
            Some("docs/design/transactional-audit-log.md"),
            "doc_path must be the subject id at birth, not a later fill-in: {:?}",
            review_step.metadata
        );
    }

    #[test]
    fn platform_workflows_steps_run_design_to_publish_in_four_steps() {
        // `workflow-design` is bundle-supplied now, so the lifecycle is
        // asserted where the lifecycle lives. The alternative — dropping
        // the test with the kind — is how a four-step contract quietly
        // becomes whatever the TOML happens to say.
        let kinds = crate::seed_loader::load_workflows(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/platform/workflows.toml"
        ))
        .expect("the platform bundle parses");
        let design = kinds
            .iter()
            .find(|k| k.kind == "workflow-design")
            .expect("workflow-design present in the bundle");
        // v2: flat steps, DAG implicit in ready_when. The lifecycle is
        // author → validate → approve(sign-off) → publish(terminal).
        let steps = &design.steps;
        assert_eq!(steps.len(), 4, "design lifecycle has 4 steps");
        assert_eq!(steps[0].kind, "task"); // author
        assert_eq!(steps[1].kind, "task"); // validate
        assert_eq!(steps[2].kind, "sign-off"); // approve
        assert_eq!(
            steps[2].sign_offs_required,
            vec!["workflow-approver".to_string()]
        );
        assert_eq!(
            steps[2].authority_role.as_deref(),
            Some("workflow-approver"),
            "approval is the operational-leader `workflow-approver` authority, \
             not platform-admin alone"
        );
        assert_eq!(steps[3].kind, "workflow-publish"); // publish
        assert!(steps[3].terminal.is_some(), "publish is the terminal step");

        // design-doc-review is still code-supplied, so it is asserted
        // against the code — this test now reads both registries, which
        // is what the split actually looks like.
        let review = platform_workflows()
            .into_iter()
            .find(|k| k.kind == "design-doc-review")
            .expect("design-doc-review present in platform_workflows");
        assert_eq!(review.steps.len(), 3, "review lifecycle has 3 steps");
    }

    #[test]
    fn platform_workflows_passes_validate_all() {
        use crate::step_registry::StepRegistry;
        use crate::workflow_lint::validate_all;

        let kinds = platform_workflows();
        let registry = StepRegistry::v1();
        let errs = validate_all(&kinds, &registry);
        assert!(
            errs.is_empty(),
            "platform_workflows() must pass the same lint that gates every other kind: {errs:?}"
        );
    }

    #[test]
    fn platform_workflows_round_trip_through_json() {
        // The wire shape used by HTTP handlers + the audit_log
        // payload must round-trip cleanly. If anything in the
        // platform spec drifts from the deserializer's expectations
        // (a missing #[serde] attribute on a new WorkflowSpec
        // field, say), this test is the early-warning line.
        let kinds = platform_workflows();
        for original in kinds {
            let wire = serde_json::to_value(&original).expect("serialize");
            let decoded: WorkflowSpec = serde_json::from_value(wire).expect("deserialize");
            assert_eq!(decoded.kind, original.kind);
            assert_eq!(decoded.steps.len(), original.steps.len());
        }
    }

    #[tokio::test]
    async fn publish_authored_flips_row_out_of_bootstrap_ownership() {
        // Sequence: bootstrap reconcile → operator publish via Job →
        // next reconcile must preserve the operator-published row.
        let registry = InMemoryWorkflows::new();
        registry
            .bootstrap_reconcile(
                &[reconcile_spec("morning-brew", "Bootstrap Label")],
                &test_actor(),
                Utc::now(),
            )
            .await
            .expect("seed bootstrap");

        registry
            .publish_authored(
                reconcile_spec("morning-brew", "Job-Authored Label"),
                JobId::new(),
                &test_actor(),
                Utc::now(),
            )
            .await
            .expect("publish via Job");

        let stats = registry
            .bootstrap_reconcile(
                &[reconcile_spec("morning-brew", "Default Label")],
                &test_actor(),
                Utc::now(),
            )
            .await
            .expect("reconcile after publish");
        assert_eq!(
            stats.preserved, 1,
            "publish_authored must flip the row out of bootstrap ownership"
        );
        let live = registry.get_active("morning-brew").await.unwrap();
        assert_eq!(live.label, "Job-Authored Label");
    }

    // -----------------------------------------------------------
    // Registry events — every write records an outbox event with
    // the row (protocol-policy-publish.md, Constraints). The
    // InMemory adapter collects what the Pg adapter records inside
    // the row transaction; `recorded_events()` is the test window.
    // -----------------------------------------------------------

    #[tokio::test]
    async fn publish_records_exactly_one_published_event() {
        let reg = InMemoryWorkflows::new();
        let actor = test_actor();
        reg.create_draft(seed_spec("repair"), &actor, Utc::now())
            .await
            .unwrap();
        let published = reg.publish("repair", &actor, Utc::now()).await.unwrap();

        let events: Vec<_> = reg
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::WORKFLOW_PUBLISHED)
            .collect();
        assert_eq!(
            events.len(),
            1,
            "publish must record exactly one jobs.kind.published"
        );
        let payload = &events[0].payload;
        // The actor rides as `_actor` in EventStamp's exact shape —
        // a Human serializes as the bare employee id.
        assert_eq!(payload["_actor"], "emp-test");
        // The payload IS the promoted spec.
        assert_eq!(payload["kind"], "repair");
        assert_eq!(payload["version"], published.version);
        assert_eq!(payload["status"], "active");
    }

    #[tokio::test]
    async fn create_draft_records_draft_saved() {
        let reg = InMemoryWorkflows::new();
        let draft = reg
            .create_draft(seed_spec("repair"), &test_actor(), Utc::now())
            .await
            .unwrap();

        let events = reg.recorded_events();
        assert_eq!(events.len(), 1, "one write, one event");
        assert_eq!(events[0].kind, crate::events::WORKFLOW_DRAFT_SAVED);
        assert_eq!(events[0].payload["kind"], "repair");
        assert_eq!(events[0].payload["version"], draft.version);
        assert_eq!(events[0].payload["status"], "draft");
    }

    #[tokio::test]
    async fn retire_records_once_and_stays_silent_when_already_retired() {
        let reg = InMemoryWorkflows::new();
        let actor = test_actor();
        reg.create_draft(seed_spec("repair"), &actor, Utc::now())
            .await
            .unwrap();
        reg.publish("repair", &actor, Utc::now()).await.unwrap();

        reg.retire("repair", &actor, Utc::now()).await.unwrap();
        // Second retire is the idempotent no-op path: rows_affected
        // is 0, so no event — the log records what happened, and
        // nothing happened (transactional-audit-log.md discipline).
        reg.retire("repair", &actor, Utc::now()).await.unwrap();

        let retired: Vec<_> = reg
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::WORKFLOW_RETIRED)
            .collect();
        assert_eq!(
            retired.len(),
            1,
            "idempotent retire must not record a second event"
        );
        assert_eq!(retired[0].payload["kind"], "repair");
        assert_eq!(retired[0].payload["status"], "retired");
    }

    #[tokio::test]
    async fn reconcile_records_published_per_touched_row_only() {
        let registry = InMemoryWorkflows::new();
        let actor = boss_core::actor::ActorId::Automation("bootstrap-reconciler".into());

        // Fresh insert → one event.
        registry
            .bootstrap_reconcile(
                &[reconcile_spec("workflow-design", "Design a Workflow")],
                &actor,
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(registry.recorded_events().len(), 1);

        // No drift → untouched row, no event.
        registry
            .bootstrap_reconcile(
                &[reconcile_spec("workflow-design", "Design a Workflow")],
                &actor,
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(
            registry.recorded_events().len(),
            1,
            "an unchanged reconcile row writes nothing and must record nothing"
        );

        // Drift → republish, one more event carrying the new version.
        registry
            .bootstrap_reconcile(
                &[reconcile_spec("workflow-design", "Renamed")],
                &actor,
                Utc::now(),
            )
            .await
            .unwrap();
        let events = registry.recorded_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, crate::events::WORKFLOW_PUBLISHED);
        assert_eq!(events[1].payload["version"], 2);
        assert_eq!(
            events[1].payload["_actor"],
            "automation:bootstrap-reconciler"
        );
    }
}

#[cfg(test)]
mod title_expansion_tests {
    use super::*;

    fn subject() -> Subject {
        Subject::new("custom", "disk-headroom")
    }

    #[test]
    fn metadata_tokens_resolve_in_step_titles() {
        // The bug this exists for: `maintenance-sweep` v1 shipped
        // `Inspect: {{ job.metadata.target }}` and rendered the mustache
        // verbatim, because the token alphabet is single-brace
        // `{metadata.<field>}` and nothing ever spoke mustache. A step
        // title reading like broken markup is worse than no
        // interpolation, since it is the sentence telling a human what
        // is being asked of them.
        let meta = serde_json::json!({ "target": "disk-headroom" });
        assert_eq!(
            expand_title("Inspect: {metadata.target}", &subject(), &meta),
            "Inspect: disk-headroom"
        );
    }

    #[test]
    fn subject_and_metadata_tokens_compose() {
        let meta = serde_json::json!({ "area": "infra" });
        assert_eq!(
            expand_title(
                "{subject.kind}/{subject.id} — {metadata.area}",
                &subject(),
                &meta
            ),
            "custom/disk-headroom — infra"
        );
    }

    #[test]
    fn numbers_and_bools_render_as_scalars() {
        let meta = serde_json::json!({ "commits_ahead": 34, "urgent": true });
        assert_eq!(
            expand_title(
                "Publish {metadata.commits_ahead} ({metadata.urgent})",
                &subject(),
                &meta
            ),
            "Publish 34 (true)"
        );
    }

    #[test]
    fn unknown_and_non_scalar_tokens_are_left_literal() {
        // Left visible on purpose, matching `{day…}` without an anchor:
        // a `{metadata.typo}` on screen is a bug report, whereas
        // blanking it renders "Inspect: " and reads as finished work.
        let meta = serde_json::json!({ "nested": { "a": 1 } });
        assert_eq!(
            expand_title("A {metadata.typo} B {metadata.nested}", &subject(), &meta),
            "A {metadata.typo} B {metadata.nested}"
        );
    }

    #[test]
    fn a_mustache_is_not_the_token_syntax_and_stays_literal() {
        // Guards the misreading directly: if someone reintroduces
        // `{{ job.metadata.x }}` in a registry row, this is what they
        // will get, and the test says so out loud rather than leaving
        // the next author to discover it in David's queue.
        let meta = serde_json::json!({ "target": "disk-headroom" });
        assert_eq!(
            expand_title("Inspect: {{ job.metadata.target }}", &subject(), &meta),
            "Inspect: {{ job.metadata.target }}"
        );
    }
}

#[cfg(test)]
mod frozen_job_tests {
    use super::*;

    /// The 32a4e70d shape: a live job gains a step (a per-question task
    /// the design-review protocol adds), its count no longer matches
    /// the spec, and from that moment no step can advance — including
    /// the terminal, so the job never closes and never leaves the
    /// reviewer's queue.
    #[test]
    fn an_added_step_freezes_the_job_and_is_detectable() {
        let spec = ship_a_change_spec();
        let subject = Subject::new("custom", "x");
        let mut n = 0u32;
        let mut steps = materialize_steps(
            &spec,
            &subject,
            JobId::new(),
            &serde_json::json!({}),
            || {
                n += 1;
                StepId::new()
            },
        );
        assert!(
            !steps_diverged_from_spec(&spec, &steps),
            "a freshly materialized job matches its spec"
        );

        // Complete every spec step, then confirm evaluation still runs.
        let before = reevaluate(&spec, &mut steps, &subject, &serde_json::json!({}));
        let _ = before;

        // Now the protocol adds a step to the live job.
        let mut extra = steps[0].clone();
        extra.id = StepId::new();
        extra.title = "Q: a question captured as a step".into();
        extra.status = StepStatus::Completed;
        steps.push(extra);

        assert!(
            steps_diverged_from_spec(&spec, &steps),
            "an added step must be detectable — this is the frozen state"
        );
        let changed = reevaluate(&spec, &mut steps, &subject, &serde_json::json!({}));
        assert!(
            changed.is_empty(),
            "a diverged job advances nothing: reevaluate must refuse rather \
             than pair spec steps against misaligned job steps"
        );
    }
}
