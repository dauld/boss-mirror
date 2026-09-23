//! `infra/gcp/publish-drift.sh` is RUN, not read — against a planted
//! checkout, a planted `publish-workflow.sh` stub that answers each of
//! the sub-verb's exit codes by name, and a stub `curl` standing in for
//! the system of record's pr-train read — so every verdict below is one
//! the script actually reached.
//!
//! WHY THE VERB EXISTS (backlog a2f97942, ratified by retro 27fad542).
//! `publish-workflow` is one kind per request by design (bounded), and
//! on 2026-09-18 13:4x that bound became the operator's loop: after the
//! maintenance-audience car landed, 23 `publish-workflow` ops-requests
//! were hand-scripted one per kind, preceded by a hand `until --check
//! ok` wait for boss-gcp's checkout to carry the train. The gap is one
//! level up: after a train lands, every kind the tree moved ahead of
//! should be published as ONE act, and the wait should be a verdict.
//!
//! WHAT IT PINS. The comparison is never re-derived here: the drift set
//! is `publish-workflow.sh <kind> --check` per kind, classified by that
//! verb's documented exit codes — 0 tree-ahead, 5 equal, 6 the live row
//! carries what the tree never said, 4 does not lint, 8 no live row —
//! so a refusal-6 kind is LISTED field by field and never published,
//! in either mode. `--check` lists and writes nothing; `--for-real`
//! publishes each tree-ahead kind in bundle order through
//! `publish-workflow.sh` itself and exits non-zero when any publish was
//! not confirmed. The first line names the checkout sha the verb read;
//! a checkout behind the newest converged train is `not yet` (75),
//! never a publish from stale files. The phrases the table reads
//! versions from are pinned to the sub-verb's own text.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

const SCRIPT: &str = "infra/gcp/publish-drift.sh";
const SUB_VERB: &str = "infra/gcp/publish-workflow.sh";
const VERB_FILE: &str = "infra/ops/verbs/publish-drift.json";

/// A pr-train listing as `GET /api/jobs?kind=pr-train` answers it: the
/// newest closed train's `merged` step carries `merge_ref`.
fn trains(merge_ref: &str) -> String {
    format!(
        r#"{{"data":[
  {{"id":"t-old","status":"closed","steps":[{{"spec_slug":"merged","status":"completed","completed_at":"2026-09-18T10:55:00Z","metadata":{{"merge_ref":"0000000000000000000000000000000000000000"}}}}]}},
  {{"id":"t-open","status":"open","steps":[{{"spec_slug":"merged","status":"pending","metadata":{{}}}}]}},
  {{"id":"t-new","status":"closed","steps":[{{"spec_slug":"merged","status":"completed","completed_at":"2026-09-18T13:58:00Z","metadata":{{"merge_ref":"{merge_ref}"}}}}]}}
],"total":3}}"#
    )
}

