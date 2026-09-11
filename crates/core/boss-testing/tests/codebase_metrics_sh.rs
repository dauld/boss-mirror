//! `infra/codebase-metrics.sh` — the codebase states its own trend.
//!
//! David, 2026-09-11: "once we have code base analysis statistics that we
//! can start to get a sense for whether we are getting simpler and more
//! reliable or more complex as we go."
//!
//! THE ANCHOR IS THE BACKFILL. Nothing had to start collecting: every
//! landing on main is a first-parent commit carrying a diffstat, so the
//! whole series is reconstructible retroactively. That is the only reason
//! this cadence is cheap — and it is also the only part that can be wrong
//! in a way nobody notices, because a series computed from git looks
//! exactly as confident whether or not the arithmetic holds. So the test
//! is a throwaway repo with a HAND-COUNTED history, including the two
//! shapes a line counter gets wrong:
//!
//!   * a landing that only DELETES (adds = 0, so the delete:add ratio has
//!     no denominator — it must report null, not divide by zero, and not
//!     quietly report 0%)
//!   * a landing that only ADDS (dels = 0, the common case, which must
//!     not be confused with "no data")
//!
//! THE PROD/TEST SPLIT IS THE HONEST HALF. A path-based bucket
//! (`crates/**/tests/*.rs`, `*.test.ts`) is easy and is what the series
//! carries. Inline `#[cfg(test)] mod tests` inside a production `.rs` file
//! is the hard half and is a LARGE share of this repo's tests — measured
//! 2026-09-11: 72,612 of 248,738 lines under `crates/**` outside a
//! `tests/` directory, 29%. A path bucket counts every one of those as
//! production code. The snapshot therefore measures them exactly, by
//! brace-matching the attribute's item, so the bias the per-landing series
//! carries is a number on the same packet rather than an unstated flaw.
//! The fixture's `lib.rs` is 4 production lines and 7 inline test lines
//! for exactly that reason: a path-only implementation passes every other
//! assertion here and fails this one.
//!
//! Everything runs in a fixture repo this process owns (pid + uid scratch,
//! never a fixed `/tmp` path), so the ambient repository's ownership —
//! which is what breaks git in a gate workspace — cannot decide whether
//! the test passes.

use std::path::PathBuf;
use std::process::Command;

/// "I could not answer." Distinct from 0 (read the repo, here is the
/// series) and 1 (a real failure) on purpose; `infra/lint/lib/git-answer.sh`
/// carries the argument.
const CANNOT_ANSWER: i32 = 3;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn script() -> PathBuf {
    repo_root().join("infra/codebase-metrics.sh")
}

/// A throwaway repo with a hand-counted history.
struct Fixture {
    dir: PathBuf,
}

/// `lib.rs` as the fixture seeds it: 4 production lines, then a
/// `#[cfg(test)]` attribute and the 6-line module it governs — 7 test
/// lines, 11 total.
const LIB_RS: &str = "pub fn one() -> u32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert_eq!(1, 1);\n    }\n}\n";

/// The same file with four more production lines appended.
const LIB_RS_GROWN: &str = "pub fn one() -> u32 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        assert_eq!(1, 1);\n    }\n}\npub fn two() -> u32 {\n    2\n}\n// four added lines\n";

/// A one-row step-type registry. The code-branch half of CLAUDE.md §9 is
/// counted against the kinds the MEASURED TREE declares, so a fixture
/// with no registry in it has no vocabulary and the counter refuses —
/// which is why the leaked-branch fixture has to seed one.
const STEP_TYPES_TOML: &str = "[[step_type]]\nkind = \"sign-off\"\nlabel = \"Sign-off\"\n";

/// Core code that must be edited to teach it a new step kind: the
/// anti-pattern §9 names, in its smallest honest form.
const LEAK_RS: &str = "pub fn body(step_kind: &str) -> u32 {\n    match step_kind {\n        \"sign-off\" => {\n            let n = one();\n            n + 1\n        }\n        _ => 0,\n    }\n}\n";

