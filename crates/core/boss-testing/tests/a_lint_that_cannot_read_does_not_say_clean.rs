//! A lint that could not read the tree must not say `clean`.
//!
//! MEASURED 2026-09-11 (backlog 6b2f4a1a). git was refusing every command
//! in a gate workspace — `fatal: detected dubious ownership`, exit 128, as
//! uid 65534 — and of the seven lints in the pre-flight roster that ask git
//! a question, SIX answered wrongly and four of those answered green:
//!
//! | lint                           | what it said on a tree it never read |
//! |--------------------------------|--------------------------------------|
//! | `no-session-paths`             | `clean`, exit 0                      |
//! | `one-palette`                  | `clean`, exit 0                      |
//! | `a-new-style-has-a-caller`     | `no trunk ref found — skipping`, 0   |
//! | `a-kind-bundle-does-not-tighten` | the same, exit 0                   |
//! | `migrations-append-only`       | `no trunk ref found … Fetch the trunk`, 1 |
//! | `steptype-bundle-ratchet`      | the same, exit 1                     |
//!
//! The trunk was present in every one of those runs. The two that exited 1
//! sent an operator after a ref that was already there; the four that
//! exited 0 are worse, because a gate records 0 as a pass and nothing
//! anywhere says the check did not happen.
//!
//! THE PROPERTY PINNED HERE, for every lint that reaches git: "git ran and
//! found nothing" and "git did not run" are different answers, and only the
//! first is `clean`. Three behaviours per lint, because a fix that only
//! made the third one work would be worse than the defect — an
//! unconditionally passing check:
//!
//!   1. git healthy, tree clean  -> exit 0, says clean
//!   2. git healthy, a REAL violation -> non-zero, NAMES the offender
//!   3. git cannot answer        -> exit 3, names the lint, the git
//!      command, the status, and git's own words; never `clean`
//!
//! Everything runs in a fixture repo this process owns: a real git repo
//! with a trunk ref, the scripts copied out of the working tree, and no
//! dependence on the ambient repository's ownership — which is the very
//! thing that was broken when this was found, and would otherwise decide
//! whether the test passes.
//!
//! Case 3 is produced two independent ways, because they reach different
//! code: `GIT_TEST_ASSUME_DIFFERENT_OWNER=1` is a real foreign-owner
//! refusal at repository discovery, and a `PATH` shim that exits 128 on a
//! named subcommand is the general form — it covers the corrupt-object and
//! unreadable-index failures ownership cannot reproduce, and it is the only
//! way to break ONE git call while leaving a lint's own git-using
//! self-test working.

use std::path::{Path, PathBuf};
use std::process::Command;

/// "I could not answer." Distinct from 0 (read the tree, it is clean) and
/// 1 (read the tree, found a violation) on purpose: an infrastructure
/// refusal is not a verdict on the branch, and exit 1 erases that
/// distinction at the only point where it is still cheap to keep.
/// `infra/lint/lib/git-answer.sh` carries the full argument.
const CANNOT_ANSWER: i32 = 3;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

/// The absolute path of the real git, resolved BEFORE the shim goes on
/// `PATH` — a shim that found itself would loop.
fn real_git() -> PathBuf {
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let candidate = Path::new(dir).join("git");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("no git on PATH — these tests drive real git");
}

struct Fixture {
    dir: PathBuf,
    /// A directory holding a `git` that refuses, for case 3.
    shim: PathBuf,
}

