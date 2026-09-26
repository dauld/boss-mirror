//! The merge-lost arm's records, pure (backlog f9256445, design d812f1b7
//! D1): the four fields the `merge-lost` terminal requires, and the ONE
//! backlog-item a train that ends there files.
//!
//! WHY. Train 2026-09-25 20:04 (PR #687) merged as c85941b4 at 20:10:41Z,
//! and by 20:11:14Z forge main was back at 777a5888. `verify_convergence`
//! asked only whether the cluster descended from the merge, so the train
//! waited, alarmed, and waited for ever — the alarm said "overdue", never
//! "main no longer carries it". The arm reads forge main itself, ends the
//! train on two NOT readings, unlands its cars through `boss car unland`'s
//! writer, and files what it read, so no one has to re-derive it.
//!
//! The adapter (the reads and the writes) is `Conductor::end_on_merge_lost`
//! and `Conductor::merge_lost_followup` in `conductor.rs`.

use super::*;

/// The `merge-lost` terminal's slug and title.
pub(crate) const MERGE_LOST_SLUG: &str = "merge-lost";
pub(crate) const MERGE_LOST_TITLE: &str =
    "Merge lost — main no longer carries it; cars back to the dock";
/// The marker its `ready_when` waits on.
pub(crate) const MERGE_LOST_MARKER: &str = "merge_lost";
/// The merge's FULL sha, on the train, for the follow-up's unlandings —
/// the step's `merge_ref` is the forge's 12-character answer, and a clone
/// that never fetched the merge can fetch it only by its full sha.
pub(crate) const MERGE_LOST_MERGE: &str = "merge_lost_merge";
/// `owed` until every car is unlanded (or named as not) and the item is
/// filed; then `done`. The sweep reads closed trains carrying `owed`, so
/// a failed filing is retried on the next pass rather than lost.
pub(crate) const MERGE_LOST_FOLLOWUP: &str = "merge_lost_followup";
/// The item the follow-up filed.
pub(crate) const MERGE_LOST_ITEM: &str = "merge_lost_item";

/// The first NOT reading, as the train records it for the next pass.
pub(crate) fn first_reading_record(r: &crate::car_unland::MainReading) -> Value {
    json!({
        "main": r.main,
        "merge": r.merge,
        "read_at": r.read_at,
        "evidence": r.evidence(),
    })
}

/// PURE: the four fields the `merge-lost` terminal requires — the merge,
/// forge main at the second read, when, and the evidence: both readings,
/// and when the forge says the PR merged.
pub(crate) fn merge_lost_fields(
    merge_ref: &str,
    first: &Value,
    second: &crate::car_unland::MainReading,
    pr_url: &str,
    merged_at: Option<&str>,
) -> Vec<(&'static str, String)> {
    let first_said = first
        .get("evidence")
        .and_then(Value::as_str)
        .unwrap_or("(the first reading carried no evidence)");
    vec![
        ("merge_ref", merge_ref.to_string()),
        ("main_at_read", second.main.clone()),
        ("read_at", second.read_at.clone()),
        (
            "evidence",
            format!(
                "two readings on two passes, both NOT. First: {first_said}. Second: {}. The \
                 PR {pr_url} merged at {} as {merge_ref}; forge main no longer carries it, so \
                 no cluster commit can descend from it and `converged` could never complete.",
                second.evidence(),
                merged_at.unwrap_or("(the forge gave no merged_at)")
            ),
        ),
    ]
}

