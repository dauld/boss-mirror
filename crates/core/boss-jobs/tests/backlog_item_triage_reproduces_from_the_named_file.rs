//! The `triage` step of `backlog-item` carries the rule an absence
//! verdict needs, on the step that makes the verdict (backlog
//! fac521bc, measured 2026-09-21 and disproved 2026-09-22).
//!
//! WHAT HAPPENED. Packet c68104cf reported that a historical note in a
//! lint comment trips `api-path-bypass-smell`. Its triage ran the real
//! lint against two tracked fixtures — a plain shell comment carrying
//! the reported phrase, and the packet's exact backticked wording —
//! got CLEAN from both, added a third fixture carrying a genuine
//! `psql -c "INSERT INTO ..."` as a CONTROL, confirmed the lint still
//! caught that one, and concluded it does not reproduce. Every part of
//! that is the method this repo asks for, and it was still wrong: the
//! reported line was never a plain shell comment. It sat inside a
//! `python3 - <<'PY'` heredoc in
//! `infra/lint/a-verb-declares-the-hosts-it-serves.sh`, and a heredoc
//! body is classified off its OPENER — python3 is not a text tool, so
//! the lexer rule that breaks code at an unquoted hash never applies.
//! Planted in THAT shape it reproduces on the first run. The file was
//! named in the packet's own related-car reference, one `git show`
//! away.
//!
//! WHY A CONTROL COULD NOT CATCH IT. A control proves the fixture is
//! STRONG enough to catch something. It says nothing about whether the
//! fixture is the right SHAPE. Two different failure modes, and the
//! method this repo teaches covers only the first — which matters
//! because the control is the part everyone remembers to add; it is in
//! the testing-strategy anti-patterns, and it is not sufficient for an
//! absence claim.
//!
//! WHY IT LIVES HERE. Had that triage routed `stale`, a real defect
//! would have closed carrying a well-evidenced, controlled,
//! reproducible-LOOKING argument — a worse record than no argument,
//! because the next reader trusts it. That is the system contributing
//! error of its own, which the correctness protocol exists to forbid.
//! A paragraph in a doc is read by whoever goes looking; a `procedure`
//! on the step is read by whoever is doing the triage, at the moment
//! they do it, and is versioned with the protocol (CLAUDE.md §9).

use boss_core::job::{JobId, StepId, Subject};
use boss_jobs::registry::{StepSpec, WorkflowSpec, materialize_steps, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;
use serde_json::json;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

fn step<'a>(spec: &'a WorkflowSpec, title: &str) -> &'a StepSpec {
    spec.steps
        .iter()
        .find(|s| s.title == title)
        .unwrap_or_else(|| panic!("backlog-item has a `{title}` step"))
}

fn triage_procedure() -> String {
    step(&bundled("backlog-item"), "triage").metadata_defaults["procedure"]
        .as_str()
        .expect("the triage step carries a procedure")
        .to_string()
}

/// Half one: reproduce from the FILE the packet names, not from its
/// prose. The prose is a report of the site; only the site is the
/// site — so the procedure has to name the act that fetches it.
#[test]
fn the_triage_procedure_sends_the_reader_to_the_file_the_packet_names() {
    let procedure = triage_procedure();
    assert!(
        procedure.contains("git show"),
        "the procedure names the read that gets the real file: {procedure}"
    );
    assert!(
        procedure.contains("heredoc"),
        "the procedure keeps the worked example that makes the rule concrete: {procedure}"
    );
}

/// Half two, and the more general one: an absence verdict states the
/// SHAPE it tested, and the procedure says why a control does not
/// substitute for that. Strip either word and the distinction the
/// packet paid for is gone, so both are pinned.
#[test]
fn the_triage_procedure_keeps_the_control_versus_shape_distinction() {
    let procedure = triage_procedure();
    for anchor in ["control", "shape"] {
        assert!(
            procedure.contains(anchor),
            "the procedure must keep `{anchor}` — a control proves strength, not shape: {procedure}"
        );
    }
}

/// The reason this is a registry row and not a doc: materialised, the
/// rule is ON the step the triager opens. A doc is read by whoever
/// goes looking; this is read by whoever is doing the work.
#[test]
fn a_materialised_backlog_item_carries_the_rule_on_its_triage_step() {
    let wf = bundled("backlog-item");
    let steps = materialize_steps(
        &wf,
        &Subject::new("custom", "bosspipeline"),
        JobId::new(),
        &json!({ "area": "platform" }),
        StepId::new,
    );
    let triage = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("triage"))
        .expect("`triage` materialised");
    let procedure = triage.metadata["procedure"]
        .as_str()
        .expect("the materialised triage step carries the procedure");
    assert_eq!(
        procedure,
        triage_procedure(),
        "the step the triager opens carries the registry's own words"
    );
}
