//! `infra/lint/a-write-to-a-childs-stdin-is-fed.sh` is RUN, not read —
//! against synthetic trees, and once against this repository.
//!
//! THE CLASS (backlog d93cc7d5). A test writes to a child's piped stdin
//! and unwraps the write. When the child exits before it reads — which
//! is exactly what a refusal under test does — the write meets a closed
//! pipe, `BrokenPipe` panics the test, and a train goes red on a verdict
//! that was correct. Three recurrences: the talos twin (28f29f0b), the
//! dns twin a week later (d0eafe94, which wrote `boss_testing::feed_stdin`
//! as the one door), and tiers_sh.rs on train 11:23 (fec29a02, which
//! found five more helpers with the same unwrap). A door nobody is sent
//! to is a comment; the lint is what sends them.
//!
//! The fixture bodies spell the two panicking adapters as `%U` (unwrap)
//! and `%X` (expect), expanded at runtime, so the shapes this file
//! plants are not also lines in it — this file lives under the lint's
//! own scan.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-write-to-a-childs-stdin-is-fed.sh";

/// A synthetic repository: a real git index (the lint reads tracked
/// files, so an untracked fixture must not count), the lint at the path
/// its own `cd` resolves from, and the libs it sources.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("childs-stdin-is-fed-{tag}"));
        boss_testing::copy_lint_libs(&root);
        let body = std::fs::read_to_string(repo_root().join(LINT))
            .unwrap_or_else(|e| panic!("read {LINT}: {e}"));
        scratch::write_exec(&root.join(LINT), &body);
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "."]);
        Tree(root)
    }

    /// Add a tracked file, `%U`/`%X` expanded to unwrap/expect.
    fn file(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        scratch::write_file(&path, &body.replace("%U", "unwrap").replace("%X", "expect"));
        git(&self.0, &["add", rel]);
        self
    }

    fn run(&self) -> Output {
        run_lint_in(&self.0)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_lint_in(root: &Path) -> Output {
    Command::new("bash")
        .arg(root.join(LINT))
        .current_dir(root)
        .env("GIT_CEILING_DIRECTORIES", "")
        .output()
        .unwrap_or_else(|e| panic!("run the lint in {}: {e}", root.display()))
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

/// The eight shapes the tree actually shipped, each in the form it had:
/// the six car 3db93efd replaced (tiers_sh's `writeln!` loop, dev_hooks'
/// bound `stdin`, checkout_lock's one-liner, ops_runner_approval's
/// sha256sum, publish_github_pr's `as_mut()` chain, tag_release's
/// `take()` chain) and the two it missed (the tunnel connector's chain,
/// example_reference_rows_sql's bound `stdin` written twice). Each is
/// named by file and line, and the verdict names the door.
#[test]
fn the_shipped_shapes_are_refused_by_file_and_line() {
    let tree = Tree::new("shipped");
    tree.file(
        "crates/core/a/tests/tiers_sh.rs",
        "\
fn shell_first_above(paths: &[&str]) {
    let mut child = Command::new(\"bash\").stdin(Stdio::piped()).spawn().%X(\"bash runs\");
    {
        let mut stdin = child.stdin.take().%X(\"piped\");
        for p in paths {
            writeln!(stdin, \"{p}\").%U();
        }
    }
}
",
    );
    tree.file(
        "crates/core/a/tests/dev_hooks_sh.rs",
        "\
fn run(payload: &str) {
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().%X(\"stdin\");
        stdin.write_all(payload.as_bytes()).%X(\"write payload\");
    }
}
",
    );
    tree.file(
        "crates/core/a/tests/checkout_lock_sh.rs",
        "fn f() {\n    live.stdin.take().%U().write_all(b\"\\n\").%U();\n}\n",
    );
    tree.file(
        "crates/core/a/tests/publish_github_pr_sh.rs",
        "\
fn git_stdin(input: &str) {
    child
        .stdin
        .as_mut()
        .%X(\"stdin is piped\")
        .write_all(input.as_bytes())
        .%X(\"git reads stdin\");
}
",
    );
    tree.file(
        "crates/modules/b/tests/example_reference_rows_sql.rs",
        "\
fn psql(sql: &str, read_only: bool) {
    let mut stdin = child.stdin.take().%U();
    if read_only {
        stdin
            .write_all(b\"SET default_transaction_read_only = on;\\n\")
            .%U();
    }
    stdin.write_all(sql.as_bytes()).%U();
}
",
    );
    tree.file(
        "crates/core/a/tests/scoped.rs",
        "\
fn a(child: &mut Child) {
    if let Some(mut s) = child.stdin.take() {
        s.write_all(b\"x\").%U();
    }
}
fn b(mut into: ChildStdin) {
    write!(into, \"{}\", 1).%X(\"written\");
}
fn c(child: &mut Child) {
    write!(child.stdin.as_mut().%U(), \"y\").%U();
}
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a tree carrying the shape must exit 1 — a fact about the BRANCH:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        "crates/core/a/tests/tiers_sh.rs:6",
        "crates/core/a/tests/dev_hooks_sh.rs:5",
        "crates/core/a/tests/checkout_lock_sh.rs:2",
        "crates/core/a/tests/publish_github_pr_sh.rs:6",
        "crates/modules/b/tests/example_reference_rows_sql.rs:5",
        "crates/modules/b/tests/example_reference_rows_sql.rs:8",
        "crates/core/a/tests/scoped.rs:3",
        "crates/core/a/tests/scoped.rs:7",
        "crates/core/a/tests/scoped.rs:10",
        // the door
        "boss_testing::feed_stdin",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?} — a verdict someone must \
             re-derive is not a verdict (CLAUDE.md §Diagnosis):\n{msg}"
        );
    }
    assert!(
        msg.contains("9 unwrapped write(s)"),
        "the verdict must count exactly the nine planted writes:\n{msg}"
    );
}

/// What is NOT the shape, each a line the tree carries or could: the
/// door itself; a write whose result is discarded (`let _ =`) or
/// propagated (`?`); an unwrap INSIDE the write's arguments; a writer
/// that is not a child's stdin (a file, a buffer) even when the same
/// file pipes a child; the `.stdin(Stdio::piped())` builder; a comment
/// telling the story; and the shape under `src/`, which is not a test
/// directory this lint reads.
#[test]
fn what_is_not_the_shape_is_clean() {
    let tree = Tree::new("clean");
    tree.file(
        "crates/core/a/tests/fed.rs",
        "\
use boss_testing::feed_stdin;
fn fed(payload: &[u8]) {
    let mut child = Command::new(\"sh\").stdin(Stdio::piped()).spawn().%X(\"sh runs\");
    feed_stdin(&mut child, payload);
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b\"{}\\n\");
    }
    let mut file = std::fs::File::create(path).%U();
    file.write_all(b\"not a pipe\").%U();
    writeln!(file, \"{}\", 2).%X(\"file\");
    let mut buf = Vec::new();
    write!(&mut buf, \"z\").%U();
    // a comment may tell it: stdin.write_all(b).%U();
    /* and a block: live.stdin.take().%U().write_all(b).%U(); */
}
fn propagated(child: &mut Child) -> std::io::Result<()> {
    let stdin = child.stdin.as_mut().%X(\"piped\");
    stdin.write_all(&data.%U())?;
    Ok(())
}
",
    );
    tree.file(
        "crates/core/a/src/lib.rs",
        "fn f() { child.stdin.take().%U().write_all(b\"x\").%U(); }\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "a tree that feeds its children must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("scanned 1 ") && msg.contains("clean"),
        "a clean tree must say how much it read (lib/scanned.sh) and that it is clean:\n{msg}"
    );
}

/// A tree the lint cannot read is refused (exit 3), never certified.
#[test]
fn a_lint_that_cannot_read_the_tree_refuses() {
    let tree = Tree::new("unreadable");
    tree.file("crates/core/a/tests/t.rs", "fn main() {}\n");
    std::fs::remove_dir_all(tree.0.join(".git")).expect("remove the git dir");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a tree the lint cannot read must exit 3 — never 0 and never 1:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("CANNOT ANSWER"),
        "the refusal must use the in-tree marker:\n{}",
        text(&out)
    );
}

/// This repository carries none of it: every test under crates/*/*/tests
/// that feeds a child goes through the door.
#[test]
fn this_tree_feeds_every_child_through_the_door() {
    let out = run_lint_in(&repo_root());
    assert!(
        out.status.success(),
        "the tree must pass its own lint; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}
