//! Tenant-authored Workflow seed loader.
//!
//! Tenants ship `[[workflow]]` rows in
//! `examples/<tenant>/seeds/workflows.toml`. This module parses
//! those files and returns `Vec<WorkflowSpec>` ready to consume by
//! the shape-driven sim, the boss-jobs-api bootstrap, or any test
//! that needs the tenant's Workflow list.
//!
//! Workflow v2 TOML shape — flat steps with predicates, no tiers:
//!
//! ```toml
//! [[workflow]]
//! kind = "morning-brew"
//! label = "Morning Brew"
//! category = "production"
//! subject_kinds = ["location"]
//! description = "..."
//!
//! [[workflow.step]]
//! title = "plan"                       # kebab-case slug, unique per kind
//! kind = "scheduling"                  # StepType
//! ready_when = "true"                  # predicate; "true" = trigger
//! title_template = "Plan today's brew — {subject.id}"
//! metadata_defaults = { recipes_planned = [] }
//!
//! [[workflow.step]]
//! title = "mash-in"
//! kind = "task"
//! ready_when = "steps.plan.done"       # references the slug above
//! title_template = "Mash in"
//! terminal = { outcome = "brewed" }    # optional; marks a terminal
//! sign_offs_required = []          # role codes; "@authority_role" resolves
//! authority_role = "head-brewer"
//! claimable = true              # role queue, not a nomination
//! metadata_defaults = { mash_temp_f = 152 }
//! ```
//!
//! The implicit step DAG is recovered from each `ready_when`'s
//! `steps.<slug>` references — there is no separate tier / edge
//! structure. The `platform_seed` defaults apply for everything the
//! TOML doesn't override (version=1, status=Active, owning_team=system).
//!
//! Every loaded bundle is run through
//! [`crate::workflow_lint::validate_all`] — the viability lint that
//! owns the structural concerns (≥1 trigger, ≥1 terminal, predicate
//! refs resolve, acyclic, reachable, fork coverage). The loader no
//! longer enforces any of that itself; its only job is TOML → spec.
//!
//! Tenants override `owning_team` to their tenant id. Pass `default_owner`
//! into [`load_workflows_with_owning_team`] to set it for every row.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::registry::{StepSpec, Terminal, WorkflowSpec};

#[derive(Debug, thiserror::Error)]
pub enum SeedLoaderError {
    #[error("reading {0}: {1}")]
    Io(String, String),
    #[error("parsing {0}: {1}")]
    Parse(String, String),
    /// One or more Workflows in the loaded file failed
    /// [`crate::workflow_lint::validate_all`]. Surfaces seed-
    /// authoring bugs (bad enum literals, type mismatches,
    /// unviable predicate graphs, side-effect bindings without
    /// metadata defaults) in seconds at startup instead of half an
    /// hour into a sim run.
    #[error("seed lint failed in {file}:\n{}", failures.join("\n"))]
    LintFailed { file: String, failures: Vec<String> },
}

