//! `infra/lint/no-personal-address-in-infra.sh` is RUN, not read —
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (2026-09-18, backlog 67e754cc). The publish-to-github
//! review of packet b831512f read the 31 files that would become public
//! for the first time and found, in infra/cluster/dns/access.toml, the
//! playground's onboarding policy declaring four individuals by
//! PERSONAL e-mail address — David's own gmail and three third parties
//! admitted as visitors. `infra/lint/no-secrets.sh` was right that they
//! are not credentials, so the tree was "clean" all the way to the one
//! step a person reads. The publish stopped at sign-off on that
//! finding. An address in `infra/` is a declaration about who may do
//! what to the estate; a person's private address does not belong in a
//! public one. The declaration now says `include.unmanaged = true` and
//! this lint refuses the next personal address at pre-flight.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "the pattern still matches and the domain rule still sorts";
//! this file owns the VERDICT on a tree — company and reserved addresses
//! are clean and the count says how many files were read, a personal
//! address exits 1 naming file, line and the address's DOMAIN (never
//! the whole address, which would copy it into a CI log), a file
//! outside infra/ is not the lint's business, and a tree it cannot read
//! exits 3 rather than certifying it.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/no-personal-address-in-infra.sh";

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
        let root = scratch::scratch_dir(&format!("no-personal-address-in-infra-{tag}"));
        // The whole lib directory, from its one definition (c3364c85):
        // a hand-typed helper list is how a sibling pin ran its lint
        // for months with a function undefined.
        boss_testing::copy_lint_libs(&root);
        let body = std::fs::read_to_string(repo_root().join(LINT))
            .unwrap_or_else(|e| panic!("read {LINT}: {e}"));
        scratch::write_exec(&root.join(LINT), &body);
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

/// How many helpers `copy_lint_libs` carries into a tree — the .sh files
/// in each of its directories (`LINT_LIB_DIRS`: infra/lint/lib/ and,
/// since backlog 834ddb7c, infra/lib/), counted, never typed.
fn lint_lib_count() -> usize {
    boss_testing::LINT_LIB_DIRS
        .iter()
        .map(|dir| {
            std::fs::read_dir(repo_root().join(dir))
                .unwrap_or_else(|e| panic!("{dir}: {e}"))
                .filter(|e| {
                    e.as_ref()
                        .ok()
                        .is_some_and(|e| e.path().extension().is_some_and(|x| x == "sh"))
                })
                .count()
        })
        .sum()
}

/// The pattern and the domain rule prove themselves on every invocation
/// and SAY so.
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

