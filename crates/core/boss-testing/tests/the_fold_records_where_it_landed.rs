//! The design-doc protocol must record WHERE a settled doc folded, and
//! its header must not claim a behaviour its row cannot implement.
//!
//! MEASURED (backlog aaf85ca2). The header of
//! `infra/platform/workflows/design-doc.toml` said "`publish` is
//! deliberately a plain terminal that records the path". The row
//! declares no field for one: `published` is `kind = "outcome"` with
//! `metadata_defaults` and a `terminal` and nothing else. So on design
//! a5368918 — answered and folded by David on 2026-09-19 — the
//! published step completed carrying only `outcome_kind`, and the only
//! thing tying that design to `docs/architecture-decisions.md` was a
//! car's commit message.
//!
//! WHY THE OBVIOUS FIX IS WRONG, and why this test pins the shape
//! rather than the field. The packet proposed putting the field on the
//! terminal: "a terminal that records the path must take it AT the
//! terminal." `published` has no `authority_role` and its `ready_when`
//! is `steps.fold.done`, so it goes ready and completes on its own. A
//! REQUIRED field there would stall every design at its terminal; an
//! OPTIONAL one would be filled by nobody. The single outcome step in
//! the whole registry that does carry a field —
//! `protocol-experiment.toml`'s `abandoned` — works only because it is
//! gated behind an explicit `job.metadata.abandoned` flag and a
//! platform-admin role, which is a human act, not an auto-complete.
//!
//! So the record belongs on `fold`: a task, completed by the actor who
//! does the folding, in the same act. That actor is the one moment the
//! destination is BOTH known and being decided — the filing-time
//! `--doc-path` cannot know it, because a design doc is residue written
//! at the END of a discussion.

use boss_testing::repo_root;

fn design_doc_toml() -> String {
    std::fs::read_to_string(repo_root().join("infra/platform/workflows/design-doc.toml"))
        .expect("the design-doc workflow is readable")
}

/// The block of one `[[workflow.step]]` by its title, up to the next step.
fn step_block(toml: &str, title: &str) -> String {
    let needle = format!("title = \"{title}\"");
    let from = toml
        .find(&needle)
        .unwrap_or_else(|| panic!("design-doc.toml declares a step titled {title}"));
    let rest = &toml[from..];
    let end = rest.find("\n[[workflow.step]]").unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn the_fold_records_where_the_settled_material_landed() {
    let block = step_block(&design_doc_toml(), "fold");
    assert!(
        block.contains("name = \"folded_into\""),
        "the fold step must carry where it landed — without it the only \
         tie between a design and the file its decision reached is a \
         car's commit message (aaf85ca2)"
    );
    assert!(
        block.contains("name = \"fold_change\""),
        "and it still records WHAT changed; the new field is beside that, not instead of it"
    );
}

#[test]
fn the_published_terminal_still_declares_no_fields() {
    // The shape this car deliberately did NOT adopt. `published`
    // auto-completes on `steps.fold.done`, so a field here is either a
    // stall (required) or dead (optional). If a later change adds one,
    // it must also give the step a role and a gate the way
    // protocol-experiment's `abandoned` has, and this test should fail
    // loudly rather than let it through quietly.
    let block = step_block(&design_doc_toml(), "published");
    assert!(
        !block.contains("[[workflow.step.fields]]"),
        "published is an auto-completing outcome: a required field stalls \
         every design at its terminal and an optional one is filled by \
         nobody. Fields on it need a role and an explicit gate first."
    );
    assert!(
        block.contains("ready_when = \"steps.fold.done\""),
        "the reasoning above depends on published completing off fold, \
         so pin that rather than assume it"
    );
}

#[test]
fn the_header_does_not_claim_the_terminal_records_a_path() {
    // The a2358e7c class: a workflow header asserting a behaviour the
    // row does not implement. The header is what a reader trusts when
    // deciding where to put something.
    let toml = design_doc_toml();
    assert!(
        !toml.contains("terminal that records the path"),
        "the header claimed the terminal records the path; the row has no \
         field for one and cannot grow one without becoming a human step"
    );
}
