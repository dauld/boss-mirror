//! `infra/lint/a-map-edge-is-served-not-drawn.sh` is RUN, not read —
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (design e765b3fc, car R3 on feedback 84cba7e2, 2026-09-26).
//! Every edge on both /it maps came from one hand-written list in three
//! copies — `boss_jobs::borders::BORDERS`, `world.ts` BORDERS and
//! `transit.ts` PATHS — pinned equal to each other and to nothing else.
//! Measured against the record, five of ten drawn edges matched a source,
//! two carried no packet, and ten real routes were drawn nowhere — among
//! them the train's own line from the dock through the gates to the
//! track, which David named. Car R3 deleted the two web copies and draws
//! only what `GET /api/yard/routes` serves; the lint keeps a hand-drawn
//! edge from coming back, and this file keeps the lint honest.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test owns
//! "the SCANNER still matches" (mawk answers a regex interval by matching
//! nothing, and that is invisible from outside); this file owns "the
//! lint's VERDICT on a tree" — clean exits 0, a pair exits 1 naming file,
//! line and the code, tests and comments are exempt, and a tree it cannot
//! read exits 3 rather than certifying it.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-map-edge-is-served-not-drawn.sh";

/// A synthetic repository the lint can be run against: a real git index
/// (the lint asks `git ls-files`, so an untracked file must not count),
/// the lint at the path its own `cd "$(dirname $0)/../.."` resolves
/// from, and the lint libraries it sources.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("a-map-edge-is-served-not-drawn-{tag}"));
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

    /// The map as car R3 left it: sections laid out from the served
    /// routes, keys built from data, prose that names routes.
    fn served_map(&self) -> &Tree {
        self.file(
            "apps/web/src/it/yard/transit.ts",
            "\
// The train runs from the dock back to the gates, then over to the
// track: from: 'gates', to: 'track' is the protocol's answer, not ours.
export const sectionKey = (from: string | null, to: string | null): string => `${from ?? ''}→${to ?? ''}`;
export const sectionsOf = (routes: Routes) => routes.routes.map((r) => ({ key: sectionKey(r.from, r.to), from: r.from, to: r.to }));
",
        )
        .file(
            "apps/web/src/it/yard/TransitMap.svelte",
            "\
<!-- once PATHS drew { from: 'dock', to: 'track' } by hand -->
{#each sections as s (s.key)}<path data-section={s.key} />{/each}
",
        )
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

/// A map that draws what it is served is clean — and a test's fixture,
/// which is the server's answer standing in, is not a drawing.
#[test]
fn a_map_that_draws_what_it_is_served_is_clean() {
    let tree = Tree::new("clean");
    tree.served_map();
    tree.file(
        "apps/web/tests/fixtures/yard.ts",
        "export const BORDERS = [\n  { from: 'receiving', to: 'marshalling' },\n];\n",
    );
    tree.file(
        "apps/web/src/it/yard/transit.test.ts",
        "expect(find('dock→').kind).toBe('exit');\nconst r = { from: 'gates', to: 'track' };\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "a map that draws what it is served must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("every edge on the map is one the server serves"),
        "a clean tree must be certified in words:\n{msg}"
    );
    assert!(
        msg.contains("a-map-edge-is-served-not-drawn: scanned 2"),
        "a scanning lint names how much it read, and skips the tests (lib/scanned.sh):\n{msg}"
    );
}

/// A hand-drawn edge is refused by file, line and the code — each shape
/// the deleted copies were written in, the exit and entry forms, and an
/// arrow key — and the verdict names the door: the routes read and the
/// three sources it derives from.
#[test]
fn a_hand_drawn_edge_is_refused_by_file_line_and_code() {
    let tree = Tree::new("drawn");
    tree.served_map();
    tree.file(
        "apps/web/src/it/yard/world.ts",
        "\
export const LINE = ['receiving', 'marshalling'];
export const BORDERS = [
  { from: 'arrivals', to: 'publish' },
  { from: \"gates\", to: \"garage\" },
];
",
    );
    tree.file(
        "apps/web/src/it/yard/ramps.ts",
        "export const EXITS = [{ from: 'dock', to: null }];\nexport const HELD = 'dock→track';\n",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a hand-drawn edge must exit 1 — a fact about the BRANCH:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        "apps/web/src/it/yard/world.ts:3",
        "from: 'arrivals', to: 'publish'",
        "apps/web/src/it/yard/world.ts:4",
        "apps/web/src/it/yard/ramps.ts:1",
        "apps/web/src/it/yard/ramps.ts:2",
        "'dock→track'",
        // the door
        "GET /api/yard/routes",
        "infra/platform/yard/handoffs.toml",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?} — a verdict someone must \
             re-derive is not a verdict (CLAUDE.md §Diagnosis):\n{msg}"
        );
    }
    assert!(
        !msg.contains("world.ts:1") && !msg.contains("transit.ts"),
        "a list of stations is a layout, not an edge, and the served map is clean:\n{msg}"
    );
}

/// A tree the lint cannot read is refused (exit 3), never certified.
/// House style since `infra/lint/lib/git-answer.sh`.
#[test]
fn a_lint_that_cannot_read_the_tree_refuses() {
    let tree = Tree::new("unreadable");
    tree.served_map();
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
        !msg.contains("every edge on the map is one the server serves"),
        "a lint that read nothing must not certify the tree:\n{msg}"
    );
}
