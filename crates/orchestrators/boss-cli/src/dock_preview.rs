//! The dock's merge preview — "will this car be left behind?" answered
//! BEFORE boarding (12a25f3e).
//!
//! David, 2026-08-31: can a parked car know it will be left behind at
//! boarding, before the boarding happens? Largely yes — but not as a
//! stored flag, which drifts the instant main moves or a co-boarder
//! parks (the fact-that-lives-twice trap, §9a). THE HONEST SHAPE is
//! the gate receipt's own discipline: a SHA-ANCHORED projection —
//! "clean as of main@<sha>, parked-set <hash>, checked at T" — that
//! reads STALE rather than wrong when its inputs move, recomputed on
//! the conductor's reconcile tick where the clone and the git op
//! already live.
//!
//! Two blocking classes, both computed with `git merge-tree
//! --write-tree` (a real in-memory merge, no checkout):
//!   - vs MAIN: does the branch still merge onto current main? The
//!     already-landed-work conflict.
//!   - vs CO-BOARDERS: pairwise trial-merges across the parked set —
//!     tonight's class (two cars clean vs main, conflicted with each
//!     other on main.rs; the later one was left at #165's assembly).
//!     Pairwise is deliberately order-free: "this SET has a mutual
//!     conflict on file X, so one of them will be left" is knowable
//!     now; WHICH one loses is assembly order and stays the
//!     boarding's own fact.
//!
//! The projection lands on each car's `metadata.merge_preview`,
//! written only when it CHANGED (a 10-minute heartbeat of identical
//! stamps would be event spam, and the checked_at alone moving is not
//! a change in the fact).

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// One car's standing against a merge target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Clean,
    /// The conflicted paths, as merge-tree names them.
    Conflicts(Vec<String>),
}

