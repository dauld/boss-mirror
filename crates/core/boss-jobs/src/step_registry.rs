//! Step type registry — maps `kind` strings to schemas, UX treatment,
//! and validation logic. Decision record:
//! `docs/architecture-decisions.md` §Step types are property bundles.
//!
//! Every registered step type has:
//! - A unique `kind` string
//! - A human-readable label and category
//! - A UX treatment hint (inline, expanded, full-screen)
//! - A set of required and optional metadata keys
//! - A validation function that checks metadata on write
//!
//! Unknown kinds are accepted (deliberately permissive) but
//! rendered as generic with no metadata validation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// UX treatment hint for the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepUx {
    Inline,
    Expanded,
    FullScreen,
}

/// Category grouping for step types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepCategory {
    Coordination,
    Operations,
    Commercial,
    Logistics,
    Admin,
}

/// How this kind of step completes — a human executor, an automated
/// agent. A domain fact about the work, not engine config (the same
/// executor model the human-powered-state-machine framing rests on:
/// "executors are humans and agents"). `Human` (default) is the
/// labor/judgment path — the assignment dispatcher routes it to a
/// role-matched Employee and the workforce drives it at human pace.
/// `Agent` is computer-speed automation or a decision: the dispatcher
/// executes it itself on `step.ready` (read state, decide, complete)
/// instead of assigning it, and the human workforce never pulls it. Gate
/// kinds (`demand-gate`, `availability-gate`) and routine automated
/// actions are `Agent`; brewing, QC, picks, sign-offs are `Human`.
///
/// How a step reaches `completed` — the one closed
/// axis of the step alphabet. Sign-off requirements compose with any
/// variant; they are completion-contract clauses, not authorities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Completion {
    /// An operator holding `authority_role` completes it. Default.
    #[default]
    Human,
    /// An agent computes the decision; with an `outcome` enum field
    /// this is a gate (fork point) resolved by `gate_resolve`.
    Agent,
    /// The D7 delegate contract: the child Job's close completes it.
    ChildJob,
    /// A bound external source's event completes it. Binding: the
    /// jobs API rejects manual completion; the policy-gated operator
    /// override is its own audited action.
    External,
    /// Trigger special case: describes job-creation
    /// conditions, auto-completed at materialization, no completion
    /// logic of its own.
    AutoOnMaterialize,
}

/// A metadata field descriptor.
///
/// `field_type` carries the whole value shape, including the variants
/// of an enum (pipe-joined, e.g. `"pass|fail|conditional"`). Consumers
/// that need to synthesize or validate a value read it directly —
/// there is no second hint field to keep in sync with it.
#[derive(Debug, Clone, Serialize)]
pub struct FieldSpec {
    pub name: &'static str,
    pub field_type: &'static str,
    pub required: bool,
    pub description: &'static str,
}

