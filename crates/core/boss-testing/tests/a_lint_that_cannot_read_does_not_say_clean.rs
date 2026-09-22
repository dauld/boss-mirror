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
//! | `a-kind-bundle-does-not-tighten` | the same, exit 0 (deleted 2026-09-18: a twin of `steptype-bundle-ratchet`, backlog cdf2d959) |
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
//!
//! A SECOND WAY TO ANSWER WRONGLY, measured 2026-09-15 (gate-run b28b9998,
//! backlog 28af807c). The clean fixture below — a bundle compared to
//! itself — was refused with `credential-rotation.credential is now
//! required`, once, on a loaded gate, and passed on re-gate. Not an
//! anchoring fault: the lint `cd`s to its own copy's root, which is the
//! fixture, and the test names the trunk (`BOSS_TRUNK_REF=main`), so HEAD
//! and trunk were both the fixture's and identical. The membership test
//! was `printf '%s\n' "$BEFORE" | grep -qxF "$pair"` under `set -o
//! pipefail`: bash line-buffers stdout, so the 48-line list leaves as 48
//! writes; `grep -q` exits at its match; the writes still owed are SIGPIPE
//! and the pipeline reports 141 for a pair that IS present. Reproduced at
//! 1 in 3000 with the pipeline pinned to one contended cpu. The roster
//! test refuses that shape; the lints use here-strings, which are written
//! whole before grep starts and are not pipelines.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

