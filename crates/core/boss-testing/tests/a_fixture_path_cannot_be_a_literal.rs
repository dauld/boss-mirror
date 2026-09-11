//! `infra/lint/a-fixture-path-cannot-be-a-literal.sh` is RUN, not read
//! — against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (backlog 74b8bf3d). A test or a binary that builds a
//! fixture at a FIXED absolute path under a world-writable sticky
//! directory (`/tmp`, `/var/tmp`, mode `1777`) collides with every other
//! process doing the same. This repo has now hit it **fourteen** times.
//! Two structural facts keep it coming back:
//!
//!   * builders run in PARALLEL WORKTREES. Worktree isolation isolates
//!     the repository, not `/tmp`. On 2026-09-11 at 14:13Z a root-owned
//!     `/tmp/all` reappeared that the verifying builder's own suite had
//!     not written; a `/proc` scan found PID 206844 — another session
//!     running `cargo test -p boss-testing --all-features` from a
//!     different worktree against the unfixed file. Two cars poisoning
//!     each other through one path, live.
//!   * the GATE runs as uid 65534 (since #310) while a pod session runs
//!     as root. `create_dir_all` on an existing directory returns `Ok`
//!     REGARDLESS of who owns it, so the fixture reports success and the
//!     run dies at the first write inside — frames away from the cause,
//!     with an errno and no path. Instance eleven scored 8/8 alone and
//!     5 passed / 3 failed as 65534 after a root run.
//!
//! Every one of the fourteen was found and fixed individually. A lint is
//! the answer to a CLASS; `boss_testing::scratch` is the answer to an
//! instance, and this lint's whole job is to send an author there.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`. The two cover
//! different failures and neither is the other's copy (CLAUDE.md §9a —
//! collapse, do not duplicate):
//!
//!   * `--self-test` owns "the SCANNER still matches". mawk — the awk in
//!     the CI image — silently matches nothing for an interval `{n}`, and
//!     a scanner that matches nothing passes every file. That failure is
//!     invisible from the outside, so the lint hands itself fixtures on
//!     every invocation, including under `infra/gate.sh --quick` where
//!     no cargo test runs. This test drives `--self-test` rather than
//!     re-authoring those fixtures.
//!   * this file owns "the lint's VERDICT on a tree" — the three
//!     behaviours the gate and a reviewer care about: a clean tree exits
//!     0, a violating tree exits 1 naming file, line and path, and a
//!     tree it cannot read exits 3 rather than reporting `clean`.
//!
//! Fixtures live under `boss_testing::scratch`, which carries the uid
//! and the pid. A proof of this rule that committed the defect it
//! forbids would be worth nothing.
//!
//! NO REFUSED SHAPE IS SPELLED IN THIS FILE. Every fixture body is
//! written with placeholders — `%S`, `%V`, `%J` — that [`Tree::rs`]
//! expands at runtime, so this file is scanned by the scanner it proves
//! like every other `.rs` file. No self-exclusion, which would be a hole
//! exactly where the author of the next fixed path is most likely to be
//! working, and the technique is already in-tree
//! (`infra/lint/a-lint-writes-only-where-it-owns.sh` passes its fixed
//! path to `printf` as an argument for the same reason).
//!
//! That hole was real, not hypothetical. Written without the
//! placeholders this file WAS a finding — and the first whole-tree scan
//! reported `clean` anyway, because the file was still UNTRACKED and
//! `git ls-files` does not list it. An absence that reads as a pass,
//! exactly the shape CLAUDE.md §Doors warns about: a wrong target
//! answers instead of erroring. It was caught by re-running the scan
//! against a tree where every file had been `git add`ed.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The shared sticky root, without the trailing `/<name>` that makes a
/// path FIXED. Spelling that trailing name here would make this file a
/// finding in its own scanner; see the module note.
const SHARED: &str = "/tmp";

/// The other shared sticky root, split the same way and for the same
/// reason.
const SHARED_VAR: &str = "/var/tmp";