impl Fixture {
    /// Five landings, each one hand-counted in the constant beside it.
    fn new(case: &str) -> Self {
        let me = Fixture {
            dir: boss_testing::scratch_dir(&format!("codebase-metrics-{case}")),
        };
        me.git(&["init", "-q", "-b", "main", "."]);
        me.git(&["config", "user.email", "t@t"]);
        me.git(&["config", "user.name", "t"]);

        // 1. seed — +13 / -0 (lib.rs 11, docs/a.md 2)
        me.write("crates/core/boss-x/src/lib.rs", LIB_RS);
        me.write("docs/a.md", "# a\n\n");
        me.commit("seed");

        // 2. a landing that ONLY ADDS, and adds a test file — +5 / -0
        me.write(
            "crates/core/boss-x/tests/it.rs",
            "#[test]\nfn t() {\n    assert!(true);\n}\n// five\n",
        );
        me.commit("adds only");

        // 3. a landing that ONLY DELETES — +0 / -2
        std::fs::remove_file(me.dir.join("docs/a.md")).expect("remove docs/a.md");
        me.commit("deletes only");

        // 4. production and web test together — +10 / -0 (rust_prod 4, web_test 6)
        me.write("crates/core/boss-x/src/lib.rs", LIB_RS_GROWN);
        me.write(
            "apps/web/src/a.test.ts",
            "import { test } from 'bun:test';\ntest('a', () => {\n  1;\n});\n// five\n// six\n",
        );
        me.commit("mixed");

        // 5. a landing that only deletes PRODUCTION code — +0 / -4
        me.write("crates/core/boss-x/src/lib.rs", LIB_RS);
        me.commit("prod delete only");

        me
    }

    /// The same five landings plus a sixth that seeds a one-row step-type
    /// registry and a core file branching on its kind — so the
    /// code-branch half of §9 has both a vocabulary to measure against
    /// and exactly one thing to find.
    fn with_a_leaked_branch(case: &str) -> Self {
        let me = Fixture::new(case);
        me.write(
            "crates/core/boss-jobs/seeds/step_types.toml",
            STEP_TYPES_TOML,
        );
        me.write("crates/core/boss-x/src/leak.rs", LEAK_RS);
        me.commit("a leaked branch");
        me
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "fixture git {args:?} failed in {}: {}{}",
            self.dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.dir.join(rel);
        if let Some(parent) = path.parent() {
            boss_testing::create_dir(parent);
        }
        boss_testing::write_file(&path, body);
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", msg]);
    }

    /// The full sha of a landing, found by its subject — the test names
    /// commits the way a reader does, not by an index into a log.
    fn sha(&self, subject: &str) -> String {
        let out = Command::new("git")
            .args(["log", "--format=%H %s", "--all"])
            .current_dir(&self.dir)
            .output()
            .expect("git log");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| {
                l.split_once(' ')
                    .map(|(_, s)| s == subject)
                    .unwrap_or(false)
            })
            .map(|l| l.split_once(' ').expect("sha and subject").0.to_string())
            .unwrap_or_else(|| panic!("no commit with subject {subject:?}"))
    }

    /// Run the script and parse its stdout as JSON. Panics naming the
    /// exit status and stderr, because a metrics script that fails
    /// silently is the defect this whole car is about.
    fn run(&self, args: &[&str]) -> serde_json::Value {
        let out = self.try_run(args);
        assert!(
            out.status.success(),
            "codebase-metrics.sh {args:?} exited {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is not JSON ({e}):\n{}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    fn try_run(&self, args: &[&str]) -> std::process::Output {
        Command::new("bash")
            .arg(script())
            .args(args)
            .arg("--repo")
            .arg(&self.dir)
            .env("BOSS_TRUNK_REF", "main")
            // THE COUNTER IS PINNED, not discovered. The script's own
            // resolution walk looks at $CARGO_TARGET_DIR and
            // `<repo>/target` — both of which differ between this pod and
            // the gate, so a test that let it search would assert a
            // different thing on each machine. Naming the binary cargo
            // just built makes the code-branch half of every assertion
            // below a fact about the script rather than about the box.
            .env(
                "BOSS_LEAKED_POLICY_BIN",
                env!("CARGO_BIN_EXE_boss-leaked-policy"),
            )
            .output()
            .expect("spawn codebase-metrics.sh")
    }
}

