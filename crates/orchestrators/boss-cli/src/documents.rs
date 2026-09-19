//! The platform document bundle — what an agent profile is told, read
//! from `infra/platform/documents/<profile>-rules.md` in the checkout
//! the verb stands in.
//!
//! WHY THIS EXISTS (design c87fb59b car 2, backlog 39d0b528). Until
//! 2026-09-18 the rules a builder was briefed with lived in a
//! scratchpad file under /tmp — `builders/RULES.md` — pasted into every
//! prompt by hand: unversioned, untested, one pod restart from gone,
//! and edited in place without a diff anyone reviewed. The rules are
//! per PROFILE, not per step (a `builder` is briefed the same way on a
//! backlog-item's `build` and a user-feedback's), so the document is
//! keyed by the profile the step's `agent` block declares (car 1,
//! `agent_spec`), and it rides the tree like every other platform
//! bundle. `boss brief` prints it after the packet and the invariants;
//! `boss dispatch` prints the same as the last part of the prompt.
//!
//! READ FROM THE CHECKOUT, NOT COMPILED IN. The invariants beside it
//! are derived from the files in the worktree the verb stands in
//! (`brief::invariants`), and the document follows the same rule: a
//! builder dispatched from a worktree reads the rules of the tree it
//! builds, and an edit here is live at the next dispatch with no seed
//! and no deploy. `include_str!` would have pinned the rules to
//! whichever tree the binary was built from, which on the pod is not
//! always the tree being briefed. The README in the directory says why
//! it is a directory and not a registry row.

use anyhow::{Context, Result};
use std::path::Path;

/// Repo-relative home of the bundle.
pub(crate) const DIR: &str = "infra/platform/documents";

/// The profile a step is briefed under when it declares none. The
/// only profile with a document today; a step that declares another
/// gets a one-line note naming the file that would serve it.
pub(crate) const DEFAULT_PROFILE: &str = "builder";

/// The repo-relative path of a profile's rules document.
pub(crate) fn path_for(profile: &str) -> String {
    format!("{DIR}/{profile}-rules.md")
}

