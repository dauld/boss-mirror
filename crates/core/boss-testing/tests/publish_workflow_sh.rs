//! `infra/gcp/publish-workflow.sh` is RUN, not read — against a stub
//! `boss` binary, a stub `curl` standing in for the system of record,
//! and a scratch git checkout carrying a bundle with history — so every
//! verdict below is one the script actually reached.
//!
//! WHY THE VERB EXISTS (backlog 3ce95b85, car 3a of 8f4e9cc0). A change
//! to an EXISTING platform workflow kind goes live only when an operator
//! runs `boss workflow publish <kind> <toml>` on a host with the
//! checkout — the seed is insert-if-missing by decision. The drift
//! measurement (df43e97b) named maintenance-sweep adrift for three days
//! for exactly that reason, and the Drift tab (#378) shows the drift and
//! offers nothing. Measured while building this, 2026-09-15: 16
//! `maintenance-*` kinds carry a `failed` step in the tree that the live
//! row lacks — landed, never published — while `ship-a-change`'s live
//! v31 carries `procedure` texts and a required `proof` field the tree
//! never said, so publishing the tree over it would REGRESS the
//! protocol. The verb has to tell those two apart.
//!
//! THE RULE IT APPLIES: the live row is safe to overwrite iff it equals
//! a row the tree once said — some revision of the bundle under
//! `infra/platform/` renders to the same label, description, category,
//! subject_kinds and steps. Then the tree moved ahead and live lags:
//! publish. Otherwise the live row carries an edit the tree lacks:
//! REFUSE, name the drift, and publish only when the request says
//! `--force-tree` (the Drift tab's approve is what says it).
//!
//! What each case pins: the four refusals (kind file absent; the tree's
//! row does not lint; live already equals the tree; live carries what
//! the tree never said), the happy path (live is an older tree revision
//! → the publish runs, signed as the runner's automation, and is
//! confirmed by reading the row back), `--check` reaching the verdict
//! without publishing, `--force-tree` publishing over drift, an
//! unconfirmed publish exiting non-zero, and the allowlist's own
//! validation exercised THROUGH `ops-runner.sh` with the real verb
//! files, as the runner on boss-gcp would.
//!
//! Nothing here touches a host or the registry. `boss` and `curl` are
//! stubs on every path; the publish a stub receives is appended to a
//! file.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// `tomllib` arrived in Python 3.11; the comparator needs it, and the
/// gate image has it. Skip honestly elsewhere.
fn has_tomllib() -> bool {
    Command::new("python3")
        .args(["-c", "import tomllib"])
        .status()
        .is_ok_and(|s| s.success())
}

const SCRIPT: &str = "infra/gcp/publish-workflow.sh";
const KIND: &str = "maintenance-widget";

/// The bundle's FIRST revision: one `workflows.toml` carrying the kind
/// with three steps and the older description — the shape the bundle
/// had before 2026-09-08, and the row the live registry still holds
/// for sixteen kinds.
const REV1_BUNDLE: &str = r#"
[[workflow]]
kind = "maintenance-widget"
label = "Widget sweep"
category = "platform"
subject_kinds = ["custom"]
owning_team = "platform"
description = "A recurring inspection of widgets, admitted on a cadence rather than remembered."

[[workflow.step]]
title = "scheduled"
kind = "trigger"
ready_when = "true"
title_template = "Timer fired"

[[workflow.step]]
title = "run"
kind = "task"
ready_when = "steps.scheduled.done"
title_template = "Sweep the widgets"
authority_role = "platform-admin"
fields = [{ name = "result", field_type = "string", required = true }]

[[workflow.step]]
title = "completed"
kind = "outcome"
ready_when = "steps.run.done"
title_template = "Widgets swept"
metadata_defaults = { outcome_kind = "completed" }
"#;

/// The bundle's SECOND revision: the kind split into its own file, a
/// longer description, and a fourth `failed` step — the tree as it
/// stands, ahead of the live row.
const REV2_KIND_FILE: &str = r#"
[[workflow]]
kind = "maintenance-widget"
label = "Widget sweep"
category = "platform"
subject_kinds = ["custom"]
owning_team = "platform"
description = "A recurring inspection of widgets, admitted on a cadence rather than remembered. A run that died closes this packet failed instead of leaving it open."

[[workflow.step]]
title = "scheduled"
kind = "trigger"
ready_when = "true"
title_template = "Timer fired"

[[workflow.step]]
title = "run"
kind = "task"
ready_when = "steps.scheduled.done"
title_template = "Sweep the widgets"
authority_role = "platform-admin"
fields = [{ name = "result", field_type = "string", required = true }]

[[workflow.step]]
title = "completed"
kind = "outcome"
ready_when = "steps.run.done AND steps.run.metadata.result = \"ok\""
title_template = "Widgets swept"
metadata_defaults = { outcome_kind = "completed" }

