//! Static validation for `WorkflowSpec` rows — the **viability lint**.
//!
//! A Workflow is a program written in the StepType alphabet; its step
//! DAG is implicit in the steps' `ready_when` predicates. The lint
//! proves the program is well-formed before it can run:
//!
//! - **Phase 0 — metadata shapes.** Every value in a step's
//!   `metadata_defaults` matches its StepType field's declared type
//!   (catches a not-in-enum value like `channel = "in-person"` at
//!   author time instead of mid-run).
//! - **Phase 1 — structural.** At least one trigger (`ready_when =
//!   "true"`), at least one terminal, every `steps.<slug>` reference
//!   resolves, the dependency graph is acyclic, and every leaf is a
//!   declared terminal.
//! - **Phase 2 — reachability.** Every step is reachable forward from
//!   some trigger and backward from some terminal — no dead code.
//! - **Phase 4 — a sign-off cannot arrive blind.** A `sign-off` step
//!   asks a PERSON to approve something, and the UI renders the step
//!   the reader is on — so a sign-off whose context is nowhere is an
//!   empty screen with an Approve button. Either the step declares its
//!   own required fields or a procedure, or some step it depends on
//!   REQUIRES a field, which is what guarantees the context exists
//!   before the packet can reach a human.
//! - **Phase 3 — fork coverage.** Where a step is a fork point (≥2
//!   successors discriminate on its outcome), every value of the
//!   discriminating enum is handled by some successor, or a wildcard
//!   fallback covers the open-ended case.
//!
//! Runs at author time (`POST /api/workflows/_validate`), publish
//! time (every registry path that can set a row ACTIVE — see
//! [`gate_active`]), and boot time (`workflow_quarantine`).
//! One definition, four call sites: the author-time dry run and the
//! publish gate cannot disagree about what "viable" means, which is
//! exactly how the 2026-08-13 outage happened — `_validate` could
//! name the problem the whole time, and publish never asked it.

use crate::registry::{StepSpec, WorkflowSpec, predicate_step_refs};
use crate::step_registry::StepRegistry;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// One viability failure. `step` is the offending step slug (empty
/// for whole-Workflow structural failures).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLintError {
    pub workflow: String,
    pub step: String,
    pub reason: String,
}

impl std::fmt::Display for WorkflowLintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.step.is_empty() {
            write!(f, "[{}] {}", self.workflow, self.reason)
        } else {
            write!(
                f,
                "[{}] step `{}`: {}",
                self.workflow, self.step, self.reason
            )
        }
    }
}

/// Validate a single WorkflowSpec. Returns every violation found; an
/// empty Vec means the Workflow is viable.
pub fn validate_workflow(spec: &WorkflowSpec, registry: &StepRegistry) -> Vec<WorkflowLintError> {
    let mut errs = Vec::new();
    // Phase 0 — metadata default value shapes.
    for step in &spec.steps {
        check_metadata_defaults_values(spec, step, registry, &mut errs);
    }
    // Phases 1–3 — viability of the predicate graph.
    check_viability(spec, registry, &mut errs);
    // Phase 4 — a human decision point must arrive with its context.
    // Platform workflows only; see the function's note.
    if spec.category == "platform" {
        check_sign_offs_are_not_blind(spec, registry, &mut errs);
    }
    // Phase 5 — a human decision point must leave a record.
    check_decisions_leave_a_record(spec, registry, &mut errs);
    errs
}

/// Phase 5: a decision must leave a record.
///
/// Phase 4 guarantees a human decision point ARRIVES with context;
/// this guarantees it LEAVES with one. They bind different moments:
/// context is a readiness concern and lives on a predecessor, but
/// required metadata is validated at COMPLETION — so the record
/// constraint belongs on the step itself (or its kind's bundle), and
/// a predecessor requirement satisfies Phase 4 while recording
/// nothing of the judgement.
///
/// WHY THIS IS A HARD ERROR. David, 2026-08-29: "Let's try to make
/// sure we can't lose my decisions again in the future. Data loss is
/// a real concern." Measured that day (cdfe2e1a): 62 of 100 completed
/// sign-off steps across every active workflow carried NOTHING — no
/// authored metadata, no notes. Not rare; the majority case, and
/// silent for nine days at a stretch. The remedy was settled by
/// experiment rather than escalation: among keys completers actually
/// write, `decision` (already the sign-off bundle's enum) dominates.
///
/// THE ORDER WAS FIX, THEN GATE. The fifteen affected workflows were
/// moved to require `decision` first (new versions, 2026-08-29/30,
/// registry writes), the seed bundles in the same change as this
/// rule — because a lint the seed corpus fails breaks startup rather
/// than preventing a defect.
///
/// UNSCOPED to category, unlike Phase 4, deliberately: Phase 4's
/// remedy (authoring real context) needs domain knowledge and so
/// left tenant workflows to someone who has it, but this phase's
/// remedy is one field the bundle already declares. The live corpus
/// carries it in every category and the seed bundles were brought
/// along in the same change.
fn check_decisions_leave_a_record(
    spec: &WorkflowSpec,
    registry: &StepRegistry,
    errs: &mut Vec<WorkflowLintError>,
) {
    // The same property Phase 4 keys on, for the same reason: ask the
    // registry, never compare a kind name (CLAUDE.md §9,
    // infra/lint/no-step-kind-match.sh).
    let is_approval = |step: &StepSpec| {
        registry
            .get(&step.kind)
            .is_some_and(|t| t.surface == "approval")
    };
    for step in spec.steps.iter().filter(|s| is_approval(s)) {
        let own_required = step.fields.iter().any(|f| f.required);
        let bundle_required = registry
            .get(&step.kind)
            .is_some_and(|t| t.fields.iter().any(|f| f.required));
        if own_required || bundle_required {
            continue;
        }
        errs.push(WorkflowLintError {
            workflow: spec.kind.clone(),
            step: step.title.clone(),
            reason: "is a decision point that can complete EMPTY: neither the step nor its \
                     kind's bundle requires any field, so what was decided is recorded only \
                     if the approver volunteers it — measured at 62 of 100 completed \
                     sign-offs carrying nothing (cdfe2e1a). Require `decision` on the step \
                     (the corpus' own shape, a new workflow version) so completion cannot \
                     lose the judgement. Context arriving is Phase 4's concern; the record \
                     leaving is this one's."
                .into(),
        });
    }
}