impl Fixture {
    /// A git repo with a `main` trunk ref, a branch checked out on top of
    /// it, and enough of the tree for every lint under test to have
    /// something real to read.
    fn new(case: &str) -> Self {
        let dir = boss_testing::scratch_dir(&format!("lint-cannot-read-{case}"));
        let root = repo_root();

        for sub in [
            "infra/lint/lib",
            "infra/postgres/schema",
            "infra/step-plugins",
            "crates/core/boss-jobs/seeds",
            "apps/web/src",
            "apps/web/tests/mocked",
            "libs/web-kit",
        ] {
            boss_testing::create_dir(&dir.join(sub));
        }

        // The scripts under test, copied from the WORKING TREE so this
        // exercises the branch rather than whatever is installed.
        for lint in LINTS {
            copy(
                &root.join("infra/lint").join(format!("{lint}.sh")),
                &dir.join("infra/lint").join(format!("{lint}.sh")),
            );
        }
        for lib in ["git-answer.sh", "trunk-ref.sh", "pattern-scan.sh"] {
            copy(
                &root.join("infra/lint/lib").join(lib),
                &dir.join("infra/lint/lib").join(lib),
            );
        }
        copy(
            &root.join("infra/lint/no-secrets-allow.txt"),
            &dir.join("infra/lint/no-secrets-allow.txt"),
        );
        // The real step-type bundle: `steptype-bundle-ratchet` refuses a
        // baseline with fewer than 10 kinds / 30 fields, so a toy file
        // would exercise the non-vacuity guard instead of the ratchet.
        copy(
            &root.join("crates/core/boss-jobs/seeds/step_types.toml"),
            &dir.join("crates/core/boss-jobs/seeds/step_types.toml"),
        );

        boss_testing::write_file(
            &dir.join("infra/postgres/schema/100-a.sql"),
            "CREATE TABLE a();\n",
        );
        boss_testing::write_file(
            &dir.join("apps/web/src/styles.css"),
            ".alpha { color: red }\n.beta { color: blue }\n",
        );
        // The caller for `.alpha` / `.beta`, so a clean fixture is clean.
        boss_testing::write_file(
            &dir.join("apps/web/src/app.ts"),
            "export const classes = ['alpha', 'beta'] as const;\n",
        );

        let shim = dir.join("shim");
        boss_testing::create_dir(&shim);
        // Breaks exactly the subcommands named in LINT_GIT_SHIM_BREAK and
        // passes everything else through, so a single git call can be made
        // to fail while a lint's own git-using self-test still runs. The
        // message is a real git failure text that ownership cannot produce.
        boss_testing::write_exec(
            &shim.join("git"),
            &format!(
                r#"#!/usr/bin/env bash
for word in ${{LINT_GIT_SHIM_BREAK:-}}; do
    if [ "$word" = "all" ] || [[ " $* " == *" $word "* ]]; then
        echo "fatal: unable to read index file .git/index: Input/output error" >&2
        exit 128
    fi
done
exec {} "$@"
"#,
                real_git().display()
            ),
        );

        let me = Fixture { dir, shim };
        me.git(&["init", "-q", "-b", "main", "."]);
        me.git(&["config", "user.email", "t@t"]);
        me.git(&["config", "user.name", "t"]);
        me.git(&["add", "-A"]);
        me.git(&["commit", "-qm", "base"]);
        // The car: the lints compare HEAD against the merge-base with the
        // trunk, so a violation has to live on a branch and be committed.
        me.git(&["checkout", "-q", "-b", "car"]);
        me
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new(real_git())
            .args(args)
            .current_dir(&self.dir)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "fixture git {args:?} failed in {}: {}{}",
            self.dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, rel: &str, body: &str) {
        boss_testing::write_file(&self.dir.join(rel), body);
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", msg]);
    }