#[derive(Debug, Clone, Deserialize)]
struct WorkflowsFile {
    #[serde(rename = "workflow", default)]
    workflows: Vec<WorkflowToml>,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkflowToml {
    kind: String,
    label: String,
    category: String,
    subject_kinds: Vec<String>,
    #[serde(default)]
    description: Option<String>,
    /// Workflow-level display/routing hints (e.g.
    /// `metadata = { surfaces = ["hr"] }`). Mirrors the registry's
    /// `metadata` JSONB column; same `serde_json::Value` shape as a
    /// tenant could supply for `metadata_schema` / `entitlements`.
    #[serde(default)]
    metadata: serde_json::Value,
    /// Inline step list — flat; the DAG is implicit in each step's
    /// `ready_when` predicate.
    #[serde(default, rename = "step")]
    steps: Vec<StepToml>,
}

/// Serde mirror of [`StepSpec`] for the v2 TOML row. Decoupled from
/// the domain struct so the TOML key set can diverge (e.g. `terminal`
/// arrives as an inline `{ outcome = "..." }` table) without leaking
/// serde attributes onto the registry type.
#[derive(Debug, Clone, Deserialize)]
struct StepToml {
    /// Stable kebab-case slug, unique within the Workflow. Referenced
    /// by other steps' `ready_when` predicates as `steps.<title>.…`.
    title: String,
    kind: String,
    /// Predicate over `(subject, job metadata, prior step states)`.
    /// `"true"` marks a trigger step that fires at Job open.
    ready_when: String,
    /// Optional terminal marker — `{ outcome = "..." }` in TOML.
    /// Reaching `Completed` on a terminal step closes the Job.
    #[serde(default)]
    terminal: Option<TerminalToml>,
    #[serde(default)]
    title_template: String,
    #[serde(default)]
    sign_offs_required: Vec<String>,
    /// A bundle may raise a step's assurance; omitted means "the
    /// StepType's floor", which is Session unless the kind says
    /// otherwise. Protocol data, so raising it is a Workflow edit.
    #[serde(default)]
    assurance_required: Option<boss_core::job::Assurance>,
    /// Spec-authored duration in hours — preferred by executors over
    /// the StepType kind's `typical_duration_hours`. Omitted means
    /// "the kind's typical duration". See `StepSpec::duration_hours`.
    #[serde(default)]
    duration_hours: Option<f64>,
    /// The labor / wall-clock split — see `StepSpec::labor_hours`.
    #[serde(default)]
    labor_hours: Option<f64>,
    #[serde(default)]
    wall_clock_hours: Option<f64>,
    #[serde(default)]
    fields: Vec<boss_core::job::StepField>,
    #[serde(default)]
    authority_role: Option<String>,
    /// Leave the step for its role to claim rather than
    /// nominating one holder. See `StepSpec::claimable`.
    #[serde(default)]
    claimable: Option<bool>,
    #[serde(default)]
    metadata_defaults: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct TerminalToml {
    outcome: String,
}

/// Load a Workflow bundle and materialize `WorkflowSpec`s. The bundle
/// is either one file (`examples/<tenant>/seeds/workflows.toml`) or a
/// DIRECTORY of kind files (`infra/platform/workflows/<kind>.toml`) —
/// see [`load_workflows_with_owning_team`] for the directory rules.
/// Owning team defaults to `"platform"` (matching `WorkflowSpec::platform_seed`).
pub fn load_workflows(path: impl AsRef<Path>) -> Result<Vec<WorkflowSpec>, SeedLoaderError> {
    load_workflows_with_owning_team(path, "platform")
}

/// Same as [`load_workflows`] but stamps every spec with
/// `owning_team = default_owner` (typically the tenant id, e.g.
/// `"brewery"` or `"used-device-shop"`).
///
/// A DIRECTORY is a bundle too: every `*.toml` file in it, read in
/// filename order, each holding exactly ONE `[[workflow]]` whose
/// `kind` is the file's stem. The platform bundle is shaped this way
/// so that adding a protocol is dropping a file in — two cars adding
/// kinds touch no shared line (CLAUDE.md §9a; the same collapse
/// `infra/postgres/schema/` had). The one-kind-per-file rule is what
/// makes the listing the definition: `ls` answers "which kinds", and
/// a file named for a kind it does not hold is refused, not read.
/// Non-TOML files (a README) are ignored.
pub fn load_workflows_with_owning_team(
    path: impl AsRef<Path>,
    default_owner: &str,
) -> Result<Vec<WorkflowSpec>, SeedLoaderError> {
    let path_ref = path.as_ref();
    if path_ref.is_dir() {
        return load_workflow_dir(path_ref, default_owner);
    }
    load_workflow_file(path_ref, default_owner)
}

fn load_workflow_file(
    path: &Path,
    default_owner: &str,
) -> Result<Vec<WorkflowSpec>, SeedLoaderError> {
    let path_str = path.display().to_string();
    let text = std::fs::read_to_string(path)
        .map_err(|e| SeedLoaderError::Io(path_str.clone(), e.to_string()))?;
    parse_workflows(&text, default_owner, &path_str)
}

/// The kind files of a bundle directory, in the order the loader
/// reads them: every `*.toml` directly inside `dir`, sorted by file
/// name. An empty listing is an error — a bundle directory with no
/// kinds is a wrong path, not an empty bundle.
pub fn bundle_files(dir: &Path) -> Result<Vec<PathBuf>, SeedLoaderError> {
    let dir_str = dir.display().to_string();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| SeedLoaderError::Io(dir_str.clone(), e.to_string()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(SeedLoaderError::Io(
            dir_str,
            "no *.toml kind files in the bundle directory".into(),
        ));
    }
    Ok(files)
}

fn load_workflow_dir(
    dir: &Path,
    default_owner: &str,
) -> Result<Vec<WorkflowSpec>, SeedLoaderError> {
    let mut specs = Vec::new();
    for file in bundle_files(dir)? {
        let file_str = file.display().to_string();
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let loaded = load_workflow_file(&file, default_owner)?;
        let kinds: Vec<&str> = loaded.iter().map(|s| s.kind.as_str()).collect();
        if kinds != [stem.as_str()] {
            return Err(SeedLoaderError::Parse(
                file_str,
                format!(
                    "a kind file holds exactly one [[workflow]] named after the file \
                     (expected kind `{stem}`, found {kinds:?})"
                ),
            ));
        }
        specs.extend(loaded);
    }
    Ok(specs)
}

/// Parse TOML text directly. Useful for inline tests; the file
/// loader is a thin wrapper around this.
///
/// Loaded specs are run through [`crate::workflow_lint::validate_all`]
/// before being returned. Any lint failure (bad enum literal in
/// metadata_defaults, type mismatch, unviable predicate graph,
/// side-effect-bound step with empty defaults) fails the load with
/// [`SeedLoaderError::LintFailed`] — surfaces seed-authoring bugs at
/// startup instead of mid-sim.
pub fn parse_workflows(
    text: &str,
    default_owner: &str,
    source: &str,
) -> Result<Vec<WorkflowSpec>, SeedLoaderError> {
    let file: WorkflowsFile = toml::from_str(text)
        .map_err(|e| SeedLoaderError::Parse(source.to_string(), e.to_string()))?;
    let specs: Vec<WorkflowSpec> = file
        .workflows
        .into_iter()
        .map(|jk| workflow_toml_to_spec(jk, default_owner))
        .collect();

    let registry = crate::step_registry::StepRegistry::v1();
    let lint_errs = crate::workflow_lint::validate_all(&specs, &registry);
    if !lint_errs.is_empty() {
        return Err(SeedLoaderError::LintFailed {
            file: source.to_string(),
            failures: lint_errs.iter().map(|e| format!("  {e}")).collect(),
        });
    }
    Ok(specs)
}

fn workflow_toml_to_spec(toml: WorkflowToml, default_owner: &str) -> WorkflowSpec {
    // Flat steps map straight onto StepSpec — the viability lint
    // (run by parse_workflows) owns every structural concern, so the
    // loader is pure deserialization now.
    let steps: Vec<StepSpec> = toml
        .steps
        .into_iter()
        .map(|s| StepSpec {
            title: s.title,
            kind: s.kind,
            assurance_required: s.assurance_required,
            duration_hours: s.duration_hours,
            labor_hours: s.labor_hours,
            wall_clock_hours: s.wall_clock_hours,
            ready_when: s.ready_when,
            terminal: s.terminal.map(|t| Terminal { outcome: t.outcome }),
            title_template: s.title_template,
            sign_offs_required: s.sign_offs_required,
            fields: s.fields,
            authority_role: s.authority_role,
            claimable: s.claimable,
            metadata_defaults: s.metadata_defaults,
        })
        .collect();

    let mut spec = WorkflowSpec::platform_seed(
        toml.kind,
        toml.label,
        toml.category,
        toml.subject_kinds,
        steps,
    );
    spec.description = toml.description;
    // An absent `metadata` key deserializes to `Value::Null` (serde's
    // default for `serde_json::Value`); keep the `platform_seed` `{}`
    // default in that case so the spec matches the column's
    // DEFAULT '{}' — the same `{}` that metadata_schema / entitlements
    // (never overwritten here) carry.
    if !toml.metadata.is_null() {
        spec.metadata = toml.metadata;
    }
    spec.owning_team = default_owner.to_string();
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_jobkind() {
        let text = r#"
[[workflow]]
kind = "test-job"
label = "Test Job"
category = "production"
subject_kinds = ["location"]

[[workflow.step]]
title = "start"
kind = "task"
ready_when = "true"
title_template = "Step one"

[[workflow.step]]
title = "finish"
kind = "task"
ready_when = "steps.start.done"
title_template = "Step two"
terminal = { outcome = "done" }
"#;
        let specs = parse_workflows(text, "platform", "<test>").unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].kind, "test-job");
        assert_eq!(specs[0].label, "Test Job");
        assert_eq!(specs[0].steps.len(), 2);
        assert_eq!(specs[0].steps[0].title, "start");
        assert_eq!(specs[0].steps[0].ready_when, "true");
        assert_eq!(specs[0].steps[1].title, "finish");
        assert_eq!(
            specs[0].steps[1]
                .terminal
                .as_ref()
                .map(|t| t.outcome.as_str()),
            Some("done")
        );
    }