/// Phase 4: every `sign-off` step must be guaranteed some context.
///
/// WHY THIS IS A HARD ERROR AND NOT ADVICE. On 2026-08-28 the same
/// protocol-retro packet reached David EMPTY three times running: its
/// work steps carried 6,654 characters and the `review` sign-off
/// carried 14 — just `authority_role`. Each time it was hand-patched
/// and nothing structural changed, which is precisely why it recurred.
/// An audit that day found the shape everywhere: of 16 active
/// workflows with a sign-off, 12 had one that could be reached with
/// nothing on it, including every human decision point in the brewery
/// tenant — a CFO approving a tax filing, an owner approving a tap
/// launch.
///
/// David, 2026-08-28: *"All the expected info is a constraint on it
/// reaching that point in the protocol."*
///
/// THE CONSTRAINT IS SATISFIED BY THE PREDECESSOR, and that is the
/// load-bearing part. Required metadata is validated AT COMPLETION, so
/// a required field on the sign-off itself would refuse only after the
/// approver had already been shown an empty screen. A required field
/// on a step the sign-off DEPENDS ON means the packet cannot reach the
/// human until the context exists.
///
/// Deliberately narrow, in two directions.
///
/// SCOPED TO `sign-off` STEPS, because those are the ones a person is
/// blocked on. 69 agent-facing steps were still arriving blind when
/// this shipped (filed 1671bece), and failing those here would
/// quarantine most of the system at boot.
///
/// SCOPED TO `category = "platform"` WORKFLOWS — the ones this
/// deployment actually operates. The two worked-example tenants carry
/// 33 more blind sign-offs between them (brewery 11, used-device-shop
/// 22), and they are a real gap in the product story rather than an
/// internal one: `refurb-used/qa-certification` and
/// `support-rma/approve-rma` ask a person to approve with nothing on
/// the screen, exactly as `protocol-retro/review` did.
///
/// They are excluded on purpose rather than overlooked. Fixing them
/// means authoring what a QA certifier or an RMA approver needs to see,
/// which is domain knowledge; a generic placeholder field would satisfy
/// this lint while teaching a reader nothing, and a lint that can be
/// satisfied without solving the problem is worse than no lint. David,
/// 2026-08-28, chose this scope deliberately. Widening it is the right
/// move once someone who knows those domains fills them in.
fn check_sign_offs_are_not_blind(
    spec: &WorkflowSpec,
    registry: &StepRegistry,
    errs: &mut Vec<WorkflowLintError>,
) {
    let by_title: HashMap<&str, &StepSpec> =
        spec.steps.iter().map(|s| (s.title.as_str(), s)).collect();
    // ASK THE REGISTRY FOR THE PROPERTY, never compare a kind name.
    // `surface = "approval"` is what makes a step a human decision
    // point, and it is declared in step_types.toml — so a new kind that
    // renders an approval surface is covered the day it is added, with
    // no edit here. Comparing the step's kind against a literal kind
    // name would be exactly what `infra/lint/no-step-kind-match.sh`
    // refuses and CLAUDE.md §9 explains: step-kind names are data, and
    // core code dispatches on properties. (That lint greps text, so it
    // flags the offending shape even inside a comment — which is how
    // this note came to be phrased the long way round.)
    //
    // The obvious alternative — a non-empty `sign_offs_required` — was
    // measured and REJECTED: 5 of 16 sign-off steps declare none,
    // including `protocol-retro/review`, the exact packet that reached
    // David empty three times. A discriminator that misses the
    // motivating case is the wrong discriminator.
    let is_approval = |step: &StepSpec| {
        registry
            .get(&step.kind)
            .is_some_and(|t| t.surface == "approval")
    };
    for step in spec.steps.iter().filter(|s| is_approval(s)) {
        let own_required = step.fields.iter().any(|f| f.required);
        let own_procedure = step
            .metadata_defaults
            .get("procedure")
            .is_some_and(|v| !v.is_null());
        // A dependency that REQUIRES something cannot complete without
        // it, and this step cannot become ready until it completes.
        let dep_required = predicate_step_refs(&step.ready_when)
            .iter()
            .filter_map(|slug| by_title.get(slug.as_str()))
            .any(|dep| dep.fields.iter().any(|f| f.required));
        if own_required || own_procedure || dep_required {
            continue;
        }
        errs.push(WorkflowLintError {
            workflow: spec.kind.clone(),
            step: step.title.clone(),
            reason: format!(
                "is a sign-off that can be reached with no context: it declares no required \
                 fields and no `procedure`, and no step it depends on ({}) requires a field. \
                 A person opening this sees an empty screen with an Approve button. Add a \
                 required field to the step it depends on — required metadata is checked at \
                 COMPLETION, so putting it on the predecessor is what stops the packet \
                 reaching a human incomplete.",
                if predicate_step_refs(&step.ready_when).is_empty() {
                    "none".to_string()
                } else {
                    predicate_step_refs(&step.ready_when).join(", ")
                }
            ),
        });
    }
}

/// Validate every WorkflowSpec in a list. One call, every error
/// reported — used by the tenant seed bundles (`load_workflows_*`
/// returns a Vec).
pub fn validate_all(specs: &[WorkflowSpec], registry: &StepRegistry) -> Vec<WorkflowLintError> {
    let mut errs = Vec::new();
    for spec in specs {
        errs.extend(validate_workflow(spec, registry));
    }
    errs
}

/// **The publish gate.** A spec may occupy the ACTIVE slot only if
/// it is viable; `Err` carries every problem, in the order the lint
/// found them.
///
/// Runs against the process-resident StepType registry — the same
/// one `POST /api/workflows/_validate` uses — so an editor showing
/// "no problems" publishes cleanly, and a spec that publishes
/// cleanly boots cleanly.
///
/// Called by every registry write that can set `status = active`
/// (`publish`, `publish_authored`, `bootstrap_reconcile`) in BOTH
/// adapters. Draft writes deliberately do NOT call it: a draft is
/// work in progress and may be saved in any state.
pub fn gate_active(spec: &WorkflowSpec) -> Result<(), Vec<WorkflowLintError>> {
    gate_active_with(spec, &StepRegistry::v1())
}

