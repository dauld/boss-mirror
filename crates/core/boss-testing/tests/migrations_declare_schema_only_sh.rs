//! `infra/lint/migrations-declare-schema-only.sh` is RUN, not read —
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (backlog 393d3234, design 42277636, consolidation H4).
//! Measured 2026-09-18 on origin/main 1a09f660: seven migrations were
//! the only home a platform station had; the same was true of
//! step_plugins (7), cadence_rules (9) and delivery_policy (2). Car 1
//! moved stations to `infra/platform/stations/` and wrote the cutover
//! stamp into the lint; car 2 moved step plugins to
//! `infra/platform/step-plugins/` and added the table beside it; car 3
//! moved cadence rules to `infra/platform/cadence/` the same way; car 4
//! moved the delivery policy to `infra/platform/delivery-policy/` and
//! closed the item. The lint is what keeps every LATER migration from
//! re-opening the old home, and this file is what keeps the lint honest.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "the SCANNER still matches" and "the cutover comparison answers
//! rightly"; this file owns "the lint's VERDICT on a tree" — clean exits
//! 0 and says how many migrations it checked, a violation exits 1 naming
//! file, line, table and the door, an empty schema directory is red
//! rather than vacuously clean, and a tree it cannot read exits 3
//! rather than certifying it.
//!
//! Fixture file names are invented — none is a migration this tree
//! carries — so the gate's file-input index does not read this test as
//! a reader of the real schema directory.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/migrations-declare-schema-only.sh";
const SCHEMA: &str = "infra/postgres/schema";

fn lint() -> PathBuf {
    repo_root().join(LINT)
}

