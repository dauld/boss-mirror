//! `infra/lint/no-employee-id-literal.sh` is RUN, not read — against
//! synthetic trees, so every property below is one the lint actually
//! has.
//!
//! THE CLASS (backlog 3c23662d, design 42277636, audit H6). On
//! 2026-09-18 sixteen production lines named a person: `"emp-david"` as
//! the `owner_id` of every packet the platform files — every car
//! (Tier-1 `boss_jobs::car`), gate-run, design doc, conductor alarm,
//! estate/cadence/sensor/DNS alarm, the forge watchdog's alert, the
//! nightly install-smoke red — and `"emp-cto"` twice in the bootstrap
//! walk's synthetic sign-off. Each was one deployment's fact in code
//! every deployment runs. The car that added this lint removed all
//! sixteen; the lint is what keeps the count at zero, and this file is
//! what keeps the lint honest.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "the SCANNER still matches" (mawk answers an interval by
//! matching nothing, and that is invisible from outside); this file owns
//! "the lint's VERDICT on a tree" — clean exits 0, a violation exits 1
//! naming file, line and id, and a tree it cannot read exits 3 rather
//! than certifying it.
//!
//! The fixture bodies write the id prefix as `%E`, expanded at runtime,
//! so the shapes this file plants are not also lines in it; this file
//! lives under `tests/` and is exempt regardless, but a proof that
//! commits what it forbids is worth less.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/no-employee-id-literal.sh";
const PREFIX: &str = "emp-";

fn lint() -> PathBuf {
    repo_root().join(LINT)
}

/// A synthetic repository the lint can be run against: a real git index
/// (the lint asks `git grep`, so an untracked fixture must not count),
/// the lint itself at the path its own `cd "$(dirname $0)/../.."`
/// resolves from, and the git-answer helper it sources.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("no-employee-id-literal-{tag}"));
        scratch::create_dir(&root.join("infra/lint/lib"));
        for rel in [LINT, "infra/lint/lib/git-answer.sh"] {
            let body = std::fs::read_to_string(repo_root().join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            scratch::write_exec(&root.join(rel), &body);
        }
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "."]);
        Tree(root)
    }

    /// Add a tracked file, `%E` expanded to the id prefix.
    fn file(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        scratch::write_file(&path, &body.replace("%E", PREFIX));
        git(&self.0, &["add", rel]);
        self
    }

    fn run(&self) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .current_dir(&self.0)
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

/// The scanner proves itself on every invocation and SAYS so.
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
        "the self-test must SAY it ran:\n{}",
        text(&out)
    );
}

/// BEHAVIOUR 1 — a tree that READS every person is clean: prose, a
/// `#[cfg(test)]` module, the platform identities (the bootstrap one and
/// the operator baseline's), a declared placeholder, and a shell file
/// that asks the registry.
#[test]
fn a_tree_that_reads_its_people_is_clean() {
    let tree = Tree::new("clean");
    tree.file(
        "infra/operator-baseline/operator_hires.toml",
        "[[hire]]\nid = \"%Eaudit\"\nrole = \"audit-readonly\"\n",
    );
    tree.file(
        "crates/core/x/src/lib.rs",
        "\
//! A doc comment may say %Esomeone to tell the story.
pub fn owner(port: &dyn PlatformOwner) -> String { port.platform_owner() }
const BOOT: &str = \"%Ebootstrap-admin\";
const AUDIT: &str = \"%Eaudit\";
fn scaffold() -> String {
    // employee-id-ok: a placeholder the operator renames
    String::from(\"%Eowner\")
}
#[cfg(test)]
mod tests {
    const WHO: &str = \"%Estaged\";
}
",
    );
    tree.file(
        "infra/forge/alert-lib.sh",
        "# a comment may say %Esomeone\nOWNER=$(curl \"$API/api/people?role=platform-admin\")\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "a tree that reads its people must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    assert!(
        text(&out).contains("no production line names an employee"),
        "a clean tree must be certified in words:\n{}",
        text(&out)
    );
}

/// BEHAVIOUR 2 — the sixteen sites this car removed, in their shipped
/// shapes: a Rust `owner_id` literal (car.rs, train.rs, gate.rs,
/// design.rs, the four handlers), a `signed_by` literal (bootstrap.rs), a
/// shell body (alert-lib.sh, nightly.sh). Each is named with file, line
/// and id, and the verdict names the door.
#[test]
fn the_removed_sites_are_refused_by_file_line_and_id() {
    let tree = Tree::new("violating");
    tree.file(
        "crates/core/boss-jobs/src/car.rs",
        "\
pub fn car_body(branch: &str) -> Value {
    json!({
        \"kind\": \"ship-a-change\",
        \"owner_id\": \"%Edavid\",
    })
}
",
    );
    tree.file(
        "crates/core/boss-jobs/src/bootstrap.rs",
        "fn walk() {\n    out.insert(\"signed_by\".into(), json!(\"%Ecto\"));\n}\n",
    );
    tree.file(
        "infra/forge/alert-lib.sh",
        "python_free_json() {\n    printf '{\"kind\":\"backlog-item\",\"owner_id\":\"%Edavid\"}'\n}\n",
    );
    tree.file(
        "infra/install-smoke/nightly.sh",
        "body=$(jq -n '{\n            owner_id: \"%Edavid\",\n        }')\n",
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
        "crates/core/boss-jobs/src/car.rs:4",
        "crates/core/boss-jobs/src/bootstrap.rs:2",
        "infra/forge/alert-lib.sh:2",
        "infra/install-smoke/nightly.sh:2",
        &format!("{PREFIX}david"),
        &format!("{PREFIX}cto"),
        // the door
        "platform_owner",
        "BOSS_PLATFORM_OWNER",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?} — a verdict someone must \
             re-derive is not a verdict (CLAUDE.md §Diagnosis):\n{msg}"
        );
    }
}

/// A `#[cfg(test)]` region ENDS: a literal after the module is
/// production, and a marker without a reason is not a marker.
#[test]
fn a_test_module_ends_and_a_bare_marker_is_no_marker() {
    let tree = Tree::new("edges");
    tree.file(
        "crates/core/x/src/lib.rs",
        "\
#[cfg(test)]
mod tests {
    const WHO: &str = \"%Estaged\";
}
const OWNER: &str = \"%Eafter\";
// employee-id-ok
const BARE: &str = \"%Ebare\";
",
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let msg = text(&out);
    assert!(
        msg.contains("src/lib.rs:5") && msg.contains("src/lib.rs:7"),
        "both the post-module literal and the bare-marker one must be named:\n{msg}"
    );
    assert!(
        !msg.contains("src/lib.rs:3"),
        "the staged id inside the test module must not be named:\n{msg}"
    );
}

/// BEHAVIOUR 3 — a tree the lint cannot read is refused (exit 3), never
/// certified. House style since `infra/lint/lib/git-answer.sh`.
#[test]
fn a_lint_that_cannot_read_the_tree_refuses() {
    let tree = Tree::new("unreadable");
    tree.file("crates/core/x/src/lib.rs", "fn main() {}\n");
    std::fs::remove_dir_all(tree.0.join(".git")).expect("remove the git dir");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a tree the lint cannot read must exit 3 — never 0 and never 1:\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("CANNOT ANSWER"),
        "the refusal must use the in-tree marker:\n{msg}"
    );
    assert!(
        !msg.contains("no production line names an employee"),
        "a lint that read nothing must not certify the tree:\n{msg}"
    );
}
