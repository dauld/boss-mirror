//! `infra/platform/workflows/rotate-a-credential.toml` keeps its decided
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

/// The rotation protocol ships as data, shaped the way David's review
/// of the maiden rotation decided (packet 7ee101aa; v3). The broker
/// handler (`credential.rotate.forgejo`) completes the machine steps
/// by SLUG and the events by phase — this test pins the spec side of
/// that contract so a bundle reshape cannot silently strand the
/// handler.
#[test]
fn the_rotation_protocol_keeps_its_decided_shape() {
    let rot = bundled("rotate-a-credential");

    // The scope step is the dedicated StepType the broker rule
    // targets, and v3 surfaces `old_token` inline — optional, so a
    // scoper who cannot name the kill target leaves the revoke to a
    // human; NEVER required, because forcing it would invite guesses.
    let scope = rot
        .steps
        .iter()
        .find(|s| s.title == "scope")
        .expect("a scope step");
    assert_eq!(scope.kind, "credential-rotation");
    let old_token = scope
        .fields
        .iter()
        .find(|f| f.name == "old_token")
        .expect("v3 surfaces old_token inline on the scope step");
    assert!(!old_token.required, "old_token is optional by decision");

    // The four machine slugs the broker completes, in protocol order —
    // the destructive one LAST.
    for (slug, depends_on) in [
        ("issue", "scope"),
        ("install", "issue"),
        ("verify", "install"),
        ("revoke", "verify"),
    ] {
        let step = rot
            .steps
            .iter()
            .find(|s| s.title == slug)
            .unwrap_or_else(|| panic!("the broker completes a `{slug}` step; it is gone"));
        assert!(
            step.ready_when
                .contains(&format!("steps.{depends_on}.done")),
            "`{slug}` must gate on `{depends_on}` — order is the protocol"
        );
    }

    // v3's revoke runbook: the human path when no old_token was named.
    let revoke = rot.steps.iter().find(|s| s.title == "revoke").unwrap();
    let procedure = revoke
        .metadata_defaults
        .get("procedure")
        .and_then(|v| v.as_str())
        .expect("v3 puts the human runbook on the revoke step as `procedure`");
    assert!(
        procedure.contains("issue step"),
        "the runbook identifies the old token via the issue step's recorded name"
    );
    assert!(
        procedure.contains("boss-credential-broker-root"),
        "the runbook must warn off the broker's root credential"
    );
}
