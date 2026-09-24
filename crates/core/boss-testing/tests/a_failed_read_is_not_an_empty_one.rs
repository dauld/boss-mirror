//! `infra/lint/a-failed-read-is-not-an-empty-one.sh` is RUN, not read —
//! against the real tree and against synthetic ones, so every property
//! below is one the lint actually has.
//!
//! THE CLASS (backlog 223ebcd6, after 7a7bfc88). One line of
//! punctuation turns an outage into an honest-looking zero:
//!
//! ```text
//! const pBody = pResp.ok ? await pResp.json() : [];
//! ```
//!
//! 7a7bfc88 lifted the fix into `apps/web/src/data/readState.ts` and
//! closed with the adoption unfinished: on 2026-09-23 the shape was
//! still live in VendorsList, VendorPage (four reads) and ExecPage,
//! found by an operator's grep rather than by anything that runs. The
//! car that adopted readState at those sites added this lint so the
//! next page cannot reintroduce the line one review at a time; this
//! file is what keeps the lint honest.
//!
//! The allowlist (PartsList's four guarded reads, the two web-kit
//! registry loaders whose `null` is refused by the `Array.isArray` on
//! the next line) is copied from the REAL tree into every fixture, so a
//! fixture is exactly as clean as the tree the lint guards.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-failed-read-is-not-an-empty-one.sh";

/// The files the lint's allowlist names, copied verbatim into fixtures.
const ALLOWED: [&str; 3] = [
    "apps/web/src/parts/PartsList.svelte",
    "libs/web-kit/src/session/classes.svelte.ts",
    "libs/web-kit/src/session/departments.svelte.ts",
];

struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("a-failed-read-is-not-an-empty-one-{tag}"));
        boss_testing::copy_lint_libs(&root);
        let body = std::fs::read_to_string(repo_root().join(LINT))
            .unwrap_or_else(|e| panic!("read {LINT}: {e}"));
        scratch::write_exec(&root.join(LINT), &body);
        let tree = Tree(root);
        git(&tree.0, &["init", "-q", "-b", "main"]);
        for rel in ALLOWED {
            let real = std::fs::read_to_string(repo_root().join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            tree.file(rel, &real);
        }
        git(&tree.0, &["add", "."]);
        tree
    }

    fn file(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        scratch::write_file(&path, body);
        git(&self.0, &["add", rel]);
        self
    }

    fn run(&self) -> Output {
        run_in(&self.0)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_in(dir: &Path) -> Output {
    Command::new("bash")
        .arg(dir.join(LINT))
        .current_dir(dir)
        .env("GIT_CEILING_DIRECTORIES", "")
        .output()
        .unwrap_or_else(|e| panic!("run the lint in {}: {e}", dir.display()))
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

/// The tree this lint guards is clean — which it was not on origin/main
/// b9ffdc81, where VendorsList, VendorPage and ExecPage carried seven
/// swallowed reads between them.
#[test]
fn the_tree_swallows_no_failed_read() {
    let out = run_in(&repo_root());
    assert!(
        out.status.success(),
        "the tree must be clean; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    assert!(
        text(&out).contains("no failed read is painted as an empty one"),
        "a clean tree must be certified in words:\n{}",
        text(&out)
    );
}

/// Every shape that paints an outage as nothing — `[]`, `null`, `{}`,
/// with or without parentheses, in a component or a module — is
/// refused, each named by file and line, and the verdict names the door.
#[test]
fn every_swallowing_shape_is_refused_by_file_and_line() {
    let tree = Tree::new("violating");
    tree.file(
        "apps/web/src/vendors/VendorsList.svelte",
        "<script lang=\"ts\">\n  const pBody = pResp.ok ? await pResp.json() : [];\n  const bBody = bResp.ok ? (await bResp.json()) : null;\n</script>\n",
    );
    tree.file(
        "libs/web-kit/src/thing.ts",
        "export async function f(r: Response) {\n  return r.ok ? await r.json() : {};\n}\n",
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
        "apps/web/src/vendors/VendorsList.svelte:2",
        "apps/web/src/vendors/VendorsList.svelte:3",
        "libs/web-kit/src/thing.ts:2",
        "readStateOfResponse",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?} — a verdict someone must \
             re-derive is not a verdict (CLAUDE.md §Diagnosis):\n{msg}"
        );
    }
}

/// What is NOT the class: a comment quoting the line (readState.ts and
/// support/reads.ts tell its story), the helper itself, a unit test, and
/// a read that checks `ok` before it parses.
#[test]
fn prose_the_helper_a_test_and_a_checked_read_are_clean() {
    let tree = Tree::new("clean");
    tree.file(
        "apps/web/src/data/readState.ts",
        "//   const pBody = pResp.ok ? await pResp.json() : [];\nexport const x = r.ok ? await r.json() : [];\n",
    );
    tree.file(
        "apps/web/src/support/reads.ts",
        "// const pBody = pResp.ok ? await pResp.json() : [];  // accounts\n/// five `pResp.ok ? await pResp.json() : []` sites\n * pResp.ok ? await pResp.json() : []\n",
    );
    tree.file(
        "apps/web/src/data/readState.test.ts",
        "const body = resp.ok ? await resp.json() : [];\n",
    );
    tree.file(
        "apps/web/src/exec/ExecPage.svelte",
        "<script lang=\"ts\">\n  peopleRead = readStateOfResponse('/api/people', pResp);\n  if (!pResp.ok) return;\n  const pBody = await pResp.json();\n</script>\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "prose, the helper, a test and a checked read must pass; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
}

/// The allowance is a count, and it must EQUAL the file's count: a site
/// fixed without lowering the allowance leaves a hole for the next one
/// to walk through (lib/allowlist.sh's rule, applied to a count).
#[test]
fn an_allowance_is_held_to_its_count_both_ways() {
    let rel = ALLOWED[0];
    let real = std::fs::read_to_string(repo_root().join(rel)).expect("read the allowed file");

    let tree = Tree::new("over");
    tree.file(
        rel,
        &format!("{real}\n<script>const extra = x.ok ? await x.json() : [];</script>\n"),
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains(rel), "{}", text(&out));

    let fixed: String = real
        .lines()
        .filter(|l| !l.contains("cpResp.ok ? await cpResp.json()"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(fixed, real, "the fixture must remove one allowed site");
    let tree = Tree::new("under");
    tree.file(rel, &fixed);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "an allowance above its file's count must be lowered:\n{}",
        text(&out)
    );
    assert!(text(&out).contains(rel), "{}", text(&out));
}

/// A tree the lint cannot read is refused (exit 3), never certified.
#[test]
fn a_lint_that_cannot_read_the_tree_refuses() {
    let tree = Tree::new("unreadable");
    std::fs::remove_dir_all(tree.0.join(".git")).expect("remove the git dir");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a tree the lint cannot read must exit 3 — never 0 and never 1:\n{}",
        text(&out)
    );
    assert!(
        !text(&out).contains("no failed read is painted as an empty one"),
        "a lint that read nothing must not certify the tree:\n{}",
        text(&out)
    );
}
