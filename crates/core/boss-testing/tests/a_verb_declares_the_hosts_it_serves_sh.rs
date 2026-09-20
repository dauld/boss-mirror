//! `infra/lint/a-verb-declares-the-hosts-it-serves.sh` is RUN, not read
//! — against the real `infra/ops/` tree and a synthetic estate, so every
//! property below is one the lint actually has.
//!
//! WHAT THIS FILE OWNS (backlog 3d18b741, 2026-09-20). The lint's
//! universe of "a real estate node id" was derived from the `INSERT INTO
//! nodes` rows in `infra/postgres/schema/*.sql`. Node rows moved to
//! `infra/estate/estate.toml` on 2026-09-18 (ee368d0c) — the estate now
//! reaches a database through `infra/seed-estate.sh`, not through a
//! migration — and migration 20260918063829 removed the rows where
//! nothing referenced them. Measured on 2026-09-20: two schema files
//! still carried such rows, against seven nodes declared in estate.toml.
//! So the lint's estate was two leftover migrations, and its floor of
//! five ids was met only by that residue.
//!
//! Two consequences, both pinned below: a host declared in estate.toml
//! TODAY was refused as "not an estate node id" — confident, specific
//! and wrong, the wrong-target shape of CLAUDE.md §Doors with the lint
//! as the wrong target; and routine tidying of those two migrations
//! would have stopped the lint outright, reading as a broken tree rather
//! than as a lint that lost its source.
//!
//! A migration is a record of how the database GOT here. estate.toml is
//! the statement of what the estate IS. This file holds the lint to the
//! second: an id the declaration names is real even when no migration
//! ever mentioned it, and an id only a migration mentions is not.

use boss_testing::{repo_root, scratch};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-verb-declares-the-hosts-it-serves.sh";
const ESTATE: &str = "infra/estate/estate.toml";

/// A synthetic repository the lint can be run against: the real lint at
/// the path its own `dirname/../..` resolves the repo from, the real
/// `infra/ops/` tree (the allowlist assembler, the runner and every verb
/// file — the lint's `GCP_MUTATING_ADMITTED` table names real verbs, so
/// a fixture of invented ones would fail as a stale admission), and an
/// estate + schema this test writes.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("a-verb-declares-the-hosts-{tag}"));
        boss_testing::copy_lint_libs(&root);
        let body = std::fs::read_to_string(repo_root().join(LINT))
            .unwrap_or_else(|e| panic!("read {LINT}: {e}"));
        scratch::write_exec(&root.join(LINT), &body);
        copy_dir(&repo_root().join("infra/ops"), &root.join("infra/ops"));
        // The verbs' argv[0] scripts live in these three directories and
        // the lint checks each is a file in the tree. Symlinked rather
        // than copied: this fixture varies the ESTATE and nothing else.
        for dir in ["infra/cluster", "infra/forge", "infra/gcp"] {
            std::os::unix::fs::symlink(repo_root().join(dir), root.join(dir))
                .unwrap_or_else(|e| panic!("link {dir}: {e}"));
        }
        scratch::create_dir(&root.join("infra/postgres/schema"));
        Tree(root)
    }

    /// Declare an estate of `ids`, in the shape `infra/estate/estate.toml`
    /// carries (flat keys the shell renderer reads, then one `[[node]]`
    /// table each).
    fn estate(&self, ids: &[String]) -> &Tree {
        let mut body = String::from("sor_url = \"http://198.51.100.7:7900\"\n\n");
        for id in ids {
            body.push_str(&format!(
                "[[node]]\nid = \"{id}\"\nlabel = \"{id}\"\naddress = \"198.51.100.1\"\n\
                 role = \"talos-worker\"\nnotes = \"a fixture node; [[node]] in prose\"\n\n"
            ));
        }
        scratch::create_dir(&self.0.join("infra/estate"));
        scratch::write_file(&self.0.join(ESTATE), &body);
        self
    }

    /// Plant a migration that seeds `nodes`, the way the two surviving
    /// ones do — the residue this lint must no longer read.
    fn migration_nodes(&self, ids: &[String]) -> &Tree {
        let mut body = String::from(
            "INSERT INTO nodes (id, label, address, role, cpu, memory_gb, disk_gb, notes) VALUES\n",
        );
        for id in ids {
            body.push_str(&format!(
                "    ('{id}', '{id}', '198.51.100.1', 'talos-worker', 1, 1, 1, 'seeded; then moved'),\n"
            ));
        }
        body.push_str("ON CONFLICT (id) DO NOTHING;\n");
        scratch::write_file(
            &self
                .0
                .join("infra/postgres/schema/999-a-fixture-seeded-nodes.sql"),
            &body,
        );
        self
    }

    /// A verb serving `hosts`, with a bare-command argv (no path for the
    /// lint's argv[0] check to resolve against this fixture).
    fn verb(&self, name: &str, hosts: &[&str]) -> &Tree {
        let hosts: Vec<String> = hosts.iter().map(|h| format!("\"{h}\"")).collect();
        scratch::write_file(
            &self.0.join(format!("infra/ops/verbs/{name}.json")),
            &format!(
                "{{\n  \"about\": \"a fixture verb — reads nothing\",\n  \
                 \"hosts\": [{}],\n  \"argv\": [\"df\", \"-h\"],\n  \"params\": []\n}}\n",
                hosts.join(", ")
            ),
        );
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

fn copy_dir(src: &Path, dst: &Path) {
    scratch::create_dir(dst);
    let entries = std::fs::read_dir(src).unwrap_or_else(|e| panic!("read {}: {e}", src.display()));
    for entry in entries {
        let from = entry.expect("a directory entry").path();
        let to = dst.join(from.file_name().expect("a file name"));
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to)
                .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
            let mode = std::fs::metadata(&from).expect("metadata").permissions();
            let _ = std::fs::set_permissions(&to, mode);
        }
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The estate node ids the tree DECLARES — read from the `[[node]]`
/// tables of `infra/estate/estate.toml`, which is where a node has been
/// declared since ee368d0c.
fn declared_node_ids() -> Vec<String> {
    let body = std::fs::read_to_string(repo_root().join(ESTATE))
        .unwrap_or_else(|e| panic!("read {ESTATE}: {e}"));
    let mut ids = Vec::new();
    let mut in_node = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_node = line == "[[node]]";
            continue;
        }
        if in_node
            && let Some(rest) = line.strip_prefix("id = \"")
            && let Some(id) = rest.strip_suffix('"')
        {
            ids.push(id.to_string());
        }
    }
    assert!(
        ids.len() >= 2,
        "{ESTATE} declares {} node(s) — this helper stopped parsing the file",
        ids.len()
    );
    ids
}