/// A synthetic repository the lint can be run against: a real git index
/// (the lint asks `git ls-files`, so an untracked fixture must not
/// count), the lint itself at the path its own `cd "$(dirname $0)/../.."`
/// resolves from, and the two helpers it sources.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("migrations-declare-schema-only-{tag}"));
        scratch::create_dir(&root.join("infra/lint/lib"));
        scratch::create_dir(&root.join(SCHEMA));
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

    /// Add a tracked migration under the schema directory.
    fn migration(&self, name: &str, body: &str) -> &Tree {
        let rel = format!("{SCHEMA}/{name}");
        scratch::write_file(&self.0.join(&rel), body);
        git(&self.0, &["add", &rel]);
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

/// A migration older than the cutover that inserts a station — the
/// history this lint leaves alone.
const HISTORY: &str = "100-a-history-row.sql";
const HISTORY_SQL: &str = "\
-- 100-a-history-row.sql — a seed the way they used to be written.
INSERT INTO stations (name, version, status, title, kind, predicate) VALUES
  ('old-station', 1, 'active', 'Old', 'batch', '{}'::jsonb)
ON CONFLICT (name, version) DO NOTHING;
";

/// A migration newer than the cutover that declares schema only, and
/// mentions the forbidden statement in prose.
const NEW_SCHEMA: &str = "20261001000000-a-column-arrives.sql";
const NEW_SCHEMA_SQL: &str = "\
-- 20261001000000-a-column-arrives.sql — no INSERT INTO stations here,
-- the row is in infra/platform/stations/.
ALTER TABLE stations ADD COLUMN IF NOT EXISTS colour TEXT;
UPDATE stations SET colour = 'red' WHERE name = 'old-station' AND status = 'active';
";

/// A migration newer than the cutover that re-opens the old home.
const NEW_INSERT: &str = "20261001000001-a-new-row-the-old-way.sql";
const NEW_INSERT_SQL: &str = "\
-- 20261001000001 — a station declared where it no longer lives.
ALTER TABLE stations ADD COLUMN IF NOT EXISTS colour TEXT;
INSERT INTO stations (name, version, status, title, kind, predicate)
SELECT 'new-station', 1, 'active', 'New', 'batch', '{}'::jsonb
 WHERE NOT EXISTS (SELECT 1 FROM stations WHERE name = 'new-station');
";

/// A migration newer than the cutover that inserts a step-plugin row —
/// car 2's table, the spelling 03-jobs.sql and six `*-plugin.sql`
/// files used.
const NEW_PLUGIN: &str = "20261001000002-a-plugin-row-the-old-way.sql";
const NEW_PLUGIN_SQL: &str = "\
-- 20261001000002 — a step plugin declared where it no longer lives.
INSERT INTO step_plugins (
    kind, version, status, label, description, category,
    metadata_schema, frontend_url, owning_team
) VALUES (
    'new-plugin', 1, 'active', 'New', 'x', 'platform', '{}', 'new-plugin.js', 'platform'
) ON CONFLICT (kind, version) DO NOTHING;
";

/// A migration newer than the cutover that re-versions a cadence rule
/// — car 3's table, in the retire-by-name supersede spelling
/// 202609032030 and 202609042110 used (the INSERT's rows from a
/// SELECT, so the table name is followed by a newline and a column
/// list rather than VALUES).
const NEW_CADENCE: &str = "20261001000003-a-cadence-rule-the-old-way.sql";
const NEW_CADENCE_SQL: &str = "\
-- 20261001000003 — a boarding threshold declared where it no longer lives.
UPDATE cadence_rules
   SET status = 'retired'
 WHERE name = 'train-board-on-dock-depth'
   AND status = 'active';
INSERT INTO cadence_rules
    (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes)
SELECT 'train-board-on-dock-depth',
       COALESCE(MAX(version), 0) + 1,
       'active', 'board', 'queue-depth', NULL, NULL, 2, 45
  FROM cadence_rules
 WHERE name = 'train-board-on-dock-depth';
";

/// A migration newer than the cutover that re-versions the delivery
/// policy — car 4's table, in the copy-the-active-row spelling
/// 202609050500 used (a retiring UPDATE, then an INSERT whose column
/// list opens on the line after the table name).
const NEW_POLICY: &str = "20261001000004-a-delivery-policy-the-old-way.sql";
const NEW_POLICY_SQL: &str = "\
-- 20261001000004 — a policy version declared where it no longer lives.
UPDATE delivery_policy
   SET status = 'retired'
 WHERE name = 'train-conductor' AND status = 'active';
INSERT INTO delivery_policy (
    name, version, status, max_red_trains, stall_hours
)
SELECT name, version + 1, 'active', max_red_trains, 9
  FROM delivery_policy
 WHERE name = 'train-conductor'
 ORDER BY version DESC
 LIMIT 1;
";

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

/// BEHAVIOUR 1 — history is history and a new migration that declares
/// schema is clean: exit 0, and the lint says how many files it checked.
#[test]
fn history_and_schema_only_migrations_are_clean() {
    let tree = Tree::new("clean");
    tree.migration(HISTORY, HISTORY_SQL)
        .migration(NEW_SCHEMA, NEW_SCHEMA_SQL);
    let out = tree.run();
    assert!(
        out.status.success(),
        "a clean tree must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("declares schema only"),
        "a clean tree must be certified in words:\n{msg}"
    );
    assert!(
        msg.contains("scanned 2 migration(s)"),
        "the lint must say how many migrations it checked (lib/scanned.sh):\n{msg}"
    );
}

/// BEHAVIOUR 2 — a migration newer than the cutover that inserts a
/// registry row is refused by file, line and table, and the verdict
/// names the door.
#[test]
fn a_post_cutover_insert_is_refused_by_file_line_and_table() {
    let tree = Tree::new("violating");
    tree.migration(HISTORY, HISTORY_SQL)
        .migration(NEW_SCHEMA, NEW_SCHEMA_SQL)
        .migration(NEW_INSERT, NEW_INSERT_SQL);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a violating tree must exit 1 — a fact about the BRANCH:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        &format!("{SCHEMA}/{NEW_INSERT}:3"),
        "inserts into stations",
        // the door
        "infra/platform/stations/",
        "boss-platform-workflow-seed",
        "version bump",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?} — a verdict someone must \
             re-derive is not a verdict (CLAUDE.md §Diagnosis):\n{msg}"
        );
    }
    assert!(
        !msg.contains(&format!("{SCHEMA}/{HISTORY}:")),
        "a pre-cutover insert is history and must not be named:\n{msg}"
    );
    assert!(
        !msg.contains(&format!("{SCHEMA}/{NEW_SCHEMA}:")),
        "prose and an UPDATE in a new migration must not be named:\n{msg}"
    );
}

/// BEHAVIOUR 2, car 2 — a post-cutover `INSERT INTO step_plugins` is
/// refused the same way, and the verdict names the step-plugin bundle
/// as the door.
#[test]
fn a_post_cutover_step_plugin_insert_is_refused_naming_its_bundle() {
    let tree = Tree::new("violating-plugin");
    tree.migration(HISTORY, HISTORY_SQL)
        .migration(NEW_PLUGIN, NEW_PLUGIN_SQL);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a violating tree must exit 1:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        &format!("{SCHEMA}/{NEW_PLUGIN}:2"),
        "inserts into step_plugins",
        "infra/platform/step-plugins/",
        "infra/step-plugins/",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?}:\n{msg}"
        );
    }
}

