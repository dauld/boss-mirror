//! A GREEN AFTER A RED AT THE SAME HEAD IS A FLAKE — the one definition.
//!
//! Retro 27fad542 counted 4 of 11 red car gates in two days that were
//! not the branch's fault: a BrokenPipe test flake (8a1c65e7), a
//! pre-flight lint blind to an untracked file (a3b26113), a lint that
//! skipped exit 0 during a SoR roll (35f4ff0c), a receipt never written
//! after the scope self-test refused (69e189b4). Each was recorded
//! exactly like an author's red, each cost a re-run by hand, and the
//! flakiest check was a memory (backlog 36cc4913).
//!
//! THE RULE, decided by the record and never by a check's name: when
//! `boss gate` launches for a head that ALREADY has a closed gate-run
//! judged `failed` or `lost` at the same branch and sha, the new run
//! carries [`REGATE_OF`] (the prior's id) and [`PRIOR_FAILED`] (the
//! checks its receipt named). If the new run comes back GREEN, the
//! auto-park handler — the actor that already reads every green verdict
//! — stamps [`FLAKE_OF`] and [`FLAKY_CHECKS`] on it, and on the car it
//! files or refreshes. A red after a red stamps nothing: a persistent
//! red is the branch's. Never an automatic retry: a retry hides the
//! flake; a named re-gate records it, so the count on `boss orient` and
//! the yard is a number read off gate-runs, not a feeling.
//!
//! WHY HERE. The writer is the CLI (`boss gate`), the stamper is a
//! dispatcher handler, the readers are `boss orient` and the yard — four
//! crates that cannot import each other. One module holds the key names
//! and the three pure decisions (which prior counts, what the re-gate
//! carries, what the green stamps), so no side can drift from another
//! (CLAUDE.md §9a).

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Stamped on a gate-run at launch: the id of the closed red/lost
/// gate-run at the same branch and head that this run re-gates.
pub const REGATE_OF: &str = "regate_of";
/// Beside [`REGATE_OF`]: the checks the prior run's receipt named as
/// failed (`[]` when it named none — a lost run, a refusal).
pub const PRIOR_FAILED: &str = "prior_failed";
/// Stamped on a gate-run — and on the car it files — when it came back
/// green carrying [`REGATE_OF`]: the prior red was a flake, and this is
/// the id of it.
pub const FLAKE_OF: &str = "flake_of";
/// Beside [`FLAKE_OF`]: the prior's [`PRIOR_FAILED`], copied — the
/// checks that went red then green at one head.
pub const FLAKY_CHECKS: &str = "flaky_checks";

/// The verdicts a prior run may carry to count as "red" here. `lost`
/// counts: a runner that died before a receipt says nothing about the
/// branch either, and a green at the same head afterwards proves it.
const RED_VERDICTS: [&str; 2] = ["failed", "lost"];

/// The tally key for a flake whose prior run named no check at all.
pub const NO_CHECK_NAMED: &str = "(no check named)";

/// The failed checks a receipt names, in its order: `checks` entries
/// whose `result` is not `pass` (the record `infra/gate.sh` writes),
/// else the `fails` list of the four-field receipts that sat on packets
/// before 2026-09-09. `None` when neither shape is present.
pub fn failing_checks(receipt: &Value) -> Option<Vec<String>> {
    Some(match receipt.get("checks").and_then(Value::as_array) {
        Some(checks) => checks
            .iter()
            .filter(|c| c.get("result").and_then(Value::as_str) != Some("pass"))
            .filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
            .collect(),
        None => receipt
            .get("fails")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|f| f.as_str().map(str::to_string))
            .collect(),
    })
}

/// The `record-verdict` step of a gate-run in its JSON job shape, found
/// by slug and, for a row an older path recorded, by title.
fn verdict_step(gate_run: &Value) -> Option<&Value> {
    let steps = gate_run.get("steps")?.as_array()?;
    steps
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("record-verdict"))
        .or_else(|| {
            steps
                .iter()
                .find(|s| s.get("title").and_then(Value::as_str) == Some("Record the receipt"))
        })
}

/// The verdict a gate-run recorded (`green` / `failed` / `lost`), or
/// `None` while it is still running.
pub fn verdict(gate_run: &Value) -> Option<&str> {
    verdict_step(gate_run)?
        .pointer("/metadata/verdict")?
        .as_str()
        .filter(|v| !v.is_empty())
}