[[workflow.step]]
title = "failed"
kind = "outcome"
ready_when = "steps.run.done AND steps.run.metadata.result != \"ok\""
title_template = "Widgets not swept"
metadata_defaults = { outcome_kind = "aborted" }
"#;

/// A live row as `GET /api/workflows/<kind>` answers it: the active
/// version, steps carrying the registry's own null/empty defaults.
fn live_row(version: u32, description: &str, steps: &str) -> String {
    format!(
        r#"{{"kind":"{KIND}","version":{version},"status":"active","label":"Widget sweep","category":"platform","subject_kinds":["custom"],"owning_team":"platform","description":"{description}","metadata":{{}},"metadata_schema":{{}},"entitlements":{{}},"authoring_job_id":null,"created_at":"2026-08-16T00:00:00Z","steps":[{steps}]}}"#
    )
}

const REV1_DESC: &str =
    "A recurring inspection of widgets, admitted on a cadence rather than remembered.";
const REV2_DESC: &str = "A recurring inspection of widgets, admitted on a cadence rather than remembered. A run that died closes this packet failed instead of leaving it open.";

const REV1_STEPS: &str = r#"{"title":"scheduled","kind":"trigger","ready_when":"true","title_template":"Timer fired","sign_offs_required":[],"fields":[],"authority_role":null,"metadata_defaults":null},{"title":"run","kind":"task","ready_when":"steps.scheduled.done","title_template":"Sweep the widgets","sign_offs_required":[],"fields":[{"name":"result","field_type":"string","required":true}],"authority_role":"platform-admin","metadata_defaults":null},{"title":"completed","kind":"outcome","ready_when":"steps.run.done","title_template":"Widgets swept","sign_offs_required":[],"fields":[],"authority_role":null,"metadata_defaults":{"outcome_kind":"completed"}}"#;

const REV2_STEPS: &str = r#"{"title":"scheduled","kind":"trigger","ready_when":"true","title_template":"Timer fired","sign_offs_required":[],"fields":[],"authority_role":null,"metadata_defaults":null},{"title":"run","kind":"task","ready_when":"steps.scheduled.done","title_template":"Sweep the widgets","sign_offs_required":[],"fields":[{"name":"result","field_type":"string","required":true}],"authority_role":"platform-admin","metadata_defaults":null},{"title":"completed","kind":"outcome","ready_when":"steps.run.done AND steps.run.metadata.result = \"ok\"","title_template":"Widgets swept","sign_offs_required":[],"fields":[],"authority_role":null,"metadata_defaults":{"outcome_kind":"completed"}},{"title":"failed","kind":"outcome","ready_when":"steps.run.done AND steps.run.metadata.result != \"ok\"","title_template":"Widgets not swept","sign_offs_required":[],"fields":[],"authority_role":null,"metadata_defaults":{"outcome_kind":"aborted"}}"#;

/// An operator's live edit the tree never said: rev1's steps with a
/// `procedure` on the run step and a `proof` field — the ship-a-change
/// shape measured on 2026-09-15.
const OPERATOR_STEPS: &str = r#"{"title":"scheduled","kind":"trigger","ready_when":"true","title_template":"Timer fired","sign_offs_required":[],"fields":[],"authority_role":null,"metadata_defaults":null},{"title":"run","kind":"task","ready_when":"steps.scheduled.done","title_template":"Sweep the widgets","sign_offs_required":[],"fields":[{"name":"result","field_type":"string","required":true},{"name":"proof","field_type":"string","required":true}],"authority_role":"platform-admin","metadata_defaults":{"procedure":"Observe the sweep working in production."}},{"title":"completed","kind":"outcome","ready_when":"steps.run.done","title_template":"Widgets swept","sign_offs_required":[],"fields":[],"authority_role":null,"metadata_defaults":{"outcome_kind":"completed"}}"#;

