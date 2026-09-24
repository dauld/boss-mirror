//! `infra/lint/a-colour-is-a-token.sh` is RUN, not read — against
//! synthetic trees, so every property below is one the lint actually has.
//!
//! THE CLASS (backlog 7eb59678, 2026-09-24). The Enamel token car
//! (e4b58c50, train #605) turned the theme LIGHT at the token layer —
//! `--ink` became `#FFFFFF` — and David opened pages whose text he could
//! not read. The tokens were right; the components were not using them.
//! 1,779 colour literals sat in 123 files under `apps/web/src` and
//! `libs/web-kit/src`: 750 bare, 802 as `var(--token, <dark hex>)`
//! fallbacks, 97 as fallbacks to a token NOTHING defined (`--chalk`,
//! `--text-muted`, `--danger`, …) so the dark-theme literal is what
//! rendered, and 130 in script. 122 text colours measured below 4.5:1 on
//! the surface they land on; 24 below 1.5:1 — near-white on white. The
//! car that added this lint moved every one onto the tokens in
//! `apps/web/src/styles.css`; the lint is what keeps a new component from
//! bringing a literal back, and this file is what keeps the lint honest.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test owns
//! "the SCANNER still matches" (mawk answers a regex interval by matching
//! nothing, and that is invisible from outside); this file owns "the
//! lint's VERDICT on a tree" — clean exits 0, a violation exits 1 naming
//! file, line and literal, and a tree it cannot read exits 3 rather than
//! certifying it.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-colour-is-a-token.sh";

fn lint() -> PathBuf {
    repo_root().join(LINT)
}

