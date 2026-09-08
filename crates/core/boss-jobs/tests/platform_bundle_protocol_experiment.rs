//! `infra/platform/workflows/protocol-experiment.toml` keeps its decided
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

/// The experiment protocol ships as data, shaped the way the design
/// review decided (network-experiments.md, packet 574c2adf; built for
/// 6ea5a12a). Q3: the experiment IS a packet with `promoted` /
/// `retired` terminal states. Q4: experimenting is IT + platform-admins
/// for now — every acting step wears the platform-admin gate. If
/// someone reshapes the bundle row, this names exactly which decided
/// property broke.
#[test]
fn the_experiment_protocol_keeps_its_decided_shape() {
    // A fresh deployment must be able to run an experiment at all.
    let exp = bundled(boss_jobs::experiments::EXPERIMENT_KIND);

    // Q3: promoted / retired are terminals (inconclusive is the
    // publish-gate-required fallback for the closed verdict set, and
    // abandoned the withdrawal door — both may exist; the two decided
    // terminals must).
    for wanted in ["promoted", "retired"] {
        let step = exp
            .steps
            .iter()
            .find(|s| s.title == wanted)
            .unwrap_or_else(|| panic!("Q3 decided a `{wanted}` terminal; it is gone"));
        assert_eq!(
            step.terminal.as_ref().map(|t| t.outcome.as_str()),
            Some(wanted),
            "`{wanted}` must be a terminal, not a mere step"
        );
    }

    // Q4: every acting (non-terminal) step is platform-admin gated.
    for step in exp.steps.iter().filter(|s| s.terminal.is_none()) {
        assert_eq!(
            step.authority_role.as_deref(),
            Some("platform-admin"),
            "Q4 limits experimenting to IT + platform-admins; step `{}` \
             dropped the gate",
            step.title
        );
    }
}
