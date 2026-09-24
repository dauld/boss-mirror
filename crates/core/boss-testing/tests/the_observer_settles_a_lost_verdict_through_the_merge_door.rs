//! THE ESTATE OBSERVER SETTLES A DEAD RUNNER'S VERDICT THROUGH THE STEP
//! MERGE DOOR; NO PUT CARRIES METADATA.
//!
//! Backlog e39a9d2a (correction 2026-09-23, measured again 2026-09-24):
//! the observer settled every gate runner that died with its node as ONE
//! step PUT of `{status: "completed", metadata: {verdict: "lost",
//! receipt}}` built with no read. The step PUT replaces metadata
//! wholesale, and the registry materializes keys onto every step at
//! admission (`metadata_defaults` — gate-run.toml gives the verdict step
//! `heartbeat_at` — plus `authority_role`, `station`, `audience`,
//! `claimable`), so each settle shed them. It is the same step the
//! gate-runner (`infra/gate-runner/run.sh`) and `boss gate`'s refusals
//! already write the merge-then-flip way (cars 2 and 3 of that item);
//! the observer is the third writer of it and now reads the same.
//!
//! A text pin, in the shape of `run_sh_verdict`'s: the script lives in a
//! CronJob manifest whose body needs kubectl and a live API, so what is
//! held is the shape of the two writes, by name.

use boss_testing::repo_root;

const OBSERVER: &str = "infra/cluster/manifests/boss-estate-observe.yaml";

/// The dead-runner settle, from its heading comment to the end of its
/// loop, with comment lines dropped — so prose that NAMES the old shape
/// cannot satisfy or fail the pin.
fn settle_block() -> String {
    let path = repo_root().join(OBSERVER);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let start = text
        .find("A DEAD RUNNER IS SETTLED BY WHOEVER CAN SEE IT DIE")
        .expect("the observer still settles dead gate runners");
    let rest = &text[start..];
    let end = rest
        .find("done <\"$WORK/dead-gates.tsv\"")
        .expect("the settle loop reads dead-gates.tsv");
    rest[..end]
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_lost_verdict_is_merged_then_the_step_flips_alone() {
    let block = settle_block();
    assert!(
        block.contains("-X PATCH"),
        "the verdict and receipt must be written with a PATCH:\n{block}"
    );
    assert!(
        block.contains("\"$JOBS_API/api/jobs/$packet/steps/$step/metadata\""),
        "…through the step merge door, PATCH …/steps/{{id}}/metadata:\n{block}"
    );
    assert!(
        block.contains("'{\"status\":\"completed\"}'"),
        "the completion must be a status-only PUT body:\n{block}"
    );
    assert!(
        !block.contains("metadata: {"),
        "no settle body may nest the verdict under `metadata` — a PUT carrying \
         metadata replaces the step's stored keys wholesale (e39a9d2a):\n{block}"
    );
}

/// Merge FIRST: `verdict` is required at done and the flip is where that
/// is judged, so a flip sent before the merge would be refused — and a
/// flip sent after a FAILED merge would be refused the same way, so the
/// flip is only attempted once the merge answered 2xx.
#[test]
fn the_merge_goes_before_the_flip() {
    let block = settle_block();
    let merge = block
        .find("/steps/$step/metadata\"")
        .expect("the merge door is written");
    let flip = block
        .find("'{\"status\":\"completed\"}'")
        .expect("the status-only flip is written");
    assert!(merge < flip, "the merge must precede the flip:\n{block}");
    assert!(
        block[merge..flip].contains("2??)"),
        "the flip must be gated on the merge answering 2xx:\n{block}"
    );
}