struct Case {
    root: PathBuf,
    bin: PathBuf,
    repo: PathBuf,
    stub: PathBuf,
    verdicts: PathBuf,
    trains: PathBuf,
    pw_log: PathBuf,
    head: String,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("publish-drift-{name}"));
        let bin = root.join("bin");
        let repo = root.join("repo");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(repo.join("infra/platform/workflows")).unwrap();
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
        // The history the freshness read is judged against; the bundle
        // itself is planted per case by `verdicts`, so each case's
        // table is exactly the kinds it names.
        write_file(&repo.join("README"), "the fixture checkout\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "the checkout"]);
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("git rev-parse runs");
        let head = String::from_utf8_lossy(&out.stdout).trim().to_string();

        // The planted sub-verb: answers `<kind> --check` and `<kind>`
        // from a verdict table (`kind rc version` per line), in the
        // sub-verb's own words for the lines the table reads, and logs
        // every call so the test can say what was published.
        let stub = root.join("publish-workflow.sh");
        write_exec(
            &stub,
            r#"#!/usr/bin/env bash
kind="$1"; mode="${2:-}"
echo "$kind ${mode:-PUBLISH}" >> "$STUB_PW_LOG"
row=$(grep -E "^$kind " "$STUB_VERDICTS" | head -n1)
rc=$(cut -d' ' -f2 <<<"$row"); ver=$(cut -d' ' -f3 <<<"$row")
[ -n "$rc" ] || { echo "publish-workflow: REFUSED — the tree has no infra/platform/workflows/$kind.toml"; exit 3; }
file="infra/platform/workflows/$kind.toml"
if [ "$mode" = "--check" ]; then
  case "$rc" in
    0) echo "publish-workflow: live $kind v$ver differs from $file:"
       echo "  description  tree=A run that died closes this packet failed.  live=<absent>"
       echo "publish-workflow: the tree moved ahead: live $kind v$ver is the row at 8cc7a03e:infra/platform/workflows.toml"
       echo "publish-workflow: --check ok: would publish $file over live v$ver at $BOSS_JOBS_URL (nothing written; packet ${OPS_REQUEST_ID:-none})"; exit 0 ;;
    5) echo "publish-workflow: REFUSED — nothing to publish — the live $kind v$ver already says what $file says (label, description, category, subject_kinds and every step compared equal)."; exit 5 ;;
    6) echo "publish-workflow: live $kind v$ver differs from $file:"
       echo "  steps[1].fields  tree=[[\"result\",\"string\",true]]  live=[[\"result\",\"string\",true],[\"proof\",\"string\",true]]"
       echo "  steps[1].metadata_defaults  tree=<absent>  live={\"procedure\":\"Observe the sweep working in production.\"}"
       echo "publish-workflow: REFUSED — live $kind v$ver carries what the tree never said: no revision of infra/platform in 500 commit(s) walked renders to it"; exit 6 ;;
    4) echo "publish-workflow: boss workflow publish --dry-run refused the tree's row; its output, whole:"
       echo "  [\"step run: ready_when references no step\"]"
       echo "publish-workflow: REFUSED — the tree's $file does not lint clean"; exit 4 ;;
    8) echo "publish-workflow: REFUSED — '$kind' has no live active row at $BOSS_JOBS_URL — the seed admits a new kind"; exit 8 ;;
    75) echo "publish-workflow: could not read $BOSS_JOBS_URL/api/workflows/$kind — HTTP 000"
        echo "publish-workflow: cannot answer: nothing compared, nothing published"; exit 75 ;;
    *) echo "stub: unexpected rc $rc"; exit 99 ;;
  esac
fi
if [ -n "$mode" ]; then echo "stub: unexpected mode $mode"; exit 2; fi
case " ${STUB_PUBLISH_FAIL:-} " in
  *" $kind "*) echo "publish-workflow: NOT CONFIRMED — the active row is still v$ver after a publish over v$ver"; exit 7 ;;