/// One fixture: a scratch checkout whose history is rev1 (one bundle
/// file) then rev2 (the per-kind file), a stub `boss` that records its
/// argv and flips the live row on a real publish, a stub `curl`
/// answering the registry read from a file, and the live row itself.
struct Case {
    root: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    live: PathBuf,
    live_after: PathBuf,
    boss_log: PathBuf,
    /// The fixture's two commits: rev1 (the one-file bundle) and rev2
    /// (HEAD). The stub `boss` reports itself built from rev2 and the
    /// script's CLI floor is rev1 unless a case says otherwise, so the
    /// version check passes by default and a case can turn it around.
    rev1: String,
    rev2: String,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("publish-workflow-{name}"));
        let bin = root.join("bin");
        let repo = root.join("repo");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(repo.join("infra/platform")).unwrap();
        let live = root.join("live.json");
        let live_after = root.join("live-after.json");
        let boss_log = root.join("boss.log");

        // rev1: one bundle file. rev2: split into the per-kind file,
        // with the description and step changes the tree carries now.
        let git = |args: &[&str]| {
            let st = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "fixture")
                .env("GIT_AUTHOR_EMAIL", "f@example.invalid")
                .env("GIT_COMMITTER_NAME", "fixture")
                .env("GIT_COMMITTER_EMAIL", "f@example.invalid")
                .output()
                .expect("git runs");
            assert!(
                st.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&st.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        write_file(&repo.join("infra/platform/workflows.toml"), REV1_BUNDLE);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "rev1: the bundle as one file"]);
        std::fs::remove_file(repo.join("infra/platform/workflows.toml")).unwrap();
        std::fs::create_dir_all(repo.join("infra/platform/workflows")).unwrap();
        write_file(
            &repo.join(format!("infra/platform/workflows/{KIND}.toml")),
            REV2_KIND_FILE,
        );
        git(&["add", "-A"]);
        git(&[
            "commit",
            "-q",
            "-m",
            "rev2: one file per kind, a failed step",
        ]);
        let rev = |spec: &str| {
            let out = Command::new("git")
                .args(["rev-parse", spec])
                .current_dir(&repo)
                .output()
                .expect("git rev-parse runs");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let rev1 = rev("HEAD~1");
        let rev2 = rev("HEAD");

        write_exec(
            &bin.join("boss"),
            r#"#!/bin/sh
# stub boss: records argv and the actor it was handed. `--dry-run`
# lints (fails when STUB_LINT_FAIL is set); a real publish flips the
# live row to STUB_LIVE_AFTER unless STUB_PUBLISH_FAIL / STUB_NO_EFFECT.
# `--version` is a read of the binary, not an act: answered from
# STUB_BUILT_FROM and never logged, so the counts below stay the acts.
case "$*" in
  --version) echo "boss 0.1.0 built from ${STUB_BUILT_FROM:-unknown}"; exit 0 ;;
esac
echo "$* actor=${BOSS_ACTOR:-unset}" >> "$STUB_BOSS_LOG"
case "$*" in
  *--dry-run*)
    if [ -n "${STUB_LINT_FAIL:-}" ]; then
      echo "Error: the spec does not lint clean, so nothing was written:" >&2
      echo '  ["step run: ready_when references no step"]' >&2
      exit 1
    fi
    echo "boss workflow: $3 lints clean (4 steps)"; exit 0 ;;
  "workflow publish "*)
    if [ -n "${STUB_PUBLISH_FAIL:-}" ]; then
      echo "Error: jobs api PUT /api/workflows/$3: 500" >&2; exit 1
    fi
    [ -n "${STUB_NO_EFFECT:-}" ] || cp "$STUB_LIVE_AFTER" "$STUB_LIVE"
    echo "boss workflow: $3 v2 is live, with the 4 steps sent — confirmed by reading the active row back"; exit 0 ;;
  *) echo "stub boss: unexpected $*" >&2; exit 99 ;;