    /// Run one lint in the fixture. `break_git` names the git subcommands
    /// a shimmed git must refuse (empty = the real git); `foreign_owner`
    /// sets the ownership refusal instead.
    fn run(&self, lint: &str, break_git: &str, foreign_owner: bool) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(format!("infra/lint/{lint}.sh"))
            .current_dir(&self.dir)
            // The lints resolve `forge/main`, `origin/main`, then `main`.
            // The fixture has only `main`, and naming it explicitly keeps
            // the test independent of any remote the fixture might grow.
            .env("BOSS_TRUNK_REF", "main")
            .env("LINT_GIT_SHIM_BREAK", break_git);
        if !break_git.is_empty() {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{}", self.shim.display(), path));
        }
        if foreign_owner {
            cmd.env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");
        }
        let out = cmd.output().unwrap_or_else(|e| panic!("spawn {lint}: {e}"));
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn copy(from: &Path, to: &Path) {
    std::fs::copy(from, to)
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
}

/// The lints this file covers: every one that asks git about the LIVE
/// tree and is converted to `lib/git-answer.sh`.
///
/// Two more lints read the live tree with git and are NOT here —
/// `a-deleted-manifest-leaves-no-object` (its `git diff` / `git show`
/// reads of deleted manifests are under `2>/dev/null`, so an unreadable
/// repo yields an empty deletion list and it reports OK) and
/// `an-expectation-names-a-rule-the-tree-declares` (`git -C "$tree"
/// ls-files` with a non-git fallback). Both are the same defect class and
/// are filed separately rather than converted blind: the first is 700
/// lines with a kubectl dependency this fixture cannot stand up, and
/// converting a check one cannot exercise is how a green that certifies
/// nothing gets shipped. Every other git call under `infra/lint/` is
/// inside a fixture repo the lint builds for itself.
const LINTS: &[&str] = &[
    "no-secrets",
    "no-session-paths",
    "one-palette",
    "migrations-append-only",
    "steptype-bundle-ratchet",
    "a-new-style-has-a-caller",
    "a-kind-bundle-does-not-tighten",
];

/// The refusal's four facts, each of which was re-derived by hand during
/// the measurement: which lint, which git command, what status, and git's
/// own words.
fn assert_refusal(lint: &str, code: i32, out: &str, git_cmd: &str, git_said: &str) {
    assert_eq!(
        code, CANNOT_ANSWER,
        "{lint} exited {code} when git could not answer; it must exit \
         {CANNOT_ANSWER} — never 0 (a gate reads 0 as a pass on a tree \
         nothing read) and never 1 (a verdict on the branch). Output:\n{out}"
    );
    // The verdict, not the word: every refusal explains that neither
    // `clean` nor a violation can be claimed, so a bare substring search
    // for "clean" matches its own remediation text.
    assert!(
        !out.contains(&format!("{lint}: clean")),
        "{lint} said `clean` on a tree it never read:\n{out}"
    );
    assert!(
        out.contains(lint),
        "the refusal does not name which lint refused:\n{out}"
    );
    assert!(
        out.contains(git_cmd),
        "the refusal does not name the git command that failed \
         (expected `{git_cmd}`):\n{out}"
    );
    assert!(
        out.contains("128"),
        "the refusal does not name git's exit status:\n{out}"
    );
    assert!(
        out.contains(git_said),
        "the refusal does not carry git's own message (expected \
         `{git_said}`) — without it the operator cannot tell an \
         ownership refusal from a corrupt object:\n{out}"
    );
}

const SHIM_SAID: &str = "unable to read index file";
const OWNER_SAID: &str = "dubious ownership";

/// The `PATH` shim is this file's own code and works anywhere. The
/// ownership refusal is GIT's, and `GIT_TEST_ASSUME_DIFFERENT_OWNER`
/// arrived in git 2.36 — on an older git it is simply ignored, and every
/// ownership sub-case below would then assert nothing while still passing.
///
/// So the mechanism is asserted ONCE, here, by name. A too-old git reds
/// this single test with a message that says which half of the coverage
/// is unavailable and why, instead of either quietly losing it or
/// reddening six tests with confusing output. (CLAUDE.md §Diagnosis: a
/// verdict must name what failed.)
#[test]
fn the_foreign_owner_refusal_is_available_on_this_git() {
    let fx = Fixture::new("owner-probe");
    let out = Command::new(real_git())
        .args(["status", "--short"])
        .current_dir(&fx.dir)
        .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")
        .output()
        .expect("spawn git status");
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let version = Command::new(real_git())
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|e| format!("<unknown: {e}>"));
    assert!(
        text.contains(OWNER_SAID),
        "GIT_TEST_ASSUME_DIFFERENT_OWNER=1 did not make git refuse a repo \
         it owns, so the ownership half of case 3 in this file proves \
         nothing: the sub-cases would run a HEALTHY git and pass. git here \
         is `{version}` (the variable needs 2.36+). The PATH-shim cases \
         still cover the property; either update git on this machine or \
         delete the ownership sub-cases rather than leaving them \
         decorative. git said: {text:?}"
    );
}