esac
next=$((ver + 1))
echo "publish-workflow: publishing $file as ${BOSS_ACTOR:-unset} for packet ${OPS_REQUEST_ID:-none}"
echo "publish-workflow: $kind v$ver -> v$next live at $BOSS_JOBS_URL — confirmed by reading the active row back and comparing it to $file (equal); packet ${OPS_REQUEST_ID:-none}"
exit 0
"#,
        );
        // Stub curl: the pr-train read, from a file; STUB_SOR_DOWN
        // answers nothing.
        write_exec(
            &bin.join("curl"),
            r#"#!/bin/sh
[ -n "${STUB_SOR_DOWN:-}" ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
for a in "$@"; do case "$a" in @*) cp "${a#@}" "$STUB_PUT"; exit 0;; esac; done
cat "$STUB_TRAINS"
"#,
        );
        let trains_f = root.join("trains.json");
        write_file(&trains_f, &trains(&head));
        Self {
            root: root.clone(),
            bin,
            repo,
            stub,
            verdicts: root.join("verdicts.txt"),
            trains: trains_f,
            pw_log: root.join("pw.log"),
            head,
        }
    }

    /// The bundle: one file per kind named, in whatever order the case
    /// lists them (the verb reads the directory in file-name order,
    /// not this order), and `kind rc version` per line for what the
    /// planted sub-verb answers each.
    fn verdicts(&self, rows: &[(&str, u32, u32)]) {
        let bundle = self.repo.join("infra/platform/workflows");
        let _ = std::fs::remove_dir_all(&bundle);
        std::fs::create_dir_all(&bundle).unwrap();
        for (k, _, _) in rows {
            write_file(
                &bundle.join(format!("{k}.toml")),
                &format!("[[workflow]]\nkind = \"{k}\"\nlabel = \"{k}\"\n"),
            );
        }
        let body: String = rows
            .iter()
            .map(|(k, rc, v)| format!("{k} {rc} {v}\n"))
            .collect();
        write_file(&self.verdicts, &body);
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
            .env("BOSS_PUBLISH_WORKFLOW_SH", &self.stub)
            .env("STUB_VERDICTS", &self.verdicts)
            .env("STUB_TRAINS", &self.trains)
            .env("STUB_PW_LOG", &self.pw_log);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("publish-drift.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// Every sub-verb call, in order: `<kind> --check` or `<kind> PUBLISH`.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.pw_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn publishes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|l| l.strip_suffix(" PUBLISH").map(str::to_string))
            .collect()
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

/// The one line the table row for `kind` is on.
fn row<'a>(text: &'a str, kind: &str) -> &'a str {
    text.lines()
        .find(|l| l.starts_with(&format!("{kind} ")) || l.starts_with(&format!("{kind}\t")))
        .unwrap_or_else(|| panic!("no table row for {kind} in:\n{text}"))
}

fn ready() -> bool {
    for (ok, why) in [(has("git"), "git"), (has("jq"), "jq")] {
        if !ok {
            eprintln!("skipping: publish-drift.sh needs {why}, and this box has none");
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// --check: the drift set, classified, nothing written.
// ---------------------------------------------------------------------------

/// Every kind in the bundle is asked once with `--check`, classified by
/// the sub-verb's exit code, and listed in one table; a refusal-6 kind
/// is named field by field; the verdict line counts; nothing is
/// published; the first line names the checkout sha.
#[test]
fn check_lists_every_kind_by_the_sub_verbs_verdict_and_publishes_nothing() {
    if !ready() {
        return;
    }
    let c = Case::new("check");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 5, 2),
        ("maintenance-gamma", 6, 31),
        ("maintenance-delta", 4, 1),
        ("maintenance-epsilon", 8, 0),
    ]);
    let (rc, out) = c.run(&["--check"]);
    assert_eq!(rc, 0, "{out}");
    let first = out.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("publish-drift: checkout ") && first.contains(&c.head[..12]),
        "the first line names the checkout sha: `{first}`"
    );
    contains_all(
        row(&out, "maintenance-alpha"),
        &["v3", "would publish"],
        "the tree-ahead row",
    );
    contains_all(
        row(&out, "maintenance-beta"),
        &["v2", "equal"],
        "the equal row",
    );
    contains_all(
        row(&out, "maintenance-gamma"),
        &["v31", "REFUSED", "never said"],
        "the refusal-6 row",
    );
    // The drift is named field by field, copied from the sub-verb —
    // the operator's decision needs the fields, not the count.
    contains_all(
        &out,
        &["steps[1].fields", "\"proof\"", "steps[1].metadata_defaults"],
        "the refusal-6 fields",
    );
    contains_all(
        row(&out, "maintenance-delta"),
        &["REFUSED", "lint"],
        "the lint-failed row",
    );
    contains_all(
        row(&out, "maintenance-epsilon"),
        &["no live row"],
        "the pending row",
    );
    contains_all(
        &out,
        &["publish-drift: would publish 1, skipped 1 equal, refused 2"],
        "the verdict line",
    );
    assert!(
        c.publishes().is_empty(),
        "--check published: {:?}",
        c.calls()
    );
    let checks: Vec<_> = c
        .calls()
        .into_iter()
        .filter(|l| l.ends_with(" --check"))
        .collect();
    assert_eq!(checks.len(), 5, "one --check per kind: {checks:?}");
}