/// PURE: the ONE backlog-item a merge-lost train files, on the
/// pipeline-failure channel: the train, the PR, both shas, both read
/// times, and what became of each car — so the reader starts from the
/// reading, not from "overdue".
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_lost_item_body(
    tid: &str,
    pr_url: &str,
    merge_ref: &str,
    main: &str,
    first_read_at: &str,
    read_at: &str,
    cars: &[String],
    owner: &str,
) -> Value {
    let short_main = &main[..12.min(main.len())];
    json!({
        "kind": "backlog-item",
        "title": format!(
            "Forge main lost train {}'s merge {merge_ref}: the train ended merge-lost and its \
             cars ride again",
            id8(tid)
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": owner,
        "status": "open",
        "tags": [],
        "priority": "urgent",
        "metadata": {
            "input_channel": "pipeline-failure",
            "reporter": "conductor",
            "merge_lost_train": tid,
            "description": format!(
                "The conductor read forge main twice, on two passes ({first_read_at} and \
                 {read_at}), and both times `git merge-base --is-ancestor {merge_ref} \
                 {short_main}` exited 1: main does not carry the merge of {pr_url}. The \
                 train ended on `merge-lost` (it holds the track no longer), and each car \
                 it carried was unlanded through `boss car unland`'s writer — its landing \
                 corrected, and a successor opened at `gate` to ride again on current \
                 main:\n\n{}\n\nWhat moved main is not read here: the forge's own log names \
                 the pusher (design d812f1b7 D4).",
                if cars.is_empty() {
                    "- (the train recorded no cars)".to_string()
                } else {
                    cars.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n")
                }
            ),
            "evidence": {
                "train": tid,
                "pr": pr_url,
                "merge_ref": merge_ref,
                "main": main,
                "first_read_at": first_read_at,
                "read_at": read_at,
                "cars": cars,
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::car_unland::MainReading;

    fn second() -> MainReading {
        MainReading {
            main: "d785c087e868511af8047651bdb1762bbf931beb".into(),
            merge: "c85941b423bcdf85f1576ab3b4bdfee45004eea7".into(),
            read_at: "2026-09-25T21:21:00Z".into(),
            carries: false,
        }
    }

    /// The four required fields, each read, none typed: the evidence
    /// holds BOTH readings and the forge's merged_at.
    #[test]
    fn the_terminal_carries_both_readings_and_when_the_pr_merged() {
        let first = first_reading_record(&MainReading {
            main: "777a5888504f6f80958ec445284c7a28b4faad14".into(),
            read_at: "2026-09-25T21:20:00Z".into(),
            ..second()
        });
        let fields = merge_lost_fields(
            "c85941b423bc",
            &first,
            &second(),
            "http://forge/boss/pulls/687",
            Some("2026-09-25T20:10:41Z"),
        );
        let names: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, ["merge_ref", "main_at_read", "read_at", "evidence"]);
        assert_eq!(fields[1].1, "d785c087e868511af8047651bdb1762bbf931beb");
        assert_eq!(fields[2].1, "2026-09-25T21:21:00Z");
        let evidence = &fields[3].1;
        assert!(
            evidence.contains("777a5888504f"),
            "the first reading: {evidence}"
        );
        assert!(evidence.contains("at 2026-09-25T21:20:00Z"), "{evidence}");
        assert!(
            evidence.contains("d785c087e868"),
            "the second reading: {evidence}"
        );
        assert!(
            evidence.contains("merged at 2026-09-25T20:10:41Z"),
            "{evidence}"
        );
        assert!(evidence.contains("exited 1"), "{evidence}");
    }

    /// One item, on the pipeline-failure channel, naming the train, the
    /// PR, both shas, both read times and every car's fate.
    #[test]
    fn the_item_names_the_train_the_pr_both_shas_both_reads_and_each_car() {
        let body = merge_lost_item_body(
            "c94d5d39-34ad-4247-83de-eeee3f81534c",
            "http://forge/boss/pulls/687",
            "c85941b423bc",
            "d785c087e868511af8047651bdb1762bbf931beb",
            "2026-09-25T21:20:00Z",
            "2026-09-25T21:21:00Z",
            &["car dec3136a unlanded; successor car 5f1e2a3b rides again from `gate`".into()],
            "emp-david",
        );
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["metadata"]["input_channel"], "pipeline-failure");
        let title = body["title"].as_str().unwrap();
        assert!(
            title.contains("c94d5d39") && title.contains("c85941b423bc"),
            "{title}"
        );
        let d = body["metadata"]["description"].as_str().unwrap();
        for said in [
            "2026-09-25T21:20:00Z",
            "2026-09-25T21:21:00Z",
            "pulls/687",
            "d785c087e868",
            "car dec3136a unlanded",
        ] {
            assert!(d.contains(said), "{said} missing from: {d}");
        }
        assert_eq!(
            body["metadata"]["evidence"]["main"],
            "d785c087e868511af8047651bdb1762bbf931beb"
        );
    }
}
