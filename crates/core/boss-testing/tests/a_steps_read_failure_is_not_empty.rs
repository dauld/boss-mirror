//! `infra/lint/a-steps-read-failure-is-not-empty.sh` is RUN, not read —
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (backlog f6c97006, after c11e9d3c). A jobs handler that
//! answers a failed steps read with `list_steps(..).unwrap_or_default()`
//! says "this packet has no steps" about a packet it could not read, and
//! whatever it counts or lists shrinks under a 200: measured at nine
//! sites under `crates/core/boss-jobs/src/http/` on 2026-09-24, from the
//! station queue dropping a packet to the yard calling an unread
//! converge "converging". Each was repaired by handling the `Err` arm;
//! this lint keeps the shape out, with no allowlist.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "the scanner still matches" (mawk silently matches nothing for an
//! interval or a `\s`), and runs on every invocation; this file owns the
//! VERDICT on a tree — the repaired shapes exit 0 and say how much they
//! read, and the defect exits 1 naming each site and the way out.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-steps-read-failure-is-not-empty.sh";
const HTTP: &str = "crates/core/boss-jobs/src/http";

/// A synthetic repository: the lint at the path its own
/// `cd "$(dirname "$0")/../.."` resolves from, its libs, and the http/
/// directory it judges.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("steps-read-lint-{tag}"));
        scratch::create_dir(&root.join("infra/lint"));
        scratch::create_dir(&root.join(HTTP));
        let body = std::fs::read_to_string(repo_root().join(LINT)).expect("read the lint");
        scratch::write_exec(&root.join(LINT), &body);
        boss_testing::copy_lint_libs(&root);
        Tree(root)
    }

    fn handler(&self, name: &str, body: &str) -> &Tree {
        scratch::write_file(&self.0.join(HTTP).join(name), body);
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

/// The repairs the handlers use today pass: a `match` on the Result that
/// returns `steps_unreadable`, and the yard's `.ok()?` whose `None` reads
/// as unread — and prose naming the defect in a doc comment, as
/// `http/mod.rs` does, is not a finding.
#[test]
fn the_repaired_shapes_are_clean() {
    let tree = Tree::new("clean");
    tree.handler(
        "jobs.rs",
        "\
/// It used to answer `list_steps(..).unwrap_or_default()`.
async fn list(state: &State) -> Response {
    for job in &jobs {
        let steps = match state
            .jobs
            .list_steps(&job.id)
            .await
        {
            Ok(steps) => steps,
            Err(e) => return steps_unreadable(&job.id, &e),
        };
        let mut j = serde_json::to_value(job).unwrap_or_default();
    }
}
",
    )
    .handler(
        "yard.rs",
        "\
async fn converge_window(state: &State) -> Option<Vec<Step>> {
    let steps = state.jobs.list_steps(&job.id).await.ok()?;
    Some(steps)
}
",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "the repaired shapes must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    assert!(
        text(&out).contains("scanned 2 Rust file(s)"),
        "the lint must say how much it read:\n{}",
        text(&out)
    );
}

/// The defect, in rustfmt's wrapped form and on one line, in two files:
/// each site is named by file:line of the call, and the refusal names the
/// way out, so nobody has to re-derive it.
#[test]
fn a_defaulted_steps_read_is_named_by_its_site() {
    let tree = Tree::new("defect");
    tree.handler(
        "stations.rs",
        "\
async fn queue(state: &State) {
    for job in rows {
        let steps = state
            .jobs
            .list_steps(&job.id)
            .await
            .unwrap_or_default();
    }
}
",
    )
    .handler(
        "steps.rs",
        "\
async fn claim(state: &State) {
    let steps = state.jobs.list_steps(&job_id).await.unwrap_or_else(|_| Vec::new());
}
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a defaulted steps read must exit 1:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        "crates/core/boss-jobs/src/http/stations.rs:5",
        "crates/core/boss-jobs/src/http/steps.rs:2",
        "steps_unreadable",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?}:\n{msg}"
        );
    }
}