/// A registered step type — the schema, UX treatment, and **observed
/// properties** of one kind of work.
///
/// The fields below `fields` are not simulator configuration. They're
/// learnable facts about *performing this kind of work* — how long it
/// typically takes, which roles do it, how often it stalls — every one
/// of which is derivable from the audit_log of real Jobs (actual
/// durations, actual assignees, actual stalls). The simulator is just
/// one **consumer**: it reads them to simulate the human/agent actor
/// realistically. The assignment dispatcher reads `required_roles` the
/// same way. Treat them as domain metadata about the work, not engine
/// knobs.
#[derive(Debug, Clone, Serialize)]
pub struct StepType {
    pub kind: &'static str,
    pub label: &'static str,
    pub category: StepCategory,
    pub ux: StepUx,
    pub version: u32,
    pub description: &'static str,
    pub fields: Vec<FieldSpec>,
    /// How long this kind of work typically takes (active → done), in
    /// hours — derivable from the audit_log's actual durations.
    /// Consumers like the simulator read it to time completions
    /// realistically (sampling within `× (1 ± typical_duration_jitter)`).
    /// `None` = the duration isn't fixed by the kind; defer to a
    /// per-Job value in metadata (`generic`, `sub-job`, `task`).
    pub typical_duration_hours: Option<f64>,
    /// The natural spread in that duration. 0.3 → values land in
    /// [0.7, 1.3] × the typical. Ignored when `typical_duration_hours`
    /// is `None`.
    pub typical_duration_jitter: f64,
    /// Which roles perform this work — Class registry codes (under
    /// `subject_kind='employee', member_attribute='role'`). A domain
    /// fact about the work; the assignment dispatcher reads it to route
    /// the step to a matching Employee, and the simulator reads it to
    /// pick a realistic actor. Empty = anyone in the roster. Per-Job
    /// overrides ride on the Workflow step's `authority_role`.
    pub required_roles: &'static [&'static str],
    /// How often this kind of work stalls on an external dependency
    /// (parts, a signature, a vendor reply) instead of completing — an
    /// empirical property of the work, learnable from the event log.
    /// `0.0` (default) = it never stalls. The simulator reads it to
    /// model realistic stalls by deferring a completion to a later tick.
    pub block_probability: f64,
    /// Whether this kind of work is performed by a human or an automated
    /// agent. The assignment dispatcher reads it to decide whether to
    /// route the step to an Employee (`Human`) or execute it itself at
    /// computer speed on `step.ready` (`Agent`); the workforce reads it to
    /// skip agent steps. Defaults to `Human`.
    pub completion: Completion,
    /// The floor a Workflow cannot go below. A kind that is always a
    /// human-presence act says so once here instead of relying on
    /// every protocol author to remember.
    #[serde(default)]
    pub assurance_floor: boss_core::job::Assurance,
    /// Which render surface mounts this kind: an id
    /// into the surface table — platform-shipped components and
    /// tenant StepPlugins are the two suppliers. "generic" renders
    /// the default fields/notes surface.
    pub surface: &'static str,
    /// Role codes that must each stamp the step in its current shape
    /// before completion (the sign-off contract). Registry-level default;
    /// Workflows may extend per step. Empty = no sign-offs required.
    pub sign_offs_required: &'static [&'static str],
    /// Is completing this kind a DECISION — a verdict someone renders —
    /// rather than work someone performs? David decided the split twice
    /// on the same day (2026-08-19): My Day partitions "yours to decide"
    /// from "yours because you own it" (d598681f), and assignment
    /// routes decisions to the role's human holder while executable
    /// work defaults to an agent (291a73a7, option c). Both consumers
    /// read THIS flag — the client's interim kind roster documented
    /// exactly this field as its upgrade path (CLAUDE.md §9a: one fact,
    /// one home).
    #[serde(default)]
    pub decision_shaped: bool,
}

/// Validation error for step metadata.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The step type registry. Built once at startup, immutable thereafter.
pub struct StepRegistry {
    types: HashMap<&'static str, StepType>,
}

impl StepRegistry {
    /// Build the v1 registry with every shipped step type — the
    /// state-machine-shape alphabet **plus** the company-modeling
    /// kinds that fire side effects into module-tier services
    /// (commerce, ledger, inventory, people, shipping, products).
    ///
    /// Most v1 deployments want this. Non-company tenants (research
    /// lab, robot fleet) opt out via `StepRegistry::core_v1()` to
    /// avoid shipping kinds whose side-effect handlers reference
    /// services they don't deploy.
    pub fn v1() -> Self {
        Self::from_types(all_v1_types())
    }

    /// Build the v1 registry with **only** state-machine-shape kinds
    /// — those with no side-effect dispatch into module-tier
    /// services. Use this for tenants that don't deploy the
    /// commerce/ledger/inventory/people/shipping/products module
    /// crates. Today: 27 kinds (coordination + generic transitions
    /// + sign-off + scheduling + acknowledgment-style data
    /// templates); the remaining 12 (`billing`, `bill-approval`,
    /// `tax-remittance`, `payroll-release`, `production-consume`,
    /// `production-produce`, `hr-hire`, `hr-terminate`,
    /// `procurement`, `shipment`, `receiving`, `repair`) live in
    /// [`company_modeling_v1_types`] and require the matching
    /// module services to be running.
    pub fn core_v1() -> Self {
        Self::from_types(core_v1_types())
    }