/// Run `git merge-tree --write-tree` between two committish refs in
/// `clone`. Exit 0 = clean; exit 1 = real conflicts (parsed from the
/// name-only section); anything else is a hard error (a missing ref
/// must not read as "clean").
pub(crate) fn trial_merge(clone: &str, ours: &str, theirs: &str) -> Result<Verdict> {
    let out = crate::git_auth::command()
        .arg("-C")
        .arg(clone)
        .args(["merge-tree", "--write-tree", "--name-only", ours, theirs])
        .output()
        .with_context(|| format!("git merge-tree {ours} {theirs}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Exit 1 is OVERLOADED: a real conflict AND "not something we can
    // merge" both exit 1 (probed against git directly — the test that
    // caught this assumed a distinct code). A real conflict writes the
    // merged tree's OID as its first stdout line; a bad ref writes
    // nothing usable. Only an OID-shaped first line may read as a
    // conflict verdict — anything else is an error, never a verdict.
    let first_is_oid = stdout
        .lines()
        .next()
        .is_some_and(|l| l.len() >= 40 && l.chars().all(|c| c.is_ascii_hexdigit()));
    match out.status.code() {
        Some(0) => Ok(Verdict::Clean),
        Some(1) if first_is_oid => Ok(Verdict::Conflicts(parse_conflicted_files(&stdout))),
        _ => anyhow::bail!(
            "merge-tree {ours} {theirs} errored (not a conflict verdict): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// The conflicted paths from `--write-tree --name-only` output: the
/// first line is the written tree OID; the following non-empty lines up
/// to the first blank are the conflicted file names; informational
/// messages (if any) come after the blank separator and are ignored.
pub(crate) fn parse_conflicted_files(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect()
}

/// The parked-set anchor: an order-free digest of `branch@head` pairs,
/// so the projection can say which SET it measured. Git's own hasher —
/// no new dependency, stable across builds.
pub(crate) fn set_hash(clone: &str, pairs: &[(String, String)]) -> Result<String> {
    let mut sorted: Vec<String> = pairs.iter().map(|(b, h)| format!("{b}@{h}")).collect();
    sorted.sort();
    let joined = sorted.join("\n");
    let out = crate::git_auth::command()
        .arg("-C")
        .arg(clone)
        .args(["hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin
                .take()
                .expect("piped")
                .write_all(joined.as_bytes())?;
            c.wait_with_output()
        })
        .context("git hash-object for the set anchor")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The projection one car carries, pure given the measurements.
pub(crate) fn preview_payload(
    vs_main: &Verdict,
    conflicts_with: &[(String, Vec<String>)],
    main_sha: &str,
    set_hash: &str,
    checked_at: &str,
) -> Value {
    let vs_main_v = match vs_main {
        Verdict::Clean => json!({ "clean": true }),
        Verdict::Conflicts(files) => json!({ "clean": false, "files": files }),
    };
    let co: Vec<Value> = conflicts_with
        .iter()
        .map(|(b, files)| json!({ "branch": b, "files": files }))
        .collect();
    json!({
        "vs_main": vs_main_v,
        "conflicts_with": co,
        "anchored": { "main": main_sha, "parked_set": set_hash },
        "checked_at": checked_at,
    })
}

/// Whether a freshly computed payload says anything the stored one does
/// not. `checked_at` alone moving is a heartbeat, not a change — the
/// fact is the verdicts plus what they were anchored to.
pub(crate) fn changed(stored: Option<&Value>, fresh: &Value) -> bool {
    let strip = |v: &Value| {
        let mut c = v.clone();
        if let Some(o) = c.as_object_mut() {
            o.remove("checked_at");
        }
        c
    };
    match stored {
        None => true,
        Some(s) => strip(s) != strip(fresh),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch repo with main + two branches: `touches-a` conflicts
    /// with main (main moved the same line) and `touches-b` is clean vs
    /// main but conflicts with a third branch `also-b` on file b.txt —
    /// the real shapes, proven against real git rather than fixtures of
    /// what we hope merge-tree prints.
    fn scratch_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tmpdir");
        let p = dir.path().to_str().unwrap().to_string();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&p)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let write = |name: &str, content: &str| {
            std::fs::write(dir.path().join(name), content).expect("write");
        };
        git(&["init", "-b", "main"]);
        write("a.txt", "base-a\n");
        write("b.txt", "base-b\n");
        git(&["add", "."]);
        git(&["commit", "-m", "base"]);
        git(&["branch", "touches-a"]);
        git(&["branch", "touches-b"]);
        git(&["branch", "also-b"]);
        // main moves a.txt (so touches-a will conflict with it).
        write("a.txt", "main-a\n");
        git(&["commit", "-am", "main moves a"]);
        git(&["checkout", "touches-a"]);
        write("a.txt", "branch-a\n");
        git(&["commit", "-am", "branch moves a"]);
        git(&["checkout", "touches-b"]);
        write("b.txt", "branch-b\n");
        git(&["commit", "-am", "branch moves b"]);
        git(&["checkout", "also-b"]);
        write("b.txt", "other-b\n");
        git(&["commit", "-am", "other moves b"]);
        git(&["checkout", "main"]);
        dir
    }

    #[test]
    fn trial_merge_tells_clean_from_conflict_and_names_the_file() {
        let repo = scratch_repo();
        let p = repo.path().to_str().unwrap();
        match trial_merge(p, "main", "touches-a").unwrap() {
            Verdict::Conflicts(files) => assert_eq!(files, vec!["a.txt".to_string()]),
            v => panic!("expected conflict on a.txt, got {v:?}"),
        }
        assert_eq!(trial_merge(p, "main", "touches-b").unwrap(), Verdict::Clean);
        // The co-boarder class: both clean vs main is not the question.
        match trial_merge(p, "touches-b", "also-b").unwrap() {
            Verdict::Conflicts(files) => assert_eq!(files, vec!["b.txt".to_string()]),
            v => panic!("expected the co-boarder conflict, got {v:?}"),
        }
    }

    #[test]
    fn a_missing_ref_is_an_error_not_a_clean() {
        let repo = scratch_repo();
        let p = repo.path().to_str().unwrap();
        assert!(trial_merge(p, "main", "no-such-branch").is_err());
    }

    #[test]
    fn the_set_hash_is_order_free_and_content_bound() {
        let repo = scratch_repo();
        let p = repo.path().to_str().unwrap();
        let ab = vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ];
        let ba = vec![
            ("b".to_string(), "2".to_string()),
            ("a".to_string(), "1".to_string()),
        ];
        let moved = vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "3".to_string()),
        ];
        assert_eq!(set_hash(p, &ab).unwrap(), set_hash(p, &ba).unwrap());
        assert_ne!(set_hash(p, &ab).unwrap(), set_hash(p, &moved).unwrap());
    }

    #[test]
    fn only_a_fact_change_counts_as_changed() {
        let fresh = preview_payload(&Verdict::Clean, &[], "m1", "s1", "T1");
        let heartbeat = preview_payload(&Verdict::Clean, &[], "m1", "s1", "T2");
        let moved = preview_payload(&Verdict::Clean, &[], "m2", "s1", "T2");
        assert!(changed(None, &fresh));
        assert!(
            !changed(Some(&fresh), &heartbeat),
            "checked_at alone is a heartbeat"
        );
        assert!(changed(Some(&fresh), &moved), "a moved anchor is a change");
    }
}