/// `j["a"]["b"]` as an i64, naming the path when it is missing — a
/// `None` read as a zero is how a wrong reading passes for a true one.
fn num(v: &serde_json::Value, path: &[&str]) -> i64 {
    let mut cur = v;
    for key in path {
        cur = cur
            .get(key)
            .unwrap_or_else(|| panic!("no {} in {v:#}", path.join(".")));
    }
    cur.as_i64()
        .unwrap_or_else(|| panic!("{} is not a number: {cur}", path.join(".")))
}

#[test]
fn the_backfilled_series_matches_a_hand_counted_history() {
    let fx = Fixture::new("series");
    let out = fx.run(&["series"]);

    let landings = out["landings"].as_array().expect("landings array").clone();
    assert_eq!(
        landings.len(),
        5,
        "five first-parent landings, got {landings:#?}"
    );
    assert_eq!(
        out["backfill"], true,
        "no --since is a backfill: {out:#}, and the series must say so"
    );

    // Newest first, the order `git log` answers in.
    let subjects: Vec<&str> = landings
        .iter()
        .map(|l| l["subject"].as_str().expect("subject"))
        .collect();
    assert_eq!(
        subjects,
        vec![
            "prod delete only",
            "mixed",
            "deletes only",
            "adds only",
            "seed"
        ]
    );

    let by_subject = |s: &str| -> serde_json::Value {
        landings
            .iter()
            .find(|l| l["subject"] == s)
            .unwrap_or_else(|| panic!("no landing {s}"))
            .clone()
    };

    // A landing that ONLY ADDS.
    let adds_only = by_subject("adds only");
    assert_eq!(num(&adds_only, &["add"]), 5);
    assert_eq!(num(&adds_only, &["del"]), 0);
    assert_eq!(
        num(&adds_only, &["test_add"]),
        5,
        "crates/**/tests/*.rs is test code"
    );

    // A landing that ONLY DELETES.
    let dels_only = by_subject("deletes only");
    assert_eq!(num(&dels_only, &["add"]), 0);
    assert_eq!(num(&dels_only, &["del"]), 2);
    assert_eq!(num(&dels_only, &["test_del"]), 0);

    // Production and a web test in one landing, split by path.
    let mixed = by_subject("mixed");
    assert_eq!(num(&mixed, &["add"]), 10);
    assert_eq!(num(&mixed, &["del"]), 0);
    assert_eq!(num(&mixed, &["test_add"]), 6, "*.test.ts is test code");

    // The root commit has no parent and is still a landing.
    let seed = by_subject("seed");
    assert_eq!(num(&seed, &["add"]), 13, "the root commit's whole tree");
    assert_eq!(num(&seed, &["del"]), 0);

    // Every row carries a UTC instant, which is what makes the series a
    // series rather than a bag of numbers.
    for l in &landings {
        let at = l["at"].as_str().expect("at");
        assert!(
            at.len() == 20 && at.ends_with('Z') && at.as_bytes()[4] == b'-',
            "at is not a UTC ISO instant: {at}"
        );
    }

    // The window, hand-counted: 13 + 5 + 0 + 10 + 0 adds, 0 + 0 + 2 + 0 + 4 dels.
    assert_eq!(num(&out, &["window", "adds"]), 28);
    assert_eq!(num(&out, &["window", "dels"]), 6);
    assert_eq!(num(&out, &["window", "net"]), 22);
    assert_eq!(num(&out, &["window", "landings"]), 5);
    // 6/28 = 21.4%. The headline ratio the founding ideas commit to
    // (`boss-codebase-shrinks`: deletion is a goal).
    assert_eq!(num(&out, &["window", "delete_add_pct"]), 21);

    // And the per-bucket split of the same window.
    assert_eq!(num(&out, &["window", "by_bucket", "rust_prod", "add"]), 15);
    assert_eq!(num(&out, &["window", "by_bucket", "rust_prod", "del"]), 4);
    assert_eq!(num(&out, &["window", "by_bucket", "rust_test", "add"]), 5);
    assert_eq!(num(&out, &["window", "by_bucket", "web_test", "add"]), 6);
    assert_eq!(num(&out, &["window", "by_bucket", "docs", "del"]), 2);
}

