//! Every probe detector reaches a door.
//!
//! `boss_jobs::probe` holds the functions that read a probe's TEXT and
//! say something about its shape. The two doors that show those
//! judgements to a person — `boss prove`'s `shape_warnings` and `boss
//! gate`'s `--park-probe` checks — chain them BY HAND, and until this
//! test nothing asserted what the set was.
//!
//! MEASURED 2026-09-21 (backlog de09c185, found by the builder of
//! e7cf78c6 while adding two detectors): ten probe-taking public
//! functions, six chained into `shape_warnings`, the other four
//! reached from `gate.rs`, `prove.rs` or `brief.rs`. So nothing was
//! orphaned that day — which is the best moment to add the pin, not
//! the worst. An eleventh detector added and never chained compiles,
//! tests green, and is simply never said at either door: a check
//! nobody reads, living inside the machinery built to stop exactly
//! that.
//!
//! WHY THIS IS NOT A COUNT. "Six are chained" is a fact that lives
//! twice, and §9a's worked examples are what happens next — the number
//! drifts from the set and a comment asks the next person to keep them
//! in sync. It would also miss the real failure: an eleventh detector
//! leaves the count correct for the ten it names. So the set is
//! DERIVED from the source that declares it, and the assertion is
//! about reachability.
//!
//! WHY "A DOOR" AND NOT "`shape_warnings`". Four of the ten are
//! deliberately not warnings: `names_an_actor` and
//! `reads_the_sor_unidentified` drive a REFUSAL, and
//! `needs_absent_tool` drives the unrunnable verdict. A pin demanding
//! membership of `shape_warnings` would have been wrong about four of
//! ten on the day it landed.

use boss_testing::repo_root;
use std::collections::BTreeSet;

/// The declared set: every `pub fn` in `probe.rs` that takes a probe's
/// text AND RETURNS A JUDGEMENT about it. Deliberately a source read
/// rather than a list — a list here would be the second copy this test
/// exists to prevent.
///
/// A DETECTOR JUDGES; AN EXTRACTOR DOES NOT, and the difference is the
/// return type. `-> Option<…>` or `-> bool` is a verdict about the
/// probe and belongs at a door. `-> Vec<…>` is a pull-apart that
/// detectors build ON: `commands_invoked` returns the commands a probe
/// runs and is used inside `needs_absent_tool` and
/// `reads_the_sor_unidentified`, so it reaches a door through them and
/// has no business being chained itself.
///
/// This test found that out on its first run — the filter was written
/// as "takes a probe" and immediately reported `commands_invoked` as
/// an orphan. The distinction is the fix, not an exemption for that
/// one name, because the next extractor would be reported too.
fn declared_detectors() -> BTreeSet<String> {
    let src = std::fs::read_to_string(repo_root().join("crates/core/boss-jobs/src/probe.rs"))
        .expect("probe.rs is readable");
    src.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let rest = l.strip_prefix("pub fn ")?;
            if !rest.contains("(probe: &str)") {
                return None;
            }
            let returns = rest.split("->").nth(1)?.trim();
            let judges = returns.starts_with("Option<") || returns.starts_with("bool");
            if !judges {
                return None;
            }
            let name = rest.split('(').next()?.trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Everything boss-cli says out loud, as one haystack. The doors are
/// several files (`prove.rs` chains the warnings, `gate.rs` runs the
/// park-probe checks, `brief.rs` renders one into a brief), so the
/// question is whether a detector is referenced AT ALL, not by which.
fn boss_cli_sources() -> String {
    let dir = repo_root().join("crates/orchestrators/boss-cli/src");
    let mut out = String::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("boss-cli src is readable") {
            let p = entry.expect("a dir entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push_str(&std::fs::read_to_string(&p).expect("a source file"));
            }
        }
    }
    out
}

#[test]
fn every_probe_detector_reaches_a_door() {
    let declared = declared_detectors();
    assert!(
        declared.len() >= 8,
        "the source read found only {} detector(s) — the signature shape this test keys on \
         has probably changed, and a test that finds nothing passes vacuously: {declared:?}",
        declared.len()
    );

    let doors = boss_cli_sources();
    let orphans: Vec<&String> = declared.iter().filter(|d| !doors.contains(*d)).collect();

    assert!(
        orphans.is_empty(),
        "these probe detectors are declared and reach NO door in boss-cli, so nothing they \
         find is ever said to anyone — chain each into `shape_warnings` (a warning), or into \
         the refusal/unrunnable paths if that is what it judges: {orphans:?}",
        orphans = orphans
    );
}

/// The control. The test above is an absence assertion, and an absence
/// assertion whose reader cannot SEE the thing it is looking for
/// passes for the wrong reason — so prove the haystack really would
/// report an orphan.
#[test]
fn a_detector_no_door_mentions_would_be_reported() {
    let doors = boss_cli_sources();
    assert!(
        !doors.contains("a_detector_that_does_not_exist_anywhere"),
        "the control name must be absent, or this control proves nothing"
    );
    assert!(
        doors.contains("asserts_its_own_negation"),
        "and a detector that IS chained must be found, or the haystack is not being read"
    );
}
