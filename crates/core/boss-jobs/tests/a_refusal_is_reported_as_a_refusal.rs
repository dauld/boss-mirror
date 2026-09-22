//! An infrastructure refusal reaches the record as a refusal, not as a
//! branch failure.
//!
//! THE DEFECT (backlog c67bdbae), measured on gate-run d14b0768,
//! 2026-09-21. `infra/gate-runner/run.sh` decided the verdict with a
//! two-way `if` over `gate.sh`'s exit status — green or failed — while
//! `gate.sh` exits **2** for a refusal, distinct from 1, and writes
//! `verdict: refused` with `refused_because` into the receipt. The
//! runner then UPLOADED that receipt while reporting the opposite
//! beside it. The packet carried both, contradicting each other in one
//! write, and the branch took a strike because the system of record was
//! mid-roll when a live-reading lint declined to guess.
//!
//! CLAUDE.md §Diagnosis names the class and its price: "An
//! infrastructure refusal is not a consist failure... Recorded as a
//! plain CI failure it strikes every car aboard, and two strikes hold a
//! car out of the queue until a human looks. The same thing happened on
//! 2026-08-22 and cost four clean cars five departures." The `gate.sh`
//! half was fixed then; the runner half was not.
//!
//! THE FACT NOW LIVES ONCE (§9a). `gate.sh` decides and writes it down;
//! the runner reads it. The exit-status reading stays only as the
//! fallback for a receipt that does not exist or will not parse, where
//! `failed` is the right conservative answer for a run whose record is
//! unreadable.
//!
//! WHY THE PROTOCOL HAD TO CHANGE TOO, and why this is not a one-line
//! car: `record-verdict` declared `verdict` as the enum
//! `green|failed|lost`. Writing `refused` was REFUSED by the field
//! validator — correctly, fail-closed — so the value needed a home AND
//! a terminal. Without the terminal a refused run would satisfy none of
//! them and the packet would hang open forever, which is worse than the
//! defect being fixed.

use boss_jobs::registry::seedable_platform_workflows;

fn gate_run() -> boss_jobs::registry::WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "gate-run")
        .expect("the gate-run protocol is in the platform bundle")
}

fn read(rel: &str) -> String {
    let p = boss_testing::repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// THE VERDICT HAS A WORD FOR A REFUSAL, and the runner can therefore
/// report one. An enum that omits it makes the honest report impossible.
#[test]
fn the_verdict_enumerates_refused() {
    let wf = gate_run();
    let step = wf
        .steps
        .iter()
        .find(|s| s.title == "record-verdict")
        .expect("record-verdict step");
    let verdict = step
        .fields
        .iter()
        .find(|f| f.name == "verdict")
        .expect("the verdict field");
    let ty = &verdict.field_type;
    for word in ["green", "failed", "lost", "refused"] {
        assert!(
            ty.contains(word),
            "the verdict enum must carry {word:?} — it is {ty:?}. Without `refused` the \
             runner has nowhere to put an honest infrastructure refusal and reports it as \
             a branch failure."
        );
    }
}

/// EVERY VERDICT REACHES A TERMINAL. A value the enum accepts but no
/// terminal matches leaves the packet open forever — and an open
/// gate-run is read by the yard as a gate still running, so the bay
/// never frees.
#[test]
fn every_verdict_the_enum_accepts_has_a_terminal_that_matches_it() {
    let wf = gate_run();
    let step = wf
        .steps
        .iter()
        .find(|s| s.title == "record-verdict")
        .expect("record-verdict step");
    let ty = step
        .fields
        .iter()
        .find(|f| f.name == "verdict")
        .expect("the verdict field")
        .field_type
        .clone();

    let values: Vec<&str> = ty
        .split('|')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect();
    assert!(
        values.len() >= 4,
        "read {values:?} out of {ty:?} — the enum's shape changed and this pin would \
         otherwise pass while checking almost nothing"
    );
    for v in values {
        let needle = format!("verdict = \\\"{v}\\\"");
        let matched = wf
            .steps
            .iter()
            .any(|s| s.ready_when.contains(&format!("verdict = \"{v}\"")));
        assert!(
            matched,
            "no terminal matches the verdict {v:?} (looked for {needle}). A verdict the \
             enum accepts and no terminal claims leaves the packet open forever, and an \
             open gate-run reads as a gate still running — the bay never frees."
        );
    }
}

/// A REFUSAL IS NOT EVIDENCE AGAINST THE BRANCH, and its terminal has to
/// say so where a reader meets it. `aborted` like `lost`, for the same
/// reason: both mean the run produced no evidence about the tree.
#[test]
fn the_refused_terminal_says_it_judged_nothing() {
    let wf = gate_run();
    let refused = wf
        .steps
        .iter()
        .find(|s| s.title == "refused")
        .expect("a `refused` terminal");
    assert_eq!(refused.kind, "outcome", "a terminal is an outcome step");
    let title = &refused.title_template;
    assert!(
        title.contains("judged nothing") || title.contains("nothing about"),
        "the terminal's title is what a reader meets at 3am; it must say the run judged \
         nothing about the branch rather than implying the branch is bad: {title:?}"
    );
}

/// THE RUNNER READS THE RECEIPT, and keeps the exit-status reading only
/// as the fallback. Both halves are pinned: without the first the fact
/// lives twice and the copies disagree; without the second a missing or
/// unparseable receipt would leave no verdict at all.
#[test]
fn the_runner_takes_its_verdict_from_the_receipt_and_falls_back_to_the_exit_status() {
    let run = read("infra/gate-runner/run.sh");
    assert!(
        run.contains(".verdict // empty"),
        "the runner must read the verdict out of the receipt gate.sh wrote, rather than \
         deriving it a second time from the exit status"
    );
    assert!(
        run.contains("VERDICT=failed"),
        "the exit-status reading must remain as the fallback: a receipt that does not \
         exist or will not parse still needs a verdict, and `failed` is the conservative \
         one for a run whose record is unreadable"
    );
    // The accepted set must be the protocol's set. A word the runner
    // reports that the enum does not declare is refused by the field
    // validator, and a refused report is a `lost` gate-run nobody can
    // read — the defect this car fixes, one layer over.
    let wf = gate_run();
    let ty = wf
        .steps
        .iter()
        .find(|s| s.title == "record-verdict")
        .expect("record-verdict")
        .fields
        .iter()
        .find(|f| f.name == "verdict")
        .expect("verdict field")
        .field_type
        .clone();
    for v in ty.split('|').map(str::trim).filter(|v| !v.is_empty()) {
        assert!(
            run.contains(v),
            "the runner's accepted verdicts must cover the protocol's enum; {v:?} is \
             declared by the Workflow and not accepted by the runner, so a receipt \
             carrying it would be silently dropped"
        );
    }
}