#[test]
fn a_window_with_no_additions_reports_no_ratio_rather_than_zero() {
    let fx = Fixture::new("no-adds");
    // The last landing deletes four production lines and adds nothing.
    let out = fx.run(&["series", "--since", &fx.sha("mixed")]);

    assert_eq!(num(&out, &["window", "landings"]), 1);
    assert_eq!(num(&out, &["window", "adds"]), 0);
    assert_eq!(num(&out, &["window", "dels"]), 4);
    assert_eq!(num(&out, &["window", "net"]), -4);
    assert!(
        out["window"]["delete_add_pct"].is_null(),
        "a window with no additions has no delete:add ratio — reporting 0 \
         would read as 'nothing was deleted', the opposite of the truth: {out:#}"
    );
}

#[test]
fn since_a_sha_measures_only_what_landed_after_it() {
    let fx = Fixture::new("since");
    let out = fx.run(&["series", "--since", &fx.sha("adds only")]);

    let subjects: Vec<&str> = out["landings"]
        .as_array()
        .expect("landings")
        .iter()
        .map(|l| l["subject"].as_str().expect("subject"))
        .collect();
    assert_eq!(subjects, vec!["prod delete only", "mixed", "deletes only"]);
    assert_eq!(out["backfill"], false, "a --since run is not a backfill");
    assert_eq!(num(&out, &["window", "adds"]), 10);
    assert_eq!(num(&out, &["window", "dels"]), 6);
}

#[test]
fn the_snapshot_counts_inline_cfg_test_modules_as_test_code() {
    let fx = Fixture::new("snapshot");
    let out = fx.run(&["snapshot"]);

    // lib.rs is 11 lines: 4 production, then the attribute and the
    // 6-line module it governs.
    assert_eq!(
        num(&out, &["totals", "by_bucket", "rust_prod"]),
        4,
        "a path-only bucket would say 11 here, and 11 is the number that \
         makes this repo look like 250k lines of production Rust: {out:#}"
    );
    assert_eq!(num(&out, &["totals", "by_bucket", "rust_test_inline"]), 7);
    assert_eq!(num(&out, &["totals", "by_bucket", "rust_test"]), 5);
    assert_eq!(num(&out, &["totals", "by_bucket", "web_test"]), 6);
    assert_eq!(num(&out, &["totals", "lines"]), 22);

    // The ratio that answers "how much of this is tests".
    assert_eq!(num(&out, &["totals", "prod_lines"]), 4);
    assert_eq!(num(&out, &["totals", "test_lines"]), 18);

    // And the method is on the record beside the numbers, because an
    // approximation whose limits are unwritten is indistinguishable from
    // a precise number that is wrong.
    assert!(
        out["method"]["inline_test_attribution"]
            .as_str()
            .unwrap_or_default()
            .contains("snapshot"),
        "the snapshot must state how it attributed inline test code: {out:#}"
    );
}