// ---------------------------------------------------------------------
// The two pattern-scan lints. One definition (lib/pattern-scan.sh) now
// covers both, which is why `one-palette` was collapsed onto it rather
// than given a second copy of the fix.
// ---------------------------------------------------------------------

#[test]
fn no_session_paths_reads_the_tree_or_says_it_could_not() {
    let fx = Fixture::new("no-session-paths");
    let lint = "no-session-paths";

    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 0, "a clean fixture must pass:\n{out}");
    assert!(
        out.contains("clean"),
        "a clean fixture must say clean:\n{out}"
    );

    // 2. A REAL violation still fails, and names the file. Without this
    //    the fix could be "always pass", which is worse than the defect.
    //
    //    ASSEMBLED, not written out: this file is tracked source under
    //    crates/, which `no-session-paths` scans, and its
    //    discovered-proof exemption only covers a path naming the lint
    //    — one file cannot name the six lints it drives. Writing the
    //    literal here reddens the lint on the real tree, which is how
    //    this comment came to be written. The fixture file still gets
    //    the real string.
    fx.write(
        "apps/web/src/bad.ts",
        &format!("const p = '/{}/someone/scratch';\n", "Users"),
    );
    fx.commit("a session path");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 1, "a tracked machine-local path must fail:\n{out}");
    assert!(
        out.contains("apps/web/src/bad.ts"),
        "the failure must name the offending file:\n{out}"
    );

    // 3. Both git failures, on both of the scan's git calls.
    let (code, out) = fx.run(lint, "ls-files", false);
    assert_refusal(lint, code, &out, "git ls-files", SHIM_SAID);
    let (code, out) = fx.run(lint, "grep", false);
    assert_refusal(lint, code, &out, "git grep", SHIM_SAID);
    let (code, out) = fx.run(lint, "", true);
    assert_refusal(lint, code, &out, "git ls-files", OWNER_SAID);
}

#[test]
fn one_palette_reads_the_tree_or_says_it_could_not() {
    let fx = Fixture::new("one-palette");
    let lint = "one-palette";

    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 0, "a clean fixture must pass:\n{out}");
    assert!(
        out.contains("clean"),
        "a clean fixture must say clean:\n{out}"
    );

    fx.write(
        "apps/web/src/theme.css",
        "@media (prefers-color-scheme: dark) { .x { color: #fff } }\n",
    );
    fx.commit("an OS theme fork");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 1, "an OS-theme branch must fail:\n{out}");
    assert!(
        out.contains("apps/web/src/theme.css"),
        "the failure must name the offending file:\n{out}"
    );

    let (code, out) = fx.run(lint, "grep", false);
    assert_refusal(lint, code, &out, "git grep", SHIM_SAID);
    let (code, out) = fx.run(lint, "", true);
    assert_refusal(lint, code, &out, "git ls-files", OWNER_SAID);
}