/// BEHAVIOUR 2, car 3 — a post-cutover `INSERT INTO cadence_rules` in
/// the retire-by-name spelling is refused, the verdict names the
/// cadence bundle as the door, and the retiring UPDATE before it is
/// not what is named (an UPDATE is still a migration's business).
#[test]
fn a_post_cutover_cadence_insert_is_refused_naming_its_bundle() {
    let tree = Tree::new("violating-cadence");
    tree.migration(HISTORY, HISTORY_SQL)
        .migration(NEW_CADENCE, NEW_CADENCE_SQL);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a violating tree must exit 1:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        &format!("{SCHEMA}/{NEW_CADENCE}:6"),
        "inserts into cadence_rules",
        "infra/platform/cadence/",
        "live-editable",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?}:\n{msg}"
        );
    }
    assert!(
        !msg.contains(&format!("{SCHEMA}/{NEW_CADENCE}:2")),
        "the retiring UPDATE is not a finding:\n{msg}"
    );
}

/// BEHAVIOUR 2, car 4 — a post-cutover `INSERT INTO delivery_policy`
/// in the copy-the-active-row spelling is refused, the verdict names
/// the delivery-policy bundle as the door and says why a bump is safe
/// for a train in flight, and the retiring UPDATE is not what is named.
#[test]
fn a_post_cutover_delivery_policy_insert_is_refused_naming_its_bundle() {
    let tree = Tree::new("violating-policy");
    tree.migration(HISTORY, HISTORY_SQL)
        .migration(NEW_POLICY, NEW_POLICY_SQL);
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a violating tree must exit 1:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        &format!("{SCHEMA}/{NEW_POLICY}:5"),
        "inserts into delivery_policy",
        "infra/platform/delivery-policy/",
        "pins the version",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?}:\n{msg}"
        );
    }
    assert!(
        !msg.contains(&format!("{SCHEMA}/{NEW_POLICY}:2")),
        "the retiring UPDATE is not a finding:\n{msg}"
    );
}

/// A schema directory with nothing in it is red, not clean: a lint that
/// scanned nothing certifies nothing (lib/scanned.sh, backlog cdf2d959).
#[test]
fn an_empty_schema_directory_is_not_certified() {
    let tree = Tree::new("empty");
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a lint that read no migration must exit 1:\n{}",
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("scanned 0") && !msg.contains("declares schema only"),
        "a zero scan must be refused, never certified:\n{msg}"
    );
}

/// BEHAVIOUR 3 — a tree the lint cannot read is refused (exit 3), never
/// certified. House style since `infra/lint/lib/git-answer.sh`.
#[test]
fn a_lint_that_cannot_read_the_tree_refuses() {
    let tree = Tree::new("unreadable");
    tree.migration(HISTORY, HISTORY_SQL);
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
        !msg.contains("declares schema only"),
        "a lint that read nothing must not certify the tree:\n{msg}"
    );
}

/// The cutover is one fourteen-digit stamp, written once, with the
/// registry list beside it — the two facts cars 2–4 of 393d3234 edit.
#[test]
fn the_cutover_is_one_fourteen_digit_stamp() {
    let body = std::fs::read_to_string(lint()).expect("read the lint");
    let stamps: Vec<&str> = body
        .lines()
        .filter_map(|l| l.strip_prefix("CUTOVER=\""))
        .filter_map(|rest| rest.strip_suffix('"'))
        .collect();
    assert_eq!(
        stamps.len(),
        1,
        "the cutover is declared exactly once: {stamps:?}"
    );
    assert!(
        stamps[0].len() == 14 && stamps[0].bytes().all(|b| b.is_ascii_digit()),
        "the cutover is a fourteen-digit `date -u +%Y%m%d%H%M%S` stamp: {}",
        stamps[0]
    );
    let tables = body
        .lines()
        .find_map(|l| l.strip_prefix("REGISTRY_TABLES=\""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("the registry list is declared once, on one line");
    let tables: Vec<&str> = tables.split_whitespace().collect();
    assert_eq!(
        tables,
        [
            "stations",
            "step_plugins",
            "cadence_rules",
            "delivery_policy"
        ],
        "the registry list names the four registries 393d3234 moved — stations \
         (car 1), step_plugins (car 2), cadence_rules (car 3), delivery_policy \
         (car 4) — and a fifth arrives with its own bundle and pin: {tables:?}"
    );
}
