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

/// v7 (backlog 321f1409, David 2026-09-19) reads the mirror's checks
/// back before the packet closes. Measured: PR #238 was merged over 64
/// unread CodeQL alerts (18 critical) on 2026-09-12 and #239 stalled on
/// 109, because v6 closed on `pr-opened` the moment the PR opened and
/// the scan ran afterwards on a surface only David sees. This names
/// which decided property broke if someone reshapes it:
///
/// (1) `read-checks` is a MACHINE step in the open-pr shape: nobody
///     nominated, the `ops_verb` marker the rule
///     `read-publish-checks-on-read-checks-ready` routes on, ready on
///     open-pr done, and the three fields the verb writes and the
///     answer rule copies.
/// (2) `judge-checks` is the agent's, at the platform operator's queue,
///     ready only when the scan did not conclude `success` — guarded
///     by `read-checks.done` so the `!=` never reads over Absent — and
///     it requires a disposition per rule plus the verdict enum.
/// (3) `pr-opened` follows the judgement, or the reading alone when the
///     scan was clean; never open-pr alone, which is the v6 defect.
#[test]
fn publish_to_github_v7_reads_the_checks_back_and_judges_them_before_closing() {
    let wf = bundled("publish-to-github");
    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("publish-to-github has no `{title}` step"))
    };

    // (1) the reading is a machine step.
    let read = step("read-checks");
    assert_eq!(read.ready_when, "steps.open-pr.done");
    assert_eq!(
        read.authority_role, None,
        "read-checks is the forge's, not a person's"
    );
    assert_eq!(
        read.metadata_defaults
            .get("ops_verb")
            .and_then(|v| v.as_str()),
        Some("read-publish-checks")
    );
    for name in ["conclusion", "alerts", "rules"] {
        assert!(
            read.fields.iter().any(|f| f.name == name && f.required),
            "read-checks requires `{name}` at done"
        );
    }

    // (2) the judgement is the agent's, and only when there is one.
    let judge = step("judge-checks");
    assert!(
        judge.ready_when.starts_with("steps.read-checks.done AND"),
        "judge-checks must be guarded by read-checks.done before any `!=`: {}",
        judge.ready_when
    );
    assert!(
        judge
            .ready_when
            .contains("steps.read-checks.metadata.conclusion != \"success\""),
        "judge-checks is for a scan that did not pass: {}",
        judge.ready_when
    );
    assert_eq!(
        judge.agent.as_ref().map(|a| a.profile.as_str()),
        Some("analyst"),
        "judge-checks declares the analyst block"
    );
    let dispositions = judge
        .fields
        .iter()
        .find(|f| f.name == "dispositions")
        .expect("judge-checks requires `dispositions`");
    assert!(dispositions.required);
    assert_eq!(dispositions.field_type, "array");
    assert_eq!(
        dispositions.item_keys,
        vec!["rule", "disposition", "reason"]
    );
    let verdict = judge
        .fields
        .iter()
        .find(|f| f.name == "verdict")
        .expect("judge-checks requires `verdict`");
    assert!(verdict.required);
    assert_eq!(verdict.field_type, "clean|noise|real");

    // (3) the terminal never closes over an unread or unjudged scan.
    let opened = step("pr-opened");
    assert_eq!(
        opened.ready_when,
        "steps.judge-checks.done OR (steps.read-checks.done AND steps.read-checks.metadata.conclusion = \"success\")"
    );
    assert!(
        !opened.ready_when.contains("steps.open-pr.done"),
        "v6's terminal — open-pr done closes the packet — is back: {}",
        opened.ready_when
    );
}

/// v8 (backlog d4bfe548, David 2026-09-24: "Sounds good"). Measured that
/// day on publish e0558b28: opened 00:00Z, the machine refreshed its
/// drift at 00:01Z, and its `measure` checklist — `platform-admin`, no
/// agent block — sat unworked until the operator did it after 03:50Z,
/// when David asked. No actor was ever dispatched to it, because nothing
/// on the step said an agent could run it. This names which decided
/// property broke if someone reshapes it:
///
/// (1) `measure` and `review` declare the analyst block — the same
///     setting as judge-checks — and say so the way every agent-workable
///     step does: `human_only = false` and a procedure.
/// (2) The procedure is a RE-MEASUREMENT with the protocol's own
///     instrument, not a transcription of the machine's `drift_refresh`
///     (a has_drift of "false" closes the packet, so it must be the
///     script's own answer), and the review READS each newly public file.
/// (3) `approve` stays David's sign-off: no agent block on it.
#[test]
fn publish_to_github_v8_hands_measure_and_review_to_an_agent_and_keeps_approve_davids() {
    let wf = bundled("publish-to-github");
    let step = |title: &str| {
        wf.steps
            .iter()
            .find(|s| s.title == title)
            .unwrap_or_else(|| panic!("publish-to-github has no `{title}` step"))
    };

    for title in ["measure", "review"] {
        let s = step(title);
        let agent = s.agent.as_ref().unwrap_or_else(|| {
            panic!("`{title}` declares no agent block, so no agent is dispatched to it")
        });
        assert_eq!(agent.profile, "analyst", "{title}");
        assert_eq!(
            s.metadata_defaults.get("human_only"),
            Some(&serde_json::json!(false)),
            "`{title}` must declare itself agent-workable"
        );
        assert!(
            s.metadata_defaults
                .get("procedure")
                .and_then(|v| v.as_str())
                .is_some_and(|p| !p.trim().is_empty()),
            "`{title}` carries no procedure — the prompt an agent runs is the step's own"
        );
    }
    let procedure = |title: &str| {
        step(title).metadata_defaults["procedure"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        procedure("measure").contains("infra/prep-github-publish.sh --json"),
        "measure re-measures with the protocol's own instrument: {}",
        procedure("measure")
    );
    assert!(
        procedure("review").contains("newly_public_files"),
        "review reads each newly public file the measurement named: {}",
        procedure("review")
    );

    let approve = step("approve");
    assert!(
        approve.agent.is_none(),
        "approve is David's sign-off and must declare no agent block"
    );
    assert_eq!(approve.authority_role.as_deref(), Some("platform-admin"));
}

/// v9 (backlog c6cb678b, 2026-09-24). PR #243 closed `pr-opened` with
/// its Gate red: the reading had recorded the Gate in_progress and the
/// judge judged CodeQL's rules alone. The verb now completes
/// `read-checks` only on every check and reads `failure` for a failing
/// Gate over a clean scan — which the `!= "success"` guard already
/// routes to the judge. What this row must add is that the judge JUDGES
/// it: a disposition list covering only the scan's rules would close a
/// red Gate as noise by omission. So the procedure names the reading's
/// `failing` list and asks one disposition per failing check.
#[test]
fn publish_to_github_v9_judges_every_failing_check_not_only_the_scan() {
    let wf = bundled("publish-to-github");
    let judge = wf
        .steps
        .iter()
        .find(|s| s.title == "judge-checks")
        .expect("publish-to-github has a judge-checks step");
    let procedure = judge.metadata_defaults["procedure"]
        .as_str()
        .unwrap_or_default();
    assert!(
        procedure.contains("code_scanning.failing"),
        "the judge reads the failing checks off the reading: {procedure}"
    );
    assert!(
        procedure.contains("EVERY FAILING CHECK IS JUDGED"),
        "the judge disposes of each failing check, not only the scan's rules: {procedure}"
    );
    assert!(
        procedure.contains("unfinished"),
        "a check that outlasted the ceiling is judged too: {procedure}"
    );
}