/// The declared estate plus `extra` — the fixtures vary the estate by
/// one id, so the real verb files (which serve forge and boss-gcp) stay
/// valid and the ONLY thing under test is where a host id comes from.
fn declared_plus(extra: &[&str]) -> Vec<String> {
    let mut ids = declared_node_ids();
    ids.extend(extra.iter().map(|id| id.to_string()));
    ids
}

/// THE REAL TREE. The lint passes, and the estate it reports is the one
/// the tree DECLARES — not a count that happens to match.
#[test]
fn the_real_tree_passes_and_its_estate_is_the_declared_one() {
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
        .current_dir(repo_root())
        .output()
        .expect("run the lint on the real tree");
    let said = text(&out);
    assert!(
        out.status.success(),
        "the lint failed on the real tree:\n{said}"
    );
    let declared = declared_node_ids();
    assert!(
        said.contains(&format!("from the {} estate node ids", declared.len())),
        "the lint reports an estate of a different size than the {} nodes {ESTATE} declares:\n{said}",
        declared.len()
    );
}

/// THE LIVE CONSEQUENCE. A node declared today, that no migration ever
/// mentioned, is a real host — a verb may serve it. Before 3d18b741 the
/// lint refused this as "not an estate node id".
#[test]
fn a_node_only_the_declaration_names_is_a_real_host() {
    let tree = Tree::new("declared-only");
    tree.estate(&declared_plus(&["w-3"]))
        .migration_nodes(&declared_node_ids())
        .verb("a-fixture-verb-on-the-new-host", &["w-3"]);
    let out = tree.run();
    let said = text(&out);
    assert!(
        out.status.success(),
        "a host declared in {ESTATE} was refused; the lint is reading something else:\n{said}"
    );
}

/// THE OTHER HALF. An id that only a MIGRATION mentions is not a host —
/// a migration records how the database got here, not what the estate
/// is. This is what proves the derivation moved rather than widened.
#[test]
fn a_node_only_a_migration_names_is_not_a_host() {
    let tree = Tree::new("migration-only");
    tree.estate(&declared_node_ids())
        .migration_nodes(&declared_plus(&["w-9-retired"]))
        .verb("a-fixture-verb-on-a-retired-host", &["w-9-retired"]);
    let out = tree.run();
    let said = text(&out);
    assert!(
        !out.status.success(),
        "a host only a migration mentions was accepted:\n{said}"
    );
    assert!(
        said.contains("w-9-retired") && said.contains("estate.toml"),
        "the refusal must name the host and where hosts are declared:\n{said}"
    );
}

/// TIDYING THE RESIDUE MUST NOT STOP THE LINT. With no `INSERT INTO
/// nodes` anywhere in the schema — which is where 20260918063829 was
/// already heading — the lint still knows the estate.
#[test]
fn an_empty_schema_directory_does_not_stop_the_lint() {
    let tree = Tree::new("no-migration-rows");
    tree.estate(&declared_node_ids())
        .verb("a-fixture-verb-on-a-worker", &["w-1"]);
    let out = tree.run();
    let said = text(&out);
    assert!(
        out.status.success(),
        "with the migration rows tidied away the lint stopped working:\n{said}"
    );
}

/// A DECLARATION IT CANNOT READ IS A REFUSAL, not a vacuous pass: with
/// no node parsed, every host check below would be meaningless.
#[test]
fn an_unreadable_declaration_is_refused_rather_than_passed() {
    let tree = Tree::new("empty-declaration");
    tree.estate(&[]).verb("a-fixture-verb-anywhere", &["w-1"]);
    let out = tree.run();
    let said = text(&out);
    assert!(
        !out.status.success(),
        "an estate of zero nodes passed vacuously:\n{said}"
    );
    assert!(
        said.contains("estate.toml"),
        "the refusal must name the file it could not read an estate from:\n{said}"
    );
}