esac
"#,
        );
        write_exec(
            &bin.join("curl"),
            r#"#!/bin/sh
# stub curl: GET /api/workflows/<kind> serves the live row from a file
# (HTTP code via -w when asked); STUB_REGISTRY_DOWN answers nothing.
[ -n "${STUB_REGISTRY_DOWN:-}" ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
out=""; code=0
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    -w) code=1 ;;
  esac
  shift
done
if [ -n "$out" ]; then cp "$STUB_LIVE" "$out"; else cat "$STUB_LIVE"; fi
[ "$code" = 1 ] && printf '200'
exit 0
"#,
        );
        write_file(&live, &live_row(1, REV1_DESC, REV1_STEPS));
        write_file(&live_after, &live_row(2, REV2_DESC, REV2_STEPS));
        Self {
            root,
            bin,
            repo,
            live,
            live_after,
            boss_log,
            rev1,
            rev2,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            // The ops runner hands a verb root's environment with no
            // HOME; the script must not need one.
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_JOBS_URL", "http://sor.invalid")
            .env("OPS_REQUEST_ID", "aaaaaaaa-0000-4000-8000-000000000000")
            .env("BOSS_PUBLISH_WORKFLOW_REPO", &self.repo)
            .env("STUB_LIVE", &self.live)
            .env("STUB_LIVE_AFTER", &self.live_after)
            .env("STUB_BOSS_LOG", &self.boss_log)
            // The stub CLI is built from HEAD and the floor is the older
            // commit: a current binary. A case overrides either.
            .env("STUB_BUILT_FROM", &self.rev2)
            .env("BOSS_PUBLISH_CLI_FLOOR", &self.rev1);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("publish-workflow.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Every `boss` invocation the stub received, in order.
    fn boss_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.boss_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn publishes(&self) -> Vec<String> {
        self.boss_calls()
            .into_iter()
            .filter(|l| l.starts_with("workflow publish ") && !l.contains("--dry-run"))
            .collect()
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

fn ready() -> bool {
    for (ok, why) in [
        (has("git"), "git"),
        (has("jq"), "jq"),
        (
            has("python3") && has_tomllib(),
            "python3 with tomllib (3.11+)",
        ),
    ] {
        if !ok {
            eprintln!("skipping: publish-workflow.sh needs {why}, and this box has none");
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// The four refusals.
// ---------------------------------------------------------------------------

/// A kind with no file in the tree is refused by path, before anything
/// is read from the registry or `boss` is invoked. New kinds are the
/// seed's business; this verb republishes existing ones.
#[test]
fn a_kind_the_tree_does_not_author_is_refused_by_path() {
    if !ready() {
        return;
    }
    let c = Case::new("absent");
    let (rc, out) = c.run(&["maintenance-nothing"]);
    assert_eq!(rc, 3, "{out}");
    contains_all(
        &out,
        &[
            "REFUSED",
            "infra/platform/workflows/maintenance-nothing.toml",
            "seed",
        ],
        "the absent-file refusal",
    );
    assert!(c.boss_calls().is_empty(), "boss was invoked: {out}");
}

/// A row that does not lint is refused with the linter's own words,
/// whole, and nothing is published.
#[test]
fn a_tree_row_that_does_not_lint_is_refused_with_the_verdict() {
    if !ready() {
        return;
    }
    let c = Case::new("lint");
    let (rc, out) = c.run_env(&[KIND], &[("STUB_LINT_FAIL", "1".into())]);
    assert_eq!(rc, 4, "{out}");
    contains_all(
        &out,
        &[
            "REFUSED",
            "does not lint clean",
            "ready_when references no step",
        ],
        "the lint refusal carries the linter's output",
    );
    assert!(c.publishes().is_empty(), "a publish ran: {out}");
}

/// When the live row already says what the tree says there is nothing
/// to publish, and the verb says so instead of minting an identical
/// version.
#[test]
fn a_live_row_equal_to_the_tree_is_nothing_to_publish() {
    if !ready() {
        return;
    }
    let c = Case::new("equal");
    write_file(&c.live, &live_row(2, REV2_DESC, REV2_STEPS));
    let (rc, out) = c.run(&[KIND]);
    assert_eq!(rc, 5, "{out}");
    contains_all(
        &out,
        &["nothing to publish", "v2", "already"],
        "the nothing-to-publish refusal",
    );
    assert!(c.publishes().is_empty(), "a publish ran: {out}");
}

/// The load-bearing refusal: a live row carrying what no revision of
/// the tree ever said is an operator's edit, and publishing the tree
/// over it would regress the protocol. Refused, with the drift named
/// by field, and the way through named too.
#[test]
fn a_live_row_the_tree_never_said_is_refused_and_the_drift_named() {
    if !ready() {
        return;
    }
    let c = Case::new("operator-edit");
    write_file(&c.live, &live_row(7, REV1_DESC, OPERATOR_STEPS));
    let (rc, out) = c.run(&[KIND]);
    assert_eq!(rc, 6, "{out}");
    contains_all(
        &out,
        &[
            "REFUSED",
            "v7",
            "never said",
            "steps[1].fields",
            "steps[1].metadata_defaults",
            "--force-tree",
        ],
        "the operator-edit refusal",
    );
    assert!(c.publishes().is_empty(), "a publish ran: {out}");
}

// ---------------------------------------------------------------------------
// The happy path, and the modes.
// ---------------------------------------------------------------------------

/// Live is rev1 — a row the tree once said, now behind rev2. The tree
/// moved ahead, so the publish runs: lint first, then exactly one real
/// publish signed as the runner's automation, then the row read back
/// and confirmed equal to the tree, with the version movement printed.
#[test]
fn a_live_row_the_tree_once_said_is_republished_and_confirmed() {
    if !ready() {
        return;
    }
    let c = Case::new("happy");
    let (rc, out) = c.run(&[KIND]);
    assert_eq!(rc, 0, "{out}");
    contains_all(
        &out,
        &[
            "tree moved ahead",
            "infra/platform/workflows.toml",
            "v1 -> v2",
            "confirmed",
            "aaaaaaaa-0000-4000-8000-000000000000",
        ],
        "the publish report",
    );
    let calls = c.boss_calls();
    assert_eq!(
        calls.len(),
        2,
        "expected a dry-run then a publish, got {calls:?}"
    );
    assert!(
        calls[0].contains("--dry-run"),
        "the first boss call is the lint: {calls:?}"
    );
    let kind_file = c.repo.join(format!("infra/platform/workflows/{KIND}.toml"));
    assert_eq!(
        calls[1],
        format!(
            "workflow publish {KIND} {} actor=automation:ops-runner",
            kind_file.display()
        ),
        "the publish names the kind, the tree's file, and signs as the runner's automation"
    );
}

/// `--check` reaches the same verdict and stops short of writing: the
/// lint runs, no publish does, and the output says what would happen.
#[test]
fn check_reaches_the_verdict_without_publishing() {
    if !ready() {
        return;
    }
    let c = Case::new("check");
    let (rc, out) = c.run(&[KIND, "--check"]);
    assert_eq!(rc, 0, "{out}");
    contains_all(
        &out,
        &["--check", "would publish", "v1"],
        "the check report",
    );
    assert!(c.publishes().is_empty(), "--check published: {out}");
    assert_eq!(
        c.boss_calls().len(),
        1,
        "--check lints once: {:?}",
        c.boss_calls()
    );
    // And a check over an operator edit reports the refusal, non-zero.
    write_file(&c.live, &live_row(7, REV1_DESC, OPERATOR_STEPS));
    let (rc, out) = c.run(&[KIND, "--check"]);
    assert_eq!(rc, 6, "{out}");
    contains_all(&out, &["REFUSED", "never said"], "the check refusal");
}

/// `--force-tree` is the approve: it publishes over a live row the tree
/// never said, and says in the output that it did so over named drift.
#[test]
fn force_tree_publishes_over_an_operator_edit_and_says_so() {
    if !ready() {
        return;
    }
    let c = Case::new("force");
    write_file(&c.live, &live_row(7, REV1_DESC, OPERATOR_STEPS));
    write_file(&c.live_after, &live_row(8, REV2_DESC, REV2_STEPS));
    let (rc, out) = c.run(&[KIND, "--force-tree"]);
    assert_eq!(rc, 0, "{out}");
    contains_all(
        &out,
        &[
            "FORCED",
            "never said",
            "steps[1].fields",
            "v7 -> v8",
            "confirmed",
        ],
        "the forced publish report",
    );
    assert_eq!(c.publishes().len(), 1, "{:?}", c.boss_calls());
}

/// A publish that `boss` reports as done but that the registry does not
/// show is not a success: the read-back is the verdict.
#[test]
fn a_publish_the_registry_does_not_show_is_not_reported_as_one() {
    if !ready() {
        return;
    }
    let c = Case::new("unconfirmed");
    let (rc, out) = c.run_env(&[KIND], &[("STUB_NO_EFFECT", "1".into())]);
    assert_eq!(rc, 7, "{out}");
    contains_all(&out, &["NOT CONFIRMED", "v1"], "the unconfirmed report");
    let (rc, out) = c.run_env(&[KIND], &[("STUB_PUBLISH_FAIL", "1".into())]);
    assert_eq!(rc, 7, "{out}");
    contains_all(
        &out,
        &["boss workflow publish", "500"],
        "the failed publish carries boss's words",
    );
}

/// A registry that cannot be read is 75 — cannot answer — never a
/// verdict about the tree; and no system of record is a configuration
/// fault, refused before anything runs.
#[test]
fn an_unreadable_registry_cannot_answer_and_no_sor_is_a_config_fault() {
    if !ready() {
        return;
    }
    let c = Case::new("down");
    let (rc, out) = c.run_env(&[KIND], &[("STUB_REGISTRY_DOWN", "1".into())]);
    assert_eq!(rc, 75, "{out}");
    contains_all(
        &out,
        &["could not read", "/api/workflows/"],
        "the cannot-answer report",
    );
    assert!(
        c.boss_calls().is_empty(),
        "boss ran without a registry: {out}"
    );
    let (rc, out) = c.run_env(&[KIND], &[("BOSS_JOBS_URL", String::new())]);
    assert_eq!(rc, 78, "{out}");
    contains_all(&out, &["BOSS_JOBS_URL"], "the config refusal");
}

/// The kind is pattern-checked in the script too, and a second arg
/// outside the two modes is refused by name — the runner's check and
/// the script's agree on the shape.
#[test]
fn a_malformed_kind_or_a_foreign_mode_is_refused_before_anything_runs() {
    if !ready() {
        return;
    }
    let c = Case::new("args");
    for (args, needle) in [
        (vec!["Maintenance Widget"], "kind"),
        (vec![KIND, "--now"], "--check"),
        (vec![], "usage"),
    ] {
        let (rc, out) = c.run(&args);
        assert_eq!(rc, 2, "{args:?}: {out}");
        assert!(
            out.contains(needle),
            "{args:?}: expected `{needle}` in {out}"
        );
    }
    assert!(c.boss_calls().is_empty());
}

// ---------------------------------------------------------------------------
// The comparator applies the loader's audience projection.
// ---------------------------------------------------------------------------

/// A file that declares `audience = { role = "platform-admin" }` and no
/// bare `authority_role` renders — through the seed loader's projection
/// (boss-jobs/src/audience.rs `selectors_for`) — to a row carrying BOTH.
/// The comparator must read the file the same way, or a correct publish
/// reads as NOT CONFIRMED, which happened on 2026-09-16 02:10Z (backlog-
/// item v8, ops-request deb264ed): `steps[1].authority_role tree=<absent>
/// live=platform-admin`. With the projection the two compare EQUAL.
#[test]
fn a_declared_audience_compares_equal_to_its_projected_authority_role() {
    if !ready() {
        return;
    }
    let c = Case::new("audience");
    let file = c.repo.join(format!("infra/platform/workflows/{KIND}.toml"));
    let with_audience = REV2_KIND_FILE.replace(
        "authority_role = \"platform-admin\"",
        "audience = { role = \"platform-admin\" }",
    );
    assert_ne!(
        with_audience, REV2_KIND_FILE,
        "the fixture must declare an audience"
    );
    write_file(&file, &with_audience);
    // Live: the projected row — audience AND authority_role, as the
    // loader writes it (REV2 steps with the audience key added).
    let projected = REV2_STEPS.replace(
        "\"authority_role\":\"platform-admin\"",
        "\"authority_role\":\"platform-admin\",\"audience\":{\"role\":\"platform-admin\"}",
    );
    assert_ne!(projected, REV2_STEPS);
    write_file(&c.live, &live_row(2, REV2_DESC, &projected));
    let (rc, out) = c.run(&[KIND, "--check"]);
    assert_eq!(
        rc, 5,
        "a projected row equals its file — nothing to publish:\n{out}"
    );
    contains_all(&out, &["nothing to publish"], "the equal verdict");
    assert!(
        !out.contains("authority_role"),
        "the projection must not surface as a diff: {out}"
    );
}

/// A step's `agent` block is part of what the tree says (design
/// c87fb59b car 1, 2026-09-18): a tree step that gains one over an
/// otherwise-equal live row is the tree AHEAD, and the same block on
/// both sides is equal. Measured the day the block landed: the live
/// backlog-item v2 carried no agent block while its file did, this
/// comparison answered "equal — nothing to publish", the lint agreed,
/// and `boss dispatch` refused every build step for want of the block
/// nobody had published. A field the comparison does not read cannot
/// reach the registry through this door.
#[test]
fn a_step_that_gains_an_agent_block_is_the_tree_ahead_and_an_equal_block_is_equal() {
    if !ready() {
        return;
    }
    let c = Case::new("agent-block");
    let file = c.repo.join(format!("infra/platform/workflows/{KIND}.toml"));
    let with_agent = REV2_KIND_FILE.replace(
        "authority_role = \"platform-admin\"",
        "authority_role = \"platform-admin\"\nagent = { profile = \"builder\", model = \"opus-5[1m]\", budget_usd = 5, effort = \"high\" }",
    );
    assert_ne!(
        with_agent, REV2_KIND_FILE,
        "the fixture must declare an agent block"
    );
    write_file(&file, &with_agent);
    // Live: REV2 without the block — the tree is ahead by the block alone.
    write_file(&c.live, &live_row(2, REV2_DESC, REV2_STEPS));
    let (rc, out) = c.run(&[KIND, "--check"]);
    assert_eq!(
        rc, 0,
        "a step that gained an agent block is the tree ahead:\n{out}"
    );
    contains_all(&out, &["would publish"], "the ahead verdict");
    // Live: the same block projected — equal, nothing to publish.
    let projected = REV2_STEPS.replace(
        "\"authority_role\":\"platform-admin\"",
        "\"authority_role\":\"platform-admin\",\"agent\":{\"profile\":\"builder\",\"model\":\"opus-5[1m]\",\"budget_usd\":5.0,\"effort\":\"high\"}",
    );
    assert_ne!(projected, REV2_STEPS);
    write_file(&c.live, &live_row(2, REV2_DESC, &projected));
    let (rc, out) = c.run(&[KIND, "--check"]);
    assert_eq!(rc, 5, "the same block on both sides is equal:\n{out}");
    contains_all(&out, &["nothing to publish"], "the equal verdict");
}

// ---------------------------------------------------------------------------
// The host's CLI must be able to read what it is asked to publish.
// ---------------------------------------------------------------------------

/// The first live run (ops-request 25cb2f71, 2026-09-15 16:36Z; backlog
/// fec2851f): boss-gcp's `/usr/local/bin/boss` predated the bundle
/// loader (c17827f3, 2026-09-08), rejected the kind file as "is not
/// JSON", and the script reported exit 4 — "the tree's file does not
/// lint clean" — about a file that lints clean. The binary is read
/// BEFORE the registry: a CLI built from a commit older than the floor
/// is a host-configuration refusal (78, where "no boss CLI" already
/// lives) that names the binary, its commit, the floor and the refresh
/// path, and nothing is compared or published.
#[test]
fn a_host_cli_older_than_the_bundle_loader_is_refused_by_name_before_the_registry_is_read() {
    if !ready() {
        return;
    }
    let c = Case::new("stale-cli");
    let (rc, out) = c.run_env(
        &[KIND, "--check"],
        &[
            ("STUB_BUILT_FROM", c.rev1.clone()),
            ("BOSS_PUBLISH_CLI_FLOOR", c.rev2.clone()),
            // The registry is dark too: the CLI refusal must come first,
            // or this would read 75 and send an operator to the SoR.
            ("STUB_REGISTRY_DOWN", "1".into()),
        ],
    );
    assert_eq!(rc, 78, "{out}");
    contains_all(
        &out,
        &[
            "REFUSED",
            &c.bin.join("boss").display().to_string(),
            "built from",
            &c.rev1[..8],
            &c.rev2[..8],
            "boss-gcp-converge",
        ],
        "the stale-CLI refusal names the binary, its commit, the floor and the refresh path",
    );
    assert!(
        !out.contains("does not lint"),
        "a stale binary was blamed on the tree: {out}"
    );
    assert!(c.boss_calls().is_empty(), "boss acted: {out}");
}

/// A binary that cannot say what it was built from, or names a commit
/// this checkout does not hold, is refused the same way — the verb
/// does not guess that an unplaceable binary reads a bundle.
#[test]
fn a_host_cli_of_unknown_or_unplaceable_provenance_is_refused_not_guessed() {
    if !ready() {
        return;
    }
    let c = Case::new("unknown-cli");
    for (built, needle) in [
        ("unknown".to_string(), "cannot say"),
        ("deadbeef".repeat(5), "not in the history"),
    ] {
        let (rc, out) = c.run_env(&[KIND, "--check"], &[("STUB_BUILT_FROM", built.clone())]);
        assert_eq!(rc, 78, "{built}: {out}");
        contains_all(&out, &["REFUSED", needle], "the provenance refusal");
        assert!(c.boss_calls().is_empty(), "{built}: boss acted: {out}");
    }
}

/// The floor the script ships is the commit that taught the CLI to read
/// a bundle: at it `workflow.rs` calls the seed loader, and at its
/// parent it does not. Skips honestly where the checkout has no history
/// to read (a shallow gate clone); the fixture cases above cover the
/// mechanism either way.
#[test]
fn the_shipped_floor_is_the_commit_that_taught_the_cli_to_read_a_bundle() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let floor = script
        .lines()
        .find_map(|l| l.strip_prefix("CLI_FLOOR_DEFAULT="))
        .expect("the script names CLI_FLOOR_DEFAULT")
        .trim_matches('"')
        .to_string();
    assert_eq!(floor.len(), 40, "the floor is a full sha: {floor}");
    let show = |rev: &str| {
        Command::new("git")
            .args([
                "show",
                &format!("{rev}:crates/orchestrators/boss-cli/src/workflow.rs"),
            ])
            .current_dir(repo_root())
            .output()
            .expect("git runs")
    };
    let at = show(&floor);
    if !at.status.success() {
        eprintln!("skipping: this checkout does not hold {floor} (shallow?)");
        return;
    }
    assert!(
        String::from_utf8_lossy(&at.stdout).contains("seed_loader::load_workflows"),
        "at the floor the CLI reads a bundle"
    );
    let before = show(&format!("{floor}~1"));
    if before.status.success() {
        assert!(
            !String::from_utf8_lossy(&before.stdout).contains("seed_loader::load_workflows"),
            "the floor's parent already read a bundle — the floor is later than it needs to be"
        );
    }
}

// ---------------------------------------------------------------------------
// THROUGH THE RUNNER, with the real allowlist, as boss-gcp.
// ---------------------------------------------------------------------------

fn shipped_verbs(root: &Path) -> PathBuf {
    let dst = root.join("verbs");
    std::fs::create_dir_all(&dst).unwrap();
    for e in std::fs::read_dir(repo_root().join("infra/ops/verbs")).expect("infra/ops/verbs/") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
    dst
}

/// One open ops-request for boss-gcp carrying the verb and args, run
/// through `ops-runner.sh` against a stubbed system of record: the stub
/// `curl` answers the jobs read with the packet, records the PUT, and
/// serves the registry row the script reads.
fn run_runner(c: &Case, verbs: &Path, args: &str) -> (String, Option<serde_json::Value>) {
    write_exec(
        &c.bin.join("curl"),
        r#"#!/bin/sh
for a in "$@"; do case "$a" in @*) cp "${a#@}" "$STUB_PUT"; exit 0;; esac; done
out=""; code=0; url=""
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; -w) code=1 ;; http*) url="$1" ;; esac
  shift
done
case "$url" in
  */api/workflows/*) if [ -n "$out" ]; then cp "$STUB_LIVE" "$out"; else cat "$STUB_LIVE"; fi; [ "$code" = 1 ] && printf '200'; exit 0 ;;
esac
cat "$STUB_JOBS"
"#,
    );
    write_file(
        &c.root.join("jobs.json"),
        &format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"publish-workflow","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    );
    let put = c.root.join("put.json");
    let _ = std::fs::remove_file(&put);
    let out = Command::new("sh")
        .arg(repo_root().join("infra/ops/ops-runner.sh"))
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                c.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOST_ID", "boss-gcp")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", c.root.join("jobs.json"))
        .env("STUB_PUT", &put)
        .env("BOSS_PUBLISH_WORKFLOW_REPO", &c.repo)
        .env("STUB_LIVE", &c.live)
        .env("STUB_LIVE_AFTER", &c.live_after)
        .env("STUB_BOSS_LOG", &c.boss_log)
        .env("STUB_BUILT_FROM", &c.rev2)
        .env("BOSS_PUBLISH_CLI_FLOOR", &c.rev1)
        .output()
        .expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = std::fs::read_to_string(&put)
        .ok()
        .map(|s| serde_json::from_str::<serde_json::Value>(&s).expect("PUT payload is JSON"))
        .map(|v| v["metadata"].clone());
    (text, meta)
}

/// The allowlist's own validation: a kind outside the pattern, or a
/// mode outside the two literals, never reaches the script, and the
/// refusal names the param on the step.
#[test]
fn the_allowlist_refuses_a_malformed_kind_or_a_foreign_mode() {
    if !ready() {
        return;
    }
    let c = Case::new("runner-refuses");
    let verbs = shipped_verbs(&c.root);
    for (args, needle) in [
        ("[]", "missing required arg kind"),
        (r#"["Maintenance Widget"]"#, "does not match"),
        (
            r#"["maintenance-widget","--now"]"#,
            "is not one of --check, --force-tree",
        ),
    ] {
        let (text, meta) = run_runner(&c, &verbs, args);
        let meta = meta.unwrap_or_else(|| panic!("no step completed for {args}: {text}"));
        assert_eq!(meta["disposition"], "refused", "{args}: {meta}");
        let reason = meta["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains(needle),
            "{args}: `{needle}` not in `{reason}`"
        );
        assert!(
            c.boss_calls().is_empty(),
            "{args} reached the script: {text}"
        );
    }
}

/// `--check` through the runner is ANSWERED (the script ran, the verdict
/// and exit code ride the step); a bare publish through the runner
/// runs the real sequence against the stubs and reports the movement.
#[test]
fn through_the_runner_check_is_answered_and_a_publish_reports_its_movement() {
    if !ready() {
        return;
    }
    let c = Case::new("runner-answers");
    let verbs = shipped_verbs(&c.root);
    let (text, meta) = run_runner(&c, &verbs, r#"["maintenance-widget","--check"]"#);
    let meta = meta.unwrap_or_else(|| panic!("no step completed: {text}"));
    assert_eq!(meta["disposition"], "answered", "{meta}");
    assert_eq!(meta["exit_code"], "0", "{meta}");
    let out = meta["output"].as_str().unwrap_or_default();
    contains_all(out, &["would publish"], "the --check answer");
    assert!(
        c.publishes().is_empty(),
        "--check published through the runner"
    );

    let (text, meta) = run_runner(&c, &verbs, r#"["maintenance-widget"]"#);
    let meta = meta.unwrap_or_else(|| panic!("no step completed: {text}"));
    assert_eq!(meta["disposition"], "answered", "{meta}");
    assert_eq!(meta["exit_code"], "0", "{meta}");
    let out = meta["output"].as_str().unwrap_or_default();
    contains_all(out, &["v1 -> v2", "confirmed"], "the publish answer");
    assert_eq!(c.publishes().len(), 1, "{:?}", c.boss_calls());
}

/// The verb file itself: MUTATING, boss-gcp only, the script this test
/// runs, a kind pattern that refuses whitespace and a leading dash, the
/// two literal modes optional, and a timeout above the runner's default
/// (a history walk plus two `boss` runs against the registry).
#[test]
fn the_verb_file_has_the_reviewed_shape() {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/publish-workflow.json"))
            .expect("infra/ops/verbs/publish-workflow.json"),
    )
    .expect("the verb file is JSON");
    let about = v["about"].as_str().unwrap_or_default();
    contains_all(
        about,
        &["MUTATING", "David", "--check", "--force-tree"],
        "the verb's about",
    );
    assert!(
        about.to_lowercase().contains("never said"),
        "the about names the load-bearing refusal"
    );
    assert_eq!(v["hosts"], serde_json::json!(["boss-gcp"]));
    assert_eq!(v["argv"][0], SCRIPT);
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}", "{2}"]));
    let params = v["params"].as_array().expect("params");
    assert_eq!(params[0]["name"], "kind");
    let pat = params[0]["pattern"].as_str().unwrap().to_string();
    let matches = |s: &str| {
        Command::new("sh")
            .args([
                "-c",
                r#"printf '%s' "$2" | grep -Eqx -- "$1""#,
                "sh",
                &pat,
                s,
            ])
            .status()
            .is_ok_and(|st| st.success())
    };
    for ok in ["ship-a-change", "maintenance-sweep", "pr-train"] {
        assert!(matches(ok), "the kind pattern refuses {ok}");
    }
    for bad in ["-x", "a b", "Ship", "a", "kind/x"] {
        assert!(!matches(bad), "the kind pattern admits {bad:?}");
    }
    assert_eq!(params[1]["name"], "mode");
    assert_eq!(
        params[1]["one_of"],
        serde_json::json!(["--check", "--force-tree"])
    );
    assert_eq!(params[1]["optional"], true);
    assert!(v["timeout"].as_u64().unwrap_or(0) >= 120);
}
