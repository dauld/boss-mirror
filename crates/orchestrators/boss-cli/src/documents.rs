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

/// The profile a step is briefed under when it declares none. Two
/// profiles have a document today — `builder` and `analyst`, the two
/// settings the platform bundle declares (8d32cc88) — and a step
/// declaring any other gets a one-line note naming the file that
/// would serve it, which is what every analyst dispatch got until
/// then. The test below reads that roster off the bundle, so the
/// count in this sentence is not a third copy of it.
pub(crate) const DEFAULT_PROFILE: &str = "builder";

/// The repo-relative path of a profile's rules document.
pub(crate) fn path_for(profile: &str) -> String {
    format!("{DIR}/{profile}-rules.md")
}

/// One key of a document's front-matter block, or `None` when the
/// file carries no block or the block does not name that key.
pub(crate) fn front_matter(doc: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let mut lines = doc.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    lines
        .take_while(|l| l.trim() != "---")
        .find_map(|l| l.trim().strip_prefix(prefix.as_str()))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The `profile:` a document's front-matter names, or `None` when the
/// file carries no front-matter block.
pub(crate) fn profile_of(doc: &str) -> Option<String> {
    front_matter(doc, "profile")
}

/// The LANE a document declares — which invariants the profile it
/// serves can actually act on (backlog c8faa7f3, 2026-09-19).
///
/// The invariants half of a brief was one set for everybody: the gate's
/// phase list, its uid and gid, fixture paths, the base check, the
/// probe-time rule — about forty lines, all of them about shipping a
/// car. The page-march pilot's first analyst dispatch (run d5e0f287)
/// measured what that costs the OTHER profile: an analyst ships no car,
/// enters no gate and pushes nothing, so not one of those lines was
/// actionable, and they crowded out what was.
///
/// The lane is declared HERE, in the front-matter beside the rules it
/// goes with, because a profile's rules and the invariants it is
/// briefed with are one editorial decision — and adding a profile stays
/// what the README says it is: dropping one file in.
pub(crate) fn lane_of(doc: &str) -> Option<String> {
    front_matter(doc, "lane")
}

/// The lane a brief renders under when no document declares one: the
/// lane that ships a car, which is what every brief carried before
/// c8faa7f3. A profile with no document — or a document with no `lane:`
/// line — is briefed exactly as it was.
pub(crate) const DEFAULT_LANE: &str = "car";

/// The lane for `profile`, read from its document in `repo`.
///
/// A lane no invariant is filed under is REFUSED, naming the file and
/// the lanes there are: rendered instead, it would print an empty
/// invariants section under a confident header — a brief that says
/// nothing where it used to say forty lines, and says it silently.
pub(crate) fn lane(repo: &Path, profile: &str) -> Result<String> {
    let declared = read(repo, profile)?
        .as_deref()
        .and_then(lane_of)
        .unwrap_or_else(|| DEFAULT_LANE.to_string());
    if !crate::brief::LANES.contains(&declared.as_str()) {
        anyhow::bail!(
            "{} declares lane `{declared}`, which no invariant is filed under; the lanes \
             are {:?}",
            path_for(profile),
            crate::brief::LANES
        );
    }
    Ok(declared)
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

/// How a rules document QUOTES an invariant instead of restating it:
/// `{{invariant:<name>}}` on a line of its own, expanded by `expand`
/// into that invariant's own lines (backlog 395d24ad, 2026-09-19).
///
/// THE CASE. The base-check advice lived twice — the `base check`
/// invariant in `brief.rs`, and rule 1 of the builder document, which
/// taught the same check in different words. Both said the tip-to-tip
/// `diff`, and both were corrected on the SAME DAY by two different
/// builders in two separate cars (8d054cb2 for the document, 9843aeb9
/// for the invariant), hours apart; the second only learned the first
/// existed while reading for its pin. Each car left a pin correctly
/// scoped to its own copy, which stops each copy drifting alone and
/// does nothing about them drifting APART — two pins that cannot see
/// each other. CLAUDE.md 9a is explicit that a pin is a holding
/// action and the collapse is the destination, and advice carries no
/// information in its second copy, so the document now quotes the
/// door rather than restating it.
pub(crate) const OPEN: &str = "{{invariant:";
pub(crate) const CLOSE: &str = "}}";

/// Every `{{invariant:<name>}}` in `body` replaced by that
/// invariant's lines, indented four spaces as the brief indents them.
///
/// A name no invariant carries is REFUSED, naming the names there
/// are: rendered through, it would hand a builder a literal
/// placeholder under a confident header — the silence class, and the
/// one way a collapse can be worse than the duplication it replaced.
pub(crate) fn expand(body: &str, invariants: &[crate::brief::Invariant]) -> Result<String> {
    let mut out = String::new();
    let mut rest = body;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let tail = &rest[at + OPEN.len()..];
        let Some(end) = tail.find(CLOSE) else {
            anyhow::bail!("unterminated {OPEN} in a rules document: no {CLOSE} closes it");
        };
        let name = tail[..end].trim();
        let Some(inv) = invariants.iter().find(|i| i.name == name) else {
            anyhow::bail!(
                "a rules document quotes the invariant `{name}`, which this tree does not \
                 carry; the invariants are {:?}",
                invariants.iter().map(|i| i.name).collect::<Vec<_>>()
            );
        };
        for line in &inv.lines {
            out.push_str(&format!("    {line}\n"));
        }
        // The lines each carry their own newline, so the one that
        // ended the placeholder's line would open a blank one.
        rest = tail[end + CLOSE.len()..]
            .strip_prefix('\n')
            .unwrap_or(&tail[end + CLOSE.len()..]);
    }
    out.push_str(rest);
    Ok(out)
}

/// The rules section of a brief: the document's body under a header
/// that names the profile and the file, or the one line that says
/// which file would serve a profile that has none.
///
/// Takes the invariants the brief already derived rather than deriving
/// them again — one reading of `infra/gate.sh --roster` per brief, and
/// the rules a builder reads cannot quote a different set from the one
/// printed above them.
pub(crate) fn section(
    repo: &Path,
    profile: &str,
    invariants: &[crate::brief::Invariant],
) -> Result<String> {
    Ok(match read(repo, profile)? {
        Some(doc) => format!(
            "== THE RULES — profile `{profile}`, from {} ==\n\n{}",
            path_for(profile),
            expand(&body(&doc), invariants)?
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
        // The pre-flight door is NOT in this list: it is quoted from
        // CLAUDE.md §Doors rather than named here, and
        // `the_builder_document_quotes_the_preflight_door` reads it
        // out of the file that decides it (backlog 5d919334).
        for door in ["wt-cargo", "boss gate", "BOSS_AGENT_RUN"] {
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
            "scratchpad/builders/<packet id>/",
        ] {
            assert!(text.contains(phrase), "the builder rules say `{phrase}`");
        }
    }

    /// THE ANALYST DOCUMENT — what the OTHER profile owes (backlog
    /// 8d32cc88, 2026-09-19). Eleven steps in the platform bundle
    /// declare `analyst` and every one of them was dispatched with the
    /// one-line note naming the file that would have served it; the
    /// page march is about to add two more at 47 packets of volume.
    /// An analyst ships no car, so the phrases pinned here are the
    /// ones its own failures are made of: the empty read that is a
    /// denied scope rather than data, the wholesale `metadata`
    /// replacement a step PUT performs, the gap list longer than the
    /// ids filed, and the context a sign-off reader sees instead of
    /// the work steps.
    #[test]
    fn the_analyst_document_says_what_evidence_and_refusal_mean() {
        let doc = read(&repo(), "analyst")
            .expect("readable")
            .expect("infra/platform/documents/analyst-rules.md is authored");
        assert_eq!(profile_of(&doc).as_deref(), Some("analyst"));
        let text = body(&doc);
        for phrase in [
            // The doors it writes through (each checked against the
            // CLI's own roster by the test below).
            "boss-api PUT /api/jobs/",
            "boss job file",
            // The lane a filing RECORDS, required of a backlog-item
            // (c5dc81a1) — a required flag whose door document does not
            // name it is a trap the analyst walks into once per filing.
            "--channel",
            "boss job patch",
            // The evidence rules, in the words the incidents left.
            "total",
            "wholesale",
            "read it back",
            "control",
            "count",
            "Measure now",
            "sign_off_context",
            "context_md",
            "single quotes",
        ] {
            assert!(text.contains(phrase), "the analyst rules say `{phrase}`");
        }
        // NOT A FORK OF THE BUILDER'S (the packet's own instruction):
        // an analyst has no worktree, no branch and no gate, so the
        // builder's doors must not appear here at all. A document that
        // tells an analyst to run cargo has been copied, not written.
        for builders_only in ["wt-cargo", "git push", "--park-", "cargo clippy"] {
            assert!(
                !text.contains(builders_only),
                "the analyst rules carry the builder's `{builders_only}`"
            );
        }
        assert!(
            !text.starts_with("---"),
            "the body is the document without its front-matter"
        );
    }

    /// The doors the analyst document names are doors that EXIST: the
    /// `boss job` verbs it points at are variants of `JobAction`, and
    /// the jobs-API shim it invokes bare is the versioned one under
    /// infra/dev/. A rules document naming a verb nobody shipped sends
    /// the agent to build the path by hand, which is the class the
    /// doors list exists to close (CLAUDE.md, Doors).
    #[test]
    fn the_analyst_documents_doors_are_doors_that_exist() {
        let text = body(
            &read(&repo(), "analyst")
                .expect("readable")
                .expect("the analyst rules are authored"),
        );
        let cli = std::fs::read_to_string(repo().join("crates/orchestrators/boss-cli/src/main.rs"))
            .expect("the CLI's command roster");
        let actions = cli
            .split_once("enum JobAction")
            .expect("`boss job` groups its verbs in JobAction")
            .1;
        for (verb, variant) in [
            ("boss job file", "    File {"),
            ("boss job patch", "    Patch {"),
        ] {
            assert!(text.contains(verb), "the analyst rules name `{verb}`");
            assert!(actions.contains(variant), "`{verb}` is a JobAction variant");
        }
        assert!(
            repo().join("infra/dev/boss-api").is_file(),
            "the jobs-API shim the rules invoke is versioned in the tree"
        );
        assert!(text.contains("boss-api"), "the analyst rules name the shim");
    }

    /// THE VERB IS THE DOOR AND THE PUT IS THE FALLBACK (backlog
    /// d1c03a44, 2026-09-19). This document is read once per analyst
    /// dispatch and the page march is about to be ~94 of them, so a
    /// mechanics section that teaches the hand-built PUT is ~94
    /// opportunities for the three silent failures `boss step complete`
    /// exists to refuse: an undeclared name stored as an annotation, a
    /// wholesale `metadata` replace, and a 204 read as evidence.
    ///
    /// Pinned BY NAME: the verb, the file door its long fields need
    /// (`controls_md`, `needs_md`, `gaps_md` are whole documents), and
    /// that the verb named is a `StepAction` variant that ships. The
    /// raw PUT's hazards stay pinned by the test above — the fallback
    /// keeps them on the page, it does not delete them.
    #[test]
    fn the_analyst_document_names_the_generic_completion_verb() {
        let text = body(
            &read(&repo(), "analyst")
                .expect("readable")
                .expect("the analyst rules are authored"),
        );
        for phrase in ["boss step complete", "--field ", "--field-file"] {
            assert!(text.contains(phrase), "the analyst rules name `{phrase}`");
        }
        let steps =
            std::fs::read_to_string(repo().join("crates/orchestrators/boss-cli/src/steps.rs"))
                .expect("the step verbs' module");
        let actions = steps
            .split_once("enum StepAction")
            .expect("`boss step` groups its verbs in StepAction")
            .1;
        assert!(
            actions.contains("    Complete {"),
            "`boss step complete` is a StepAction variant"
        );
        // ONE PUT, NOT TWO. The freeze in `boss-jobs/src/http/steps.rs`
        // is gated on the step's OLD status, so a step still ready
        // takes its metadata and its completion in one body; a
        // document that teaches a write and THEN a completion teaches
        // a second write the API answers with a 409.
        assert!(
            text.contains("one PUT"),
            "the analyst rules say a completion is one PUT, not two"
        );
    }

    /// THE ROSTER IS THE BUNDLE, NOT A LIST HERE (CLAUDE.md 9a). The
    /// profiles that need a document are exactly the ones the platform
    /// Workflow rows' `agent` blocks declare, so this reads them from
    /// the bundle rather than naming them: a step that starts declaring
    /// a third profile fails here until a document serves it, instead
    /// of dispatching an agent with no rules at all — which is how
    /// `analyst` went eleven steps unserved.
    #[test]
    fn every_profile_the_platform_bundle_declares_has_a_document() {
        let bundle =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .expect("the platform bundle parses");
        let profiles: std::collections::BTreeSet<String> = bundle
            .iter()
            .flat_map(|w| w.steps.iter().filter_map(|s| s.agent.as_ref()))
            .map(|a| a.profile.clone())
            .collect();
        assert!(
            profiles.contains(DEFAULT_PROFILE) && profiles.contains("analyst"),
            "the two settings in use are declared in the bundle: {profiles:?}"
        );
        let invs = crate::brief::invariants(&repo()).expect("the invariants derive");
        for profile in &profiles {
            let rendered = section(&repo(), profile, &invs).expect("the section renders");
            assert!(
                !rendered.contains("has no document"),
                "{} declares profile `{profile}` and nothing serves it",
                path_for(profile)
            );
        }
    }

    #[test]
    fn a_profile_with_no_document_is_a_line_that_names_the_file() {
        let invs = crate::brief::invariants(&repo()).expect("the invariants derive");
        let s =
            section(&repo(), "no-such-profile", &invs).expect("a missing document is not an error");
        assert!(s.contains("has no document"), "{s}");
        assert!(
            s.contains("infra/platform/documents/no-such-profile-rules.md"),
            "{s}"
        );
    }

    #[test]
    fn the_section_names_the_profile_and_the_file_it_came_from() {
        let invs = crate::brief::invariants(&repo()).expect("the invariants derive");
        let s = section(&repo(), "builder", &invs).expect("the builder rules render");
        assert!(s.starts_with(
            "== THE RULES — profile `builder`, from infra/platform/documents/builder-rules.md =="
        ));
        assert!(s.contains("# Builder rules"));
    }

    /// THE LANE IS DECLARED BY EVERY DOCUMENT IN THE BUNDLE
    /// (c8faa7f3). A profile whose document names no lane is briefed
    /// with the car lane's forty lines — which is exactly what the
    /// analyst dispatch measured — so the bundle is walked rather than
    /// the two known names being asserted: a third document dropped in
    /// without a `lane:` line fails here instead of reaching an agent
    /// with the other profile's invariants.
    #[test]
    fn every_document_in_the_bundle_declares_the_lane_it_is_briefed_under() {
        let dir = repo().join(DIR);
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("the document bundle") {
            let path = entry.expect("a bundle entry").path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(profile) = name.strip_suffix("-rules.md") else {
                continue;
            };
            let doc = std::fs::read_to_string(&path).expect("the document reads");
            let declared = lane_of(&doc)
                .unwrap_or_else(|| panic!("{name} declares no `lane:` in its front-matter"));
            assert!(
                crate::brief::LANES.contains(&declared.as_str()),
                "{name} declares lane `{declared}`, which no invariant is filed under: {:?}",
                crate::brief::LANES
            );
            assert_eq!(
                lane(&repo(), profile).expect("the lane reads"),
                declared,
                "the lane read for `{profile}` is the one its document declares"
            );
            seen += 1;
        }
        assert!(seen >= 2, "the bundle has both documents, saw {seen}");
        // The two settings in use, and they are NOT the same lane —
        // the whole point of the key.
        assert_eq!(lane(&repo(), "builder").expect("builder"), "car");
        assert_eq!(lane(&repo(), "analyst").expect("analyst"), "step");
        // A profile nothing serves is briefed as it always was.
        assert_eq!(
            lane(&repo(), "no-such-profile").expect("no document is not an error"),
            DEFAULT_LANE
        );
    }

    /// A lane nothing is filed under is refused rather than rendered:
    /// the reader would get an empty invariants section under a
    /// confident header, which is silence where forty lines were.
    #[test]
    fn a_document_declaring_a_lane_no_invariant_uses_is_refused() {
        let dir = boss_testing::scratch_dir("documents-unknown-lane");
        let bundle = dir.join(DIR);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            bundle.join("courier-rules.md"),
            "---\nprofile: courier\nlane: sidings\n---\n\n# Courier rules\n",
        )
        .unwrap();
        let err = lane(&dir, "courier").expect_err("refused");
        assert!(err.to_string().contains("`sidings`"), "{err}");
        assert!(err.to_string().contains("courier-rules.md"), "{err}");
    }

    #[test]
    fn front_matter_is_parsed_and_stripped() {
        let doc = "---\nprofile: analyst\nlane: step\n---\n\n# Analyst rules\n\n1. Read.\n";
        assert_eq!(front_matter(doc, "lane").as_deref(), Some("step"));
        assert_eq!(lane_of(doc).as_deref(), Some("step"));
        assert_eq!(lane_of("---\nprofile: builder\n---\n"), None);
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

    /// The placeholder as a document writes it. Built rather than
    /// typed, because `{` doubles inside a format string and a test
    /// that typed the literal would be asserting a different string
    /// from the one `expand` looks for.
    fn quote_of(name: &str) -> String {
        format!("{OPEN}{name}{CLOSE}")
    }

    /// THE COLLAPSE (backlog 395d24ad, 2026-09-19). The base-check
    /// advice lived twice: the `base check` invariant in `brief.rs`
    /// and rule 1 of this document, teaching the same check in
    /// different words. Both said the tip-to-tip form, and both were
    /// corrected on the SAME DAY by two different builders in two
    /// separate cars (8d054cb2 for the document, 9843aeb9 for the
    /// invariant) — the second only learning the first existed while
    /// reading for its pin. Each car left its own pin, correctly
    /// scoped to its own copy, which stopped each copy drifting alone
    /// and did nothing about them drifting APART. So the document now
    /// QUOTES the invariant instead of restating it, and the
    /// restatement is refused here: one definition, one pin (CLAUDE.md
    /// 9a — a pin is a holding action, the collapse is the
    /// destination).
    #[test]
    fn the_builder_rules_quote_the_base_check_invariant_rather_than_restating_it() {
        let text = body(
            &read(&repo(), "builder")
                .expect("readable")
                .expect("the builder rules are authored"),
        );
        assert!(
            text.contains(&quote_of("base check")),
            "rule 1 quotes the base-check invariant rather than restating it"
        );
        // No second copy of the commands, in either flag spelling.
        for restated in [
            "merge-base --is-ancestor",
            "git diff --numstat origin/main",
            "git diff --stat origin/main",
        ] {
            assert!(
                !text.contains(restated),
                "the builder rules restate the base check's `{restated}`; quote the \
                 invariant instead (395d24ad)"
            );
        }
        // And what a builder actually reads carries the invariant's
        // own lines, verbatim.
        let invs = crate::brief::invariants(&repo()).expect("the invariants derive");
        let inv = invs
            .iter()
            .find(|i| i.name == "base check")
            .expect("a base-check invariant");
        let rendered = section(&repo(), "builder", &invs).expect("the section renders");
        for line in &inv.lines {
            assert!(
                rendered.contains(line.as_str()),
                "the rendered rules drop the invariant's line `{line}`"
            );
        }
        assert!(
            !rendered.contains(OPEN),
            "an unexpanded placeholder reached a reader: {rendered}"
        );
    }

    /// A placeholder naming an invariant this tree does not carry is
    /// REFUSED, naming the names there are. Rendered through, it would
    /// hand a builder a literal placeholder under a confident header —
    /// the silence class, and the one way a collapse can be worse than
    /// the duplication it replaced.
    #[test]
    fn a_document_quoting_an_invariant_that_does_not_exist_is_refused() {
        let invs = crate::brief::invariants(&repo()).expect("the invariants derive");
        let err = expand(
            &format!("before\n\n{}\n\nafter\n", quote_of("base chck")),
            &invs,
        )
        .expect_err("an unknown invariant name is refused");
        assert!(err.to_string().contains("base chck"), "{err}");
        assert!(err.to_string().contains("base check"), "{err}");
        let err = expand(&format!("{OPEN}base check\n"), &invs).expect_err("unterminated");
        assert!(err.to_string().contains("unterminated"), "{err}");
    }

    #[test]
    fn a_quoted_invariant_is_indented_as_a_block_and_the_prose_around_it_is_kept() {
        let invs = vec![crate::brief::Invariant {
            name: "base check",
            authority: "infra/gate.sh".into(),
            lines: vec!["one".into(), "two".into()],
            lanes: vec![crate::brief::LANE_CAR],
            grounding: crate::brief::Grounding::Written,
        }];
        assert_eq!(
            expand(
                &format!("before\n\n{}\n\nafter\n", quote_of("base check")),
                &invs
            )
            .expect("expands"),
            "before\n\n    one\n    two\n\nafter\n"
        );
        assert_eq!(
            expand("nothing to do\n", &invs).expect("expands"),
            "nothing to do\n"
        );
    }

    /// THE PRE-FLIGHT DOOR IS QUOTED, NOT RESTATED (backlog 5d919334,
    /// 2026-09-22).
    ///
    /// Rules 4, 9 and 16 each named `infra/gate.sh --quick` in their
    /// own words — three copies of a door CLAUDE.md §Doors had already
    /// moved to `--lint`, after `--quick` alone cost two gates to
    /// clippy errors (410e21e2). Two builders hit the contradiction
    /// within one hour on 2026-09-22 and both judged the tree correct;
    /// a builder who did not notice would run the weaker door and pay
    /// for it at the gate. The whole contract of this document is that
    /// its invariants are DERIVED from the files that decide them, so
    /// the fix is 395d24ad's placeholder, not a corrected literal.
    ///
    /// The negative half is the load-bearing one: no gate.sh mode may
    /// be spelled in the body at all, because a second spelling is
    /// exactly what drifted.
    #[test]
    fn the_builder_document_quotes_the_preflight_door() {
        let doc = read(&repo(), "builder")
            .expect("readable")
            .expect("infra/platform/documents/builder-rules.md is authored");
        let text = body(&doc);
        assert!(
            text.contains("{{invariant:pre-flight}}"),
            "the builder rules no longer quote the pre-flight invariant"
        );
        assert!(
            !text.contains("infra/gate.sh --"),
            "the builder rules spell a gate.sh mode of their own; CLAUDE.md §Doors              decides it and {}pre-flight{} quotes it",
            OPEN,
            CLOSE
        );

        let invs = crate::brief::invariants(&repo()).expect("the invariants derive from this tree");
        let expanded = expand(&text, &invs).expect("the placeholder expands");
        let claude = std::fs::read_to_string(repo().join("CLAUDE.md")).expect("CLAUDE.md");
        let door = crate::brief::preflight_door(&claude).expect("§Doors names a door");
        assert!(
            expanded.contains(&door),
            "a builder reading the expanded rules never sees the door §Doors names: `{door}`"
        );
    }
}