/// [`gate_active`] against a caller-supplied registry — for the
/// batch paths that would otherwise rebuild the StepType registry
/// once per spec.
pub fn gate_active_with(
    spec: &WorkflowSpec,
    registry: &StepRegistry,
) -> Result<(), Vec<WorkflowLintError>> {
    let errs = validate_workflow(spec, registry);
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// The wire shape for a list of lint problems: `[{step, reason,
/// message}]`. One definition so the author-time dry run
/// (`_validate`, 200 + `ok:false`) and the publish refusal (422)
/// hand the editor the same JSON to render.
pub fn problems_json(errs: &[WorkflowLintError]) -> Vec<Value> {
    errs.iter()
        .map(|e| {
            serde_json::json!({
                "step": e.step,
                "reason": e.reason,
                "message": e.to_string(),
            })
        })
        .collect()
}

fn err(spec: &WorkflowSpec, step: &str, reason: impl Into<String>) -> WorkflowLintError {
    WorkflowLintError {
        workflow: spec.kind.clone(),
        step: step.to_string(),
        reason: reason.into(),
    }
}

// ---------------------------------------------------------------------------
// Phases 1–3 — viability
// ---------------------------------------------------------------------------

fn check_viability(
    spec: &WorkflowSpec,
    registry: &StepRegistry,
    errs: &mut Vec<WorkflowLintError>,
) {
    let slugs: HashSet<&str> = spec.steps.iter().map(|s| s.title.as_str()).collect();

    // Duplicate slugs make every `steps.<slug>` reference ambiguous.
    let mut seen = HashSet::new();
    for s in &spec.steps {
        if !seen.insert(s.title.as_str()) {
            errs.push(err(
                spec,
                &s.title,
                "duplicate step title (slugs must be unique)",
            ));
        }
    }

    // ----- Phase 1: structural invariants -----
    let triggers: Vec<&StepSpec> = spec
        .steps
        .iter()
        .filter(|s| s.ready_when.trim() == "true")
        .collect();
    if triggers.is_empty() {
        errs.push(err(
            spec,
            "",
            "no trigger step (none with ready_when = \"true\")",
        ));
    }
    let terminals: Vec<&StepSpec> = spec.steps.iter().filter(|s| s.terminal.is_some()).collect();
    if terminals.is_empty() {
        errs.push(err(spec, "", "no terminal step (none carries an outcome)"));
    }

    // Predicate refs must resolve, and must parse.
    for s in &spec.steps {
        if s.ready_when.trim().is_empty() {
            errs.push(err(spec, &s.title, "empty ready_when predicate"));
            continue;
        }
        if boss_expr::parse(&s.ready_when).is_err() {
            errs.push(err(
                spec,
                &s.title,
                format!("ready_when does not parse: `{}`", s.ready_when),
            ));
            continue;
        }
        for r in predicate_step_refs(&s.ready_when) {
            if !slugs.contains(r.as_str()) {
                errs.push(err(
                    spec,
                    &s.title,
                    format!("ready_when references unknown step `{r}`"),
                ));
            }
        }
    }

    // Dependency edges: A → B iff B.ready_when references A.
    let deps: HashMap<String, Vec<String>> = spec
        .steps
        .iter()
        .map(|s| (s.title.clone(), predicate_step_refs(&s.ready_when)))
        .collect();

    if let Some(cycle) = find_cycle(&spec.steps, &deps) {
        errs.push(err(
            spec,
            "",
            format!("ready_when predicates form a cycle: {}", cycle.join(" → ")),
        ));
        return; // reachability + fork coverage assume acyclic.
    }

    // Every leaf (referenced by no successor) must be a declared
    // terminal — otherwise it dead-ends the Job.
    let referenced: HashSet<&str> = deps.values().flatten().map(|s| s.as_str()).collect();
    for s in &spec.steps {
        let is_leaf = !referenced.contains(s.title.as_str());
        if is_leaf && s.terminal.is_none() {
            errs.push(err(
                spec,
                &s.title,
                "leaf step is not a terminal (nothing depends on it and it carries no outcome)",
            ));
        }
    }

    // ----- Phase 2: reachability -----
    let forward = reachable_forward(&spec.steps, &deps, &triggers);
    let backward = reachable_backward(&spec.steps, &deps, &terminals);
    for s in &spec.steps {
        if !forward.contains(s.title.as_str()) {
            errs.push(err(spec, &s.title, "unreachable from any trigger"));
        } else if !backward.contains(s.title.as_str()) {
            errs.push(err(spec, &s.title, "cannot reach any terminal"));
        }
    }

    // ----- Phase 3: fork coverage -----
    check_fork_coverage(spec, registry, &deps, errs);
}

/// DFS cycle detection over the predicate dependency graph. Returns
/// the offending chain (slugs) if a cycle exists.
fn find_cycle(steps: &[StepSpec], deps: &HashMap<String, Vec<String>>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn dfs(
        node: &str,
        deps: &HashMap<String, Vec<String>>,
        state: &mut HashMap<String, Mark>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        state.insert(node.to_string(), Mark::Visiting);
        stack.push(node.to_string());
        for dep in deps.get(node).into_iter().flatten() {
            // Edge node depends-on dep; walk toward predecessors to
            // surface a readable cycle chain.
            match state.get(dep.as_str()).copied() {
                Some(Mark::Visiting) => {
                    let mut chain = stack.clone();
                    chain.push(dep.clone());
                    return Some(chain);
                }
                Some(Mark::Done) => {}
                None => {
                    if let Some(c) = dfs(dep, deps, state, stack) {
                        return Some(c);
                    }
                }
            }
        }
        stack.pop();
        state.insert(node.to_string(), Mark::Done);
        None
    }

    let mut state: HashMap<String, Mark> = HashMap::new();
    let mut stack: Vec<String> = Vec::new();
    for s in steps {
        if !state.contains_key(s.title.as_str()) {
            if let Some(c) = dfs(&s.title, deps, &mut state, &mut stack) {
                return Some(c);
            }
            stack.clear();
        }
    }
    None
}

/// BFS forward from the triggers: a step B is reachable once any step
/// it depends on is reachable (or it is itself a trigger).
fn reachable_forward(
    steps: &[StepSpec],
    deps: &HashMap<String, Vec<String>>,
    triggers: &[&StepSpec],
) -> HashSet<String> {
    let mut reached: HashSet<String> = triggers.iter().map(|t| t.title.clone()).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for s in steps {
            if reached.contains(s.title.as_str()) {
                continue;
            }
            let any_dep_reached = deps
                .get(s.title.as_str())
                .into_iter()
                .flatten()
                .any(|d| reached.contains(d.as_str()));
            if any_dep_reached {
                reached.insert(s.title.clone());
                changed = true;
            }
        }
    }
    reached
}