/// No argument is `--check`: the mode a rule-filed packet carries by
/// the verb file's default, and the safe one.
#[test]
fn no_mode_is_check() {
    if !ready() {
        return;
    }
    let c = Case::new("default-mode");
    c.verdicts(&[("maintenance-alpha", 0, 3)]);
    let (rc, out) = c.run(&[]);
    assert_eq!(rc, 0, "{out}");
    contains_all(&out, &["would publish 1"], "the default mode's verdict");
    assert!(
        c.publishes().is_empty(),
        "no mode published: {:?}",
        c.calls()
    );
}

// ---------------------------------------------------------------------------
// --for-real: each tree-ahead kind, in bundle order, through the sub-verb.
// ---------------------------------------------------------------------------

/// Only the tree-ahead kinds are published, in bundle (file-name)
/// order, each through `publish-workflow.sh <kind>`; the equal and the
/// refused are listed and left; the table shows each movement.
#[test]
fn for_real_publishes_only_tree_ahead_kinds_in_bundle_order() {
    if !ready() {
        return;
    }
    let c = Case::new("for-real");
    // Listed out of order on purpose: bundle order is file-name order.
    c.verdicts(&[
        ("maintenance-epsilon", 0, 1),
        ("maintenance-gamma", 6, 31),
        ("maintenance-beta", 0, 7),
        ("maintenance-delta", 5, 2),
        ("maintenance-alpha", 0, 3),
    ]);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        c.publishes(),
        vec![
            "maintenance-alpha".to_string(),
            "maintenance-beta".to_string(),
            "maintenance-epsilon".to_string()
        ],
        "the tree-ahead kinds, in bundle order, and no other: {:?}",
        c.calls()
    );
    contains_all(
        row(&out, "maintenance-alpha"),
        &["v3", "v4", "published"],
        "a published row shows its movement",
    );
    contains_all(
        row(&out, "maintenance-gamma"),
        &["v31", "REFUSED", "never said"],
        "the refusal-6 row under --for-real",
    );
    contains_all(
        &out,
        &["publish-drift: published 3, skipped 1 equal, refused 1"],
        "the verdict line",
    );
    // Every publish is signed as the runner's automation, the
    // identity rule every `boss` verb applies.
    contains_all(&out, &["as automation:ops-runner"], "the actor");
}

/// The load-bearing negative: a live row the tree never said is never
/// published by this verb, even when it is the only drift there is —
/// no `--force-tree` exists here; that stays the Drift tab's approve,
/// one kind at a time, with the field named.
#[test]
fn a_refusal_6_kind_is_never_published_even_alone() {
    if !ready() {
        return;
    }
    let c = Case::new("refusal-6-alone");
    c.verdicts(&[("maintenance-gamma", 6, 31)]);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(
        rc, 0,
        "a refusal is the operator's decision, not the verb's failure: {out}"
    );
    assert!(
        c.publishes().is_empty(),
        "published over a refusal-6: {:?}",
        c.calls()
    );
    contains_all(
        &out,
        &[
            "steps[1].fields",
            "publish-drift: published 0, skipped 0 equal, refused 1",
            "force-tree",
        ],
        "the refusal listed, and the way through named",
    );
    assert!(
        !out.contains("--for-real --force-tree"),
        "the verb must not offer itself a force: {out}"
    );
}

