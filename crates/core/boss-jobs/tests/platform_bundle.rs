//! The platform Workflow bundle says exactly what the code says.
//!
//! This is protocols-as-data Q4 made mechanical. David's answer: "moving
//! `user-feedback` v10 from code to bundle must produce a row identical
//! to the live v10 — not v11. If the loader publishes a new version
//! instead of recognising the existing one, every in-flight packet keeps
//! its old spec and the board grows a second lineage."
//!
//! The field-by-field comparison against the shipped spec lives in
//! `registry.rs`'s own test module, because the builders it compares
//! against are private and stay that way. This file keeps the half that
//! needs no privileged access.
//!
//! The comparison has to run in this direction. `WorkflowSpec` does NOT
//! serialize to TOML — TOML has no null, so the first `None` field fails
//! with `UnsupportedType(unit)` — so the bundle cannot be generated from
//! the code and diffed. It is authored, and this test is what makes that
//! safe: it parses the bundle with the same reader the tenant bundles
//! use and asserts each row equals the spec it is replacing, field for
//! field, including every step.

use boss_jobs::registry::WorkflowSpec;
use boss_jobs::seed_loader::load_workflows;

const BUNDLE: &str = "../../../infra/platform/workflows.toml";

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(BUNDLE).expect("the platform bundle parses")
}

/// Every row in the bundle passes the same viability gate a publish
/// runs, so a malformed bundle fails here rather than at boot on the
/// deployment that loaded it.
#[test]
fn every_bundled_workflow_is_viable() {
    let reg = boss_jobs::step_registry::StepRegistry::v1();
    for row in bundle() {
        let problems = boss_jobs::workflow_lint::validate_workflow(&row, &reg);
        assert!(
            problems.is_empty(),
            "{} is not viable: {problems:?}",
            row.kind
        );
    }
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
    let rows = bundle();
    let exp = rows
        .iter()
        .find(|w| w.kind == boss_jobs::experiments::EXPERIMENT_KIND)
        .expect(
            "protocol-experiment ships in the platform bundle — a fresh \
                 deployment must be able to run an experiment at all",
        );

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

/// The rotation protocol ships as data, shaped the way David's review
/// of the maiden rotation decided (packet 7ee101aa; v3). The broker
/// handler (`credential.rotate.forgejo`) completes the machine steps
/// by SLUG and the events by phase — this test pins the spec side of
/// that contract so a bundle reshape cannot silently strand the
/// handler.
#[test]
fn the_rotation_protocol_keeps_its_decided_shape() {
    let rows = bundle();
    let rot = rows
        .iter()
        .find(|w| w.kind == "rotate-a-credential")
        .expect("rotate-a-credential ships in the platform bundle (v3 review decision)");

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

/// The emergency lane keeps its decided shape (packet c6c0f3b1). The
/// one trust decision — bypassing the train — is a sign-off owed by a
/// platform-admin; the merge cannot become ready without an approved
/// decision; the rollback lever is tried first and only `no-good-
/// revision` opens the lane; the filer must state the outage; and the
/// packet closes only with a retro filed. If any of these bends, the
/// lane is back to being improvised.
#[test]
fn the_emergency_merge_keeps_its_decided_shape() {
    use boss_core::job::FilledBy;
    let rows = bundle();
    let lane = rows
        .iter()
        .find(|w| w.kind == "emergency-merge")
        .expect("emergency-merge is in the platform bundle");
    let step = |title: &str| {
        lane.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("emergency-merge has a `{title}` step"))
    };
    let outage = step("undo-first")
        .fields
        .iter()
        .find(|f| f.name == "outage")
        .expect("the filer states the outage");
    assert!(outage.required && outage.filled_by == FilledBy::Filer);
    assert!(
        step("gate-locally")
            .ready_when
            .contains("undo_result = \"no-good-revision\""),
        "only a failed rollback lever opens the merge lane"
    );
    let approve = step("approve");
    assert_eq!(approve.kind, "sign-off");
    assert_eq!(approve.sign_offs_required, vec!["platform-admin"]);
    assert!(
        step("merge").ready_when.contains("decision = \"approved\""),
        "nothing merges without the approved decision"
    );
    assert!(
        step("gate-locally")
            .fields
            .iter()
            .any(|f| f.name == "receipt_sha" && f.required),
        "the approver signs against a receipt sha, not a claim"
    );
    assert!(
        step("merged").ready_when.contains("steps.retro.done"),
        "the lane closes only with its retro filed"
    );
    let terminals: Vec<&str> = lane
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    assert_eq!(terminals, vec!["rolled-back", "refused", "merged"]);
}