/// The collapse itself (§9a). `one-palette` carried its own `git grep …
/// || true` and its own hand-listed exemption for the spec that proves
/// it; both are now the shared scan's, whose discovered-proof convention
/// exempts exactly that file. A reader has to be able to see that the
/// second copy is gone, or it will grow back.
#[test]
fn the_pattern_scan_is_the_only_place_a_forbidden_pattern_is_searched_for() {
    let root = repo_root();
    for lint in ["no-session-paths", "one-palette"] {
        let body = std::fs::read_to_string(root.join("infra/lint").join(format!("{lint}.sh")))
            .expect("the lint is readable");
        assert!(
            body.contains("pattern_scan "),
            "{lint}.sh no longer uses the shared scan — the `git grep` \
             status check would then live in two places, and it was \
             wrong in both"
        );
        assert!(
            !body.contains("|| true)"),
            "{lint}.sh swallows a status with `|| true` again: that is \
             the whole defect — `git grep` exits 1 for no match and >1 \
             for an error, and `|| true` discards the difference"
        );
        assert!(
            body.contains("|| exit $?"),
            "{lint}.sh does not propagate the scan's refusal, so a scan \
             that never ran would print `clean` and exit 0"
        );
    }
}

// ---------------------------------------------------------------------
// The four baseline-comparing lints. They shared a copy-pasted
// `resolve_base` with one defect in all four copies; it is now
// lib/trunk-ref.sh. They DISAGREE on purpose about what an absent trunk
// means, which is the distinction the defect erased.
// ---------------------------------------------------------------------

#[test]
fn migrations_append_only_reads_the_tree_or_says_it_could_not() {
    let fx = Fixture::new("migrations-append-only");
    let lint = "migrations-append-only";

    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 0, "a clean fixture must pass:\n{out}");
    assert!(
        out.contains("clean"),
        "a clean fixture must say clean:\n{out}"
    );

    // A NEW migration is how a schema change is supposed to arrive.
    fx.write("infra/postgres/schema/101-b.sql", "CREATE TABLE b();\n");
    fx.commit("a new migration");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 0, "adding a migration must stay legal:\n{out}");

    // Editing an applied one is the defect this lint exists for.
    fx.write(
        "infra/postgres/schema/100-a.sql",
        "-- a comment\nCREATE TABLE a();\n",
    );
    fx.commit("edit an applied migration");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 1, "editing an applied migration must fail:\n{out}");
    assert!(
        out.contains("100-a.sql") && out.contains("VIOLATION"),
        "the failure must name the migration it refused:\n{out}"
    );

    // 3. This lint builds its own git fixtures in a temp directory before
    //    it reads the tree, so a blanket git failure breaks them first and
    //    used to be reported as "SELF-TEST FAILED" — this script's
    //    detectors blamed for a broken machine. It now asks git the one
    //    question whose answer it already knows, first.
    let (code, out) = fx.run(lint, "", true);
    assert_refusal(lint, code, &out, "git rev-parse --git-dir", OWNER_SAID);
    assert!(
        !out.contains("SELF-TEST FAILED"),
        "a machine where git cannot run is reported as a self-test \
         failure, which blames the detectors:\n{out}"
    );

    //    Breaking one subcommand is what reaches the deeper paths — and is
    //    also the shape of a corrupt ref or an unreadable index, which the
    //    ownership trick cannot produce.
    for (broken, named) in [
        ("rev-parse", "git rev-parse"),
        ("merge-base", "git merge-base"),
        ("diff", "git diff"),
    ] {
        let (code, out) = fx.run(lint, broken, false);
        assert_refusal(lint, code, &out, named, SHIM_SAID);
        assert!(
            !out.contains("Fetch the trunk"),
            "a git that could not answer still tells the operator to \
             fetch a trunk that is already there — the wrong \
             remediation this packet exists to delete:\n{out}"
        );
    }
}