    #[test]
    fn parses_a_diamond_of_steps() {
        // start → {left, right} → join(terminal). A viable diamond:
        // the DAG is implicit in the `steps.<slug>.done` references,
        // no tier / edge structure in sight.
        let text = r#"
[[workflow]]
kind = "diamond"
label = "Diamond"
category = "production"
subject_kinds = ["account"]

[[workflow.step]]
title = "start"
kind = "task"
ready_when = "true"
title_template = "Trigger"

[[workflow.step]]
title = "left"
kind = "task"
ready_when = "steps.start.done"
title_template = "Left"

[[workflow.step]]
title = "right"
kind = "task"
ready_when = "steps.start.done"
title_template = "Right"

[[workflow.step]]
title = "join"
kind = "task"
ready_when = "steps.left.done AND steps.right.done"
title_template = "Join"
terminal = { outcome = "done" }
"#;
        let specs = parse_workflows(text, "platform", "<test>").unwrap();
        assert_eq!(specs[0].steps.len(), 4);
        let titles: Vec<&str> = specs[0].steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["start", "left", "right", "join"]);
    }

    #[test]
    fn carries_step_metadata_through() {
        let text = r#"
[[workflow]]
kind = "with-metadata"
label = "With Metadata"
category = "production"
subject_kinds = ["location"]

[[workflow.step]]
title = "trigger"
kind = "task"
ready_when = "true"
title_template = "Open"

[[workflow.step]]
title = "mash-in"
kind = "task"
ready_when = "steps.trigger.done"
title_template = "Mash in"
sign_offs_required = ["head-brewer"]
authority_role = "head-brewer"
metadata_defaults = { mash_temp_f = 152, mash_minutes = 60 }
terminal = { outcome = "brewed" }
"#;
        let specs = parse_workflows(text, "brewery", "<test>").unwrap();
        let step = &specs[0].steps[1];
        assert_eq!(step.kind, "task");
        assert_eq!(step.title, "mash-in");
        assert_eq!(step.authority_role.as_deref(), Some("head-brewer"));
        assert_eq!(step.metadata_defaults["mash_temp_f"], 152);
        assert_eq!(step.metadata_defaults["mash_minutes"], 60);
        assert_eq!(specs[0].owning_team, "brewery");
    }

    #[test]
    fn carries_field_filled_by_through_and_defaults_it_to_executor() {
        // The filer-field TOML shape: `filled_by = "filer"` on a
        // `[[workflow.step.fields]]` row marks a field admission
        // validates against the FILER. Absent, the field keeps the
        // executor's required-at-done contract — every seed authored
        // before the key existed parses unchanged.
        let text = r#"
[[workflow]]
kind = "with-filer-field"
label = "With Filer Field"
category = "platform"
subject_kinds = ["custom"]

[[workflow.step]]
title = "start"
kind = "task"
ready_when = "true"
title_template = "Open"

[[workflow.step]]
title = "review"
kind = "task"
ready_when = "steps.start.done"
title_template = "Review it"
terminal = { outcome = "done" }

[[workflow.step.fields]]
name = "markdown"
field_type = "string"
required = true
filled_by = "filer"

[[workflow.step.fields]]
name = "decision"
field_type = "string"
required = true
"#;
        let specs = parse_workflows(text, "platform", "<test>").unwrap();
        let fields = &specs[0].steps[1].fields;
        assert_eq!(fields[0].filled_by, boss_core::job::FilledBy::Filer);
        assert_eq!(
            fields[1].filled_by,
            boss_core::job::FilledBy::Executor,
            "an unmarked field stays executor-filled — required-at-done, unchanged"
        );
    }

    #[test]
    fn carries_step_duration_hours_through() {
        // A step may author its own duration (`duration_hours`) —
        // preferred by executors over the StepType kind's
        // `typical_duration_hours`. The key must flow from seed TOML
        // into `StepSpec.duration_hours`; a step without it stays
        // `None` (kind-default pacing).
        let text = r#"
[[workflow]]
kind = "with-duration"
label = "With Duration"
category = "production"
subject_kinds = ["location"]

[[workflow.step]]
title = "trigger"
kind = "task"
ready_when = "true"
title_template = "Open"

[[workflow.step]]
title = "fermentation-start"
kind = "task"
ready_when = "steps.trigger.done"
title_template = "Ferment"
duration_hours = 168.0
terminal = { outcome = "brewed" }
"#;
        let specs = parse_workflows(text, "brewery", "<test>").unwrap();
        let steps = &specs[0].steps;
        assert_eq!(steps[0].duration_hours, None, "unset key stays None");
        assert_eq!(steps[1].duration_hours, Some(168.0));
    }

    #[test]
    fn carries_jobkind_metadata_through() {
        // A Workflow-level `metadata` inline table (the display/routing
        // hint blob — `surfaces` is the first key) must round-trip from
        // the seed TOML into `WorkflowSpec.metadata`, mirroring how
        // `description` flows through. Proves the seed→spec path for
        // the new column.
        let text = r#"
[[workflow]]
kind = "with-surfaces"
label = "With Surfaces"
category = "production"
subject_kinds = ["location"]
metadata = { surfaces = ["qa"] }

[[workflow.step]]
title = "start"
kind = "task"
ready_when = "true"
title_template = "Open"

[[workflow.step]]
title = "done"
kind = "task"
ready_when = "steps.start.done"
title_template = "Done"
terminal = { outcome = "done" }
"#;
        let specs = parse_workflows(text, "brewery", "<test>").unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].metadata,
            serde_json::json!({ "surfaces": ["qa"] }),
            "Workflow-level metadata must round-trip from TOML into the spec"
        );
    }

    #[test]
    fn jobkind_metadata_defaults_to_empty_object_when_absent() {
        // No `metadata` key in the TOML → the spec carries the
        // `platform_seed` default (`{}`), matching the column's
        // DEFAULT '{}'::jsonb.
        let text = r#"
[[workflow]]
kind = "no-surfaces"
label = "No Surfaces"
category = "production"
subject_kinds = ["location"]

[[workflow.step]]
title = "start"
kind = "task"
ready_when = "true"
title_template = "Open"

[[workflow.step]]
title = "done"
kind = "task"
ready_when = "steps.start.done"
title_template = "Done"
terminal = { outcome = "done" }
"#;
        let specs = parse_workflows(text, "platform", "<test>").unwrap();
        assert_eq!(specs[0].metadata, serde_json::json!({}));
    }

    #[test]
    fn parses_optional_description() {
        let text = r#"
[[workflow]]
kind = "described"
label = "Described"
category = "production"
subject_kinds = ["location"]
description = """
Multi-line description
explaining the job.
"""

[[workflow.step]]
title = "start"
kind = "task"
ready_when = "true"
title_template = "Open"

[[workflow.step]]
title = "done"
kind = "task"
ready_when = "steps.start.done"
title_template = "Done"
terminal = { outcome = "done" }
"#;
        let specs = parse_workflows(text, "platform", "<test>").unwrap();
        assert!(
            specs[0]
                .description
                .as_deref()
                .unwrap()
                .contains("Multi-line description")
        );
    }

    // A bad enum literal in metadata_defaults (channel = "in-person"
    // outside the email|phone|meeting|demo|other enum) must be
    // rejected by the load-time gate up front — with the offending
    // value in the message — not surface mid-run.
    #[test]
    fn rejects_metadata_defaults_with_bad_enum_literal() {
        let text = r#"
[[workflow]]
kind = "bad-enum-seed"
label = "Bad Enum Seed"
category = "production"
subject_kinds = ["account"]

[[workflow.step]]
title = "start"
kind = "task"
ready_when = "true"
title_template = "Open"

[[workflow.step]]
title = "outreach"
kind = "outreach"
ready_when = "steps.start.done"
title_template = "Bad outreach"
metadata_defaults = { channel = "in-person", recipient_id = "x" }
terminal = { outcome = "sent" }
"#;
        let err = parse_workflows(text, "platform", "<test>").unwrap_err();
        let SeedLoaderError::LintFailed { file, failures } = err else {
            panic!("expected LintFailed, got {err:?}");
        };
        assert_eq!(file, "<test>");
        assert!(
            failures
                .iter()
                .any(|f| f.contains("channel") && f.contains("in-person")),
            "failure should name the bad field + value, got: {failures:?}"
        );
    }

    #[test]
    fn rejects_unviable_jobkind_missing_terminal() {
        // No terminal step → the viability lint rejects it at load
        // time. Structure is wholly the lint's concern.
        let text = r#"
[[workflow]]
kind = "no-terminal"
label = "No Terminal"
category = "production"
subject_kinds = ["location"]

[[workflow.step]]
title = "start"
kind = "task"
ready_when = "true"
title_template = "Open"
"#;
        let err = parse_workflows(text, "platform", "<test>").unwrap_err();
        let SeedLoaderError::LintFailed { failures, .. } = err else {
            panic!("expected LintFailed, got {err:?}");
        };
        assert!(
            failures.iter().any(|f| f.contains("no terminal")),
            "expected a 'no terminal' lint failure, got: {failures:?}"
        );
    }

    #[test]
    fn empty_file_returns_empty_list() {
        let specs = parse_workflows("", "platform", "<test>").unwrap();
        assert!(specs.is_empty());
    }

    #[test]
    fn round_trips_brewery_seed_bundle() {
        // Smoke test against the actual brewery file. Belt-and-suspenders
        // that the loader's TOML schema matches what the brewery seed
        // bundle ships.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/brewery/seeds/workflows.toml");
        let specs = load_workflows_with_owning_team(&path, "brewery").unwrap();
        assert!(!specs.is_empty(), "brewery seed bundle should yield kinds");
        let kinds: Vec<&str> = specs.iter().map(|s| s.kind.as_str()).collect();
        // Sanity-check the ones brewery_e2e exercises.
        for required in &["morning-brew", "wholesale-keg-order", "ingredient-restock"] {
            assert!(
                kinds.contains(required),
                "expected `{required}` in brewery Workflows; got {kinds:?}"
            );
        }
        // Every spec carries the brewery owning_team stamp.
        assert!(specs.iter().all(|s| s.owning_team == "brewery"));
        // Fermentation is the headline fidelity case for spec-authored
        // durations: each morning-brew family ferments for days, not
        // one 8h kind-default workday. Pin the seed values here so the
        // seed bundle and the loader cannot drift apart.
        for (kind, hours) in [
            ("morning-brew", 168.0),
            ("morning-brew-ipa", 168.0),
            ("morning-brew-hazy", 144.0),
            ("morning-brew-stout", 240.0),
            ("morning-brew-lager", 336.0),
        ] {
            let spec = specs.iter().find(|s| s.kind == kind).unwrap_or_else(|| {
                panic!("expected `{kind}` in brewery Workflows");
            });
            let ferment = spec
                .steps
                .iter()
                .find(|s| s.title == "fermentation-start")
                .unwrap_or_else(|| panic!("`{kind}` should have a fermentation-start step"));
            assert_eq!(
                ferment.duration_hours,
                Some(hours),
                "`{kind}` fermentation-start should carry an honest duration"
            );
        }
    }

    #[test]
    fn round_trips_used_device_shop_seed_bundle() {
        // Sibling smoke test for the used-device-shop tenant: this
        // file is the data form of the tenant's 10 Workflows.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples/used-device-shop/seeds/workflows.toml");
        let specs = load_workflows_with_owning_team(&path, "used-device-shop").unwrap();
        let kinds: Vec<&str> = specs.iter().map(|s| s.kind.as_str()).collect();
        // Two batches:
        // - The ten Workflows explicitly named in step 4 of the
        //   retirement doc.
        // - The fourteen Workflows lifted from
        //   `crates/boss-jobs/src/seed_kinds.rs` per
        //   `docs/design/platform-vs-tenant-jobkinds.md` so the
        //   tenant TOML carries the full catalog before step 7's
        //   engine swap drops the seed_kinds.rs duplicates.
        for required in &[
            // Step 4 batch
            "device-intake",
            "refurb-used",
            "field-service",
            "sale",
            "support-incident",
            "support-rma",
            "support-sla-renewal",
            "service-agreement",
            "decommission",
            "training-session",
            // Tier-2 batch (lifted from seed_kinds.rs)
            "refurb-oem-new",
            "receiving",
            "shipping",
            "installation",
            "preventive-maintenance-visit",
            "certification",
            "demo",
            "account-onboarding",
            "contract-renewal",
            "collections",
            "vendor-payment",
            "purchase",
            "listing",
            "campaign",
            "marketing-motion",
            "marketing-project",
            "marketing-request",
            "onboarding",
            "offboarding",
            "hiring",
            "vendor-onboarding",
            "vendor-negotiation",
            "rfq",
            "vendor-catalog-refresh",
            "vendor-contract-renewal",
            "ad-hoc",
        ] {
            assert!(
                kinds.contains(required),
                "expected `{required}` in used-device-shop Workflows; got {kinds:?}"
            );
        }
        assert!(specs.iter().all(|s| s.owning_team == "used-device-shop"));
        // Every spec must lint clean against the StepType registry —
        // catches typos in step.kind (would slip past the loader since
        // unknown kinds parse fine but mis-route at runtime) and the
        // side-effect-binding-needs-metadata-defaults contract.
        let registry = crate::step_registry::StepRegistry::v1();
        let errs = crate::workflow_lint::validate_all(&specs, &registry);
        assert!(
            errs.is_empty(),
            "used-device-shop Workflows failed Workflow lint: {errs:#?}"
        );
    }

    /// A directory is a bundle: every `*.toml` in it, in file-name
    /// order regardless of the order they were written, one kind per
    /// file, named for it. Anything that is not a kind file (a README)
    /// is ignored.
    #[test]
    fn a_bundle_directory_loads_its_kind_files_in_name_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("zeta.toml"), viable_row("zeta")).unwrap();
        std::fs::write(dir.path().join("alpha.toml"), viable_row("alpha")).unwrap();
        std::fs::write(dir.path().join("README.md"), "# not a kind file\n").unwrap();
        let specs = load_workflows_with_owning_team(dir.path(), "platform").unwrap();
        let kinds: Vec<&str> = specs.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, vec!["alpha", "zeta"]);
        assert!(specs.iter().all(|s| s.owning_team == "platform"));
        let files = bundle_files(dir.path()).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["alpha.toml", "zeta.toml"]);
    }

    /// The file name IS the kind list. A file named for a kind it does
    /// not hold, or holding two, is refused by name — read wrong, it
    /// would seed a protocol nobody can find by `ls`.
    #[test]
    fn a_kind_file_must_hold_exactly_the_kind_it_is_named_for() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("liar.toml"), viable_row("truth")).unwrap();
        let e = load_workflows(dir.path()).unwrap_err().to_string();
        assert!(e.contains("liar.toml"), "{e}");
        assert!(e.contains("expected kind `liar`"), "{e}");
        assert!(e.contains("[\"truth\"]"), "{e}");

        let dir = tempfile::tempdir().unwrap();
        let two = format!("{}\n{}", viable_row("pair"), viable_row("other"));
        std::fs::write(dir.path().join("pair.toml"), two).unwrap();
        let e = load_workflows(dir.path()).unwrap_err().to_string();
        assert!(
            e.contains("pair.toml") && e.contains("exactly one [[workflow]]"),
            "{e}"
        );
    }

    /// A bundle directory with no kind files is a wrong path, not an
    /// empty bundle — the seed must not report "nothing to do" over a
    /// typo.
    #[test]
    fn an_empty_bundle_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# nothing here\n").unwrap();
        let e = load_workflows(dir.path()).unwrap_err().to_string();
        assert!(e.contains("no *.toml kind files"), "{e}");
    }

    /// A lint failure inside a directory names the FILE, so the author
    /// of a thirty-file bundle is not sent to grep for the row.
    #[test]
    fn a_directory_lint_failure_names_the_kind_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.toml"), viable_row("ok")).unwrap();
        std::fs::write(
            dir.path().join("broken.toml"),
            r#"
[[workflow]]
kind = "broken"
label = "Broken"
category = "platform"
subject_kinds = ["custom"]
[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
"#,
        )
        .unwrap();
        match load_workflows(dir.path()) {
            Err(SeedLoaderError::LintFailed { file, .. }) => {
                assert!(file.ends_with("broken.toml"), "{file}")
            }
            other => panic!("expected a lint failure naming broken.toml, got {other:?}"),
        }
    }

    fn viable_row(kind: &str) -> String {
        format!(
            r#"[[workflow]]
kind = "{kind}"
label = "{kind}"
category = "platform"
subject_kinds = ["custom"]
[[workflow.step]]
title = "opened"
kind = "trigger"
ready_when = "true"
[[workflow.step]]
title = "done"
kind = "outcome"
ready_when = "steps.opened.done"
terminal = {{ outcome = "completed" }}
"#
        )
    }
}