    /// Build a registry from an explicit list. Mostly used by
    /// tests that want to exercise non-default field values
    /// (block_probability, …) without
    /// dragging in the entire v1 catalog.
    pub fn from_types(specs: Vec<StepType>) -> Self {
        let mut types = HashMap::new();
        for st in specs {
            types.insert(st.kind, st);
        }
        Self { types }
    }

    /// Look up a step type by kind. Returns None for unknown kinds.
    pub fn get(&self, kind: &str) -> Option<&StepType> {
        self.types.get(kind)
    }

    /// All registered step types, sorted by category then kind.
    pub fn all(&self) -> Vec<&StepType> {
        let mut v: Vec<&StepType> = self.types.values().collect();
        v.sort_by(|a, b| {
            format!("{:?}", a.category)
                .cmp(&format!("{:?}", b.category))
                .then(a.kind.cmp(b.kind))
        });
        v
    }

    /// Validate step metadata against the registered schema.
    /// Returns Ok(()) for unknown kinds (permissive per D1).
    /// Returns Err with validation errors for known kinds with bad metadata.
    /// Validate step-authored fields (inline authoring) —
    /// the union partner of [`validate_metadata`]: required-at-done
    /// presence + the same per-type value checks.
    pub fn validate_authored_fields(
        fields: &[boss_core::job::StepField],
        metadata: &serde_json::Value,
    ) -> Result<(), Vec<ValidationError>> {
        if fields.is_empty() {
            return Ok(());
        }
        let obj = match metadata.as_object() {
            Some(o) => o,
            None => {
                return Err(vec![ValidationError {
                    field: "(root)".into(),
                    message: "metadata must be a JSON object".into(),
                }]);
            }
        };
        let mut errors = Vec::new();
        for field in fields {
            if field.required && !obj.contains_key(&field.name) {
                errors.push(ValidationError {
                    field: field.name.clone(),
                    message: format!("required field '{}' is missing", field.name),
                });
            }
            if let Some(value) = obj.get(&field.name)
                && let Err(e) = validate_field_type(&field.name, &field.field_type, value)
            {
                errors.push(e);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn validate_metadata(
        &self,
        kind: &str,
        metadata: &serde_json::Value,
    ) -> Result<(), Vec<ValidationError>> {
        let Some(step_type) = self.types.get(kind) else {
            // Unknown kind — permissive, accept anything
            return Ok(());
        };

        let obj = match metadata.as_object() {
            Some(o) => o,
            None => {
                return Err(vec![ValidationError {
                    field: "(root)".into(),
                    message: "metadata must be a JSON object".into(),
                }]);
            }
        };

        let mut errors = Vec::new();

        for field in &step_type.fields {
            if field.required && !obj.contains_key(field.name) {
                errors.push(ValidationError {
                    field: field.name.to_string(),
                    message: format!("required field '{}' is missing", field.name),
                });
            }

            if let Some(value) = obj.get(field.name)
                && let Err(e) = validate_field_type(field.name, field.field_type, value)
            {
                errors.push(e);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn validate_field_type(
    name: &str,
    expected: &str,
    value: &serde_json::Value,
) -> Result<(), ValidationError> {
    let ok = match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "date" => value.as_str().is_some_and(|s| s.len() == 10), // loose date check
        "date-time" => value.as_str().is_some_and(|s| s.len() >= 19),
        "uri" => value.is_string(),
        // Enum types (pipe-separated values)
        s if s.contains('|') => {
            let allowed: Vec<&str> = s.split('|').collect();
            value.as_str().is_some_and(|v| allowed.contains(&v))
        }
        _ => true, // unknown type spec — accept
    };

    if ok {
        Ok(())
    } else {
        Err(ValidationError {
            field: name.to_string(),
            message: format!("expected type '{expected}', got {}", value_type_name(value)),
        })
    }
}

fn value_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// TOML loader — the v1 alphabet ships as data
// ---------------------------------------------------------------------------
//
// The catalog itself lives in `seeds/step_types.toml`. At first
// call, `all_v1_types()` parses that file via `LoadedStepType` (a
// private serde mirror of `StepType` with owned `String` fields),
// converts each row into a `StepType` by leaking its strings to
// `'static`, and caches the result in a `OnceLock<Vec<StepType>>`.
//
// The leak is one-time per process (one tiny `Box`-leak per
// distinct string), the OnceLock guarantees we only do it once,
// and the public `StepType` API stays exactly as it was — every
// caller still gets `&'static str` slices it can compare against
// string literals. Same shape D2 (`boss-ml`) used for
// `phase_two_models.toml`.

const STEP_TYPES_TOML: &str = include_str!("../seeds/step_types.toml");

#[derive(Debug, Deserialize)]
struct StepTypesToml {
    step_type: Vec<LoadedStepType>,
}

#[derive(Debug, Deserialize)]
struct LoadedStepType {
    kind: String,
    label: String,
    category: StepCategory,
    ux: StepUx,
    version: u32,
    description: String,
    #[serde(default)]
    fields: Vec<LoadedFieldSpec>,
    #[serde(default)]
    typical_duration_hours: Option<f64>,
    typical_duration_jitter: f64,
    #[serde(default)]
    required_roles: Vec<String>,
    #[serde(default)]
    block_probability: f64,
    #[serde(default)]
    completion: Completion,
    #[serde(default = "default_surface")]
    surface: String,
    #[serde(default)]
    sign_offs_required: Vec<String>,
    #[serde(default)]
    decision_shaped: bool,
}

fn default_surface() -> String {
    "generic".to_string()
}

#[derive(Debug, Deserialize)]
struct LoadedFieldSpec {
    name: String,
    field_type: String,
    required: bool,
    description: String,
}

impl LoadedFieldSpec {
    fn into_field_spec(self) -> FieldSpec {
        FieldSpec {
            name: String::leak(self.name),
            field_type: String::leak(self.field_type),
            required: self.required,
            description: String::leak(self.description),
        }
    }
}

impl LoadedStepType {
    fn into_step_type(self) -> StepType {
        let roles: Vec<&'static str> = self
            .required_roles
            .into_iter()
            .map(|r| -> &'static str { String::leak(r) })
            .collect();
        StepType {
            kind: String::leak(self.kind),
            label: String::leak(self.label),
            category: self.category,
            ux: self.ux,
            version: self.version,
            description: String::leak(self.description),
            fields: self
                .fields
                .into_iter()
                .map(LoadedFieldSpec::into_field_spec)
                .collect(),
            typical_duration_hours: self.typical_duration_hours,
            typical_duration_jitter: self.typical_duration_jitter,
            required_roles: Vec::leak(roles),
            block_probability: self.block_probability,
            completion: self.completion,
            assurance_floor: Default::default(),
            surface: String::leak(self.surface),
            sign_offs_required: Vec::leak(
                self.sign_offs_required
                    .into_iter()
                    .map(|r| -> &'static str { String::leak(r) })
                    .collect::<Vec<_>>(),
            ),
            decision_shaped: self.decision_shaped,
        }
    }
}

// ---------------------------------------------------------------------------
// V1 type definitions
// ---------------------------------------------------------------------------
//
// Two tiers ship in v1:
//
// 1. **Core (state-machine OS)** — kinds with no side-effect
//    dispatch. Generic transitions, coordination gates, acknowledgment-
//    style data templates. Any tenant (company, research lab, robot
//    robot pool) can use these without deploying module-tier services.
//    Returned by [`core_v1_types`] / `StepRegistry::core_v1()`.
//
// 2. **Company modeling** — kinds whose completion fires a side
//    effect into a module-tier service (commerce.invoice.issue,
//    ledger.payroll.run.submit, inventory.parts.consume,
//    people.hire, …). Deploying these without the matching service
//    means the step completes but the downstream projection is
//    missing. Returned by [`company_modeling_v1_types`].
//
// [`all_v1_types`] composes both tiers — that's what
// `StepRegistry::v1()` ships and what every existing tenant
// expects. The split exists so a non-company tenant can opt out
// of company verbs without forking core (`StepRegistry::core_v1()`).
//
// The partition criterion lives in infra/dispatcher/rules.toml now —
// a step kind is "company-modeling" if any rule's `on_event` matches
// `step.done.<kind>`. step_types.toml is pure schema declaration.

/// State-machine-shape kinds — same set every tenant ships today,
/// independent of dispatcher rule coverage.
pub fn core_v1_types() -> Vec<StepType> {
    all_v1_types()
}

/// Company-modeling kinds. Currently empty — the core/module-tier
/// partition isn't modeled at the StepType level. It would return a
/// non-empty set only if a Workflow-level "needs module tier"
/// predicate were introduced.
pub fn company_modeling_v1_types() -> Vec<StepType> {
    Vec::new()
}

fn all_v1_types() -> Vec<StepType> {
    static CACHE: OnceLock<Vec<StepType>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let parsed: StepTypesToml = toml::from_str(STEP_TYPES_TOML)
                .expect("step_types.toml ships with the crate; parse should never fail");
            parsed
                .step_type
                .into_iter()
                .map(LoadedStepType::into_step_type)
                .collect()
        })
        .clone()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_43_types() {
        // Test name predates the count bumps; leaving the name alone
        // keeps blame-diff churn down. Count is just "length of
        // seeded types", not any load-bearing invariant. 48 since
        // `credential-rotation` (the rotation packet's dedicated scope
        // kind, so the credential broker's rule can target
        // `step.done.credential-rotation` instead of firing on every
        // `task` — the same reasoning as `gate-verdict` before it).
        let reg = StepRegistry::v1();
        assert_eq!(reg.all().len(), 48);
    }

    /// The reason `scope-declaration` is registered at all.
    ///
    /// The dispatcher decides DECIDES vs EXECUTES from this registry
    /// and defaults an UNKNOWN kind to decision-shaped
    /// (dispatcher.rs, `unwrap_or(true)`). So leaving this kind
    /// unregistered is not neutral: the moment ship-a-change points
    /// its `scope` step at it, every new car's boundary step becomes a
    /// "decision", gets nominated to the single human holder of
    /// `platform-admin`, and lands in that person's queue — moments
    /// before `boss park` fills it anyway.
    ///
    /// A count assertion cannot catch that. This pins the property the
    /// entry exists for, so deleting the flag or the block fails here
    /// with the reason rather than as an off-by-one.
    #[test]
    fn declaring_a_boundary_is_work_not_a_verdict() {
        let reg = StepRegistry::v1();
        let scope = reg
            .get("scope-declaration")
            .expect("scope-declaration registered — an unregistered kind reads as a decision");
        assert!(
            !scope.decision_shaped,
            "declaring what a change contains is executable work; marking it \
             decision-shaped routes every car's scope step to a human queue"
        );
    }

    #[test]
    fn every_step_type_carries_sim_affordances() {
        // Sim affordances ship as fields on every StepType — that's
        // the whole point of this lift. The shape-driven simulator
        // walks `reg.all()` and reads each kind's
        // `typical_duration_hours` / `typical_duration_jitter` /
        // `required_roles` to schedule step completions and pick
        // assignees without any per-domain Rust code. This test
        // pins a few representative values so an accidental wipe
        // (e.g. a bulk regex over the registry) shows up as a test
        // failure rather than a silently-zeroed sim.
        let reg = StepRegistry::v1();
        let approval = reg.get("sign-off").expect("sign-off registered");
        assert_eq!(approval.typical_duration_hours, Some(0.5));
        assert!(approval.typical_duration_jitter > 0.0);

        let repair = reg.get("repair").expect("repair registered");
        assert_eq!(repair.typical_duration_hours, Some(8.0));

        let milestone = reg.get("milestone").expect("milestone registered");
        // Milestones are point-in-time markers — instant by design.
        assert_eq!(milestone.typical_duration_hours, Some(0.0));
        assert_eq!(milestone.typical_duration_jitter, 0.0);

        // `task` and `generic` deliberately leave duration `None` —
        // the sim should defer to per-Job metadata, not synthesize
        // a kind-level default.
        let task = reg.get("task").expect("task registered");
        assert_eq!(task.typical_duration_hours, None);
        let generic = reg.get("task").expect("task registered");
        assert_eq!(generic.typical_duration_hours, None);
    }

    #[test]
    fn every_platform_jobkind_step_has_a_defined_step_type() {
        // Invariant I-3: the alphabet is closed; programs are open.
        // Every step kind referenced by a platform-shipped Workflow
        // must be defined in the StepType registry — otherwise the
        // step exists on the Job without a metadata validator,
        // which silently fails the "required at done" contract.
        //
        // Tenant-specific kinds live in per-tenant TOMLs, linted at
        // load time by the seed_loader test. `platform_workflows()` carries
        // just `workflow-design`.
        let reg = StepRegistry::v1();
        let defined: std::collections::HashSet<&str> = reg.all().iter().map(|t| t.kind).collect();

        let mut missing: Vec<(String, String)> = Vec::new();
        for spec in crate::registry::platform_workflows() {
            for step in &spec.steps {
                if !defined.contains(step.kind.as_str()) {
                    missing.push((spec.kind.clone(), step.kind.clone()));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "platform Workflows reference undefined StepType kinds: {missing:?}"
        );
    }

    #[test]
    fn all_kinds_are_unique() {
        let reg = StepRegistry::v1();
        let kinds: Vec<&str> = reg.all().iter().map(|t| t.kind).collect();
        let mut deduped = kinds.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(kinds.len(), deduped.len());
    }

    #[test]
    fn generic_accepts_anything() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({"whatever": true, "count": 42});
        assert!(reg.validate_metadata("task", &meta).is_ok());
    }

    #[test]
    fn unknown_kind_is_permissive() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({"foo": "bar"});
        assert!(reg.validate_metadata("totally-custom", &meta).is_ok());
    }

    #[test]
    fn approval_accepts_valid_metadata() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({
            "decision": "approved",
            "comment": "Looks good"
        });
        assert!(reg.validate_metadata("sign-off", &meta).is_ok());
    }

    #[test]
    fn approval_rejects_bad_decision_enum() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({"decision": "maybe"});
        let err = reg.validate_metadata("sign-off", &meta).unwrap_err();
        assert!(err.iter().any(|e| e.field == "decision"));
    }

    #[test]
    fn handoff_requires_from_and_to() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({"from_id": "team-a"});
        let err = reg.validate_metadata("handoff", &meta).unwrap_err();
        assert!(err.iter().any(|e| e.field == "to_id"));
    }

    #[test]
    fn handoff_passes_with_required_fields() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({"from_id": "team-a", "to_id": "team-b"});
        assert!(reg.validate_metadata("handoff", &meta).is_ok());
    }

    #[test]
    fn repair_accepts_full_metadata() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({
            "asset_id": "SYS-00012345",
            "labor_hours": 3.5,
            "parts_consumed": [{"sku": "PRT-001", "qty": 2}],
            "work_notes": "Replaced cooling module",
            "photos": ["https://example.com/photo1.jpg"]
        });
        assert!(reg.validate_metadata("repair", &meta).is_ok());
    }

    #[test]
    fn billing_requires_account_and_amount() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({"account_id": "P-001"});
        let err = reg.validate_metadata("billing", &meta).unwrap_err();
        assert!(err.iter().any(|e| e.field == "amount_cents"));
        assert!(err.iter().any(|e| e.field == "currency"));
    }