#[test]
fn steptype_bundle_ratchet_reads_the_tree_or_says_it_could_not() {
    let fx = Fixture::new("steptype-bundle-ratchet");
    let lint = "steptype-bundle-ratchet";

    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 0, "a clean fixture must pass:\n{out}");

    // A tightening: the first optional field in the bundle becomes
    // required. Read out of the real file rather than written here, so the
    // fixture cannot drift from the bundle's actual idiom.
    let bundle_path = fx.dir.join("crates/core/boss-jobs/seeds/step_types.toml");
    let bundle = std::fs::read_to_string(&bundle_path).expect("the bundle is readable");
    assert!(
        bundle.contains("required = false"),
        "the bundle has no optional field to tighten, so this case \
         proves nothing"
    );
    let tightened = bundle.replacen("required = false", "required = true", 1);
    boss_testing::write_file(&bundle_path, &tightened);
    fx.commit("tighten a bundle field");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 1, "an optional -> required move must fail:\n{out}");
    assert!(
        out.contains("optional -> required"),
        "the failure must say what it refused:\n{out}"
    );

    for (broken, named) in [
        ("rev-parse", "git rev-parse"),
        ("merge-base", "git merge-base"),
        ("ls-tree", "git ls-tree"),
        ("show", "git show"),
    ] {
        let (code, out) = fx.run(lint, broken, false);
        assert_refusal(lint, code, &out, named, SHIM_SAID);
        assert!(
            !out.contains("nothing to ratchet") && !out.contains("the parse broke"),
            "a git that could not answer is reported as an absent \
             baseline or a broken parser — two diagnoses a reader then \
             has to go disprove:\n{out}"
        );
    }
    let (code, out) = fx.run(lint, "", true);
    assert_refusal(lint, code, &out, "git rev-parse", OWNER_SAID);
}

#[test]
fn a_new_style_has_a_caller_reads_the_tree_or_says_it_could_not() {
    let fx = Fixture::new("a-new-style-has-a-caller");
    let lint = "a-new-style-has-a-caller";

    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 0, "a clean fixture must pass:\n{out}");

    fx.write(
        "apps/web/src/styles.css",
        ".alpha { color: red }\n.beta { color: blue }\n.orphan { color: green }\n",
    );
    fx.commit("a class with no caller");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 1, "an unreferenced new class must fail:\n{out}");
    assert!(
        out.contains(".orphan"),
        "the failure must name the class it refused:\n{out}"
    );

    // This lint SKIPS an absent trunk on purpose, which is exactly why it
    // must not skip a git that could not answer: the skip exits 0.
    for (broken, named) in [
        ("rev-parse", "git rev-parse"),
        ("merge-base", "git merge-base"),
        ("ls-tree", "git ls-tree"),
        ("show", "git show"),
    ] {
        let (code, out) = fx.run(lint, broken, false);
        assert_refusal(lint, code, &out, named, SHIM_SAID);
        assert!(
            !out.contains("skipping"),
            "a git that could not answer is reported as a skip, and a \
             skip exits 0:\n{out}"
        );
    }
    let (code, out) = fx.run(lint, "", true);
    assert_refusal(lint, code, &out, "git rev-parse", OWNER_SAID);
}

#[test]
fn a_kind_bundle_does_not_tighten_reads_the_tree_or_says_it_could_not() {
    let fx = Fixture::new("a-kind-bundle-does-not-tighten");
    let lint = "a-kind-bundle-does-not-tighten";

    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 0, "a clean fixture must pass:\n{out}");

    let bundle_path = fx.dir.join("crates/core/boss-jobs/seeds/step_types.toml");
    let bundle = std::fs::read_to_string(&bundle_path).expect("the bundle is readable");
    let tightened = bundle.replacen("required = false", "required = true", 1);
    boss_testing::write_file(&bundle_path, &tightened);
    fx.commit("tighten a bundle field");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 1, "a newly required field must fail:\n{out}");

    for (broken, named) in [
        ("rev-parse", "git rev-parse"),
        ("merge-base", "git merge-base"),
        ("show", "git show"),
    ] {
        let (code, out) = fx.run(lint, broken, false);
        assert_refusal(lint, code, &out, named, SHIM_SAID);
        assert!(
            !out.contains("skipping"),
            "a git that could not answer is reported as a skip, and a \
             skip exits 0:\n{out}"
        );
    }
    let (code, out) = fx.run(lint, "", true);
    assert_refusal(lint, code, &out, "git rev-parse", OWNER_SAID);
}

