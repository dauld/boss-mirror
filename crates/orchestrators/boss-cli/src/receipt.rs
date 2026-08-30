//! `boss receipt <car>` — what does this car's receipt actually vouch for?
//!
//! WHY THIS IS A VERB. The third of the three Class A questions in
//! backlog-item 26b3d203. A gate receipt is the only evidence a car
//! carries, and it vouches for something narrower than people read it
//! as. Two gaps, both of which have cost real work:
//!
//!   THE HEAD MOVED. A receipt names the sha it gated. Rebase or
//!   force-push the branch afterwards and the receipt still reads
//!   "green" while describing a tree that is no longer there. Three
//!   cars' stale receipts were "repaired" on 2026-08-27, the API
//!   answered 204 three times, nothing was written, and the cars were
//!   reported fixed while staying unboardable (09576fab).
//!
//!   THE MODE IS NOT THE SAME GATE. `--auto` derives its scope from the
//!   tree and skips cargo entirely when nothing implies a crate. That
//!   is correct and it is NOT a full gate: a green receipt in auto mode
//!   vouches for the lints, not the suites. "A green gate only covers
//!   what it runs" is a whole separate finding, and reading a mode-auto
//!   receipt as a full one is how a ci.yml change gated green and broke
//!   CI on the train.
//!
//! So the verb answers three things a reader needs and a shell pipeline
//! keeps re-deriving: what sha the receipt covers, whether the branch
//! still points there, and how much of the gate actually ran.

use anyhow::Result;

/// What a receipt is worth right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// The receipt describes the branch's current head.
    Current,
    /// The branch has moved since it was gated. The receipt is honest
    /// about a tree that no longer exists.
    Stale { gated: String, now: String },
    /// The branch is gone. For an arrived car that is expected — the
    /// train deletes what it merged — so this is not a fault.
    BranchGone,
    /// Could not tell. Never reported as Current.
    Unknown(String),
}

/// Everything the check needs, so the decision is pure and testable.
#[derive(Debug, Clone, Default)]
pub struct ReceiptFacts {
    /// `receipt.head` — the sha the gate actually ran against.
    pub gated_head: Option<String>,
    /// The branch's head on the forge now. `None` = the ref is gone.
    pub branch_head: Option<String>,
    /// Did the ref lookup itself succeed? Distinguishes "the branch is
    /// gone" from "I could not ask", which look identical otherwise and
    /// must not.
    pub remote_readable: bool,
    /// `receipt.verdict`.
    pub verdict: Option<String>,
    /// `receipt.mode` — full, auto, or a crate-scoped run.
    pub mode: Option<String>,
}

fn same_commit(a: &str, b: &str) -> bool {
    let n = a.len().min(b.len());
    n >= 7 && a[..n].eq_ignore_ascii_case(&b[..n])
}

/// The decision, pure.
pub fn standing(f: &ReceiptFacts) -> Standing {
    let Some(gated) = &f.gated_head else {
        return Standing::Unknown(
            "the car carries no gate receipt — there is nothing vouching for it".into(),
        );
    };
    if !f.remote_readable {
        return Standing::Unknown(
            "could not read the branch from the forge. An unreadable ref is not \
             a missing one, and a receipt cannot be called current against a \
             head nobody looked up"
                .into(),
        );
    }
    let Some(now) = &f.branch_head else {
        return Standing::BranchGone;
    };
    if same_commit(gated, now) {
        Standing::Current
    } else {
        Standing::Stale {
            gated: gated.clone(),
            now: now.clone(),
        }
    }
}

/// How much of the gate a receipt's mode actually covers, in words.
/// `None` mode is treated as unknown coverage rather than assumed full —
/// the assumption is the defect.
pub fn coverage(mode: Option<&str>) -> &'static str {
    match mode {
        Some("full") => "the full gate: fmt, every lint, clippy, build and the suites",
        Some("auto") => {
            "AUTO — scope derived from the tree. Cargo is skipped entirely when \
             nothing changed implies a crate, so this vouches for the lints and \
             not necessarily for any suite"
        }
        Some(_) => {
            "a SCOPED run — cargo phases limited to the named crates. Lints and \
             fmt ran repo-wide; the suites did not"
        }
        None => "unrecorded — coverage cannot be read off this receipt",
    }
}

