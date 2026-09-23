//! `infra/lint/a-flattened-struct-cannot-deny-unknown-fields.sh` is RUN,
//! not read — against synthetic trees, so every property below is one
//! the lint actually has.
//!
//! THE CLASS (backlog b36a99e5). serde hands a `#[serde(flatten)]`ed
//! struct only the keys it names, so a `deny_unknown_fields` on that
//! struct refuses nothing: the attribute reads as enforcement to every
//! reader and holds nothing. Measured live on the dispatcher's draft POST
//! body (backlog a2358e7c F3, 2026-09-19): it flattened `RawRule`, and a
//! body carrying a misspelled trigger key parsed CLEAN and would have
//! landed a draft rule with no trigger at all. That instance was repaired
//! by lifting the extra key off the object by hand and deserializing the
//! rest as the closed struct (`split_draft_body`). This lint refuses the
//! class, so the next person to add the attribute to one of the flatten
//! targets is told at the pre-flight rather than believing it took.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "the scanner still matches" (mawk silently matches nothing for
//! an interval or a `\s`, and a scanner that matches nothing passes every
//! file), and runs on every invocation; this file owns the VERDICT on a
//! tree — clean exits 0, the trap exits 1 naming BOTH ends of it.
//!
//! NO REFUSED SHAPE IS SPELLED IN THIS FILE. Fixture bodies use `%D` for
//! the serde attribute's closed-set word and `%F` for the flatten word,
//! expanded at runtime, so this file is scanned by the lint it proves
//! like every other `.rs` under crates/ — the technique
//! `a_fixture_path_cannot_be_a_literal.rs` already uses.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-flattened-struct-cannot-deny-unknown-fields.sh";

/// A synthetic repository: the lint at the path its own
/// `cd "$(dirname "$0")/../.."` resolves from, its libs, and a crates/.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("flatten-deny-lint-{tag}"));
        scratch::create_dir(&root.join("infra/lint"));
        let body = std::fs::read_to_string(repo_root().join(LINT)).expect("read the lint");
        scratch::write_exec(&root.join(LINT), &body);
        boss_testing::copy_lint_libs(&root);
        Tree(root)
    }

    /// Add a `.rs` file, expanding `%D` and `%F` (see the module note).
    fn rs(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        let expanded = body
            .replace("%D", "deny_unknown_fields")
            .replace("%F", "flatten");
        scratch::write_file(&path, &expanded);
        self
    }

    fn run(&self) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The scanner proves itself; this test only insists it is run and says so.
#[test]
fn the_scanner_proves_itself_on_every_invocation() {
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
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
        "the self-test must SAY it ran:\n{}",
        text(&out)
    );
}

/// Every shape the tree uses today and must keep using passes: a closed
/// struct nobody flattens, a flatten of an open struct, the dispatcher's
/// repaired shape (no flatten, the key lifted by hand), and prose in a
/// doc comment that names both words — as `boss-dispatcher/src/http.rs`
/// does to explain its own repair.
#[test]
fn a_tree_without_the_trap_is_clean() {
    let tree = Tree::new("clean");
    tree.rs(
        "crates/a/src/lib.rs",
        "\
/// NOT `#[serde(%F)]`: `RawRule`'s `#[serde(%D)]` is inert under one.
#[derive(Deserialize)]
#[serde(rename_all = \"snake_case\", %D)]
pub struct RawRule {
    pub name: String,
}

#[derive(Serialize, Deserialize)]
pub struct Account {
    pub id: String,
}

#[derive(Serialize)]
pub struct AccountWithContacts {
    #[serde(%F)]
    pub account: Account,
    pub contacts: Vec<String>,
}
",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "a tree without the trap must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    assert!(
        text(&out).contains("scanned"),
        "the lint must say how much it read:\n{}",
        text(&out)
    );
}

/// The trap, across two crates and in rustfmt's wrapped attribute form,
/// is refused — and the verdict names BOTH ends: the struct whose
/// attribute is inert, and the flatten that makes it so. Either end alone
/// sends the reader to re-derive the other.
#[test]
fn the_trap_is_named_by_the_struct_and_the_flatten_site() {
    let tree = Tree::new("trap");
    tree.rs(
        "crates/rules/src/raw.rs",
        "\
#[derive(Debug, Deserialize)]
#[serde(
    rename_all = \"snake_case\",
    %D
)]
/// A closed rule spec.
pub(crate) struct RawRule<T> {
    pub name: T,
}
",
    )
    .rs(
        "crates/api/src/http.rs",
        "\
#[derive(Deserialize)]
struct DraftBody {
    /// who declares it
    source: Option<String>,
    #[serde(default, %F)]
    rule: Option<rules::RawRule<String>>,
}
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "the trap must exit 1:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        "RawRule",
        "crates/rules/src/raw.rs:7",
        "crates/api/src/http.rs:5",
        "split_draft_body",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?}:\n{msg}"
        );
    }
}