/// A publish the sub-verb did not confirm is reported on its row and
/// the verb exits non-zero — while the other kinds still land, because
/// each kind's publish is independent and a stopped loop would leave
/// the operator back in the loop.
#[test]
fn an_unconfirmed_publish_is_reported_and_exits_nonzero() {
    if !ready() {
        return;
    }
    let c = Case::new("unconfirmed");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 0, 7),
        ("maintenance-gamma", 0, 1),
    ]);
    let (rc, out) = c.run_env(
        &["--for-real"],
        &[("STUB_PUBLISH_FAIL", "maintenance-beta".into())],
    );
    assert_eq!(rc, 1, "{out}");
    contains_all(
        row(&out, "maintenance-beta"),
        &["NOT CONFIRMED"],
        "the unconfirmed row",
    );
    contains_all(
        row(&out, "maintenance-gamma"),
        &["published"],
        "the kind after the failure still lands",
    );
    contains_all(
        &out,
        &["publish-drift: published 2, skipped 0 equal, refused 0, not confirmed 1"],
        "the verdict counts the failure by name",
    );
}

// ---------------------------------------------------------------------------
// Freshness: a stale checkout is `not yet`, never a publish.
// ---------------------------------------------------------------------------

/// The newest converged train's merge sha is read off the system of
/// record; when the checkout does not carry it the answer is `not yet`
/// with both shas named, exit 75, and the sub-verb is never asked —
/// the operator's `until --check ok` loop, as a verdict.
#[test]
fn a_checkout_behind_the_newest_train_is_not_yet_75_and_asks_nothing() {
    if !ready() {
        return;
    }
    let c = Case::new("stale");
    c.verdicts(&[("maintenance-alpha", 0, 3)]);
    let ahead = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    write_file(&c.trains, &trains(ahead));
    for mode in ["--check", "--for-real"] {
        let (rc, out) = c.run(&[mode]);
        assert_eq!(rc, 75, "{mode}: {out}");
        contains_all(
            &out,
            &[
                &format!(
                    "not yet: checkout at {}, main at {}",
                    &c.head[..8],
                    &ahead[..8]
                ),
                "t-new",
            ],
            "the not-yet line names both shas and the train",
        );
        assert!(
            c.calls().is_empty(),
            "{mode} asked the sub-verb: {:?}",
            c.calls()
        );
    }
}

