//! `infra/platform/workflows/publish-to-github.toml` keeps its decided
//! shape. One pin file per kind file (see `platform_bundle.rs`), so a
//! new protocol touches no shared line.

use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// `publish-to-github` v6 ships in the bundle shaped the way design
/// packet 7b59af2c decided (David, 2026-09-08); this names which decided
/// property broke if someone reshapes it.
///
/// (1) The `superseded` terminal is gated on marker PRESENCE. v5 wrote
///     `job.metadata.superseded_by != ""`, which is TRUE over an absent
///     marker (boss-expr: Absent is unequal to every literal), so every
///     fresh Job closed as superseded the instant it opened — the boot
///     guard retired v5 for exactly that on 2026-09-07. The idiom is
///     `> ""`: false for absent, false for empty, true only when set.
/// (2) `open-pr` is a MACHINE step: nobody is nominated for it
///     (`authority_role` None, the gate-run `record-verdict` precedent)
///     and it carries the `ops_verb` marker the dispatcher rule
///     `publish-github-pr-on-open-pr-ready` routes on — the forge
///     ops-runner runs the verb and completes the step with `pr_url`.
/// The rest is v5: measure → nothing-to-publish | review → approve →
/// open-pr → pr-opened | declined, plus superseded.
#[test]
fn publish_to_github_v6_keeps_its_decided_shape() {
    // The daily rule spawns it, so a fresh deployment must carry it.
    let wf = bundled("publish-to-github");

    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("publish-to-github has no `{title}` step"))
    };

    // (1) presence, not inequality.
    let superseded = step("superseded");
    assert_eq!(
        superseded.terminal.as_ref().map(|t| t.outcome.as_str()),
        Some("superseded")
    );
    for marker in ["superseded_by", "supersession_translation"] {
        assert!(
            superseded
                .ready_when
                .contains(&format!("job.metadata.{marker} > \"\"")),
            "superseded must test `job.metadata.{marker} > \"\"` (present and non-empty); got `{}`",
            superseded.ready_when
        );
        assert!(
            !superseded
                .ready_when
                .contains(&format!("job.metadata.{marker} != \"\"")),
            "the v5 footgun `!= \"\"` is back on `{marker}` — it reads true over an absent marker"
        );
    }

    // (2) a machine step: nobody nominated, the routing marker set.
    let open_pr = step("open-pr");
    assert_eq!(
        open_pr.authority_role, None,
        "open-pr is run by the forge ops-runner, not nominated to a person"
    );
    assert_eq!(
        open_pr
            .metadata_defaults
            .get("ops_verb")
            .and_then(|v| v.as_str()),
        Some("publish-github-pr"),
        "open-pr carries the ops_verb marker the dispatcher rule routes on"
    );
    assert!(
        open_pr
            .fields
            .iter()
            .any(|f| f.name == "pr_url" && f.required),
        "open-pr still requires pr_url at done — the machine records where the PR is"
    );

    // The terminal set is v5's.
    let mut terminals: Vec<&str> = wf
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    terminals.sort_unstable();
    assert_eq!(
        terminals,
        vec!["declined", "nothing-to-publish", "pr-opened", "superseded"]
    );
}
