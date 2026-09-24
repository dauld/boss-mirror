//! `infra/platform/workflows/correct-the-record.toml` keeps its decided
//! shape. One pin file per kind file (see `platform_bundle.rs`), so a
//! new protocol touches no shared line.

use boss_jobs::corrections::TERMINAL_STEP_HINT;
use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// The one spelling of the door, as every refusal of a write to a
/// terminal step names it. Read OUT of the hint rather than typed here,
/// so the protocol's procedure and the API's 409 cannot name two
/// different verbs (CLAUDE.md §9a).
fn the_door() -> &'static str {
    let start = TERMINAL_STEP_HINT
        .find("`boss correct ")
        .expect("the terminal-step hint names `boss correct`");
    let rest = &TERMINAL_STEP_HINT[start + 1..];
    let end = rest
        .find('`')
        .expect("the hint closes the verb's backticks");
    &rest[..end]
}

/// The `applied` step lands an accepted correction THROUGH THE DOOR
/// (design 4105b020, the plan's later item on backlog 56727f95). Until
/// then it asked for `landed`, a free-text string: the correction it
/// recorded was a sentence of the author's own, with nothing tying it
/// to the step it corrected — the very shape the design retired, and
/// the step API refuses the write this step used to ask for anyway
/// (a completed step is never rewritten). Its evidence is now what the
/// door answered, which names the packet, the step and the index.
#[test]
fn the_applied_step_lands_the_correction_through_the_door() {
    let ctr = bundled("correct-the-record");
    let applied = ctr
        .steps
        .iter()
        .find(|s| s.title == "applied")
        .expect("an applied step");

    let procedure = applied
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("the applied step carries its runbook as `procedure`");
    let door = the_door();
    assert!(
        procedure.contains(door),
        "the procedure must name the door exactly as the API's refusal does ({door:?}); \
         it reads: {procedure}"
    );
    assert!(
        procedure.contains("-file"),
        "the prose a correction carries already died once in argv (2376b89e) — the \
         procedure names the -file twins"
    );

    let names: Vec<&str> = applied.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(
        !names.contains(&"landed"),
        "`landed` was free text with no tie to what it corrected; it is gone: {names:?}"
    );
    let correction = applied
        .fields
        .iter()
        .find(|f| f.name == "correction")
        .unwrap_or_else(|| panic!("the applied step records `correction`: {names:?}"));
    assert!(
        correction.required,
        "an applied step with no door answer is a belief, not a record"
    );
}
