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

    // Design 1c90d183 (David, 2026-09-26): the scoper may name the old
    // token by its LAST EIGHT, computed on the host that holds it — also
    // optional, for the same reason as old_token.
    let by_last8 = scope
        .fields
        .iter()
        .find(|f| f.name == "old_token_last_eight")
        .expect("the scope step surfaces old_token_last_eight inline");
    assert!(!by_last8.required, "old_token_last_eight is optional");

    // An OFF-HOST credential is delivered by its host after the install,
    // and the revoke waits for that record. The `delivered` step is its
    // own kind so the broker's second rule targets exactly it; it goes
    // ready only for a rotation whose install said `off-host` (the
    // handler writes `delivery` on every install), so a mount-delivered
    // rotation never shows a step nobody will complete.
    let delivered = rot
        .steps
        .iter()
        .find(|s| s.title == "delivered")
        .expect("a delivered step for off-host credentials");
    assert_eq!(delivered.kind, "credential-delivery");
    assert!(
        delivered.ready_when.contains("steps.install.done")
            && delivered
                .ready_when
                .contains("steps.install.metadata.delivery = \"off-host\""),
        "delivered goes ready after an off-host install and only then: {}",
        delivered.ready_when
    );
    let evidence = delivered
        .fields
        .iter()
        .find(|f| f.name == "delivered_last_eight")
        .expect("the host records the last eight it installed");
    assert!(
        evidence.required,
        "a delivery without its evidence is a claim, not a record"
    );
    let revoke = rot.steps.iter().find(|s| s.title == "revoke").unwrap();
    assert!(
        revoke.ready_when.contains("steps.delivered.done")
            && revoke
                .ready_when
                .contains("steps.install.metadata.delivery != \"off-host\""),
        "an off-host revoke waits for delivery; a mount one keeps today's order: {}",
        revoke.ready_when
    );

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