/// "I could not answer." Distinct from 0 (read the tree, it is clean) and
/// 1 (read the tree, found a violation) on purpose: an infrastructure
/// refusal is not a verdict on the branch, and exit 1 erases that
/// distinction at the only point where it is still cheap to keep.
/// `infra/lint/lib/git-answer.sh` carries the full argument.
const CANNOT_ANSWER: i32 = 3;

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
        for lib in [
            "git-answer.sh",
            "trunk-ref.sh",
            "pattern-scan.sh",
            "scanned.sh",
            "allowlist.sh",
        ] {
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

/// The bundle path inside a fixture, and the first pipe-enum
/// `field_type` line in it — read out of the real bundle rather than
/// written here, so these cases cannot drift from its actual idiom.
fn first_enum_field(fx: &Fixture) -> (std::path::PathBuf, String, String) {
    let path = fx.dir.join("crates/core/boss-jobs/seeds/step_types.toml");
    let bundle = std::fs::read_to_string(&path).expect("the bundle is readable");
    let line = bundle
        .lines()
        .find(|l| l.trim_start().starts_with("field_type = \"") && l.contains('|'))
        .expect("the bundle declares at least one pipe-enum field")
        .to_string();
    let ty = line
        .split('"')
        .nth(1)
        .expect("a quoted field_type")
        .to_string();
    (path, line, ty)
}

/// A PIPE-ENUM THAT ONLY GAINS VALUES IS A LOOSENING (backlog ff5b9634).
///
/// The ratchet refused every `field_type` change, on the reasoning that
/// telling a safe widening from a tightening "needs judgment a lint
/// should refuse rather than guess". A strict superset needs no
/// judgment: every value the trunk accepted is still accepted, so no
/// in-flight step of that kind becomes uncompletable — the same
/// property that already makes `required -> optional` legal here. The
/// measured case was the `gate-verdict` bundle gaining `refused` to
/// match a Workflow row that had already been published with it, where
/// the alternative was leaving the two halves of one contract
/// disagreeing.
///
/// Both halves are pinned: without the first the loosening is refused,
/// without the second a SHRUNK enum rides in as a widening and
/// retightens every in-flight step of the kind at the next restart.
#[test]
fn steptype_bundle_ratchet_allows_a_widened_enum_and_still_refuses_a_shrunk_one() {
    let lint = "steptype-bundle-ratchet";

    let fx = Fixture::new("steptype-widen");
    let (path, line, ty) = first_enum_field(&fx);
    let widened = line.replace(&ty, &format!("{ty}|a-value-nothing-in-flight-carries"));
    let bundle = std::fs::read_to_string(&path).expect("readable");
    boss_testing::write_file(&path, &bundle.replacen(&line, &widened, 1));
    fx.commit("widen a bundle enum");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(
        code, 0,
        "a pipe-enum that only GAINS a value strands no in-flight step, so the ratchet \
         must let it through:\n{out}"
    );

    let fx = Fixture::new("steptype-shrink");
    let (path, line, ty) = first_enum_field(&fx);
    let kept = ty
        .rsplit_once('|')
        .expect("a pipe-enum has at least two values")
        .0
        .to_string();
    let shrunk = line.replace(&ty, &kept);
    let bundle = std::fs::read_to_string(&path).expect("readable");
    boss_testing::write_file(&path, &bundle.replacen(&line, &shrunk, 1));
    fx.commit("shrink a bundle enum");
    let (code, out) = fx.run(lint, "", false);
    assert_eq!(
        code, 1,
        "dropping a value from a pipe-enum retightens every in-flight step of that kind \
         at the next restart, with no version to pin against:\n{out}"
    );
    assert!(
        out.contains("field_type"),
        "the failure must say what it refused:\n{out}"
    );
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

// ---------------------------------------------------------------------
// The SIGPIPE coin, for every shell under infra/
// ---------------------------------------------------------------------

/// A multi-line list on the LEFT of a pipe whose reader on the RIGHT
/// exits before the writer is done.
///
/// The list idiom is `printf '%s\n' "$…"` — a variable, an array
/// (`"${A[@]}"`), or the argument list (`"$@"`). MEASURED on this pod's
/// bash 5.2.15 (2026-09-15, backlog 9840e529) by reading the producing
/// subshell's `/proc/<pid>/io`: a 50-line, 141-byte variable leaves
/// `printf '%s\n'` in 3 write() calls, a 5000-line one in 686, an array
/// in one write per element, a loop in one per iteration. `printf '%s'`
/// of a variable is ONE write and is not this idiom — but one write is
/// still SIGPIPEd once it is longer than the pipe (measured 2026-09-18,
/// below), so under `pipefail` the producer check owns that shape too.
///
/// The early-exiting readers are `grep -q` / `grep -m` (stop at a
/// match), `head` (stops at N lines) and a lone `read` (stops at one).
/// `while read` drains, `awk`/`sed`/`sort`/`wc`/`grep -c` drain; none
/// of those can SIGPIPE its writer and none is refused.
///
/// WIDER than the shape the first fix pinned, on purpose: that matcher
/// asked the left side to START with `printf`, and the roster's next
/// instance of the same coin was `if ! printf '%s\n' "$declared" |
/// grep -qxF -- "$tool"` (ci-tools-declared.sh:141) — invisible to it.
/// Anything before the idiom on the left (`if`, `if !`, `elif`, `&&`)
/// is still the idiom. A comment quoting it is not.
fn is_a_list_piped_into_an_early_exiting_reader(line: &str) -> bool {
    let l = line.trim_start();
    if l.starts_with('#') {
        return false;
    }
    let Some((left, right)) = l.split_once('|') else {
        return false;
    };
    if !left.contains("printf '%s\\n' \"$") {
        return false;
    }
    reader_exits_early(right)
}

/// Every `*.sh` under `dir`, recursively — the roster is the directory.
fn shell_scripts_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            out.extend(shell_scripts_under(&path));
        } else if path.extension().is_some_and(|x| x == "sh") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// The coin is refused everywhere it can be flipped, not only in the
/// four lints that were converted first. 166 of the 193 shells under
/// infra/ set `pipefail` (measured 2026-09-15), and the class was found
/// in a lint (a-kind-bundle-does-not-tighten), a sourced library
/// (forge/prune-ci-images.lib.sh) and a systemd hook (boss-step.sh)
/// alike — so the roster here is the tree, and this is the ONE place the
/// shape is defined (§9a).
#[test]
fn no_shell_under_infra_pipes_a_list_into_an_early_exiting_reader() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for path in shell_scripts_under(&root.join("infra")) {
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (i, line) in body.lines().enumerate() {
            if is_a_list_piped_into_an_early_exiting_reader(line) {
                offenders.push(format!("  {rel}:{}\n      {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{} line(s) pipe a multi-line list into a reader that exits early, \
         under `set -o pipefail`. bash emits `printf '%s\\n' \"$LIST\"` in \
         several write() calls; `grep -q` / `head` / `read` exit at their \
         match; every write the producer still owes is SIGPIPE, and the \
         pipeline reports 141 for a needle that IS in the list — a false \
         red, or with `|| continue` a false GREEN (gate-run b28b9998, \
         backlog 28af807c; the sweep is 9840e529). A here-string \
         (`grep -q … <<< \"$LIST\"`) is written whole before the reader \
         starts and is not a pipeline. Single-line `printf '%s' \"$x\" | \
         grep -q` is one write and is not refused.\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

#[test]
fn the_shape_check_reads_the_line_that_fired_and_not_its_repair() {
    let fired = r#"    printf '%s\n' "$BEFORE" | grep -qxF "$kind	$field" && continue"#;
    assert!(
        is_a_list_piped_into_an_early_exiting_reader(fired),
        "the line that reported a present field as newly required must be caught"
    );
    let mirror = r#"    printf '%s\n' "$KINDS_BEFORE" | grep -qxF "$kind" || continue"#;
    assert!(
        is_a_list_piped_into_an_early_exiting_reader(mirror),
        "the mirror-image line, which would wave a real tightening through, must be caught"
    );
    for coin in [
        // The roster's next instance: the idiom behind `if !` (ci-tools-declared.sh:141).
        r#"    if ! printf '%s\n' "$declared" | grep -qxF -- "$tool"; then"#,
        r#"        elif printf '%s\n' "$SEEN_IDS" | grep -qx -- "$f_id"; then"#,
        // One write per element / per argument.
        r#"    printf '%s\n' "${PG_TESTS[@]}" | grep -q 'x'"#,
        r#"if [ -n "${SERVICE_RESULT:-}" ] && ! printf '%s\n' "$@" | grep -q '^result='; then"#,
        // The other early-exiting readers.
        r#"    first=$(printf '%s\n' "$rows" | head -1)"#,
        r#"    printf '%s\n' "$rows" | read -r first"#,
        r#"    printf '%s\n' "$rows" | IFS= read -r first"#,
        r#"    printf '%s\n' "$rows" | grep -m1 x"#,
        r#"    printf '%s\n' "$rows" | grep -i -q x"#,
    ] {
        assert!(
            is_a_list_piped_into_an_early_exiting_reader(coin),
            "a list piped into a reader that exits early must be caught: {coin}"
        );
    }
    for repaired in [
        r#"    grep -qxF "$kind	$field" <<< "$BEFORE" && continue"#,
        r#"    if ! grep -qxF -- "$tool" <<< "$declared"; then"#,
        // One line, one write: a self-test reading its own captured output.
        r#"    if printf '%s' "$out" | grep -q "VIOLATION"; then"#,
        // Drains its input; the writer is never SIGPIPEd.
        r#"    printf '%s\n' "$BEFORE" | grep -c . >/dev/null"#,
        r#"    printf '%s\n' "$rows" | sort -u"#,
        r#"    printf '%s\n' "$rows" | awk -F'\t' '$1==k' | head -1"#,
        r#"    printf '%s\n' "$rows" | while IFS= read -r row; do"#,
        r#"    printf '%s\n' "$rows" | grep -v '^$' | wc -l"#,
        // Not a pipe.
        r#"    printf '%s\n' "$rows" || echo none"#,
        // A comment quoting the idiom is not the idiom.
        r#"# was: printf '%s\n' "$BEFORE" | grep -qxF"#,
    ] {
        assert!(
            !is_a_list_piped_into_an_early_exiting_reader(repaired),
            "a shape that cannot SIGPIPE its writer must not be refused: {repaired}"
        );
    }
}

// ---------------------------------------------------------------------
// The second SIGPIPE class: ANY producer on the left, under pipefail
// ---------------------------------------------------------------------

/// The line with every quoted character masked to `_`, indices kept, so
/// a `|` inside `'…'` or `"…"` (a `sed 's|^|  |'` delimiter, a prose
/// string quoting the idiom) is not read as a pipe — while a `|` inside
/// `"$( … )"` still is, because a command substitution opens a fresh
/// unquoted context even inside double quotes.
fn mask_quoted(line: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Ctx {
        Bare,
        Single,
        Double,
    }
    let chars: Vec<char> = line.chars().collect();
    let mut stack = vec![Ctx::Bare];
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let top = *stack.last().unwrap_or(&Ctx::Bare);
        let next = chars.get(i + 1).copied();
        match top {
            Ctx::Single => {
                if c == '\'' {
                    stack.pop();
                }
                out.push('_');
            }
            Ctx::Double => {
                if c == '\\' {
                    out.push('_');
                    out.push('_');
                    i += 2;
                    continue;
                }
                if c == '"' {
                    stack.pop();
                    out.push('_');
                } else if c == '$' && next == Some('(') {
                    stack.push(Ctx::Bare);
                    out.push('$');
                    out.push('(');
                    i += 2;
                    continue;
                } else {
                    out.push('_');
                }
            }
            Ctx::Bare => match c {
                '\\' => {
                    out.push(c);
                    if let Some(n) = next {
                        out.push(n);
                    }
                    i += 2;
                    continue;
                }
                '\'' => {
                    stack.push(Ctx::Single);
                    out.push('_');
                }
                '"' => {
                    stack.push(Ctx::Double);
                    out.push('_');
                }
                ')' if stack.len() > 1 => {
                    stack.pop();
                    out.push(c);
                }
                _ => out.push(c),
            },
        }
        i += 1;
    }
    out
}

/// The command word that produces a pipe stage's output — `git` in
/// `x=$(git ls-files | grep -q …)`, `find` in `if ! find … | head -1`.
/// `left` is the masked text before the `|`. Empty when the producer is
/// not on this line (a continuation that starts with the pipe).
fn producer_word(left: &str) -> &str {
    // The stage starts after the last opener or separator on the line.
    let start = ["$(", "<(", "&&", "||", ";", "{", "("]
        .iter()
        .filter_map(|s| left.rfind(s).map(|i| i + s.len()))
        .max()
        .unwrap_or(0);
    let mut rest = left[start..].trim_start();
    loop {
        let Some(word) = rest.split_whitespace().next() else {
            return "";
        };
        match word {
            "if" | "elif" | "while" | "until" | "then" | "do" | "else" | "!" | "time"
            | "command" | "sudo" | "env" | "nice" => {
                rest = rest[word.len()..].trim_start();
            }
            // `sudo -u postgres psql …`: the flag is not the producer.
            w if w.starts_with('-') => rest = rest[word.len()..].trim_start(),
            _ => return word,
        }
    }
}

/// `read`, `head`, or `grep` with `-q` / `-m` — behind any number of
/// environment assignments (`IFS= read`, `LC_ALL=C grep -q`). The ONE
/// definition of "a reader that exits early", for both classes: the two
/// pins each carried a copy that knew `IFS=` and nothing else, so
/// `printf '%s\n' "$managed_ns" | LC_ALL=C grep -qxF` stood through two
/// sweeps and refused `boss` as unowned in the line that listed it as
/// owned (backlog 0f2ecbda, 2026-09-19).
fn reader_exits_early(stage: &str) -> bool {
    let mut words = stage
        .split_whitespace()
        .skip_while(|w| is_environment_assignment(w));
    match words.next() {
        Some("grep") => words
            .take_while(|w| w.starts_with('-') && *w != "--")
            .any(|w| !w.starts_with("--") && (w.contains('q') || w.contains('m'))),
        Some("head") | Some("read") => true,
        _ => false,
    }
}

/// `NAME=…` where NAME is a shell identifier: a per-command environment
/// assignment, not the command.
fn is_environment_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A producer on the left of a `|` whose reader on the right exits
/// before the producer is done.
///
/// The first class (`is_a_list_piped_into_an_early_exiting_reader`) is a
/// shell variable the builtin emits in several writes. This is the second
/// (backlog 76d04429, measured 2026-09-15 after the first sweep landed
/// #376): `git ls-files | grep -q …`, `grep -n … | head -1`, `find … |
/// head -1`, a shell function or a `done` loop piped into `grep -q`. An
/// external command is fully buffered, so ONE write below 4 KB and the
/// coin is rarer than the variable case — but a producer whose output
/// exceeds a pipe buffer, or that flushes per line (`git ls-files` on a
/// large tree, `grep -n` over many files, any loop that echoes per
/// iteration), is SIGPIPEd exactly when the reader has already matched,
/// and under `pipefail` the pipeline reports 141 for a needle that IS
/// there: a lint that refuses a clean tree, or with `|| continue` /
/// `if !` passes a real finding, by chance.
///
/// The builtins are not exempt (backlog 2c257761, measured 2026-09-18 on
/// this pod, 200 runs per size): `printf '%s' "$out" | grep -q needle`
/// with the needle on the first line of `$out` is one write() and 0/200
/// up to 65 KB; from 70 to 96 KB a coin (1–2/50 per size; 0/50 at 97,
/// 100 and 112 KB — the window is not monotone); and 200/200 exit 141
/// at 128 KB and 512 KB — a `write()` longer than the pipe blocks until
/// the reader drains it, the reader has matched and gone, and the
/// writer is SIGPIPEd for the rest. A self-test that captures a whole lint's
/// output into `$out` and asks `printf '%s' "$out" | grep -q` has no
/// bound on that length; 47 such sites stood in infra/lint alone. The
/// here-string (`grep -q … <<< "$out"`) is written to a temp file
/// before grep starts, so nothing is left to SIGPIPE: 0/200 at every
/// size.
///
/// Every pipe on the line is a producer/reader pair (`grep -n … |
/// grep -v '^#' | head -1` is caught at its second pipe, where the
/// filter is the producer). A comment quoting the idiom, or prose inside
/// quotes, is not the idiom. Drainers (`while read`, `sort`, `wc`,
/// `grep -c`, `awk`) are never refused.
fn is_a_producer_piped_into_an_early_exiting_reader(line: &str) -> bool {
    let l = line.trim_start();
    if l.starts_with('#') {
        return false;
    }
    let masked = mask_quoted(l);
    let bytes = masked.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'|' {
            i += 1;
            continue;
        }
        // `||` is a list operator and `|&` is stderr-too; neither is judged.
        if bytes.get(i + 1) == Some(&b'|') || bytes.get(i + 1) == Some(&b'&') {
            i += 2;
            continue;
        }
        if i > 0 && bytes[i - 1] == b'|' {
            i += 1;
            continue;
        }
        let (left, right) = (&masked[..i], &masked[i + 1..]);
        let producer = producer_word(left);
        if !producer.is_empty() && reader_exits_early(right) {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether each line of `body` runs under `pipefail`: on from the first
/// `set -… pipefail`, off again after a `set +o pipefail`, back on at the
/// next `set -o`. A library (`lib/*.sh`, `*lib.sh`) sets nothing itself
/// and inherits its sourcer's, and every sourcer under infra/ sets it —
/// the first class was found in `forge/prune-ci-images.lib.sh` — so a
/// library starts on.
/// A `#!/bin/sh` file has no pipefail to be under: dash (the sh on the
/// forge, boss-gcp and this pod) refuses `set -o pipefail` as an illegal
/// option, and a library whose shebang says sh is sourced by sh scripts
/// (observe-host.sh sources observe-lib.sh). Reading such a file as
/// "under pipefail" named a repair — a here-string — that dash cannot
/// parse; train #439 shipped it and two hosts went unobserved
/// (2026-09-18). The parser named on line one decides.
fn says_sh(body: &str) -> bool {
    let first = body.lines().next().unwrap_or("").trim_end();
    matches!(first, "#!/bin/sh" | "#!/usr/bin/env sh")
        || first.starts_with("#!/bin/sh ")
        || first.starts_with("#!/usr/bin/env sh ")
}

fn pipefail_per_line(body: &str, is_library: bool) -> Vec<bool> {
    if says_sh(body) {
        return body.lines().map(|_| false).collect();
    }
    let mut on = is_library;
    body.lines()
        .map(|line| {
            let l = line.trim();
            if l.starts_with("set +o pipefail") {
                on = false;
            } else if l.starts_with("set -") && l.contains("pipefail") {
                on = true;
            }
            on
        })
        .collect()
}

/// Shells whose `printf '%s' "$x" | grep -q` sites were left for the car
/// that owns the file: cdf2d959 is editing no-wallclock.sh's allowlists
/// on the same day (2026-09-18) as this pin widened, so its seven sites
/// ride that car. Delete the entry with the conversion; an empty list
/// here is the destination.
const PRODUCER_COIN_LEFT_FOR_CDF2D959: &[&str] = &["infra/lint/no-wallclock.sh"];

/// The second class, refused across every shell under infra/ that runs
/// the line under `pipefail` (the ONE place this shape is defined, §9a).
/// Repairs: capture first (`out=$(git ls-files …)`, then `grep -q … <<<
/// "$out"`), drain (`grep -c`, `awk 'END'`), or `set +o pipefail` around
/// the one pipeline with a comment naming why.
#[test]
fn no_shell_under_infra_pipes_a_producer_into_an_early_exiting_reader() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for path in shell_scripts_under(&root.join("infra")) {
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        if PRODUCER_COIN_LEFT_FOR_CDF2D959.contains(&rel.as_str()) {
            continue;
        }
        let is_library = rel.contains("/lib/") || rel.ends_with("lib.sh");
        let pipefail = pipefail_per_line(&body, is_library);
        for (i, line) in body.lines().enumerate() {
            if pipefail[i] && is_a_producer_piped_into_an_early_exiting_reader(line) {
                offenders.push(format!("  {rel}:{}\n      {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{} line(s) pipe a producer into a reader that exits early, under \
         `set -o pipefail`. `grep -q` / `head` / `read` exit at their \
         match; a producer still writing (a tree larger than a pipe \
         buffer, a per-line flush, a loop, a `printf '%s' \"$out\"` longer \
         than 64 KB) is SIGPIPE, and the pipeline reports 141 for a \
         needle that IS there — a false red, or with `|| continue` / \
         `if !` a false GREEN (backlog 76d04429 and 2c257761; the \
         variable-list class is 9840e529). Read a capture through a \
         here-string (`grep -q … <<< \"$out\"`), use a reader that drains \
         (`grep -c`, `awk`), or `set +o pipefail` around the one pipeline \
         with a comment naming why — in a bash script. A file whose first \
         line says #!/bin/sh has no pipefail and no here-string (dash), \
         and is not read here; infra/lint/a-sh-script-parses-under-sh.sh \
         holds it to that shebang. no-producer-coin: the offenders are\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}

#[test]
fn the_producer_check_reads_the_pipe_and_not_the_prose() {
    for coin in [
        // The packet's named shapes.
        r#"    if git ls-files -- 'infra/*.sh' | grep -q 'x'; then"#,
        r#"    first=$(grep -n "$RUNNER" "$INIT" | head -1 | cut -d: -f1)"#,
        r#"    SRC=$(find "$WORKDIR" -maxdepth 1 -type d | head -1)"#,
        // The second pipe is the coin: a filter producer, an early reader.
        r#"code_line() { grep -n "$1" "$sweep" | grep -vE '^[0-9]+:\s*#' | head -1 | cut -d: -f1; }"#,
        // A shell function and a loop are producers of unknown write count.
        r#"    if ! verdict success 0 | grep -q 'result=ok'; then"#,
        r#"    if code_only "$path" | grep -qE "$FATAL"; then"#,
        r#"    done < <(sources "$repo") | head -3 | sed 's|^|    |' >&2"#,
        // Inside a double-quoted command substitution the pipe is real.
        r#"    echo "  kanidmd: $(kanidmd version 2>/dev/null | head -1 || echo present)""#,
        // Behind `if`, `!`, `sudo`, an assignment, a `&&` chain.
        r#"if ! sudo -u postgres psql -tc "SELECT 1" | grep -q 1; then"#,
        r#"    if ip=$(dig +short "$DOMAIN" 2>/dev/null | head -1) && [ -n "$ip" ]; then"#,
        r#"    [ -n "$x" ] && jq -r '.a // ""' "$sum" | grep -q "$stem" \"#,
        r#"    grep -vE '^\s*#' "$pub" | grep -qE '\$HOME' && fail "reads HOME""#,
        r#"    sed -n "${line},$((line + 2))p" "$MANIFEST" | grep -qE 'valueFrom' && ok"#,
        r#"    ls -d "$WORKDIR"/boss-backup-* | IFS= read -r first"#,
        r#"    systemctl --no-pager status caddy | head -n 10 || true"#,
        // The builtins: one write, but a write longer than the pipe is
        // SIGPIPEd once the reader has matched (200/200 at 128 KB).
        r#"    if printf '%s' "$out" | grep -q "VIOLATION"; then"#,
        r#"    if echo "$line_content" | grep -qE 'NOW\s*\(\s*\)'; then"#,
        r#"    || ! printf '%s' "$post" | grep -qF -- "BOSS_JOBS_URL=$sor "; then"#,
        r#"    printf '%s\n' "$BEFORE" | grep -qxF "$kind	$field" && continue"#,
    ] {
        assert!(
            is_a_producer_piped_into_an_early_exiting_reader(coin),
            "a producer piped into a reader that exits early must be caught: {coin}"
        );
    }
    for not_refused in [
        // The repairs: a here-string is not a pipe; a capture is read whole.
        r#"    grep -q 'x' <<< "$files" || continue"#,
        r#"    files=$(git ls-files -- 'infra/*.sh')"#,
        // Drainers.
        r#"    git ls-files | grep -c . >/dev/null"#,
        r#"    grep -n "$1" "$f" | grep -vE '^#' | sort -u"#,
        r#"    find . -name '*.sh' | while IFS= read -r f; do"#,
        r#"    jq -r '.x' "$f" | awk 'END { print NR }'"#,
        // A pipe inside quotes is a delimiter or prose, not a pipe.
        r#"    sed 's|^|    |' "$f""#,
        r#"    why="a swallowed stderr ('2>&1 | grep -q') hides the cause""#,
        // `||` and `|&` are not the pipe judged here.
        r#"    git fetch origin || head -1 "$log""#,
        // A case pattern.
        r#"    success|head) echo yes ;;"#,
        // A comment quoting the idiom is not the idiom.
        r#"# was: git ls-files | grep -q x"#,
        // The producer is on the previous line: not judged by a line check.
        r#"    | grep -q '"ready":[[:space:]]*true'; then"#,
    ] {
        assert!(
            !is_a_producer_piped_into_an_early_exiting_reader(not_refused),
            "a shape that cannot SIGPIPE its writer must not be refused: {not_refused}"
        );
    }
}

/// The line that fired on 2026-09-19 (backlog 0f2ecbda): undeclared-
/// objects.sh refused `Service/boss/…` as "the tree does not own
/// namespace `boss`" in the same breath as "Owned: boss boss-dev". Its
/// membership test was `printf '%s\n' "$managed_ns" | LC_ALL=C grep
/// -qxF "$1"` — the first class exactly, and BOTH pins above read the
/// reader as `LC_ALL=C`, not `grep`, so the line stood through two sweeps.
/// An environment assignment before the command is not the command;
/// `IFS= read` was the one instance either pin had learned.
#[test]
fn a_reader_behind_an_environment_assignment_is_the_same_reader() {
    for coin in [
        r#"owns_ns() { printf '%s\n' "$managed_ns" | LC_ALL=C grep -qxF "$1"; }"#,
        r#"    if ! printf '%s\n' "$names" | LC_ALL=C grep -qxF "$name"; then"#,
        r#"registry_line=$(printf '%s\n' "$read_out" | LC_ALL=C grep -m1 '^REGISTRY' || true)"#,
        r#"    first=$(printf '%s\n' "$rows" | LANG=C LC_ALL=C head -1)"#,
    ] {
        assert!(
            is_a_list_piped_into_an_early_exiting_reader(coin),
            "a list piped into `VAR=… grep -q` is the list-into-grep coin: {coin}"
        );
        assert!(
            is_a_producer_piped_into_an_early_exiting_reader(coin),
            "a producer piped into `VAR=… grep -q` is the producer coin: {coin}"
        );
    }
    for not_refused in [
        // The repair: the array is read by a loop, and the here-string is not a pipe.
        r#"owns_ns() { in_set "$1" "${managed_ns[@]}"; }"#,
        r#"    if ! LC_ALL=C grep -qxF "$name" <<<"$names"; then"#,
        // A drainer behind the same assignment.
        r#"    printf '%s\n' "$rows" | LC_ALL=C sort -u"#,
        r#"    printf '%s\n' "$rows" | LC_ALL=C grep '^RULE' || true"#,
    ] {
        assert!(
            !is_a_list_piped_into_an_early_exiting_reader(not_refused),
            "a shape that cannot SIGPIPE its writer must not be refused: {not_refused}"
        );
        assert!(
            !is_a_producer_piped_into_an_early_exiting_reader(not_refused),
            "a shape that cannot SIGPIPE its writer must not be refused: {not_refused}"
        );
    }
}

#[test]
fn the_producer_check_honours_a_pipefail_window() {
    let body = "set -euo pipefail\nfind . | head -1\n# why: one line wanted\nset +o pipefail\nfind . | head -1\nset -o pipefail\nfind . | head -1\n";
    assert_eq!(
        pipefail_per_line(body, false),
        vec![true, true, true, false, false, true, true]
    );
    let library = "f() {\n  git ls-files | grep -q x\n}\n";
    assert!(
        pipefail_per_line(library, true).iter().all(|&on| on),
        "a sourced library runs under its sourcer's pipefail"
    );
    assert!(
        pipefail_per_line(library, false).iter().all(|&on| !on),
        "a script that never sets pipefail cannot flip this coin"
    );
}

/// The shape #439 "repaired": observe-lib.sh is a sourced library
/// (`lib.sh`) whose first line is `#!/bin/sh`. Under sh there is no
/// pipefail, so its `printf | sed | head -n 1` was never the coin, and
/// the here-string this pin's message names as the repair is not
/// available to it. A bash library of the same body IS under its
/// sourcer's pipefail, as before.
#[test]
fn a_sh_library_is_never_under_pipefail() {
    let sh = "#!/bin/sh\nspool_put() {\n    at=$(printf '%s' \"$1\" | sed -n 's/x/y/p' | head -n 1)\n}\n";
    assert!(
        pipefail_per_line(sh, true).iter().all(|&on| !on),
        "a #!/bin/sh library runs under dash, which has no pipefail"
    );
    let bash = "#!/usr/bin/env bash\nspool_put() {\n    at=$(printf '%s' \"$1\" | sed -n 's/x/y/p' | head -n 1)\n}\n";
    assert!(
        pipefail_per_line(bash, true).iter().all(|&on| on),
        "a bash library runs under its sourcer's pipefail"
    );
    let env_sh = "#!/usr/bin/env sh\nset -e\nls | head -1\n";
    assert!(pipefail_per_line(env_sh, false).iter().all(|&on| !on));
}