/// A synthetic repository the lint can be run against: a real git index
/// (the lint asks `git ls-files`, so an untracked fixture must not
/// count), the lint at the path its own `cd "$(dirname $0)/../.."`
/// resolves from, and the lint libraries it sources.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("a-colour-is-a-token-{tag}"));
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

    /// The token file every clean tree carries: its `:root` blocks are
    /// where a colour literal BELONGS.
    fn tokens(&self) -> &Tree {
        self.file(
            "apps/web/src/styles.css",
            "\
/* The dark system was #0D1014; a comment may name what it replaced. */
:root {
  --ink: #FFFFFF;
  --fog: #0E1B2E;
  --wash: rgba(14, 27, 46, 0.04);
}
:root {
  --band: var(--fog);
  --on-band: #FFFFFF;
}
body { background: var(--ink); color: var(--fog); }
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

/// BEHAVIOUR 1 — a tree whose components read tokens is clean: the token
/// file's `:root` blocks, comments that name an old colour, Svelte's
/// `{#each}` / `{#if}` (a `#` followed by letters is not a colour), an
/// issue number in a comment, a declared data paragraph, and test files.
#[test]
fn a_tree_that_reads_its_tokens_is_clean() {
    let tree = Tree::new("clean");
    tree.tokens();
    tree.file(
        "apps/web/src/jobs/Card.svelte",
        "\
<script lang=\"ts\">
  // Pre-#101 this card was painted by hand.
  let rows = $state([]);
</script>
{#each rows as r (r)}
  {#if r}<span style=\"color: var(--fog)\">{r}</span>{/if}
{/each}
<!-- the old chip was #1c1917 on #e7e5e4 -->
<style>
  /* replaced #7a838c */
  .card { background: var(--ink); color: var(--fog); border: 1px solid transparent; }
  .card:hover { background: currentColor; }
</style>
",
    );
    tree.file(
        "libs/web-kit/src/ui/palette.ts",
        "\
// colour-literal-ok: categorical identity hues, data not styling
export const HUES = [
  '#7FB4D8',
  '#C9A96B',
];

export const n = 2;
",
    );
    tree.file(
        "apps/web/src/enamel-tokens.test.ts",
        "expect(tokens['--fog']).toBe('#0E1B2E');\n",
    );
    tree.file(
        "apps/web/tests/mocked/chrome.mocked.spec.ts",
        "expect(bg).toBe('rgb(255, 255, 255)');\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "a tree that reads its tokens must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    let msg = text(&out);
    assert!(
        msg.contains("every colour is a token"),
        "a clean tree must be certified in words:\n{msg}"
    );
    assert!(
        msg.contains("a-colour-is-a-token: scanned"),
        "a scanning lint names how much it read (lib/scanned.sh):\n{msg}"
    );
}

/// BEHAVIOUR 2 — each shape this car removed is refused by file, line and
/// literal: a bare hex in a component's style block, a `var()` fallback
/// (dead or live, it is still a second palette), `rgba()` in a script
/// string, a named colour, and a literal in the token file OUTSIDE its
/// `:root` blocks. The verdict names the door: the tokens file.
#[test]
fn every_removed_shape_is_refused_by_file_line_and_literal() {
    let tree = Tree::new("violating");
    tree.tokens();
    tree.file(
        "apps/web/src/it/Page.svelte",
        "\
<style>
  .a { color: #d6d3d1; }
  .b { color: var(--chalk, #f4f7fa); }
</style>
",
    );
    tree.file(
        "apps/web/src/it/perf.ts",
        "export const ramp = (ms: number) => (ms < 10 ? 'rgba(34, 197, 94, 0.15)' : '');\n",
    );
    tree.file(
        "libs/web-kit/src/Chrome.svelte",
        "<div style=\"background: var(--ink); color: white\"></div>\n",
    );
    tree.file(
        "apps/web/src/styles.css",
        "\
:root {
  --fog: #0E1B2E;
}
.shell { background: #1c1917; }
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
        "apps/web/src/it/Page.svelte:2",
        "#d6d3d1",
        "apps/web/src/it/Page.svelte:3",
        "#f4f7fa",
        "apps/web/src/it/perf.ts:1",
        "rgba(34, 197, 94, 0.15)",
        "libs/web-kit/src/Chrome.svelte:1",
        "white",
        "apps/web/src/styles.css:4",
        "#1c1917",
        // the door
        "apps/web/src/styles.css",
        ":root",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?} — a verdict someone must \
             re-derive is not a verdict (CLAUDE.md §Diagnosis):\n{msg}"
        );
    }
    assert!(
        !msg.contains("styles.css:2"),
        "a token defined in :root must not be named:\n{msg}"
    );
}

/// The simulator is a web app too, and it renders the same web-kit parts
/// (backlog 6f471ff6, car 4). Until 2026-09-24 the lint read only
/// `apps/web` and `libs/web-kit`, so `apps/simulator` kept its own stone
/// and brew palette — 683 literals in its stylesheet and 91 in its
/// components — under a Google-CDN Inter and Fraunces, which nothing
/// refused. A literal there is refused like one anywhere else, and its
/// tokens come from the same one file.
#[test]
fn the_simulator_is_read_like_the_web_app() {
    let tree = Tree::new("simulator");
    tree.tokens();
    tree.file(
        "apps/simulator/src/shell/SimShell.svelte",
        "\
<style>
  .sim-sidebar { background: #1c1917; color: var(--fog); }
</style>
",
    );
    tree.file(
        "apps/simulator/src/styles.css",
        "\
:root {
  --brew-amber: #d99b3a;
}
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a literal in the simulator must be refused:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        "apps/simulator/src/shell/SimShell.svelte:2",
        "#1c1917",
        // Only the one token file's :root is where a colour belongs; a
        // second :root in the simulator is the second palette itself.
        "apps/simulator/src/styles.css:2",
        "#d99b3a",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?}:\n{msg}"
        );
    }
}

/// A declaration is a paragraph, not a file: the marker exempts the lines
/// up to the next blank line, and a marker without a reason exempts nothing.
#[test]
fn a_marker_reaches_its_paragraph_and_needs_a_reason() {
    let tree = Tree::new("markers");
    tree.tokens();
    tree.file(
        "apps/web/src/jobs/atlas.ts",
        "\
// colour-literal-ok: department palette is data, not styling
const DEPT = { it: '#0F6E9F' };

const LATER = '#3b82f6';
// colour-literal-ok
const BARE = '#dc2626';
",
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let msg = text(&out);
    assert!(
        msg.contains("atlas.ts:4") && msg.contains("atlas.ts:6"),
        "the literal past the paragraph and the one under a reasonless \
         marker must both be named:\n{msg}"
    );
    assert!(
        !msg.contains("atlas.ts:2"),
        "the declared paragraph must not be named:\n{msg}"
    );
}

/// BEHAVIOUR 3 — a tree the lint cannot read is refused (exit 3), never
/// certified. House style since `infra/lint/lib/git-answer.sh`.
#[test]
fn a_lint_that_cannot_read_the_tree_refuses() {
    let tree = Tree::new("unreadable");
    tree.tokens();
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
        !msg.contains("every colour is a token"),
        "a lint that read nothing must not certify the tree:\n{msg}"
    );
}
