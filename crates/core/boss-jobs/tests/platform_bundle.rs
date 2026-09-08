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

/// The design-doc protocol states its own requirement (packet
/// 2e136a67). The one field the whole protocol exists to process —
/// `questions` — is a FILER field on the review step, bound from the
/// Job, and it declares the element shape the tracker renders:
/// `{anchor, title, proposal}`. Before this the registry said nothing
/// about questions at all, and eight title-less drafts in one session
/// were admitted silently and dead-ended at "nothing to review".
#[test]
fn the_design_doc_protocol_declares_its_questions() {
    use boss_core::job::FilledBy;
    let rows = bundle();
    let doc = rows
        .iter()
        .find(|w| w.kind == "design-doc")
        .expect("design-doc is in the platform bundle");
    let review = doc
        .steps
        .iter()
        .find(|s| s.title == "review")
        .expect("design-doc has a review step");
    let questions = review
        .fields
        .iter()
        .find(|f| f.name == "questions")
        .expect("the review step declares `questions`");
    assert!(questions.required, "questions is required");
    assert_eq!(
        questions.filled_by,
        FilledBy::Filer,
        "questions is owed by the filer at admission, not by the reviewer at done"
    );
    assert_eq!(questions.field_type, "array");
    assert_eq!(
        questions.item_keys,
        vec!["anchor", "title", "proposal"],
        "each question carries the three keys the tracker renders"
    );
    assert_eq!(
        review.metadata_defaults.get("questions"),
        Some(&serde_json::json!("{metadata.questions}")),
        "the step reads its questions off the Job — the whole-value binding"
    );
    // The live row (v4, 2026-09-05) carries a fold step; the bundle
    // must not fall behind the registry it seeds.
    let titles: Vec<&str> = doc.steps.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["drafted", "review", "fold", "published"]);
}

/// A maintenance packet ends in its verdict. Until 2026-09-05 every
/// maintenance kind had one terminal, reached on `steps.run.done`
/// regardless of how the run went — and the unit only completed the
/// run step from ExecStartPost, which systemd skips on failure. So a
/// failed run either sat open looking like a run in progress
/// (disk-floor-sweep, 16:10, two FLOOR UNMET runs) or was closed "ok"
/// by the next run's recovery (forge-converge, 17:39, exit 1). Now the
/// run step's `result` routes: `ok` completes, anything else fails —
/// in the bundle AND in the three kinds still compiled into
/// platform_workflows(), which are one contract.
#[test]
fn every_maintenance_kind_ends_in_its_verdict() {
    let mut kinds: Vec<WorkflowSpec> = bundle()
        .into_iter()
        .filter(|w| w.kind.starts_with("maintenance-"))
        .collect();
    kinds.extend(
        boss_jobs::registry::platform_workflows()
            .into_iter()
            .filter(|w| w.kind.starts_with("maintenance-")),
    );
    assert!(
        kinds.len() >= 19,
        "expected the 16 bundled + 3 compiled maintenance kinds"
    );
    for w in kinds {
        let terminals: Vec<(&str, &str)> = w
            .steps
            .iter()
            .filter_map(|s| {
                s.terminal
                    .as_ref()
                    .map(|t| (t.outcome.as_str(), s.ready_when.as_str()))
            })
            .collect();
        assert_eq!(
            terminals.iter().map(|(o, _)| *o).collect::<Vec<_>>(),
            vec!["completed", "failed"],
            "{}: a maintenance packet ends in completed or failed, nothing else",
            w.kind
        );
        assert!(
            terminals[0]
                .1
                .contains("steps.run.metadata.result = \"ok\""),
            "{}: completed must require the run's result to be ok",
            w.kind
        );
        assert!(
            terminals[1]
                .1
                .contains("steps.run.metadata.result != \"ok\""),
            "{}: failed must be every other result",
            w.kind
        );
    }
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
    let rows = bundle();
    let wf = rows
        .iter()
        .find(|w| w.kind == "publish-to-github")
        .expect("publish-to-github ships in the platform bundle — the daily rule spawns it");

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

/// A fresh incident-post-mortem does not supersede itself (backlog-item
/// cb9661fe). v1's `superseded` terminal read `superseded_by != ""`,
/// and a missing metadata path is `Absent` — unequal to every literal —
/// so the terminal was ready the instant a packet opened and the
/// dispatcher closed it before a step could be filled. The bundle row
/// gates on a PRESENT, NON-EMPTY marker: false while it is absent or
/// empty, true only once someone names the packet that replaces this
/// one. Evaluated with the same engine the dispatcher runs, over the
/// payload a freshly-opened Job presents (the trigger done, nothing
/// else, no job metadata).
#[test]
fn a_fresh_incident_post_mortem_does_not_supersede_itself() {
    use boss_expr::{Context, NoHelpers, Value, eval, parse};
    let rows = bundle();
    let ipm = rows
        .iter()
        .find(|w| w.kind == "incident-post-mortem")
        .expect("incident-post-mortem ships in the platform bundle");
    let superseded = ipm
        .steps
        .iter()
        .find(|s| s.title == "superseded")
        .expect("a `superseded` terminal");
    assert_eq!(
        superseded.terminal.as_ref().map(|t| t.outcome.as_str()),
        Some("superseded")
    );
    let predicate = parse(&superseded.ready_when).expect("the predicate parses");
    let fires = |job_metadata: serde_json::Value| {
        let payload = serde_json::json!({
            "subject": {},
            "job": { "metadata": job_metadata },
            "steps": { "opened": { "done": true, "metadata": {} } },
        });
        eval(
            &predicate,
            &Context {
                payload: &payload,
                helpers: &NoHelpers,
            },
        )
    };
    assert_eq!(
        fires(serde_json::json!({})),
        Ok(Value::Bool(false)),
        "a packet opened with no `superseded_by` must stay open"
    );
    assert_eq!(
        fires(serde_json::json!({ "superseded_by": "" })),
        Ok(Value::Bool(false)),
        "an empty marker is no marker"
    );
    assert_eq!(
        fires(serde_json::json!({ "superseded_by": "9bfd0b89" })),
        Ok(Value::Bool(true)),
        "naming the superseding packet is what closes this one"
    );
}