#[test]
fn the_row_carries_the_two_headline_ratios_and_says_what_it_could_not_count() {
    let fx = Fixture::new("row");
    let out = fx.run(&["row"]);

    // Headline 1: delete:add (boss-codebase-shrinks).
    assert_eq!(num(&out, &["measured", "window", "delete_add_pct"]), 21);
    // Headline 2: the registry half of CLAUDE.md §9 is counted exactly...
    assert!(
        out["measured"]["registry"]["rows"].is_i64(),
        "registry rows are countable and must be counted: {out:#}"
    );
    // ...and so is the code-branch half, by `boss-leaked-policy` over an
    // AST. THIS FIXTURE HAS NO REGISTRY IN IT, so the counter refuses
    // rather than reporting that a tree with no registries has no leaked
    // branches — and the row carries the refusal in words. Gate 2 of the
    // classification rule is "an arm literal is a registry-declared
    // kind", so with an empty vocabulary every site would classify as if
    // BOSS had no registries, and a confident 0 is exactly the shape of a
    // query against the wrong deployment answering `total: 0`.
    assert!(
        out["measured"]["registry"]["code_branches_on_kind"].is_null(),
        "a tree with no registry seeds has no kind vocabulary to measure \
         against, so there is no number to report: {out:#}"
    );
    let why = out["measured"]["registry"]["code_branches_not_counted_why"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        why.len() > 40,
        "an honest 'not measured' has to say why: {out:#}"
    );
    assert!(
        why.contains("registry kinds") || why.contains("CANNOT ANSWER") || why.contains("refused"),
        "the reason must name the REFUSAL it came from, not a generic blank — \
         a reader has to be able to tell 'this box could not look' from \
         'this codebase has none': {why}"
    );

    // The row is one object carrying both halves plus the series, so a
    // reader queries `metadata.measured` and nothing else.
    assert!(out["measured"]["totals"]["lines"].is_i64());
    assert_eq!(out["landings"].as_array().expect("landings").len(), 5);
    assert!(
        out["measured"]["head"].as_str().unwrap_or_default().len() == 40,
        "the row names the commit it measured: {out:#}"
    );
}