    #[test]
    fn shipment_validates_direction_enum() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({
            "direction": "sideways",
            "origin": "warehouse",
            "destination": "account"
        });
        let err = reg.validate_metadata("shipment", &meta).unwrap_err();
        assert!(err.iter().any(|e| e.field == "direction"));
    }

    #[test]
    fn shipment_accepts_valid_enums() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!({
            "direction": "outbound",
            "origin": "Boss Warehouse",
            "destination": "Westside Derm"
        });
        assert!(reg.validate_metadata("shipment", &meta).is_ok());
    }

    #[test]
    fn metadata_must_be_object() {
        let reg = StepRegistry::v1();
        let meta = serde_json::json!("not an object");
        let err = reg.validate_metadata("sign-off", &meta).unwrap_err();
        assert!(err[0].field == "(root)");
    }

    #[test]
    fn each_category_has_at_least_one_type() {
        let reg = StepRegistry::v1();
        let categories: Vec<StepCategory> = reg.all().iter().map(|t| t.category).collect();
        assert!(categories.contains(&StepCategory::Coordination));
        assert!(categories.contains(&StepCategory::Operations));
        assert!(categories.contains(&StepCategory::Commercial));
        assert!(categories.contains(&StepCategory::Logistics));
        assert!(categories.contains(&StepCategory::Admin));
    }

    #[test]
    fn toml_parses_at_compile_time() {
        // Belt-and-suspenders: the `OnceLock` parse in `all_v1_types()`
        // is lazy, so a malformed `step_types.toml` would only surface
        // on the first registry call. This test forces the parse and
        // asserts the kind count so a typo (or an accidental block
        // delete) fails at `cargo test`, not at boot. Same shape as
        // the D2 `boss-ml` toml_parses_at_compile_time guard.
        let v = all_v1_types();
        assert_eq!(
            v.len(),
            48,
            "step_types.toml should have 48 [[step_type]] blocks"
        );
    }

    #[test]
    fn authored_fields_validate_in_union_with_the_bundle() {
        use boss_core::job::StepField;
        let fields = vec![
            StepField {
                name: "counter_offer_cents".into(),
                field_type: "integer".into(),
                required: true,
                filled_by: boss_core::job::FilledBy::Executor,
            },
            StepField {
                name: "notes".into(),
                field_type: "string".into(),
                required: false,
                filled_by: boss_core::job::FilledBy::Executor,
            },
        ];
        // Missing required authored field → error naming it.
        let err =
            StepRegistry::validate_authored_fields(&fields, &serde_json::json!({ "notes": "x" }))
                .unwrap_err();
        assert!(err.iter().any(|e| e.field == "counter_offer_cents"));
        // Wrong type → error; right shape → ok.
        assert!(
            StepRegistry::validate_authored_fields(
                &fields,
                &serde_json::json!({ "counter_offer_cents": "not-an-int" }),
            )
            .is_err()
        );
        assert!(
            StepRegistry::validate_authored_fields(
                &fields,
                &serde_json::json!({ "counter_offer_cents": 125_00 }),
            )
            .is_ok()
        );
        // No authored fields → permissive.
        assert!(StepRegistry::validate_authored_fields(&[], &serde_json::json!("x")).is_ok());
    }

    #[test]
    fn completion_defaults_human_and_gates_are_agent() {
        let reg = StepRegistry::v1();
        // Unmarked kinds default to Human — labor + judgment stay on the
        // human workforce (brewing, sign-off).
        assert_eq!(
            reg.get("production-consume").unwrap().completion,
            Completion::Human
        );
        assert_eq!(reg.get("sign-off").unwrap().completion, Completion::Human);
        // demand-gate is agent-executed — the dispatcher resolves it at
        // computer speed on step.ready, not the human workforce.
        assert_eq!(
            reg.get("demand-gate").unwrap().completion,
            Completion::Agent
        );
    }
}