/// BEHAVIOUR 1 — company and reserved addresses are clean, and the
/// count is the files under infra/ READ, not the addresses found: a
/// tree with no address at all is still a tree that was scanned.
#[test]
fn company_and_reserved_addresses_are_clean_and_the_count_is_files_read() {
    let tree = Tree::new("clean");
    tree.file(
        "infra/cluster/dns/access.toml",
        "[[application.policy]]\nname = \"operators\"\ninclude.emails = [\"david@algedonic.dev\", \"guest@algedonic.dev\"]\n# mail the IdP sends from\nfrom = \"auth@send.algedonic.dev\"\n",
    );
    tree.file(
        "infra/lint/fixture.sh",
        "#!/usr/bin/env bash\nadmin=alice@example.com\nlint=lint@example.invalid\naudit=audit@boss.example\nva=validation-admin@boss.local\nt=test@x.test\n",
    );
    tree.file(
        "infra/gcp/wg.service",
        "[Unit]\nRequires=wg-quick@wg0.service\nPartOf=wg-quick@wg0.service\n",
    );
    tree.file(
        "infra/forge/publish-github-pr.sh",
        "#!/usr/bin/env bash\ngit -c user.email=dauld@users.noreply.github.com commit -q\n",
    );
    // A no-reply mailbox is a machine's, not a person's — the commit
    // trailer every car carries named one and the train gate for #461
    // refused the assembled tree on it (2026-09-18, the day the lint
    // landed).
    tree.file(
        "infra/platform/documents/builder-rules.md",
        "Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>\nReply-To: no-reply@example-vendor.io\n",
    );
    tree.file("infra/estate/estate.toml", "[node]\nid = \"forge\"\n");
    let out = tree.run();
    assert!(
        out.status.success(),
        "company, reserved and no-reply addresses must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    let said = text(&out);
    // Six fixtures, plus the lint and every helper under lib/ — which
    // live under infra/ themselves and are read like any other file.
    let want = format!("scanned {} file(s)", 6 + 1 + lint_lib_count());
    assert!(
        said.contains(&want),
        "the count is every file under infra/ read ({want}):\n{said}"
    );
    assert!(said.contains("no personal address"), "{said}");
}

/// BEHAVIOUR 2 — the shape the mirror review found: a personal address
/// in a declaration under infra/, refused with file, line and the
/// address's domain — and only the domain, because a lint that names
/// the address has copied it into the gate log.
#[test]
fn a_personal_address_under_infra_is_refused_by_file_line_and_domain_only() {
    let tree = Tree::new("personal");
    tree.file(
        "infra/cluster/dns/access.toml",
        "[[application.policy]]\nname = \"onboarding\"\n# a comment naming someone@gmail.com is a finding too: the file is public\ninclude.emails = [\"david@algedonic.dev\", \"visitor-one@gmail.com\", \"visitor-two@saouma.ch\"]\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a personal address under infra/ is a violation (exit 1):\n{}",
        text(&out)
    );
    let said = text(&out);
    assert!(
        said.contains("infra/cluster/dns/access.toml:3:") && said.contains("gmail.com"),
        "the comment's address is named by file, line and domain:\n{said}"
    );
    assert!(
        said.contains("infra/cluster/dns/access.toml:4:") && said.contains("saouma.ch"),
        "the declared address is named by file, line and domain:\n{said}"
    );
    assert!(
        !said.contains("visitor-one")
            && !said.contains("visitor-two")
            && !said.contains("someone@"),
        "the local part is never printed — the refusal must not copy the address into a log:\n{said}"
    );
    assert!(
        !said.contains("david@algedonic.dev"),
        "the company address on the same line is not a finding:\n{said}"
    );
}

/// BEHAVIOUR 3 — a file outside infra/ is not the lint's business
/// (fixtures, seeds and docs carry made-up people on purpose), and an
/// UNTRACKED file under infra/ is not read (the lint asks git).
#[test]
fn files_outside_infra_and_untracked_files_are_not_read() {
    let tree = Tree::new("elsewhere");
    tree.file(
        "examples/brewery/seeds/people.toml",
        "[[employee]]\nemail = \"brewer@gmail.com\"\n",
    );
    tree.file(
        "crates/core/boss-people/src/lib.rs",
        "const SAMPLE: &str = \"someone@hotmail.com\";\n",
    );
    tree.file("infra/estate/estate.toml", "[node]\nid = \"forge\"\n");
    let untracked = tree.0.join("infra/forge/notes.txt");
    scratch::create_dir(untracked.parent().unwrap());
    scratch::write_file(&untracked, "ask person@yahoo.com\n");
    let out = tree.run();
    assert!(
        out.status.success(),
        "addresses outside infra/ and an untracked file are not findings; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    // One fixture under infra/, plus the lint and every helper under lib/.
    let want = format!("scanned {} file(s)", 1 + 1 + lint_lib_count());
    assert!(text(&out).contains(&want), "{want}:\n{}", text(&out));
}

/// BEHAVIOUR 4 — a tree git cannot list is exit 3, never clean.
#[test]
fn a_tree_it_cannot_read_is_a_refusal_not_a_pass() {
    let root = scratch::scratch_dir("no-personal-address-in-infra-nogit");
    boss_testing::copy_lint_libs(&root);
    let body = std::fs::read_to_string(repo_root().join(LINT)).unwrap();
    scratch::write_exec(&root.join(LINT), &body);
    let out = Command::new("bash")
        .arg(root.join(LINT))
        .current_dir(&root)
        .env("GIT_CEILING_DIRECTORIES", root.parent().unwrap())
        .output()
        .expect("run the lint outside a repository");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        out.status.code(),
        Some(3),
        "no repository to list is exit 3 (the machine could not answer):\n{}",
        text(&out)
    );
}

/// The tree this lint ships in passes it: the mirror review's finding
/// is gone and nothing else under infra/ names a person by a private
/// address.
#[test]
fn the_shipped_tree_carries_no_personal_address_under_infra() {
    let out = Command::new("bash")
        .arg(lint())
        .current_dir(repo_root())
        .output()
        .expect("run the lint on the tree");
    assert!(
        out.status.success(),
        "the shipped infra/ must be clean (exit {:?}):\n{}",
        out.status.code(),
        text(&out)
    );
}
