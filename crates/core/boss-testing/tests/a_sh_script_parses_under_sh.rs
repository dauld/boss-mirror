//! `infra/lint/a-sh-script-parses-under-sh.sh` is RUN, not read —
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (2026-09-18, estate alarms 356e1885 "forge unobserved" and
//! 2d5e26fd "boss-gcp observe-host failed"). Train #439 turned
//! `printf '%s' "$1" | sed … | head -n 1` in infra/estate/observe-lib.sh
//! into a here-string — the repair the producer-coin pin names for a
//! bash script — and the gate was green because the only lint that runs
//! that library sources it into bash. Its first line says `#!/bin/sh`;
//! on the forge and boss-gcp that is dash, which refuses `<<<` at parse
//! time, so every observer firing after 04:16Z died on line 69 before
//! its first command ran. Six hours of two hosts unobserved, for one
//! token the tree's own shebang had already forbidden.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "dash still refuses and the regex still matches"; this file owns
//! the VERDICT on a tree — a tree of honest shebangs exits 0 and says
//! how many it parsed, a sh script needing bash exits 1 naming file and
//! line, a bash script using bash is not the lint's business, and a tree
//! it cannot read exits 3 rather than certifying it.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-sh-script-parses-under-sh.sh";

fn lint() -> PathBuf {
    repo_root().join(LINT)
}

/// A synthetic repository the lint can be run against: a real git index
/// (the lint asks `git ls-files`, so an untracked file must not count),
/// the lint at the path its own `cd "$(dirname $0)/../.."` resolves
/// from, and the two helpers it sources.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("a-sh-script-parses-under-sh-{tag}"));
        scratch::create_dir(&root.join("infra/lint/lib"));
        for rel in [
            LINT,
            "infra/lint/lib/git-answer.sh",
            "infra/lint/lib/scanned.sh",
        ] {
            let body = std::fs::read_to_string(repo_root().join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            scratch::write_exec(&root.join(rel), &body);
        }
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "."]);
        Tree(root)
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

/// The parser and the regex prove themselves on every invocation and
/// SAY so.
#[test]
fn the_lint_proves_itself_on_every_invocation() {
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
    assert!(text(&out).contains("self-test ok"), "{}", text(&out));
}

/// BEHAVIOUR 1 — honest shebangs are clean, and the count is the sh
/// scripts PARSED, not every shell in the tree: a bash script may use
/// a here-string, `[[` and pipefail, and is not read.
#[test]
fn a_tree_of_honest_shebangs_is_clean_and_counts_the_sh_scripts() {
    let tree = Tree::new("clean");
    tree.file(
        "infra/estate/a-sh-library.sh",
        "#!/bin/sh\nspool_put() {\n    at=${1#*'\"observed_at\":\"'}\n    if [ \"$at\" = \"$1\" ]; then at=; else at=${at%%'\"'*}; fi\n    ls -1 \"$d\" | grep -c '\\.json$'\n}\n",
    );
    tree.file(
        "infra/oss-quickstart/database-from-url.sh",
        "#!/usr/bin/env sh\nurl=\"$1\"\ndb=\"${url##*/}\"\nprintf '%s\\n' \"$db\"\n",
    );
    tree.file(
        "infra/forge/a-bash-tool.sh",
        "#!/usr/bin/env bash\nset -euo pipefail\n[[ -n \"$1\" ]] && grep -q x <<<\"$1\"\nx=${1//a/b}\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "honest shebangs must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    let said = text(&out);
    assert!(
        said.contains("scanned 2 sh script(s)"),
        "the count is the two sh scripts, not the bash one:\n{said}"
    );
    assert!(
        said.contains("every #!/bin/sh script parses under sh"),
        "{said}"
    );
}

/// BEHAVIOUR 2 — the shape #439 shipped, in a sh library: refused with
/// file, line and dash's own words. And the bashisms dash parses as
/// something else rather than refusing (`[[`, pipefail) are named the
/// same way.
#[test]
fn a_sh_script_that_needs_bash_is_refused_by_file_and_line() {
    let tree = Tree::new("here-string");
    tree.file(
        "infra/estate/a-sh-library.sh",
        "#!/bin/sh\n# a comment may say <<< without harm\nspool_put() {\n    at=$(sed -n 's/x/y/p' <<<\"$1\")\n}\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a here-string under #!/bin/sh is a violation (exit 1):\n{}",
        text(&out)
    );
    let said = text(&out);
    assert!(
        said.contains("infra/estate/a-sh-library.sh:4:"),
        "the refusal names the file and the LINE (not the comment on line 2):\n{said}"
    );
    assert!(
        said.contains("Syntax error: redirection unexpected"),
        "the refusal carries dash's own words, the ones the journal showed:\n{said}"
    );

    let tree = Tree::new("bashisms");
    tree.file(
        "infra/gcp/hook.sh",
        "#!/bin/sh\nset -euo pipefail\n[[ -f \"$1\" ]] && echo yes\n",
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let said = text(&out);
    assert!(
        said.contains("infra/gcp/hook.sh:2:"),
        "pipefail is named:\n{said}"
    );
    assert!(
        said.contains("infra/gcp/hook.sh:3:"),
        "[[ is named:\n{said}"
    );
}

/// BEHAVIOUR 3 — a tree with no sh script at all is not a clean tree,
/// it is a scan of nothing (lib/scanned.sh): exit 1, never `clean`.
#[test]
fn a_tree_with_no_sh_script_is_a_scan_of_nothing() {
    let tree = Tree::new("nothing");
    tree.file("infra/x.sh", "#!/usr/bin/env bash\necho hi\n");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "zero sh scripts parsed is red, not clean:\n{}",
        text(&out)
    );
    assert!(
        !text(&out).contains("every #!/bin/sh script parses under sh"),
        "{}",
        text(&out)
    );
}

/// BEHAVIOUR 4 — without a parser the lint cannot answer: exit 3, and
/// it says which parser it wanted. Never 0.
#[test]
fn without_dash_the_lint_says_it_cannot_answer() {
    let tree = Tree::new("no-dash");
    tree.file("infra/x.sh", "#!/bin/sh\necho hi\n");
    let out = Command::new("bash")
        .arg(tree.0.join(LINT))
        .current_dir(&tree.0)
        .env("GIT_CEILING_DIRECTORIES", "")
        .env("LINT_SH", "a-shell-this-machine-does-not-have")
        .output()
        .expect("run the lint");
    assert_eq!(
        out.status.code(),
        Some(3),
        "no parser is a fact about the machine (exit 3):\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("a-shell-this-machine-does-not-have"),
        "{}",
        text(&out)
    );
}

/// The live tree: the lint passes on the checkout this test runs in,
/// and the count is the sh scripts it holds today (8 at this writing;
/// any positive number is the assertion — the exact figure is the
/// tree's to change).
#[test]
fn the_repository_itself_parses_under_sh() {
    let out = Command::new("bash")
        .arg(lint())
        .current_dir(repo_root())
        .output()
        .expect("run the lint on the repository");
    assert!(
        out.status.success(),
        "the repository's sh scripts must parse under sh:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("scanned "), "{}", text(&out));
}