/// The `profile:` a document's front-matter names, or `None` when the
/// file carries no front-matter block.
pub(crate) fn profile_of(doc: &str) -> Option<String> {
    let mut lines = doc.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    lines
        .take_while(|l| l.trim() != "---")
        .find_map(|l| l.trim().strip_prefix("profile:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The document without its front-matter block — what a reader is
/// shown.
pub(crate) fn body(doc: &str) -> String {
    let mut lines = doc.lines();
    if lines.next().map(str::trim) != Some("---") {
        return doc.to_string();
    }
    let rest: Vec<&str> = lines.skip_while(|l| l.trim() != "---").skip(1).collect();
    let mut out = rest.join("\n");
    out.push('\n');
    out.trim_start_matches('\n').to_string()
}

/// A profile's document, read from `repo`: `Ok(None)` when no file
/// serves that profile, `Err` when the file exists but does not name
/// the profile it is filed under — a document under the wrong name
/// is worse than none.
pub(crate) fn read(repo: &Path, profile: &str) -> Result<Option<String>> {
    let rel = path_for(profile);
    let path = repo.join(&rel);
    if !path.is_file() {
        return Ok(None);
    }
    let doc = std::fs::read_to_string(&path).with_context(|| format!("reading {rel}"))?;
    match profile_of(&doc) {
        Some(p) if p == profile => Ok(Some(doc)),
        Some(p) => {
            anyhow::bail!("{rel} is filed for profile `{profile}` but its front-matter names `{p}`")
        }
        None => anyhow::bail!("{rel} has no front-matter naming its profile"),
    }
}

/// The rules section of a brief: the document's body under a header
/// that names the profile and the file, or the one line that says
/// which file would serve a profile that has none.
pub(crate) fn section(repo: &Path, profile: &str) -> Result<String> {
    Ok(match read(repo, profile)? {
        Some(doc) => format!(
            "== THE RULES — profile `{profile}`, from {} ==\n\n{}",
            path_for(profile),
            body(&doc)
        ),
        None => format!(
            "== THE RULES — profile `{profile}` has no document ==\n\n\
             No file serves this profile. One would live at {} with a front-matter \
             `profile: {profile}` line.\n",
            path_for(profile)
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The repo this test's own crate lives in — three levels up from
    /// `crates/orchestrators/boss-cli`.
    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .expect("the workspace root is above this crate")
    }

    /// THE BUNDLE PIN: the builder document exists, is filed under the
    /// profile it names, and names the gate's own checks — the reason
    /// a builder is briefed with it at all. The check names are read
    /// from the gate the way the brief derives them, not retyped.
    #[test]
    fn the_builder_document_exists_and_names_the_gates_checks() {
        let doc = read(&repo(), "builder")
            .expect("readable")
            .expect("infra/platform/documents/builder-rules.md is authored");
        assert_eq!(profile_of(&doc).as_deref(), Some("builder"));
        let text = body(&doc);
        let gate = std::fs::read_to_string(repo().join("infra/gate.sh")).expect("infra/gate.sh");
        for phase in ["fmt", "clippy", "svelte-check"] {
            assert!(
                crate::brief::gate_phases(&gate).iter().any(|p| p == phase),
                "the gate runs {phase}"
            );
            assert!(text.contains(phase), "the builder rules name `{phase}`");
        }
        for door in [
            "wt-cargo",
            "infra/gate.sh --quick",
            "boss gate",
            "BOSS_AGENT_RUN",
        ] {
            assert!(text.contains(door), "the builder rules name `{door}`");
        }
        assert!(
            !text.starts_with("---"),
            "the body is the document without its front-matter"
        );
    }

    /// The three sentences three builders left on backlog dafa4203
    /// (2026-09-19), each a gate a builder reddened or nearly did:
    ///
    /// - 1b847556's first gate went red because a lint-only car (no
    ///   Rust crate touched) changed what the boss-testing pins
    ///   observe — they drive `infra/lint/*.sh` on the real bundle —
    ///   and the builder read "the touched crates' whole suite" as
    ///   none. The directories the rule names are read back from the
    ///   pins themselves: each one must be a real directory that at
    ///   least one boss-testing test references, so the list cannot
    ///   name a directory nothing pins.
    /// - 1b52c278 squashed with `reset --soft origin/main` after
    ///   another session's fetch had moved that ref, and folded the
    ///   other train's landed work into its commit as a reversal —
    ///   caught only by the diff read before push.
    /// - 0f2ecbda's `scratchpad/commit-msg.txt` was overwritten by a
    ///   concurrent builder's: the scratchpad root is shared.
    #[test]
    fn the_builder_document_names_what_an_infra_only_car_must_still_run() {
        let doc = read(&repo(), "builder")
            .expect("readable")
            .expect("infra/platform/documents/builder-rules.md is authored");
        let text = body(&doc);
        assert!(
            text.contains("crates/core/boss-testing"),
            "the builder rules name the crate whose pins drive infra scripts"
        );
        let tests = repo().join("crates/core/boss-testing/tests");
        let pins: Vec<String> = std::fs::read_dir(&tests)
            .expect("boss-testing/tests")
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap_or_default())
            .collect();
        for dir in [
            "infra/lint/",
            "infra/forge/",
            "infra/ops/",
            "infra/dev/",
            "infra/platform/",
        ] {
            assert!(repo().join(dir).is_dir(), "{dir} is a directory");
            assert!(
                pins.iter().any(|t| t.contains(dir.trim_end_matches('/'))),
                "a boss-testing pin reads {dir}"
            );
            assert!(
                text.contains(dir),
                "the builder rules name {dir} as pinned by boss-testing"
            );
        }
        for phrase in [
            "wt-cargo test -p boss-testing --all-features",
            "reset --soft origin/main",
            "git diff --stat origin/main...HEAD",
            "merge-base",
            "scratchpad/builders/<packet id>/",
        ] {
            assert!(text.contains(phrase), "the builder rules say `{phrase}`");
        }
        // 8d054cb2 (2026-09-19): the rule prescribed the TWO-dot form,
        // which diffs the tips, so every commit that landed on main while
        // a builder worked was reported back as the branch's own deletions
        // (car 6e738252: two-dot said 47 files / 5235 deletions, three-dot
        // said 7 files / 16 deletions, and the branch was one commit
        // behind). That false alarm invites the merge-or-reset this same
        // rule forbids, so no occurrence may be left in tip-to-tip form.
        let check = "git diff --stat origin/main";
        for (at, _) in text.match_indices(check) {
            let three_dot = text[at + check.len()..].starts_with("...HEAD");
            let named_wrong = text[..at].ends_with("the two-dot `");
            assert!(
                three_dot || named_wrong,
                "a tip-to-tip diff check survives in the builder rules at byte {at}"
            );
        }
    }

    #[test]
    fn a_profile_with_no_document_is_a_line_that_names_the_file() {
        let s = section(&repo(), "no-such-profile").expect("a missing document is not an error");
        assert!(s.contains("has no document"), "{s}");
        assert!(
            s.contains("infra/platform/documents/no-such-profile-rules.md"),
            "{s}"
        );
    }

    #[test]
    fn the_section_names_the_profile_and_the_file_it_came_from() {
        let s = section(&repo(), "builder").expect("the builder rules render");
        assert!(s.starts_with(
            "== THE RULES — profile `builder`, from infra/platform/documents/builder-rules.md =="
        ));
        assert!(s.contains("# Builder rules"));
    }

    #[test]
    fn front_matter_is_parsed_and_stripped() {
        let doc = "---\nprofile: analyst\n---\n\n# Analyst rules\n\n1. Read.\n";
        assert_eq!(profile_of(doc).as_deref(), Some("analyst"));
        assert_eq!(body(doc), "# Analyst rules\n\n1. Read.\n");
        assert_eq!(profile_of("# No front matter\n"), None);
        assert_eq!(body("# No front matter\n"), "# No front matter\n");
        assert_eq!(profile_of("---\nowner: x\n---\n"), None);
    }

    /// A file under one profile's name that names another is refused
    /// rather than served: the reader would be told the wrong rules
    /// under a confident header.
    #[test]
    fn a_document_filed_under_the_wrong_profile_is_refused() {
        let dir = boss_testing::scratch_dir("documents-wrong-profile");
        let bundle = dir.join(DIR);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            bundle.join("builder-rules.md"),
            "---\nprofile: analyst\n---\n\n# not a builder\n",
        )
        .unwrap();
        let err = read(&dir, "builder").expect_err("refused");
        assert!(err.to_string().contains("names `analyst`"), "{err}");
        std::fs::write(bundle.join("builder-rules.md"), "# no front matter\n").unwrap();
        let err = read(&dir, "builder").expect_err("refused");
        assert!(err.to_string().contains("no front-matter"), "{err}");
    }
}