/// `PathBuf::join`, held apart from the `temp_dir()` that precedes it in
/// a fixture: `temp_dir().join("<literal>")` is the very shape the lint
/// refuses, so this file cannot spell the two adjacent.
const JOIN: &str = "join";

fn lint() -> PathBuf {
    repo_root().join("infra/lint/a-fixture-path-cannot-be-a-literal.sh")
}

/// A synthetic repository the lint can be run against: a real git index
/// (the lint asks `git ls-files`, so an untracked fixture must not count)
/// and the lint itself at the path its own `cd "$(dirname $0)/../.."`
/// resolves from.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("a-fixture-path-lint-{tag}"));
        scratch::create_dir(&root.join("infra/lint"));
        scratch::create_dir(&root.join("src"));
        let body = std::fs::read_to_string(lint()).expect("read the lint under test");
        scratch::write_exec(
            &root.join("infra/lint/a-fixture-path-cannot-be-a-literal.sh"),
            &body,
        );
        git(&root, &["init", "-q", "-b", "main"]);
        // `git ls-files` reads the index, which `git add` is enough to
        // populate — no commit and so no author identity needed.
        Tree(root)
    }

    /// Add a tracked `.rs` file, expanding the placeholders that keep the
    /// refused shapes out of this file's own source: `%S` is `/tmp`, `%V`
    /// is `/var/tmp`, `%J` is `join`.
    fn rs(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        let expanded = body
            .replace("%S", SHARED)
            .replace("%V", SHARED_VAR)
            .replace("%J", JOIN);
        scratch::write_file(&path, &expanded);
        git(&self.0, &["add", rel]);
        self
    }

    fn run(&self) -> Output {
        Command::new("bash")
            .arg(
                self.0
                    .join("infra/lint/a-fixture-path-cannot-be-a-literal.sh"),
            )
            .current_dir(&self.0)
            // A fixture tree is owned by whoever ran the suite; git's
            // dubious-ownership refusal is what behaviour 3 is about and
            // must not fire by accident in behaviours 1 and 2.
            .env("GIT_CEILING_DIRECTORIES", "")
            .output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The scanner still matches what it is supposed to match.
///
/// mawk answers an interval `{n}` by matching NOTHING, and a scanner
/// that matches nothing passes every file — a green check on a tree it
/// never read, which is the failure this repo keeps paying for. The
/// fixtures are the lint's own; this test only insists they are run.
#[test]
fn the_scanner_proves_itself_on_every_invocation() {
    let out = Command::new("bash")
        .arg(lint())
        .arg("--self-test")
        .output()
        .expect("run the lint's self-test");
    assert!(
        out.status.success(),
        "the lint's --self-test must pass:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("self-test ok"),
        "the self-test must SAY it ran — a silent pass is indistinguishable \
         from a scanner that matched nothing:\n{}",
        text(&out)
    );
}

/// BEHAVIOUR 1 — a tree with no fixed temp path exits 0.
///
/// Every shape here is one the repo uses and must keep using: the
/// sanctioned `scratch` helpers, a pid-bearing `format!`, a `Uuid`, a
/// `mktemp`-equivalent, and prose that merely MENTIONS a fixed path.
#[test]
fn a_hermetic_tree_is_clean() {
    let tree = Tree::new("clean");
    tree.rs(
        "src/ok.rs",
        "\
//! A comment may name %S/all — this one does, and must not trip it.
use boss_testing::scratch;
fn a() -> std::path::PathBuf { scratch::scratch_dir(\"boss-a\") }
fn b() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(\"boss-b-{}\", std::process::id()))
}
fn c() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        \"boss-c-{tag}-{}\",
        std::process::id()
    ))
}
fn d() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(\"boss-d-{}\", Uuid::new_v4()))
}
/// A join of a computed name is not a literal and is not judged here.
fn e(name: &std::path::Path) -> std::path::PathBuf { std::env::temp_dir().join(name) }
",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "a hermetic tree must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}

/// BEHAVIOUR 2, the one that matters most — a real violation fails and
/// names file, line and the offending path.
///
/// The four shapes are the four the tree has actually shipped.
#[test]
fn a_fixed_temp_path_is_named_with_its_file_and_line() {
    let tree = Tree::new("violating");
    tree.rs(
        "src/bad.rs",
        "\
fn one() -> std::path::PathBuf {
    std::env::temp_dir().%J(\"boss-thing-test\")
}
fn two() -> std::path::PathBuf {
    std::path::PathBuf::from(\"%S/boss-two-fixture\")
}
fn three(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().%J(format!(\"boss-pubreq-{name}\"))
}
fn four() -> String {
    String::from(\"%V/boss-four-fixture\")
}
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a violating tree must exit 1 — a fact about the BRANCH:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        // the file, and each offending line, so nobody re-derives them
        "src/bad.rs:2",
        "src/bad.rs:5",
        "src/bad.rs:8",
        "src/bad.rs:11",
        // the offending path itself, quoted back
        "boss-thing-test",
        "boss-two-fixture",
        "boss-pubreq-",
        "boss-four-fixture",
        // and the door: the API that already solves this
        "scratch",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?} — a verdict someone must go \
             re-derive is not a verdict (CLAUDE.md §Diagnosis):\n{msg}"
        );
    }
}

/// Instance ELEVEN, the one that motivated the lint, in its pre-fix
/// shape — and its post-fix shape, which must pass.
///
/// Pre-fix, `backup_files_its_packet.rs` rebased the container's mount
/// paths and `/tmp/k` into a scratch dir but not `/tmp/all`, which the
/// `offsite-gcs` leg writes. The REBASE chain is the line the fix
/// changed, and it is the line this lint names. Post-fix the same
/// literals are still there — they are the container's paths, named in
/// order to be redirected — and the const's doc comment declares that,
/// so the same file passes. A lint that cannot tell the two apart is
/// not finished.
#[test]
fn instance_eleven_fails_before_its_fix_and_passes_after() {
    let before = Tree::new("instance-eleven-before");
    before.rs(
        "src/backup.rs",
        "\
fn rebase(script: String, dir: &std::path::Path) -> String {
    script
        .replace(\"/backup\", &format!(\"{}/backup\", dir.display()))
        .replace(\"%S/k\", &format!(\"{}/k\", dir.display()))
}
",
    );
    let out = before.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "instance eleven's pre-fix rebase chain must be named:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("src/backup.rs:4"),
        "the verdict must point at the rebase line, which is where the \
         missing entry belonged:\n{}",
        text(&out)
    );

    let after = Tree::new("instance-eleven-after");
    after.rs(
        "src/backup.rs",
        "\
/// Every absolute path the container scripts name, and the scratch
/// subdirectory it is rebased onto.
///
/// shared-tmp-ok: these are the CONTAINER's own paths, named here in
/// order to be redirected; `assert_rebased` proves none survives into
/// a script this host runs.
const REBASE: &[(&str, &str)] = &[
    (\"/backup\", \"backup\"),
    (\"%S/k\", \"k\"),
    (\"%S/all\", \"all\"),
];
",
    );
    let out = after.run();
    assert!(
        out.status.success(),
        "the post-fix file declares its intent at the site and must \
         pass; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}

/// The exemption marker is an intent declaration, not a rubber stamp:
/// it must carry a reason, and it covers only the hit line, the comment
/// block above it, and the enclosing item's own comment block.
#[test]
fn a_marker_without_a_reason_is_not_a_marker() {
    let tree = Tree::new("bare-marker");
    tree.rs(
        "src/bare.rs",
        "\
fn one() -> String {
    // shared-tmp-ok
    String::from(\"%S/boss-bare-one\")
}
fn two() -> String {
    // shared-tmp-ok: container
    String::from(\"%S/boss-bare-two\")
}
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a marker with no reason, and a marker with a one-word reason, \
         must both still fail:\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("src/bare.rs:3") && msg.contains("src/bare.rs:7"),
        "both bare markers must be named:\n{msg}"
    );
}

/// A marker cannot reach across a sibling item — otherwise one
/// declaration silently covers a path added later somewhere else in the
/// file, which is the hole a lint-side allowlist has.
#[test]
fn a_marker_does_not_cover_a_sibling_item() {
    let tree = Tree::new("marker-scope");
    tree.rs(
        "src/scope.rs",
        "\
/// shared-tmp-ok: this declaration belongs to `one` and only to `one`.
fn one() -> String {
    String::from(\"%S/boss-scope-one\")
}

fn two() -> String {
    String::from(\"%S/boss-scope-two\")
}
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "the second item is undeclared and must fail:\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("src/scope.rs:7"),
        "the undeclared sibling must be named:\n{msg}"
    );
    assert!(
        !msg.contains("src/scope.rs:3"),
        "the declared item must NOT be named — a lint that cries wolf \
         gets exempted into uselessness:\n{msg}"
    );
}

/// BEHAVIOUR 3 — when the lint cannot read the tree it must refuse, not
/// report `clean`.
///
/// House style since `infra/lint/lib/git-answer.sh`: exit 3 is "I never
/// read the tree", a fact about the MACHINE that no author can fix by
/// editing code. Measured on 2026-09-11, four lints printed `clean` and
/// exited 0 in a workspace where git refused every command — a green
/// check on a tree nothing looked at.
///
/// The refusal is provoked the way it happened in production: a
/// repository git will not touch. `safe.directory` is not settable here
/// (it needs a config the gate's uid may not own), so the `.git`
/// directory is removed instead — `git ls-files` then exits 128 with its
/// own explanation, which is the case the wrapper is for.
#[test]
fn a_lint_that_cannot_read_the_tree_refuses() {
    let tree = Tree::new("unreadable");
    tree.rs("src/ok.rs", "fn main() {}\n");
    std::fs::remove_dir_all(tree.0.join(".git")).expect("remove the git dir");

    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a tree the lint cannot read must exit 3 — never 0 (which \
         CERTIFIES a tree nothing read) and never 1 (which strikes every \
         car aboard for an infrastructure fault):\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("CANNOT ANSWER"),
        "the refusal must use the in-tree marker so a reader who knows \
         the vocabulary reads it right:\n{msg}"
    );
    // Not "must not contain the word clean" — the refusal text
    // legitimately says that neither `clean` nor a violation can be
    // CLAIMED. The property is that it must not CERTIFY, so the thing
    // that must be absent is the certification sentence itself.
    assert!(
        !msg.contains("Rust file(s) read"),
        "a lint that read nothing must not certify the tree:\n{msg}"
    );
    assert!(
        msg.contains("git"),
        "the refusal must name the command that failed and pass through \
         git's OWN words — the only part that says whether this is an \
         ownership refusal, a corrupt object or a full disk:\n{msg}"
    );
}

/// The real tree passes. This is the ratchet: the lint is in the
/// pre-flight roster, so a car that reintroduces the class reds here
/// before it reds a train.
///
/// Exit 3 is accepted — LOUDLY, never silently — because it is not a
/// verdict on the branch and this test must not turn an infrastructure
/// refusal into a consist failure (CLAUDE.md §Diagnosis). It is reachable
/// in one real situation: a builder re-running this suite under
/// `setpriv --reuid=65534` inside a root-owned worktree, where git
/// refuses for dubious ownership. The gate clones inside its own pod as
/// its own uid, so there it exits 0 — and if it ever did refuse there,
/// the lint's own place in the roster is what reports it, not this test.
#[test]
fn the_repository_itself_is_clean() {
    let out = Command::new("bash")
        .arg(lint())
        .current_dir(repo_root())
        .output()
        .expect("run the lint against the repository");
    if out.status.code() == Some(3) {
        eprintln!(
            "SKIPPED LOUDLY: the lint could not read this worktree, so the \
             ratchet was not exercised. This is an environment fault, not a \
             clean tree:\n{}",
            text(&out)
        );
        return;
    }
    assert!(
        out.status.success(),
        "the tree must be clean; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}