/// A system of record that cannot be read, or one that lists no
/// converged train (a denied scope answers an empty page, not an
/// error), is 75 — never a publish from an unjudged checkout.
#[test]
fn an_unreadable_or_empty_train_listing_cannot_answer() {
    if !ready() {
        return;
    }
    let c = Case::new("sor-down");
    c.verdicts(&[("maintenance-alpha", 0, 3)]);
    let (rc, out) = c.run_env(&["--for-real"], &[("STUB_SOR_DOWN", "1".into())]);
    assert_eq!(rc, 75, "{out}");
    contains_all(&out, &["cannot answer"], "the unreadable read");
    assert!(c.calls().is_empty(), "{:?}", c.calls());

    write_file(&c.trains, r#"{"data":[],"total":0}"#);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 75, "{out}");
    contains_all(
        &out,
        &["no ", "pr-train"],
        "the empty listing is named, not read as a fact",
    );
    assert!(c.calls().is_empty(), "{:?}", c.calls());
}

/// A sub-verb answer that is not a verdict — the registry could not be
/// read for one kind — stops the run at 75: half a drift set judged
/// from half a registry is the confident wrong answer.
#[test]
fn a_sub_verb_that_cannot_answer_stops_the_run() {
    if !ready() {
        return;
    }
    let c = Case::new("sub-75");
    c.verdicts(&[
        ("maintenance-alpha", 0, 3),
        ("maintenance-beta", 75, 0),
        ("maintenance-gamma", 0, 1),
    ]);
    let (rc, out) = c.run(&["--for-real"]);
    assert_eq!(rc, 75, "{out}");
    contains_all(
        &out,
        &["maintenance-beta", "cannot answer"],
        "the kind that could not be read",
    );
    assert!(
        c.publishes().is_empty(),
        "published past an unreadable kind: {:?}",
        c.calls()
    );
}

/// No system of record is a configuration fault, a mode outside the
/// two is usage, and a sub-verb that is not there is configuration —
/// each refused before anything is read.
#[test]
fn no_sor_a_foreign_mode_or_a_missing_sub_verb_is_refused_first() {
    if !ready() {
        return;
    }
    let c = Case::new("config");
    c.verdicts(&[("maintenance-alpha", 0, 3)]);
    let (rc, out) = c.run(&["--force-tree"]);
    assert_eq!(rc, 2, "{out}");
    contains_all(&out, &["--check", "--for-real"], "usage names the modes");
    let (rc, out) = c.run_env(&["--check"], &[("BOSS_JOBS_URL", String::new())]);
    assert_eq!(rc, 78, "{out}");
    contains_all(&out, &["BOSS_JOBS_URL"], "the missing address is named");
    let (rc, out) = c.run_env(
        &["--check"],
        &[(
            "BOSS_PUBLISH_WORKFLOW_SH",
            c.root.join("absent.sh").display().to_string(),
        )],
    );
    assert_eq!(rc, 78, "{out}");
    contains_all(&out, &["absent.sh"], "the missing sub-verb is named");
    assert!(c.calls().is_empty(), "{:?}", c.calls());
}

// ---------------------------------------------------------------------------
// One definition: the sub-verb's exit codes and phrases are what this
// verb reads. Pinned rather than collapsed — the phrases live in the
// sub-verb's say/refuse calls and the classification in its EXIT block.
// ---------------------------------------------------------------------------

/// The phrases `publish-drift.sh` reads a version or a verdict out of
/// are the sub-verb's own words: each appears in BOTH files, so a
/// rewording of one names the other here instead of reading `?`.
#[test]
fn the_phrases_the_table_reads_are_the_sub_verbs_own() {
    let drift = std::fs::read_to_string(repo_root().join(SCRIPT)).expect(SCRIPT);
    let sub = std::fs::read_to_string(repo_root().join(SUB_VERB)).expect(SUB_VERB);
    for phrase in [
        "--check ok: would publish",
        "already says what",
        "carries what the tree never said",
        "-> v",
    ] {
        assert!(sub.contains(phrase), "{SUB_VERB} no longer says `{phrase}`");
        assert!(drift.contains(phrase), "{SCRIPT} does not read `{phrase}`");
    }
    // The exit codes are the sub-verb's documented contract, read by
    // number in the classifier: each number the EXIT block documents
    // for a refusal appears as a case there.
    for code in ["5)", "6)", "4)", "8)", "7)"] {
        assert!(
            drift.contains(code),
            "{SCRIPT} has no case for sub-verb exit {code}"
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
/// through `ops-runner.sh` against the stub `curl`: the jobs read
/// answers the packet, the pr-train read answers the fixture, the PUT
/// is recorded.
fn run_runner(c: &Case, verbs: &Path, args: &str) -> (String, Option<serde_json::Value>) {
    write_exec(
        &c.bin.join("curl"),
        r#"#!/bin/sh
for a in "$@"; do case "$a" in @*) cp "${a#@}" "$STUB_PUT"; printf 200; exit 0;; esac; done
url=""
for a in "$@"; do case "$a" in http*) url="$a" ;; esac; done
case "$url" in
  *kind=pr-train*) cat "$STUB_TRAINS"; exit 0 ;;
esac
cat "$STUB_JOBS"
"#,
    );
    write_file(
        &c.root.join("jobs.json"),
        &format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"publish-drift","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    );
    let put = c.root.join("put.json");
    let _ = std::fs::remove_file(&put);
    let _ = std::fs::remove_file(&c.pw_log);
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
        .env("BOSS_PUBLISH_WORKFLOW_SH", &c.stub)
        .env("STUB_VERDICTS", &c.verdicts)
        .env("STUB_TRAINS", &c.trains)
        .env("STUB_PW_LOG", &c.pw_log)
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

/// A packet with NO args — the shape a `jobs.spawn` rule files, since a
/// rule cannot carry a list — runs `--check` by the verb file's default
/// and is ANSWERED with the table; `--for-real` publishes; a word
/// outside the two literals is REFUSED by the runner with the reason
/// on the step, and the script never runs.
#[test]
fn through_the_runner_no_args_is_check_for_real_publishes_and_a_foreign_mode_is_refused() {
    if !ready() {
        return;
    }
    let c = Case::new("runner");
    c.verdicts(&[("maintenance-alpha", 0, 3), ("maintenance-gamma", 6, 31)]);
    let verbs = shipped_verbs(&c.root);

    let (text, meta) = run_runner(&c, &verbs, "[]");
    let meta = meta.unwrap_or_else(|| panic!("no step completed: {text}"));
    assert_eq!(meta["disposition"], "answered", "{meta}");
    assert_eq!(meta["exit_code"], "0", "{meta}");
    let out = meta["output"].as_str().unwrap_or_default();
    contains_all(
        out,
        &[
            "publish-drift: checkout ",
            "would publish 1, skipped 0 equal, refused 1",
        ],
        "the no-args answer is a --check",
    );
    assert!(
        c.publishes().is_empty(),
        "no args published: {:?}",
        c.calls()
    );

    let (text, meta) = run_runner(&c, &verbs, r#"["--for-real"]"#);
    let meta = meta.unwrap_or_else(|| panic!("no step completed: {text}"));
    assert_eq!(meta["disposition"], "answered", "{meta}");
    assert_eq!(meta["exit_code"], "0", "{meta}");
    let out = meta["output"].as_str().unwrap_or_default();
    contains_all(
        out,
        &["published 1, skipped 0 equal, refused 1"],
        "the --for-real answer",
    );
    assert_eq!(c.publishes(), vec!["maintenance-alpha".to_string()]);

    let (text, meta) = run_runner(&c, &verbs, r#"["--force-tree"]"#);
    let meta = meta.unwrap_or_else(|| panic!("no step completed: {text}"));
    assert_eq!(meta["disposition"], "refused", "{meta}");
    let reason = meta["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("is not one of --check, --for-real"),
        "the refusal names the literal list: {reason}"
    );
    assert!(
        c.calls().is_empty(),
        "a refused mode reached the script: {:?}",
        c.calls()
    );
}

/// The verb file itself: MUTATING, authorized by name, boss-gcp only,
/// the script this test runs with one literal-list param whose default
/// is `--check` (so a rule-filed packet, which carries no args, can
/// only ever check), and a timeout sized for a whole bundle of
/// sub-verb runs rather than one.
#[test]
fn the_verb_file_has_the_reviewed_shape() {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join(VERB_FILE)).expect(VERB_FILE),
    )
    .expect("the verb file is JSON");
    let about = v["about"].as_str().unwrap_or_default();
    contains_all(
        about,
        &[
            "MUTATING",
            "David",
            "--check",
            "--for-real",
            "publish-workflow.sh",
            "27fad542",
        ],
        "the verb's about",
    );
    assert!(
        about.to_lowercase().contains("never said"),
        "the about names the refusal this verb never overrides"
    );
    assert!(
        about.contains("no --force-tree"),
        "the about says this verb has no --force-tree"
    );
    assert_eq!(v["hosts"], serde_json::json!(["boss-gcp"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}"]));
    let params = v["params"].as_array().expect("params");
    assert_eq!(params.len(), 1, "one param, the mode: {params:?}");
    assert_eq!(params[0]["name"], "mode");
    assert_eq!(
        params[0]["one_of"],
        serde_json::json!(["--check", "--for-real"])
    );
    assert_eq!(
        params[0]["default"], "--check",
        "a packet with no args must check, never publish"
    );
    let sub: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/publish-workflow.json"))
            .expect("infra/ops/verbs/publish-workflow.json"),
    )
    .expect("the sub-verb file is JSON");
    let one = sub["timeout"]
        .as_u64()
        .expect("publish-workflow declares a timeout");
    let whole = v["timeout"]
        .as_u64()
        .expect("publish-drift declares a timeout");
    assert!(
        whole >= 4 * one,
        "publish-drift's timeout ({whole}s) must cover many sub-verb runs (one is {one}s)"
    );
}