/// BFS backward from the terminals: a step A can reach a terminal if
/// it is one, or if some step that depends on A can.
fn reachable_backward(
    steps: &[StepSpec],
    deps: &HashMap<String, Vec<String>>,
    terminals: &[&StepSpec],
) -> HashSet<String> {
    let mut reached: HashSet<String> = terminals.iter().map(|t| t.title.clone()).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for s in steps {
            for dep in deps.get(s.title.as_str()).into_iter().flatten() {
                // s depends on dep, so dep can reach whatever s can.
                if reached.contains(s.title.as_str()) && !reached.contains(dep.as_str()) {
                    reached.insert(dep.clone());
                    changed = true;
                }
            }
        }
    }
    reached
}

/// Phase 3: for every fork point (a step whose outcome ≥2 successors
/// discriminate on), prove the successors cover every value the
/// discriminating enum can take, or that a wildcard fallback handles
/// the open-ended case.
fn check_fork_coverage(
    spec: &WorkflowSpec,
    registry: &StepRegistry,
    deps: &HashMap<String, Vec<String>>,
    errs: &mut Vec<WorkflowLintError>,
) {
    for fork in &spec.steps {
        let successors: Vec<&StepSpec> = spec
            .steps
            .iter()
            .filter(|s| {
                deps.get(s.title.as_str())
                    .is_some_and(|d| d.contains(&fork.title))
            })
            .collect();
        if successors.len() < 2 {
            continue; // not a fork.
        }
        // Which of fork's metadata fields do successors branch on?
        let fields = discriminator_fields(&fork.title, &successors);
        // A fallback successor is one satisfied by `fork` completing
        // with NO particular metadata (references fork.done only).
        let has_fallback = successors.iter().any(|s| {
            eval_pred(
                &s.ready_when,
                &synth_fork_payload(spec, registry, deps, &fork.title, None),
            ) == Some(true)
        });

        for field in &fields {
            match enum_domain(registry, fork, field) {
                Some(values) => {
                    for v in &values {
                        let payload = synth_fork_payload(
                            spec,
                            registry,
                            deps,
                            &fork.title,
                            Some((field, Value::String(v.clone()))),
                        );
                        let covered = successors
                            .iter()
                            .any(|s| eval_pred(&s.ready_when, &payload) == Some(true));
                        if !covered {
                            errs.push(err(
                                spec,
                                &fork.title,
                                format!(
                                    "fork outcome {field} = \"{v}\" is handled by no successor (orphan outcome)"
                                ),
                            ));
                        }
                    }
                }
                None => {
                    // Free-text / open-ended discriminator: D9 requires
                    // an explicit wildcard fallback.
                    if !has_fallback {
                        errs.push(err(
                            spec,
                            &fork.title,
                            format!(
                                "fork over free-text `{field}` needs a fallback successor (ready_when = \"steps.{}.done\")",
                                fork.title
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// The metadata fields of `fork` that the successors' predicates read
/// (`steps.<fork>.metadata.<field>`).
fn discriminator_fields(fork: &str, successors: &[&StepSpec]) -> Vec<String> {
    let mut fields = Vec::new();
    for s in successors {
        let Ok(expr) = boss_expr::parse(&s.ready_when) else {
            continue;
        };
        for path in boss_expr::references(&expr) {
            if path.len() == 4 && path[0] == "steps" && path[1] == fork && path[2] == "metadata" {
                let f = path[3].clone();
                if !fields.contains(&f) {
                    fields.push(f);
                }
            }
        }
    }
    fields
}

/// The declared enum values of `field` on the fork step, if the
/// field_type is pipe-shaped (`a|b|c`). `None` for free-text fields.
///
/// An **inline** field authored on the step itself wins over the
/// StepType's field of the same name — so a tenant seed can declare a
/// fork's outcome vocabulary as data (keeping the core StepType generic)
/// and still get exhaustive fork-coverage checking, exactly as Phase-0
/// already type-checks inline `metadata_defaults`.
fn enum_domain(registry: &StepRegistry, fork: &StepSpec, field: &str) -> Option<Vec<String>> {
    let field_type = fork
        .fields
        .iter()
        .find(|f| f.name == field)
        .map(|f| f.field_type.to_string())
        .or_else(|| {
            let st = registry.get(&fork.kind)?;
            st.fields
                .iter()
                .find(|f| f.name == field)
                .map(|f| f.field_type.to_string())
        })?;
    if field_type.contains('|') {
        Some(field_type.split('|').map(|s| s.to_string()).collect())
    } else {
        None
    }
}

/// Build a synthetic predicate payload where `fork` has completed
/// (optionally with one metadata field set), every other step is
/// not-done. Used to probe successor coverage at lint time.
/// `fork` plus every step it transitively depends on — the set that
/// must have completed for the fork to have completed.
///
/// `deps[x]` is what x depends on, so this walks upstream. The
/// `seen` guard also terminates on a malformed cyclic edge set; the
/// lint reports the cycle separately and must not hang first.
fn ancestors_inclusive<'a>(
    deps: &'a HashMap<String, Vec<String>>,
    fork: &'a str,
) -> HashSet<&'a str> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack = vec![fork];
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur) {
            continue;
        }
        for parent in deps.get(cur).into_iter().flatten() {
            stack.push(parent.as_str());
        }
    }
    seen
}

fn synth_fork_payload(
    spec: &WorkflowSpec,
    registry: &StepRegistry,
    deps: &HashMap<String, Vec<String>>,
    fork: &str,
    field: Option<(&str, Value)>,
) -> Value {
    // A fork cannot complete before the steps it depends on have, so a
    // payload where the fork is the ONLY done step describes a state
    // the engine can never reach. It matters because an ancestor's
    // required fields are set BY its completion: marking only the fork
    // done leaves an ancestor's key absent, and a successor reading it
    // then looks uncoverable (or, worse, coverable for the wrong
    // reason) on a graph that behaves correctly at run time.
    let done_set = ancestors_inclusive(deps, fork);
    let mut steps = serde_json::Map::new();
    for s in &spec.steps {
        let done = done_set.contains(s.title.as_str());
        // Model what a materialized step ACTUALLY looks like, because
        // `boss-expr` errors on a missing identifier rather than reading
        // it as null — and an erroring predicate is indistinguishable
        // here from a false one.
        //
        // A payload that named only the fork's own field reported every
        // fork outcome as an orphan for any successor whose predicate
        // ALSO read a second step's metadata. That is the shape of every
        // RE-ROUTING protocol ("this branch is reachable from triage, or
        // from the investigation that reached the same conclusion later")
        // — the lint refused to let anyone publish one.
        //
        // Two layers, and which apply depends on whether the step has
        // run — because that is what decides whether a key is there:
        //   1. `metadata_defaults`, on EVERY step. Stamped at
        //      materialization, so a pending step already carries them
        //      (verified live: an open packet's `closed` step holds its
        //      `outcome_kind` default while still pending).
        //   2. declared fields as null, on DONE steps only. Completion
        //      is what sets them, and a required field is validated at
        //      done — so a step that has run has its keys, and one that
        //      has not does not. Seeding them on a pending step would
        //      make the lint MORE permissive than the engine: the clause
        //      would read false here and error there, and an orphan
        //      covered only by that clause would pass the gate and then
        //      never become ready.
        let mut metadata = serde_json::Map::new();
        if let Value::Object(defaults) = &s.metadata_defaults {
            for (k, v) in defaults {
                metadata.insert(k.clone(), v.clone());
            }
        }
        if done {
            for f in s.fields.iter().map(|f| f.name.to_string()).chain(
                registry
                    .get(&s.kind)
                    .into_iter()
                    .flat_map(|st| st.fields.iter().map(|f| f.name.to_string())),
            ) {
                metadata.entry(f).or_insert(Value::Null);
            }
        }
        if let (true, Some((f, v))) = (s.title == fork, &field) {
            // The discriminator under test wins over any default: this
            // payload exists to ask "what happens when the fork completes
            // with THIS value".
            metadata.insert((*f).to_string(), v.clone());
        }
        let metadata = Value::Object(metadata);
        steps.insert(
            s.title.clone(),
            serde_json::json!({ "done": done, "metadata": metadata }),
        );
    }
    serde_json::json!({
        "subject": {},
        "job": { "metadata": {} },
        "steps": Value::Object(steps),
    })
}

fn eval_pred(src: &str, payload: &Value) -> Option<bool> {
    let expr = boss_expr::parse(src).ok()?;
    let ctx = boss_expr::Context {
        payload,
        helpers: &boss_expr::NoHelpers,
    };
    boss_expr::eval(&expr, &ctx).ok()?.as_bool()
}

// ---------------------------------------------------------------------------
// Phase 0 — metadata default value shapes (carried over from v1)
// ---------------------------------------------------------------------------

/// Every value present in a step's `metadata_defaults` matches the
/// StepType field's declared `field_type`.
///
/// Catches a bad enum literal — e.g. `channel = "in-person"` when the
/// field's enum is `email|phone|meeting|demo|other` — at
/// Workflow-load time, so it fails fast instead of surfacing at
/// step-completion time mid-run.
///
/// Permissive about placeholders: empty strings on date / date-time /
/// uri fields are accepted (seeds leave these blank for
/// completion-time fill). Templated values like `"{subject.id}"` pass
/// through. Unknown field names are ignored. Skipped for unknown step
/// kinds (a separate error class).
fn check_metadata_defaults_values(
    spec: &WorkflowSpec,
    step: &StepSpec,
    registry: &StepRegistry,
    errs: &mut Vec<WorkflowLintError>,
) {
    let Some(step_type) = registry.get(&step.kind) else {
        return;
    };
    let Some(metadata) = step.metadata_defaults.as_object() else {
        return;
    };

    for field in &step_type.fields {
        let Some(value) = metadata.get(field.name) else {
            continue;
        };
        if is_placeholder_default(field.field_type, value) {
            continue;
        }
        if let Some(reason) = check_field_value(field.field_type, value, field.name) {
            errs.push(WorkflowLintError {
                workflow: spec.kind.clone(),
                step: step.title.clone(),
                reason,
            });
        }
    }

    // Inline authoring: step-authored fields get the same
    // defaults-shape check as the kind bundle's fields.
    for field in &step.fields {
        let Some(value) = metadata.get(&field.name) else {
            continue;
        };
        if is_placeholder_default(&field.field_type, value) {
            continue;
        }
        if let Some(reason) = check_field_value(&field.field_type, value, &field.name) {
            errs.push(WorkflowLintError {
                workflow: spec.kind.clone(),
                step: step.title.clone(),
                reason,
            });
        }
    }
}

/// True when the value is an obvious placeholder rather than a real
/// default (e.g. `""` for a date field a downstream step populates at
/// completion time).
fn is_placeholder_default(field_type: &str, value: &Value) -> bool {
    matches!(
        (field_type, value),
        ("date" | "date-time" | "uri", Value::String(s)) if s.is_empty()
    )
}

/// Per-field type check shaped for the lint surface. Template tokens
/// (`{subject.id}`, `{day_minus_13}`) stand in for materialize-time
/// values — accept them; the runtime type-checks the expanded value.
fn check_field_value(field_type: &str, value: &Value, field_name: &str) -> Option<String> {
    if let Value::String(s) = value
        && is_template_token(s)
    {
        return None;
    }
    let ok = match field_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "date" => value.as_str().is_some_and(|s| s.len() == 10),
        "date-time" => value.as_str().is_some_and(|s| s.len() >= 19),
        "uri" => value.is_string(),
        s if s.contains('|') => {
            let allowed: Vec<&str> = s.split('|').collect();
            value.as_str().is_some_and(|v| allowed.contains(&v))
        }
        _ => true,
    };
    if ok {
        return None;
    }
    Some(format!(
        "metadata_defaults field `{}` value {} does not match declared type `{}`",
        field_name,
        truncate_value(value),
        field_type,
    ))
}

/// True iff the value is a single template token like `{subject.id}`
/// or `{day_minus_13}` — expanded at materialize-time, so the
/// seed-time lint can't type-check it.
fn is_template_token(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3 || !s.starts_with('{') || !s.ends_with('}') {
        return false;
    }
    let inner = &s[1..s.len() - 1];
    !inner.contains(['{', '}', ' ', '\t', '\n'])
}

/// Render a JSON value short enough to fit in an error message.
fn truncate_value(value: &Value) -> String {
    let s = value.to_string();
    if s.len() > 80 {
        format!("{}…", &s[..77])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{StepSpec, Terminal};
    use serde_json::json;

    /// Minimal viable two-step Workflow: a trigger that flows into a
    /// terminal. Passes all phases.
    fn viable_spec(kind: &str) -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            kind,
            kind,
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "start".into(),
                    kind: "trigger".into(),
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

    #[test]
    fn minimal_viable_jobkind_passes() {
        let reg = StepRegistry::v1();
        assert!(validate_workflow(&viable_spec("ok"), &reg).is_empty());
    }

    #[test]
    fn missing_trigger_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("no-trigger");
        spec.steps[0].ready_when = "steps.finish.done".into(); // no "true"
        let errs = validate_workflow(&spec, &reg);
        assert!(errs.iter().any(|e| e.reason.contains("no trigger")));
    }

    #[test]
    fn missing_terminal_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("no-terminal");
        spec.steps[1].terminal = None;
        let errs = validate_workflow(&spec, &reg);
        assert!(errs.iter().any(|e| e.reason.contains("no terminal")));
    }

    #[test]
    fn dangling_reference_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("dangling");
        spec.steps[1].ready_when = "steps.ghost.done".into();
        let errs = validate_workflow(&spec, &reg);
        assert!(
            errs.iter()
                .any(|e| e.reason.contains("unknown step `ghost`"))
        );
    }

    #[test]
    fn cycle_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("cyclic");
        // start depends on finish, finish depends on start.
        spec.steps[0].ready_when = "steps.finish.done".into();
        spec.steps[1].ready_when = "steps.start.done".into();
        let errs = validate_workflow(&spec, &reg);
        assert!(errs.iter().any(|e| e.reason.contains("cycle")));
    }

    #[test]
    fn unreachable_step_fails() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("unreachable");
        // An island step depending on nothing reachable and reaching
        // no terminal.
        spec.steps.push(StepSpec {
            title: "island".into(),
            kind: "task".into(),
            ready_when: "steps.island.done".into(), // self-ref → its own cycle guard
            ..Default::default()
        });
        let errs = validate_workflow(&spec, &reg);
        assert!(!errs.is_empty());
    }

    #[test]
    fn fork_orphan_outcome_is_caught() {
        // approval.decision is an enum approved|rejected|changes-requested.
        // Cover only "approved" → the other two are orphan outcomes.
        let reg = StepRegistry::v1();
        let spec = WorkflowSpec::platform_seed(
            "fork",
            "fork",
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "decide".into(),
                    kind: "sign-off".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "ship".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.decision = \"approved\"".into(),
                    terminal: Some(Terminal {
                        outcome: "shipped".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "scrap".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.decision = \"rejected\"".into(),
                    terminal: Some(Terminal {
                        outcome: "scrapped".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let errs = validate_workflow(&spec, &reg);
        assert!(
            errs.iter()
                .any(|e| e.reason.contains("orphan outcome")
                    && e.reason.contains("changes-requested")),
            "expected changes-requested orphan, got: {errs:?}"
        );
    }

    #[test]
    fn a_successor_reading_another_steps_default_is_not_an_orphan() {
        // The re-routing shape (user-feedback v10): a branch is reachable
        // from EITHER the triage fork or a later working step that
        // reaches the same conclusion with better information.
        //
        // The second clause reads `steps.investigate.metadata.route`,
        // and `investigate` declares that key in `metadata_defaults` —
        // so at materialization the step carries it and the predicate
        // evaluates. Synthesizing the fork payload WITHOUT defaults made
        // the whole OR error on a missing identifier (boss-expr is
        // strict), which reported every triage outcome as an orphan and
        // refused a workflow that routes correctly at run time.
        let reg = StepRegistry::v1();
        let spec = WorkflowSpec::platform_seed(
            "reroute",
            "reroute",
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "decide".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    fields: vec![boss_core::job::StepField {
                        name: "route".into(),
                        field_type: "ship|scrap|investigate".into(),
                        required: true,
                        filled_by: boss_core::job::FilledBy::Executor,
                    }],
                    ..Default::default()
                },
                StepSpec {
                    title: "investigate".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.route = \"investigate\"".into(),
                    fields: vec![boss_core::job::StepField {
                        name: "route".into(),
                        field_type: "ship|scrap".into(),
                        required: true,
                        filled_by: boss_core::job::FilledBy::Executor,
                    }],
                    // Stamped at materialization, so the step carries the
                    // key from the moment it exists — which is why the
                    // predicates below evaluate rather than error.
                    metadata_defaults: serde_json::json!({ "route": "scrap" }),
                    ..Default::default()
                },
                StepSpec {
                    title: "ship".into(),
                    kind: "task".into(),
                    ready_when: "(steps.decide.metadata.route = \"ship\") \
                                 OR (steps.investigate.metadata.route = \"ship\")"
                        .into(),
                    terminal: Some(Terminal {
                        outcome: "shipped".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "scrap".into(),
                    kind: "task".into(),
                    ready_when: "(steps.decide.metadata.route = \"scrap\") \
                                 OR (steps.investigate.metadata.route = \"scrap\")"
                        .into(),
                    terminal: Some(Terminal {
                        outcome: "scrapped".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let errs = validate_workflow(&spec, &reg);
        assert!(
            !errs.iter().any(|e| e.reason.contains("orphan outcome")),
            "every decision outcome is handled; got: {errs:?}"
        );
    }

    #[test]
    fn a_clause_reading_an_unrun_steps_unset_field_does_not_count_as_coverage() {
        // The other side of the payload fix, and the reason it seeds
        // declared fields on DONE steps only.
        //
        // `ship` is reachable only via a clause reading `watch`'s
        // `flag`, and `watch` has neither run nor declared a default —
        // so at run time `steps.watch.metadata.flag` is ABSENT, boss-expr
        // errors, and `ship` never becomes ready. Seeding every declared
        // field as null regardless of state would make `!=` read true
        // here and error there: the gate would pass a workflow with a
        // branch that can never be taken.
        let reg = StepRegistry::v1();
        let spec = WorkflowSpec::platform_seed(
            "unrun",
            "unrun",
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "decide".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    fields: vec![boss_core::job::StepField {
                        name: "route".into(),
                        field_type: "ship|scrap".into(),
                        required: true,
                        filled_by: boss_core::job::FilledBy::Executor,
                    }],
                    ..Default::default()
                },
                StepSpec {
                    title: "watch".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    fields: vec![boss_core::job::StepField {
                        name: "flag".into(),
                        field_type: "string".into(),
                        required: false,
                        filled_by: boss_core::job::FilledBy::Executor,
                    }],
                    ..Default::default()
                },
                StepSpec {
                    title: "ship".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.done AND steps.watch.metadata.flag != \"stop\""
                        .into(),
                    terminal: Some(Terminal {
                        outcome: "shipped".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "scrap".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.route = \"scrap\"".into(),
                    terminal: Some(Terminal {
                        outcome: "scrapped".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let errs = validate_workflow(&spec, &reg);
        assert!(
            errs.iter()
                .any(|e| e.reason.contains("orphan outcome") && e.reason.contains("\"ship\"")),
            "route=\"ship\" is reachable only through a key that will not exist; \
             expected an orphan, got: {errs:?}"
        );
    }

    #[test]
    fn inline_field_enum_makes_fork_coverage_pass_without_a_fallback() {
        // A fork step whose kind declares no `outcome` enum (so the domain
        // is free-text by default) but which authors `outcome` inline as
        // `package|skip`. Both values are covered by a successor → the fork
        // is exhaustively covered and needs no `.done` fallback. This is how
        // a tenant seed (e.g. the brewery's packaging allocation) declares a
        // fork's vocabulary as data without a brewery-specific core StepType.
        let reg = StepRegistry::v1();
        let spec = WorkflowSpec::platform_seed(
            "inline-fork",
            "inline-fork",
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "allocate".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    fields: vec![boss_core::job::StepField {
                        name: "outcome".into(),
                        field_type: "package|skip".into(),
                        required: false,
                        filled_by: boss_core::job::FilledBy::Executor,
                    }],
                    ..Default::default()
                },
                StepSpec {
                    title: "package".into(),
                    kind: "task".into(),
                    ready_when: "steps.allocate.metadata.outcome = \"package\"".into(),
                    terminal: Some(Terminal {
                        outcome: "packaged".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "skip".into(),
                    kind: "task".into(),
                    ready_when: "steps.allocate.metadata.outcome = \"skip\"".into(),
                    terminal: Some(Terminal {
                        outcome: "skipped".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let errs = validate_workflow(&spec, &reg);
        assert!(
            !errs.iter().any(|e| e.reason.contains("fork")),
            "inline package|skip enum should make the fork exhaustive, got: {errs:?}"
        );
    }

    #[test]
    fn inline_field_enum_still_catches_an_orphan_outcome() {
        // Same inline `outcome = package|skip`, but only "package" is handled.
        // "skip" is now a provable orphan (not free-text), so the lint must
        // flag it rather than silently accept an uncovered branch.
        let reg = StepRegistry::v1();
        let spec = WorkflowSpec::platform_seed(
            "inline-orphan",
            "inline-orphan",
            "test",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "allocate".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    fields: vec![boss_core::job::StepField {
                        name: "outcome".into(),
                        field_type: "package|skip".into(),
                        required: false,
                        filled_by: boss_core::job::FilledBy::Executor,
                    }],
                    ..Default::default()
                },
                StepSpec {
                    title: "package".into(),
                    kind: "task".into(),
                    ready_when: "steps.allocate.metadata.outcome = \"package\"".into(),
                    terminal: Some(Terminal {
                        outcome: "packaged".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "hold".into(),
                    kind: "task".into(),
                    ready_when: "steps.allocate.metadata.outcome = \"package\"".into(),
                    terminal: Some(Terminal {
                        outcome: "held".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let errs = validate_workflow(&spec, &reg);
        assert!(
            errs.iter()
                .any(|e| e.reason.contains("orphan outcome") && e.reason.contains("skip")),
            "expected `skip` orphan from the inline enum, got: {errs:?}"
        );
    }

    #[test]
    fn outreach_channel_outside_enum_fails_at_load_time() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("regression-21");
        spec.steps[1].kind = "outreach".into();
        spec.steps[1].metadata_defaults =
            json!({ "channel": "in-person", "recipient_id": "{subject.id}" });
        let errs = validate_workflow(&spec, &reg);
        assert!(
            errs.iter()
                .any(|e| e.reason.contains("channel") && e.reason.contains("in-person")),
            "expected channel enum violation, got: {errs:?}"
        );
    }

    #[test]
    fn templated_subject_id_passes_string_field() {
        let reg = StepRegistry::v1();
        let mut spec = viable_spec("campaign");
        spec.steps[1].kind = "outreach".into();
        spec.steps[1].metadata_defaults =
            json!({ "channel": "email", "recipient_id": "{subject.id}" });
        // No Phase-0 violation (other phases still pass for this shape).
        assert!(
            !validate_workflow(&spec, &reg)
                .iter()
                .any(|e| e.reason.contains("metadata_defaults")),
            "{{subject.id}} is a valid string template"
        );
    }

    // ----- Phase 4: a sign-off cannot arrive blind -----

    /// Trigger -> work -> sign-off -> terminal. The sign-off depends on
    /// `work`, so `work` is where the constraint belongs.
    fn signoff_spec(kind: &str) -> WorkflowSpec {
        // category MUST be "platform" — Phase 4 is scoped to the
        // workflows this deployment operates, so a "test"-category
        // fixture would silently skip the very check under test.
        WorkflowSpec::platform_seed(
            kind,
            kind,
            "platform",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "start".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "work".into(),
                    kind: "task".into(),
                    ready_when: "steps.start.done".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "approve".into(),
                    kind: "sign-off".into(),
                    ready_when: "steps.work.done".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "done".into(),
                    kind: "task".into(),
                    ready_when: "steps.approve.done".into(),
                    terminal: Some(Terminal {
                        outcome: "done".into(),
                    }),
                    ..Default::default()
                },
            ],
        )
    }

    fn required(name: &str) -> boss_core::job::StepField {
        boss_core::job::StepField {
            name: name.into(),
            field_type: "string".into(),
            required: true,
            filled_by: boss_core::job::FilledBy::Executor,
        }
    }

    /// THE DEFECT: a sign-off reachable with nothing on it. Shipped to
    /// David three times before this lint existed.
    #[test]
    fn a_sign_off_with_no_guaranteed_context_fails() {
        let reg = StepRegistry::v1();
        let errs = validate_workflow(&signoff_spec("blind"), &reg);
        let hit = errs.iter().find(|e| e.step == "approve");
        let hit = hit.expect("a blind sign-off must be refused");
        assert!(hit.reason.contains("no context"), "{}", hit.reason);
        // The refusal must name what to fix and where.
        assert!(hit.reason.contains("predecessor"), "{}", hit.reason);
        assert!(
            hit.reason.contains("work"),
            "must name the dependency: {}",
            hit.reason
        );
    }

    /// THE FIX DAVID SPECIFIED: the constraint on the step BEFORE.
    #[test]
    fn a_required_field_on_the_predecessor_satisfies_it() {
        let reg = StepRegistry::v1();
        let mut spec = signoff_spec("guarded");
        spec.steps[1].fields.push(required("sign_off_context"));
        // Phase 5 still (correctly) wants a record on the step itself,
        // so filter to THIS phase's concern rather than any error on
        // the step.
        assert!(
            validate_workflow(&spec, &reg)
                .iter()
                .all(|e| e.step != "approve" || !e.reason.contains("no context")),
            "a required field on the dependency guarantees the context exists"
        );
    }

    /// An OPTIONAL field on the predecessor does NOT satisfy it — it can
    /// complete without ever being filled, which is the whole failure.
    #[test]
    fn an_optional_field_on_the_predecessor_does_not_satisfy_it() {
        let reg = StepRegistry::v1();
        let mut spec = signoff_spec("optional");
        spec.steps[1].fields.push(boss_core::job::StepField {
            name: "sign_off_context".into(),
            field_type: "string".into(),
            required: false,
            filled_by: boss_core::job::FilledBy::Executor,
        });
        assert!(
            validate_workflow(&spec, &reg)
                .iter()
                .any(|e| e.step == "approve"),
            "optional means it can arrive empty, which is the defect"
        );
    }

    /// A step that carries its own procedure or its own required field
    /// is fine too — the rule asks that context be GUARANTEED, not that
    /// it arrive from any particular direction.
    #[test]
    fn a_sign_off_carrying_its_own_context_passes() {
        let reg = StepRegistry::v1();
        let mut own_proc = signoff_spec("own-proc");
        own_proc.steps[2].metadata_defaults = json!({"procedure": "what to check"});
        // A procedure satisfies Phase 4 (context) but not Phase 5 (a
        // record) — filter to this phase's reason.
        assert!(
            validate_workflow(&own_proc, &reg)
                .iter()
                .all(|e| e.step != "approve" || !e.reason.contains("no context"))
        );

        let mut own_field = signoff_spec("own-field");
        own_field.steps[2].fields.push(required("decision"));
        assert!(
            validate_workflow(&own_field, &reg)
                .iter()
                .all(|e| e.step != "approve")
        );
    }

    // ----- Phase 5: a decision must leave a record -----

    /// THE DEFECT (cdfe2e1a): 62 of 100 completed sign-offs recorded
    /// nothing, because nothing required anything at completion.
    #[test]
    fn a_decision_that_can_complete_empty_fails() {
        let reg = StepRegistry::v1();
        let errs = validate_workflow(&signoff_spec("empty-ok"), &reg);
        let hit = errs
            .iter()
            .find(|e| e.step == "approve" && e.reason.contains("complete EMPTY"))
            .expect("a recordless decision point must be refused");
        // The refusal must name the remedy the corpus settled on.
        assert!(hit.reason.contains("`decision`"), "{}", hit.reason);
    }

    /// A required field on the step itself is the fix — completion
    /// validates required metadata, so the record cannot be lost.
    #[test]
    fn a_decision_with_its_own_required_field_passes_phase_five() {
        let reg = StepRegistry::v1();
        let mut spec = signoff_spec("records");
        spec.steps[2].fields.push(required("decision"));
        assert!(
            validate_workflow(&spec, &reg)
                .iter()
                .all(|e| !e.reason.contains("complete EMPTY")),
        );
    }

    /// THE DISTINGUISHING CASE: a predecessor requirement satisfies
    /// Phase 4 and does nothing for Phase 5 — the packet still reaches
    /// the approver with context and leaves with no judgement recorded.
    /// This is precisely how 4e0e42b2 arrived full and 188d79ea left
    /// empty on the same day.
    #[test]
    fn a_predecessor_requirement_does_not_make_a_decision_recorded() {
        let reg = StepRegistry::v1();
        let mut spec = signoff_spec("dep-only");
        spec.steps[1].fields.push(required("sign_off_context"));
        assert!(
            validate_workflow(&spec, &reg)
                .iter()
                .any(|e| e.step == "approve" && e.reason.contains("complete EMPTY")),
            "context arriving is not the same as a record leaving"
        );
    }

    /// Unscoped to category, unlike Phase 4: a tenant CFO's empty
    /// sign-off loses a judgement exactly as a platform one does, and
    /// the remedy needs no domain knowledge.
    #[test]
    fn phase_five_judges_tenant_workflows_too() {
        let reg = StepRegistry::v1();
        let mut spec = signoff_spec("tenant");
        spec.category = "operations".into();
        assert!(
            validate_workflow(&spec, &reg)
                .iter()
                .any(|e| e.step == "approve" && e.reason.contains("complete EMPTY")),
        );
    }

    /// Scoped to sign-offs on purpose: 69 agent-facing steps were still
    /// arriving blind when this shipped, and failing those here would
    /// quarantine most of the system at boot.
    #[test]
    fn a_blind_task_step_is_not_failed_by_this_phase() {
        let reg = StepRegistry::v1();
        let mut spec = signoff_spec("task-blind");
        spec.steps[2].kind = "task".into();
        assert!(
            validate_workflow(&spec, &reg)
                .iter()
                .all(|e| e.step != "approve"),
            "this phase judges human decision points only"
        );
    }
}