fn sh(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(args[0])
        .args(&args[1..])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

pub async fn run(branch: &str, clone: Option<String>, remote: Option<String>) -> Result<()> {
    let clone = clone.unwrap_or_else(|| ".".to_string());
    let remote = remote.unwrap_or_else(|| "origin".to_string());

    // The receipt lives on the car's gate step; this reads it from the
    // packet rather than re-deriving it, because a receipt that is
    // retyped is not a receipt (that is what boss park fixed).
    //
    // SPEAKS TO THE API DIRECTLY. The first version shelled out to
    // ~/bin/boss-api, which made the verb work on exactly one laptop —
    // caught by the no-session-paths lint before it reached a gate.
    // Going through gate.rs's client also inherits the rule that
    // BOSS_JOBS_URL has no default: a wrong instance does not error
    // here, it answers, which is worse.
    let http = reqwest::Client::new();
    let body = crate::gate::api(
        &http,
        reqwest::Method::GET,
        "/api/jobs?kind=ship-a-change&status=open&limit=100",
        None,
    )
    .await?;
    let facts = facts_from(body.as_ref(), branch, &clone, &remote);

    println!("boss receipt: {branch}");
    println!("  verdict  {}", facts.verdict.as_deref().unwrap_or("none"));
    println!(
        "  gated    {}",
        facts.gated_head.as_deref().unwrap_or("none")
    );
    println!(
        "  now      {}",
        facts.branch_head.as_deref().unwrap_or("absent")
    );
    println!("  covers   {}", coverage(facts.mode.as_deref()));
    match standing(&facts) {
        Standing::Current => println!("  CURRENT — the receipt describes the branch's head"),
        Standing::Stale { gated, now } => println!(
            "  STALE — gated {} but the branch is now {}. The receipt is honest \n\
             \x20   about a tree that no longer exists; re-gate before boarding.",
            &gated[..gated.len().min(12)],
            &now[..now.len().min(12)]
        ),
        Standing::BranchGone => println!(
            "  BRANCH GONE — expected for an arrived car, since a train deletes \n\
             \x20   what it merged. Not a fault."
        ),
        Standing::Unknown(why) => println!("  UNKNOWN — {why}"),
    }
    Ok(())
}

/// Split out so the JSON walk is testable without a live API.
fn facts_from(
    packets: Option<&serde_json::Value>,
    branch: &str,
    clone: &str,
    remote: &str,
) -> ReceiptFacts {
    let ls = sh(&[
        "git",
        "-C",
        clone,
        "ls-remote",
        remote,
        &format!("refs/heads/{branch}"),
    ]);
    let remote_readable = ls.is_some();
    let branch_head = ls
        .as_deref()
        .and_then(|s| s.split_whitespace().next())
        .map(str::to_string);

    let mut out = ReceiptFacts {
        remote_readable,
        branch_head,
        ..Default::default()
    };
    // PARSED, NOT SCANNED. The first version of this walked the raw
    // body for `"head":` inside a window after the branch name, and it
    // reported NO RECEIPT for a car that had one — the receipt lives on
    // the gate STEP, past the window, and field order is not a contract.
    // Writing an ad-hoc string scan inside the verb built to retire
    // ad-hoc string scans is the joke this comment exists to prevent
    // repeating.
    if let Some(v) = packets {
        for job in crate::gate::rows(Some(v.clone())) {
            let is_ours = job
                .pointer("/metadata/branch")
                .and_then(|b| b.as_str())
                .is_some_and(|b| b == branch);
            if !is_ours {
                continue;
            }
            for step in job
                .get("steps")
                .and_then(|s| s.as_array())
                .into_iter()
                .flatten()
            {
                // The receipt is stored as a JSON STRING on the gate
                // step, so it needs a second parse.
                let Some(raw) = step.pointer("/metadata/receipt").and_then(|r| r.as_str()) else {
                    continue;
                };
                if let Ok(r) = serde_json::from_str::<serde_json::Value>(raw) {
                    out.gated_head = r.get("head").and_then(|x| x.as_str()).map(str::to_string);
                    out.verdict = r
                        .get("verdict")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    out.mode = r.get("mode").and_then(|x| x.as_str()).map(str::to_string);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> ReceiptFacts {
        ReceiptFacts {
            gated_head: Some("c8ccb133770f6507422a7d2438261213e9e897ce".into()),
            branch_head: Some("c8ccb133770f6507422a7d2438261213e9e897ce".into()),
            remote_readable: true,
            verdict: Some("green".into()),
            mode: Some("full".into()),
        }
    }

    #[test]
    fn a_receipt_matching_the_head_is_current() {
        assert_eq!(standing(&f()), Standing::Current);
    }

    /// THE 09576fab CASE. A rebase or force-push leaves the receipt
    /// green and describing a tree that is gone. Three cars were
    /// reported repaired on that basis while staying unboardable.
    #[test]
    fn a_moved_branch_makes_the_receipt_stale() {
        let moved = ReceiptFacts {
            branch_head: Some("ffffffffffffffffffffffffffffffffffffffff".into()),
            ..f()
        };
        assert!(matches!(standing(&moved), Standing::Stale { .. }));
    }

    /// An arrived train deletes the branches it merged, so a missing
    /// ref is expected rather than a fault — the same trap `boss
    /// merged` encodes.
    #[test]
    fn a_gone_branch_is_expected_not_a_fault() {
        let gone = ReceiptFacts {
            branch_head: None,
            ..f()
        };
        assert_eq!(standing(&gone), Standing::BranchGone);
    }

    /// ...but only when the lookup SUCCEEDED and found nothing. An
    /// unreadable remote is a different fact and must not be reported
    /// as a deleted branch.
    #[test]
    fn an_unreadable_remote_is_not_a_deleted_branch() {
        let blind = ReceiptFacts {
            branch_head: None,
            remote_readable: false,
            ..f()
        };
        assert!(matches!(standing(&blind), Standing::Unknown(_)));
    }

    #[test]
    fn no_receipt_is_unknown_rather_than_current() {
        let none = ReceiptFacts {
            gated_head: None,
            ..f()
        };
        assert!(matches!(standing(&none), Standing::Unknown(_)));
    }

    /// The mode is the half people read past. A green auto receipt is
    /// green about the lints, not the suites, and saying so is the
    /// whole point of surfacing coverage.
    #[test]
    fn coverage_distinguishes_auto_from_full() {
        assert!(coverage(Some("full")).contains("suites"));
        assert!(coverage(Some("auto")).contains("not necessarily"));
        assert!(coverage(None).contains("cannot be read"));
    }

    /// Abbreviated and full spellings of one sha are the same commit —
    /// the bug that `boss running` shipped with until a live run caught
    /// it, so it is pinned here rather than rediscovered.
    #[test]
    fn an_abbreviated_head_still_matches() {
        let short = ReceiptFacts {
            branch_head: Some("c8ccb133770f".into()),
            ..f()
        };
        assert_eq!(standing(&short), Standing::Current);
    }
}