// ---------------------------------------------------------------------
// no-secrets: the one that already refused, and still said nothing
// ---------------------------------------------------------------------

#[test]
fn no_secrets_reads_the_tree_or_says_it_could_not() {
    let fx = Fixture::new("no-secrets");
    let lint = "no-secrets";

    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 0, "a clean fixture must pass:\n{out}");

    // A credential-shaped line in a tracked file. `wireguard-key` is a
    // detector with no placeholder escape, and the bytes below are
    // random base64, not a key. Assembled for the same reason as the
    // session path above — written whole, it is a hit in THIS file and
    // `no-secrets` fails on the real tree (measured: it did).
    fx.write(
        "infra/secret.conf",
        &format!(
            "{}Key = aGVsbG9Ob3RBUmVhbEtleUp1c3RGaXh0dXJlQnl0ZXMxMjM0NTY3OD0=\n",
            "Private"
        ),
    );
    fx.commit("a credential-shaped line");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(code, 1, "credential material must fail:\n{out}");
    assert!(
        out.contains("infra/secret.conf"),
        "the failure must name the file:\n{out}"
    );

    // It exited 128 with git's bare fatal before: honest, but naming no
    // lint, no scanned-file count, and a status no reader of this roster
    // has a meaning for.
    let (code, out) = fx.run(lint, "ls-files", false);
    assert_refusal(lint, code, &out, "git ls-files", SHIM_SAID);
    let (code, out) = fx.run(lint, "", true);
    assert_refusal(lint, code, &out, "git ls-files", OWNER_SAID);
}

// ---------------------------------------------------------------------
// The roster itself
// ---------------------------------------------------------------------

/// A lint that was converted must stay converted, and a lint that asks
/// git anything must not reintroduce the two idioms that caused this.
///
/// Scoped to the two exact shapes rather than to "a git call somewhere",
/// because a lint's own fixtures use git legitimately and deliberately
/// suppress it (`if leaked=$(git rev-parse --absolute-git-dir
/// 2>/dev/null)` is a tripwire that WANTS the failure). A loose rule
/// would red on those and teach the next author to work around it.
#[test]
fn a_converted_lint_stays_converted() {
    let dir = repo_root().join("infra/lint");
    for lint in LINTS {
        let body = std::fs::read_to_string(dir.join(format!("{lint}.sh")))
            .unwrap_or_else(|e| panic!("{lint}.sh is readable: {e}"));
        assert!(
            body.contains("git-answer.sh")
                || body.contains("trunk-ref.sh")
                || body.contains("pattern-scan.sh"),
            "{lint}.sh no longer reaches git through lib/git-answer.sh, so \
             nothing tells \"git found nothing\" from \"git did not run\""
        );
        assert!(
            !body.contains("resolve_base()"),
            "{lint}.sh has its own trunk walk again. That walk lived in \
             four lints and carried the same defect in all four copies; \
             it is lib/trunk-ref.sh now (§9a)"
        );
        for (i, line) in body.lines().enumerate() {
            let l = line.trim();
            if l.starts_with('#') {
                continue;
            }
            assert!(
                !(l.contains("git grep") && l.contains("|| true")),
                "{lint}.sh:{} discards `git grep`'s status with `|| true`: \
                 exit 1 is \"no match\" and exit >1 is \"I could not \
                 look\", and this is the line that printed `clean` on a \
                 tree nothing read.\n  {l}",
                i + 1
            );
            assert!(
                !(l.contains("rev-parse --verify") && l.contains("2>")),
                "{lint}.sh:{} suppresses git's stderr on a trunk \
                 resolution, which is how \"the repository is unreadable\" \
                 became \"no trunk ref found — fetch the trunk\".\n  {l}",
                i + 1
            );
        }
    }
}