#[test]
fn a_repo_it_cannot_read_refuses_rather_than_reporting_an_empty_series() {
    let out = Command::new("bash")
        .arg(script())
        .args(["series", "--repo", "/nonexistent-codebase-metrics-repo"])
        .output()
        .expect("spawn codebase-metrics.sh");

    assert_eq!(
        out.status.code(),
        Some(CANNOT_ANSWER),
        "a repo git cannot read must exit {CANNOT_ANSWER}, never 0 with an empty \
         series — a zero-landing day and a machine that could not look are \
         different facts, and only one of them is about the codebase.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("CANNOT ANSWER"),
        "the refusal must carry the marker every other git-reading check uses: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// THE CODE-BRANCH HALF of CLAUDE.md §9 — the field this script carried as
// `null` until `boss-leaked-policy` existed.
//
// The row has to distinguish THREE states that a single nullable integer
// cannot: counted (here is the number), this machine has no counter, and
// the counter refused. The first two are tested here; the third is tested
// above, where a fixture with no registry seeds makes the counter refuse
// for a real reason rather than a simulated one.
// ---------------------------------------------------------------------------

#[test]
fn the_snapshot_counts_the_code_branches_and_names_where_they_are() {
    let fx = Fixture::with_a_leaked_branch("code-branches");
    let out = fx.run(&["snapshot"]);
    let reg = &out["registry"];

    assert_eq!(
        num(reg, &["code_branches_on_kind"]),
        1,
        "one `match step_kind` over a kind the fixture's step_types.toml \
         declares: {out:#}"
    );
    assert_eq!(
        num(reg, &["code_branches_unclassified"]),
        0,
        "a count that silently guesses is the thing this pass exists to \
         avoid, so the undecided ones are their own number: {out:#}"
    );
    assert!(
        reg["code_branches_not_counted_why"].is_null(),
        "there IS a number, so there is nothing left to excuse: {out:#}"
    );

    // The audit trail. An integer nobody can go and check by hand is an
    // integer that gets quoted and never verified, which is the whole
    // objection the `null` was recorded under in the first place.
    let sites = reg["code_branches_sites"]["leaked_policy"]
        .as_array()
        .unwrap_or_else(|| panic!("no leaked_policy sites array: {out:#}"));
    assert_eq!(sites.len(), 1, "{sites:#?}");
    assert_eq!(sites[0]["file"], "crates/core/boss-x/src/leak.rs");
    assert_eq!(sites[0]["scrutinee"], "step_kind");
    assert_eq!(
        sites[0]["registry_literals"],
        serde_json::json!(["sign-off"]),
        "the site names WHICH registry kind it branched on"
    );

    // And the rule that produced the number rides on the same row as the
    // number — an approximation whose limits are unwritten is
    // indistinguishable from a precise number that is wrong.
    let method = reg["code_branches_method"].as_str().unwrap_or_default();
    assert!(
        method.contains("gate") || method.contains("scrutinee"),
        "the row must state the classification rule, not just its output: {method}"
    );

    // The scope is on the row too: widening it would move the number for
    // a reason that is not a change in the codebase.
    assert_eq!(
        reg["code_branches_scanned"]["scopes"],
        serde_json::json!(["crates/core"])
    );
    assert!(num(reg, &["code_branches_scanned", "vocabulary_kinds"]) >= 1);
}

#[test]
fn a_machine_with_no_counter_says_so_instead_of_reporting_zero() {
    // boss-gcp's converge deliberately does not build, so the counter is
    // present on a box only once somebody built it there. That box must
    // report "unmeasured here" — never 0 leaked branches, which is the
    // same defect as a query against the wrong deployment answering
    // `total: 0`: well-formed, confident and wrong.
    let fx = Fixture::with_a_leaked_branch("no-counter");
    let absent = fx.dir.join("no-such-counter");
    let out = Command::new("bash")
        .arg(script())
        .args(["snapshot", "--repo"])
        .arg(&fx.dir)
        .env("BOSS_TRUNK_REF", "main")
        .env("BOSS_LEAKED_POLICY_BIN", &absent)
        .output()
        .expect("spawn codebase-metrics.sh");
    assert!(
        out.status.success(),
        "a missing counter costs one field, never the whole row — exited {:?}:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let row: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("the snapshot is still JSON");
    let reg = &row["registry"];

    assert!(
        reg["code_branches_on_kind"].is_null(),
        "this fixture HAS a leaked branch; reporting 0 because the counter \
         was missing would be a measurement of the machine: {row:#}"
    );
    let why = reg["code_branches_not_counted_why"]
        .as_str()
        .unwrap_or_default();
    assert!(
        why.contains(absent.to_string_lossy().as_ref()),
        "the reason must NAME the path it could not run, so the fix is one \
         command away rather than a hunt: {why}"
    );
    // The rest of the row is untouched — the registry half is still
    // counted exactly, which is the point of keeping the two independent.
    assert!(num(reg, &["rows"]) >= 1, "{row:#}");
}

// ---------------------------------------------------------------------------
// THE FILING RUN — the half that actually runs at 05:10, against a stub
// system of record.
//
// Everything above measures. This writes, and it is the only part whose
// failure is invisible: a measurement that computes perfectly and files
// nowhere leaves a series that simply stops, and a flat trend reads as a
// calm fortnight. It also carries the one piece of state the cadence has
// — where the window starts — read off the newest CLOSED packet, so a bug
// here silently re-measures the wrong span every night.
//
// Skips rather than fails when python3 or curl is absent, so a machine
// without them does not manufacture a red.
// ---------------------------------------------------------------------------

const PACKET: &str = "11111111-2222-4333-8444-555555555555";

/// A stub jobs API: two GETs and one PATCH, with every PATCH body logged.
/// argv: port, log path, the `measured.head` the closed packet carries,
/// the readiness marker, the open packet's id.
const STUB: &str = r#"
import http.server, json, sys, pathlib
port = int(sys.argv[1]); log = sys.argv[2]; last_head = sys.argv[3]; bound = sys.argv[4]

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _reply(self, code, body=b'{}'):
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        if "status=open" in self.path:
            body = {"data": [{"id": sys.argv[5], "status": "open", "metadata": {}}]}
        else:
            body = {"data": [
                {"id": "yesterday", "status": "closed",
                 "created_at": "2026-09-10T05:10:00Z",
                 "metadata": {"measured": {"head": last_head}}},
                {"id": "day-before", "status": "closed",
                 "created_at": "2026-09-09T05:10:00Z",
                 "metadata": {"measured": {"head": "0" * 40}}},
            ]}
        self._reply(200, json.dumps(body).encode())
    def do_PATCH(self):
        n = int(self.headers.get("content-length", "0"))
        with open(log, "a") as f:
            f.write(self.path + "\n" + self.rfile.read(n).decode() + "\n")
        self._reply(200)

srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), H)
pathlib.Path(bound).write_text("bound\n")
srv.serve_forever()
"#;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn the_filing_run_patches_the_open_packet_and_starts_at_the_last_filed_head() {
    if !have("python3") || !have("curl") {
        eprintln!("skipping: python3 or curl absent — not manufacturing a red");
        return;
    }
    let fx = Fixture::new("file");
    let last_head = fx.sha("adds only");

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let stub_py = fx.dir.join("stub.py");
    let log = fx.dir.join("patches.log");
    let bound = fx.dir.join("bound");
    boss_testing::write_file(&stub_py, STUB);

    let mut child = Command::new("python3")
        .arg(&stub_py)
        .arg(port.to_string())
        .arg(&log)
        .arg(&last_head)
        .arg(&bound)
        .arg(PACKET)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("python3 runs");

    // READINESS IS A FACT THE STUB STATES. A successful connect can happen
    // with nothing listening (a loopback self-connect on the same ephemeral
    // port), and walking into a dark stub turns a fixture fault into "the
    // measurement was never filed".
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !bound.exists() {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("the stub exited early ({status}); its stderr is above");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the stub never bound {}",
            bound.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let out = Command::new("bash")
        .arg(script())
        .args(["file", "--repo"])
        .arg(&fx.dir)
        .env("BOSS_TRUNK_REF", "main")
        .env("BOSS_JOBS_URL", format!("http://127.0.0.1:{port}"))
        // Pinned for the same reason `try_run` pins it: the script's
        // resolution walk reads $CARGO_TARGET_DIR, which is set on this
        // pod and unset in the gate, so letting it search would make the
        // filed row's code-branch half differ by machine.
        .env(
            "BOSS_LEAKED_POLICY_BIN",
            env!("CARGO_BIN_EXE_boss-leaked-policy"),
        )
        .output()
        .expect("spawn codebase-metrics.sh file");
    let _ = child.kill();
    let _ = child.wait();

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "the filing run exited {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    let (path, body) = logged
        .split_once('\n')
        .unwrap_or_else(|| panic!("no PATCH reached the stub; stdout was:\n{stdout}"));
    assert_eq!(
        path,
        format!("/api/jobs/{PACKET}/metadata"),
        "the row must be MERGED onto the open packet's metadata, never written \
         through the full job PUT, which replaces the envelope"
    );

    let patch: serde_json::Value = serde_json::from_str(body).expect("the patch body is JSON");
    assert_eq!(
        patch["measured"]["since"], last_head,
        "the window must start at the newest CLOSED packet's measured head — not \
         the open one it is about to write, and not the oldest: {patch:#}"
    );
    assert_eq!(num(&patch, &["measured", "window", "landings"]), 3);
    assert_eq!(patch["landings"].as_array().expect("landings").len(), 3);
    assert_eq!(patch["measured"]["backfill"], false);

    // THE JOURNAL GETS THE HEADLINE, not a digest of it: a reduction before
    // the record is stored throws away the only copy, and this is what
    // `systemctl status` shows whoever is standing in front of it.
    for want in ["delete:add", "registry rows", "filed on"] {
        assert!(
            stdout.contains(want),
            "the run's own output does not mention {want:?}:\n{stdout}"
        );
    }
}