/// The receipt on a gate-run's verdict step, parsed — `None` when there
/// is none or it is not JSON (a runner that died leaves prose there).
pub fn receipt(gate_run: &Value) -> Option<Value> {
    let raw = verdict_step(gate_run)?
        .pointer("/metadata/receipt")?
        .as_str()?;
    serde_json::from_str(raw).ok()
}

fn md_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.pointer(&format!("/metadata/{key}"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The instant a gate-run opened, for ordering: the `opened_at` stamp
/// (RFC3339, whole seconds, `Z` — string order is time order), else the
/// `opened_on` date, else nothing.
fn opened_key(gate_run: &Value) -> String {
    md_str(gate_run, "opened_at")
        .or_else(|| gate_run.get("opened_on").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// The closed red this launch re-gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorRed {
    /// The prior gate-run's packet id.
    pub id: String,
    /// `failed` or `lost`, as recorded.
    pub verdict: String,
    /// The checks its receipt named as failed; empty when it named none.
    pub failed: Vec<String>,
}

/// THE PRIOR THAT COUNTS: the newest closed gate-run at exactly this
/// branch and sha, if its verdict was red. A green newest run — the
/// head is already green and the operator is gating it again — is not a
/// re-gate of anything, whatever older reds sit behind it; the relation
/// recorded is to the run immediately before.
///
/// FILTERED HERE AS WELL AS AT THE SERVER. The caller narrows the read
/// with `metadata={"branch":…,"sha":…}`, but a server that does not know
/// that parameter answers the unfiltered page with a straight face
/// (CLAUDE.md §Doors: a wrong target answers instead of erroring), and a
/// re-gate stamped against another branch's red would count a flake
/// that never happened.
pub fn prior_red(runs: &[Value], branch: &str, sha: &str) -> Option<PriorRed> {
    let newest = runs
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("closed"))
        .filter(|r| md_str(r, "branch") == Some(branch))
        .filter(|r| md_str(r, "sha") == Some(sha))
        .fold(None::<(&Value, String)>, |best, r| {
            let key = opened_key(r);
            match best {
                Some((_, ref k)) if *k >= key => best,
                _ => Some((r, key)),
            }
        })
        .map(|(r, _)| r)?;
    let verdict = verdict(newest)?;
    if !RED_VERDICTS.contains(&verdict) {
        return None;
    }
    Some(PriorRed {
        id: newest.get("id")?.as_str()?.to_string(),
        verdict: verdict.to_string(),
        failed: receipt(newest)
            .and_then(|r| failing_checks(&r))
            .unwrap_or_default(),
    })
}

/// The metadata a re-gate carries at launch, merged onto the fresh
/// gate-run: the prior's id and what it named.
pub fn regate_patch(prior: &PriorRed) -> Value {
    json!({ REGATE_OF: prior.id, PRIOR_FAILED: prior.failed })
}

/// The line `boss gate` prints at launch, so the operator running the
/// re-gate knows what a green here will record.
pub fn launch_line(sha: &str, prior: &PriorRed) -> String {
    let on = if prior.failed.is_empty() {
        format!("no named check ({})", prior.verdict)
    } else {
        prior.failed.join(", ")
    };
    format!(
        "boss gate: re-gating {}: the prior run {} was red on {on}; a green here records it as a flake",
        &sha[..12.min(sha.len())],
        &prior.id[..8.min(prior.id.len())]
    )
}

/// What a GREEN gate-run carrying [`REGATE_OF`] stamps on itself and on
/// its car: `None` when it re-gated nothing. The caller has already
/// established the verdict is green — a red after a red must not reach
/// this, and the handler's cheap reject is that guard.
pub fn flake_patch(gate_run_metadata: &Value) -> Option<Value> {
    let prior = gate_run_metadata
        .get(REGATE_OF)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let checks: Vec<String> = gate_run_metadata
        .get(PRIOR_FAILED)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(json!({ FLAKE_OF: prior, FLAKY_CHECKS: checks }))
}

/// The one line `boss gate --wait` adds under a green verdict that
/// re-gated a red, so the record's stamp is also said where the
/// operator is looking.
pub fn green_note(gate_run_metadata: &Value) -> Option<String> {
    let prior = gate_run_metadata
        .get(REGATE_OF)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())?;
    Some(format!(
        "boss gate: green at the head the prior run {} was red on — recorded as a flake \
         ({FLAKE_OF} on this gate-run and its car)",
        &prior[..8.min(prior.len())]
    ))
}

/// THE COUNT: how many times each check went red then green at one
/// head, over gate-runs carrying [`FLAKE_OF`]. A flake whose prior
/// named no check counts under [`NO_CHECK_NAMED`], so a lost run or a
/// refusal is a number too rather than a row that vanishes.
pub fn tally(gate_runs: &[Value]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for md in gate_runs.iter().filter_map(|r| r.get("metadata")) {
        if md
            .get(FLAKE_OF)
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
        {
            continue;
        }
        let checks: Vec<&str> = md
            .get(FLAKY_CHECKS)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if checks.is_empty() {
            *out.entry(NO_CHECK_NAMED.to_string()).or_insert(0) += 1;
        }
        for c in checks {
            *out.entry(c.to_string()).or_insert(0) += 1;
        }
    }
    out
}

/// The FLAKES line, one line either way: the checks by count, most
/// flaky first, or a stated none.
pub fn line(tally: &BTreeMap<String, usize>, days: i64) -> String {
    if tally.is_empty() {
        return format!(
            "FLAKES — none: no gate went red then green at the same head in the last {days} days"
        );
    }
    let mut rows: Vec<(&String, &usize)> = tally.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let listed: Vec<String> = rows.iter().map(|(c, n)| format!("{c}: {n}")).collect();
    format!(
        "FLAKES — {} check(s) red then green at the same head in the last {days} days: {}",
        tally.len(),
        listed.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        id: &str,
        status: &str,
        branch: &str,
        sha: &str,
        opened_at: &str,
        verdict: Option<&str>,
        receipt: Option<&str>,
    ) -> Value {
        let mut md = json!({ "verdict": verdict });
        if let Some(r) = receipt {
            md["receipt"] = json!(r);
        }
        json!({
            "id": id,
            "kind": "gate-run",
            "status": status,
            "opened_on": &opened_at[..10],
            "metadata": { "branch": branch, "sha": sha, "opened_at": opened_at },
            "steps": [
                { "spec_slug": "launched", "metadata": {} },
                { "spec_slug": "record-verdict", "metadata": md }
            ]
        })
    }

    const RED: &str = r#"{"verdict":"failed","head":"abc","checks":[{"name":"fmt","result":"pass","seconds":1},{"name":"test","result":"FAIL","seconds":90},{"name":"clippy","result":"pass","seconds":30}]}"#;

    /// The retro's shape: a red at a head, re-gated at the SAME head.
    /// The prior is found, and it names the check that failed.
    #[test]
    fn a_closed_red_at_the_same_head_is_the_prior() {
        let runs = vec![run(
            "aaaa1111",
            "closed",
            "fix/x",
            "abc",
            "2026-09-18T10:00:00Z",
            Some("failed"),
            Some(RED),
        )];
        let prior = prior_red(&runs, "fix/x", "abc").expect("a red at the same head counts");
        assert_eq!(prior.id, "aaaa1111");
        assert_eq!(prior.verdict, "failed");
        assert_eq!(prior.failed, vec!["test".to_string()]);
        assert_eq!(
            regate_patch(&prior),
            json!({ "regate_of": "aaaa1111", "prior_failed": ["test"] })
        );
        let line = launch_line("abcdef0123456789", &prior);
        assert!(line.contains("re-gating abcdef012345"), "{line}");
        assert!(line.contains("aaaa1111 was red on test"), "{line}");
        assert!(
            line.contains("a green here records it as a flake"),
            "{line}"
        );
    }

    /// A different sha is the author's fix, not a re-gate; a different
    /// branch is someone else's red — even when the server sent it,
    /// which an old server ignoring `metadata=` would.
    #[test]
    fn a_red_at_another_head_or_branch_is_not_a_prior() {
        let runs = vec![
            run(
                "aaaa1111",
                "closed",
                "fix/x",
                "abc",
                "2026-09-18T10:00:00Z",
                Some("failed"),
                Some(RED),
            ),
            run(
                "bbbb2222",
                "closed",
                "fix/y",
                "def",
                "2026-09-18T11:00:00Z",
                Some("failed"),
                Some(RED),
            ),
        ];
        assert!(prior_red(&runs, "fix/x", "def").is_none(), "moved head");
        assert!(prior_red(&runs, "fix/y", "abc").is_none(), "another branch");
        assert!(
            prior_red(&[], "fix/x", "abc").is_none(),
            "nothing gated yet"
        );
    }

    /// The NEWEST run at the head decides. A red, then a green, then a
    /// third gate: the head is already green and nothing is re-gated. An
    /// open run at the head is still running and is not a prior.
    #[test]
    fn only_the_newest_closed_run_at_the_head_is_consulted() {
        let runs = vec![
            run(
                "aaaa1111",
                "closed",
                "fix/x",
                "abc",
                "2026-09-18T10:00:00Z",
                Some("failed"),
                Some(RED),
            ),
            run(
                "cccc3333",
                "closed",
                "fix/x",
                "abc",
                "2026-09-18T12:00:00Z",
                Some("green"),
                None,
            ),
        ];
        assert!(prior_red(&runs, "fix/x", "abc").is_none(), "already green");
        let runs = vec![
            run(
                "cccc3333",
                "closed",
                "fix/x",
                "abc",
                "2026-09-18T09:00:00Z",
                Some("green"),
                None,
            ),
            run(
                "aaaa1111",
                "closed",
                "fix/x",
                "abc",
                "2026-09-18T10:00:00Z",
                Some("failed"),
                Some(RED),
            ),
            run(
                "dddd4444",
                "open",
                "fix/x",
                "abc",
                "2026-09-18T13:00:00Z",
                None,
                None,
            ),
        ];
        assert_eq!(
            prior_red(&runs, "fix/x", "abc").map(|p| p.id).as_deref(),
            Some("aaaa1111")
        );
    }

    /// A LOST run (the environment died, no receipt) is a prior too: a
    /// green at the same head afterwards is what proves the loss was not
    /// the branch's. It names no check, and the launch line says so
    /// rather than printing an empty list.
    #[test]
    fn a_lost_run_is_a_prior_that_names_no_check() {
        let runs = vec![run(
            "aaaa1111",
            "closed",
            "fix/x",
            "abc",
            "2026-09-18T10:00:00Z",
            Some("lost"),
            Some("runner died before a receipt"),
        )];
        let prior = prior_red(&runs, "fix/x", "abc").expect("lost counts");
        assert!(prior.failed.is_empty());
        assert!(launch_line("abc", &prior).contains("no named check (lost)"));
    }

    /// GREEN AFTER RED: the stamp copies the prior's id and its checks.
    /// A run that re-gated nothing stamps nothing.
    #[test]
    fn a_green_carrying_regate_of_stamps_the_flake() {
        let md = json!({ "branch": "fix/x", "regate_of": "aaaa1111", "prior_failed": ["test"] });
        assert_eq!(
            flake_patch(&md),
            Some(json!({ "flake_of": "aaaa1111", "flaky_checks": ["test"] }))
        );
        assert!(green_note(&md).expect("a note").contains("aaaa1111"));
        let plain = json!({ "branch": "fix/x" });
        assert_eq!(flake_patch(&plain), None);
        assert_eq!(green_note(&plain), None);
        let blank = json!({ "branch": "fix/x", "regate_of": " " });
        assert_eq!(flake_patch(&blank), None);
    }

    /// THE COUNT: three flakes on `test`, one on `clippy`, one whose
    /// prior named nothing — most flaky first, and a stated none.
    #[test]
    fn the_tally_counts_checks_over_flake_stamped_runs_and_says_none() {
        let flaked =
            |checks: Value| json!({ "metadata": { "flake_of": "p", "flaky_checks": checks } });
        let runs = vec![
            flaked(json!(["test"])),
            flaked(json!(["test", "clippy"])),
            flaked(json!(["test"])),
            flaked(json!([])),
            json!({ "metadata": { "branch": "fix/plain" } }),
            json!({ "metadata": { "regate_of": "p", "prior_failed": ["fmt"] } }),
        ];
        let t = tally(&runs);
        assert_eq!(t.get("test"), Some(&3));
        assert_eq!(t.get("clippy"), Some(&1));
        assert_eq!(t.get(NO_CHECK_NAMED), Some(&1));
        assert_eq!(t.len(), 3, "a re-gate still red is not a flake: {t:?}");
        assert_eq!(
            line(&t, 7),
            "FLAKES — 3 check(s) red then green at the same head in the last 7 days: test: 3, (no check named): 1, clippy: 1"
        );
        assert_eq!(
            line(&tally(&[]), 7),
            "FLAKES — none: no gate went red then green at the same head in the last 7 days"
        );
    }
}
