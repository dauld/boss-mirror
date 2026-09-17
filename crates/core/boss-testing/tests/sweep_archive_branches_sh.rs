//! `infra/forge/sweep-archive-branches.sh` is RUN, not read — against a
//! stubbed `kubectl` (the Secret and the archive's one psql answer are
//! files) and a REAL bare git repository standing in for the forge (the
//! fixture checkout's `forgejo` remote), so a delete is a delete the
//! read-back can observe and a dry run's "pushed nothing" is measured
//! on the remote itself. Nothing here touches a cluster or a database.
//!
//! WHY THE VERB EXISTS (backlog dfd83788, measured 2026-09-17 06:30Z):
//! `boss orient` listed 50 forge heads no packet claims. The arrival
//! sweep deletes a landed car's branch on the JOB RECORD's evidence
//! (train.rs `deletable_branches` + `sweep_guard`), and every car that
//! landed before the 2026-09-16 23:55Z database switch lives in the
//! ARCHIVE database, not the system of record — so the sweep can never
//! see them and the 50 are permanent. This verb applies the sweep's
//! own rule to the archive's records, through the read-only psql door
//! tenant-census uses and the checkout's forgejo remote.
//!
//! THE SECOND SOURCE (backlog 2c10a25d, measured 2026-09-17 08:00Z):
//! the first dry run (ops-request 54547b33) planned 6 of 50 orphans —
//! the cars of trains #384–#411 closed inside the converge hold and
//! after the database switch, so no database recorded their landing.
//! The forge did: `refs/pull/N/head` is the assembled consist of train
//! PR N, main carries `… (#N)` when N merged, and a branch head that is
//! an ANCESTOR of a merged PR's head landed with it (37 of the 50, by
//! `git merge-base --is-ancestor`). The same run showed two defects:
//! the record named all 1,014 GONE branches (184 KB, cut at the
//! runner's 102,400-byte cap, taking the plan off the packet), and the
//! `unrecorded` set held the live system of record's own open cars.
//!
//! What each case pins:
//!
//!   * THE DECISION IS THE SWEEP'S. A branch is deleted iff a closed,
//!     `merged` ship-a-change car in the archive names it (its own
//!     branch with `boarded_head`, or a `rerail_origins` entry with its
//!     head) AND the forge's current head equals the recorded head.
//!     Moved = kept and named; claimed with no head = kept and named;
//!     unclaimed = kept and named (a human's decision); gone = counted,
//!     nothing to do.
//!   * PULL-REQUEST ANCESTRY IS THE SECOND EVIDENCE. A head the archive
//!     did not plan is planned with `evidence: "pull-request", pr: N`
//!     when it is an ancestor of `refs/pull/N/head` for a MERGED N (a
//!     `(#N)` subject on the forge's main), newest N first; a head only
//!     in unmerged PRs is not evidence; the delete is leased to the
//!     head that proved ancestry, so a branch that moves between the
//!     read and the push is refused by the forge and recorded `failed`.
//!   * GONE IS A COUNT. The record carries no `branches.gone` list, and
//!     a record with 1,000 gone branches and 50 heads stays under the
//!     runner's 100,000-byte cap.
//!   * A LIVE CAR'S BRANCH IS NAMED `live`, never unrecorded and never
//!     planned — even when PR ancestry would call it landed. The live
//!     record is read through `boss-sor-read` as a read-scoped actor;
//!     a reader that is absent, refuses, or answers a cut listing is a
//!     REFUSAL (unreadable live record = cannot tell live from stale).
//!   * THE ARCHIVE MUST NOT BE THE LIVE DATABASE: the Secret's
//!     database-url is parsed in a variable, the name compared, and THE
//!     PASSWORD IS NEVER PRINTED on any path.
//!   * THE VERDICT LINE IS FIRST — before the record and the per-branch
//!     lines — because the ops-runner keeps only the first 100 KB.
//!   * THE DRY RUN PUSHES NOTHING; the real run deletes exactly the
//!     planned set, reads each ref back, and records it.
//!   * THE REMOTE'S URL NEVER REACHES THE OUTPUT, even when git fails.
//!   * THE VERB FILE serves the forge, is MUTATING, and takes
//!     mode/namespace/archive_db with the switch verb's patterns.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/forge/sweep-archive-branches.sh";
const PASSWORD: &str = "s3cretpw0123";
const LIVE_URL: &str =
    "postgres://boss:s3cretpw0123@postgres.boss.svc.cluster.local:5432/algedonic";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "f@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "f@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One fixture: a bare "forge" with a set of branches, a checkout whose
/// `forgejo` remote is that forge, the stub kubectl, the Secret's value,
/// the archive's answer and the live system of record's answer in
/// files, and a stub `boss-sor-read` that prints the latter.
struct Case {
    bin: PathBuf,
    answers: PathBuf,
    secret: PathBuf,
    log: PathBuf,
    forge: PathBuf,
    tree: PathBuf,
    /// The working clone the fixture's branches were made in; a test
    /// that needs a pull-request ref or a moved branch pushes from here.
    seed: PathBuf,
    /// What the stub `boss-sor-read` prints for the open-cars listing.
    live: PathBuf,
    /// branch -> the head the forge holds for it
    heads: Vec<(String, String)>,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("sweep-archive-branches-{name}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let answers = root.join("answers");
        std::fs::create_dir_all(&answers).unwrap();
        let secret = root.join("secret.url");
        write_file(&secret, LIVE_URL);
        let log = root.join("argv.log");
        // The live system of record, empty unless a test says otherwise:
        // the listing shape /api/jobs answers (data, total, limit, offset).
        let live = root.join("live.json");
        write_file(
            &live,
            r#"{"data":[],"total":0,"limit":200,"offset":0}
"#,
        );
        // The stub reader: logs its one argument and the identity it was
        // handed, then prints the file — or refuses the way the real one
        // does when it is told to.
        write_exec(
            &bin.join("boss-sor-read"),
            r#"#!/usr/bin/env bash
set -u
{ printf 'boss-sor-read\n%s\nBOSS_SOR_USER=%s\n' "$*" "${BOSS_SOR_USER:-}"; echo '=== call ==='; } >> "$STUB_LOG"
if [ -n "${STUB_LIVE_FAIL:-}" ]; then
    echo "boss-sor-read: REFUSED — ${STUB_LIVE_FAIL}" >&2
    exit 2
fi
cat "$STUB_LIVE"
"#,
        );

        // The forge: a bare repository. The checkout: a clone of it whose
        // remote is named `forgejo`, the way the converged checkout's is.
        let forge = root.join("forge.git");
        git(&root, &["init", "-q", "--bare", "-b", "main", "forge.git"]);
        let seed = root.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q", "-b", "main"]);
        write_file(&seed.join("README"), "fixture\n");
        git(&seed, &["add", "."]);
        git(
            &seed,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "main"],
        );
        git(
            &seed,
            &["remote", "add", "forgejo", forge.to_str().unwrap()],
        );
        git(&seed, &["push", "-q", "forgejo", "main"]);
        // Each branch is one commit off main, so every head differs.
        let mut heads = Vec::new();
        for b in [
            "feat/landed-at-head",
            "feat/moved-after-boarding",
            "feat/claimed-without-a-head",
            "feat/nobody-claims",
            "feat/rerail-original",
        ] {
            git(&seed, &["checkout", "-q", "-b", b, "main"]);
            write_file(&seed.join(format!("{}.txt", b.replace('/', "-"))), b);
            git(&seed, &["add", "."]);
            git(
                &seed,
                &["-c", "commit.gpgsign=false", "commit", "-q", "-m", b],
            );
            heads.push((b.to_string(), git(&seed, &["rev-parse", "HEAD"])));
            git(&seed, &["push", "-q", "forgejo", b]);
        }
        let tree = root.join("tree");
        git(
            &root,
            &[
                "clone",
                "-q",
                "-o",
                "forgejo",
                forge.to_str().unwrap(),
                "tree",
            ],
        );

        let head = |b: &str| -> String {
            heads
                .iter()
                .find(|(n, _)| n == b)
                .map(|(_, h)| h.clone())
                .unwrap()
        };
        // The archive's answer: the rows the one query returns, as the
        // json_agg the script asks for. Five cars:
        //   - landed-at-head: boarded_head == the forge's head -> delete
        //   - moved-after-boarding: boarded_head is an older sha -> moved
        //   - claimed-without-a-head: no boarded_head -> kept, named
        //   - gone-already: not on the forge -> gone
        //   - a car re-railed OFF feat/rerail-original at its head -> delete
        let moved_recorded = git(&seed, &["rev-parse", "main"]);
        let archive = serde_json::json!([
            {"id": "11111111-aaaa-4aaa-8aaa-111111111111", "branch": "feat/landed-at-head",
             "boarded_head": head("feat/landed-at-head"), "rerail_origins": []},
            {"id": "22222222-aaaa-4aaa-8aaa-222222222222", "branch": "feat/moved-after-boarding",
             "boarded_head": moved_recorded, "rerail_origins": []},
            {"id": "33333333-aaaa-4aaa-8aaa-333333333333", "branch": "feat/claimed-without-a-head",
             "boarded_head": null, "rerail_origins": []},
            {"id": "44444444-aaaa-4aaa-8aaa-444444444444", "branch": "feat/gone-already",
             "boarded_head": "0123456789abcdef0123456789abcdef01234567", "rerail_origins": []},
            {"id": "55555555-aaaa-4aaa-8aaa-555555555555", "branch": "feat/rerail-original-rerail",
             "boarded_head": "fedcba9876543210fedcba9876543210fedcba98",
             "rerail_origins": [{"branch": "feat/rerail-original", "head": head("feat/rerail-original")}]},
        ]);
        write_file(
            &answers.join("merged_cars"),
            &format!("{}\n", serde_json::to_string(&archive).unwrap()),
        );

        // The stub kubectl: every argv is logged; `get secret` prints the
        // file's URL base64-encoded the way the API does; `exec …
        // psql` answers by the `-- sweep-archive-branches:<tag>` on its
        // last -c, and records which database it was asked against.
        write_exec(
            &bin.join("kubectl"),
            r#"#!/usr/bin/env bash
set -u
{ printf '%s\n' "$@"; echo '=== call ==='; } >> "$STUB_LOG"
words=("$@")
if [ "${words[2]:-}" = get ] && [ "${words[3]:-}" = secret ]; then
    [ -n "${STUB_SECRET_UNREADABLE:-}" ] && { echo 'Error from server (Forbidden): secrets "boss-secrets" is forbidden' >&2; exit 1; }
    printf '%s' "$(cat "$STUB_SECRET")" | base64 -w0
    exit 0
fi
if [ "${words[2]:-}" = exec ] && [ "${words[3]:-}" = sts/postgres ] && [ "${words[7]:-}" = psql ]; then
    [ -n "${STUB_PSQL_FAIL:-}" ] && { echo "psql: error: connection to server failed: FATAL: password authentication failed for user \"boss\" (${STUB_PSQL_FAIL})" >&2; exit 2; }
    last="${words[${#words[@]}-1]}"
    tag=$(printf '%s' "$last" | sed -n 's/^-- sweep-archive-branches:\([a-z_]*\).*/\1/p' | head -1)
    [ -n "$tag" ] || { echo "stub kubectl: no sweep tag in the last argv word" >&2; exit 1; }
    [ -f "$STUB_ANSWERS/$tag" ] || { echo "stub kubectl: ERROR:  no answer for $tag" >&2; exit 1; }
    cat "$STUB_ANSWERS/$tag"; exit 0
fi
echo "stub kubectl: unexpected argv: $*" >&2
exit 1
"#,
        );
        Case {
            bin,
            answers,
            secret,
            log,
            forge,
            tree,
            seed,
            live,
            heads,
        }
    }

    fn head(&self, b: &str) -> &str {
        self.heads
            .iter()
            .find(|(n, _)| n == b)
            .map(|(_, h)| h.as_str())
            .unwrap()
    }

    /// A pull request on the forge: `refs/pull/<n>/head` is the
    /// assembled consist — one commit on top of `base`, the way a train
    /// branch sits on top of the cars it carries — so `base`'s head is
    /// an ancestor of it. `merged` adds the squash commit to the forge's
    /// main, titled the way the conductor titles one (`… (#n)`), which
    /// is the only place a merge is recorded. Returns the PR head.
    fn pull_request(&self, n: u32, base: &str, merged: bool) -> String {
        let seed = &self.seed;
        git(seed, &["checkout", "-q", "--detach", base]);
        write_file(&seed.join(format!("consist-{n}.txt")), "consist");
        git(seed, &["add", "."]);
        git(
            seed,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                &format!("train: consist {n}"),
            ],
        );
        let head = git(seed, &["rev-parse", "HEAD"]);
        git(
            seed,
            &["push", "-q", "forgejo", &format!("HEAD:refs/pull/{n}/head")],
        );
        if merged {
            git(seed, &["checkout", "-q", "main"]);
            write_file(&seed.join(format!("squash-{n}.txt")), "squash");
            git(seed, &["add", "."]);
            git(
                seed,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-q",
                    "-m",
                    &format!("train: 2026-09-17 x ({n} changes) (#{n})"),
                ],
            );
            git(seed, &["push", "-q", "forgejo", "main"]);
        }
        head
    }

    /// The live system of record's open cars, each naming a branch.
    fn set_live(&self, cars: &[(&str, &str)]) {
        let data: Vec<serde_json::Value> = cars
            .iter()
            .map(|(id, branch)| {
                serde_json::json!({"id": id, "kind": "ship-a-change", "status": "open",
                    "metadata": {"branch": branch}})
            })
            .collect();
        let total = data.len();
        write_file(
            &self.live,
            &format!(
                "{}\n",
                serde_json::json!({"data": data, "total": total, "limit": 200, "offset": 0})
            ),
        );
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_KUBECTL", self.bin.join("kubectl"))
            .env("STUB_LOG", &self.log)
            .env("STUB_SECRET", &self.secret)
            .env("STUB_ANSWERS", &self.answers)
            .env("STUB_LIVE", &self.live)
            .env("BOSS_SWEEP_SOR_READ", self.bin.join("boss-sor-read"))
            .env("BOSS_SWEEP_TREE", &self.tree);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("sweep-archive-branches.sh runs");
        // stdout then stderr, the way the runner captures (`> raw 2>&1`)
        // would interleave them only if both were written in order — so
        // the verdict-first assertion reads stdout and stderr separately.
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// The run's output captured the runner's way: one stream, in the
    /// order the script wrote it.
    fn run_merged(&self, args: &[&str]) -> (i32, String) {
        self.run_merged_env(args, &[])
    }

    fn run_merged_env(&self, args: &[&str], extra: &[(&str, String)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(format!(
                "exec bash '{}' \"$@\" 2>&1",
                repo_root().join(SCRIPT).display()
            ))
            .arg("sweep")
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("BOSS_KUBECTL", self.bin.join("kubectl"))
            .env("STUB_LOG", &self.log)
            .env("STUB_SECRET", &self.secret)
            .env("STUB_ANSWERS", &self.answers)
            .env("STUB_LIVE", &self.live)
            .env("BOSS_SWEEP_SOR_READ", self.bin.join("boss-sor-read"))
            .env("BOSS_SWEEP_TREE", &self.tree);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("sweep-archive-branches.sh runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    }

    /// Every kubectl call, as its argv words.
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .split("=== call ===\n")
            .filter(|c| !c.trim().is_empty())
            .map(|c| c.lines().map(str::to_string).collect())
            .collect()
    }

    /// The forge's heads now, read off the bare repository itself.
    fn forge_heads(&self) -> Vec<String> {
        let out = git(
            &self.forge,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
        );
        out.lines().map(str::to_string).collect()
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

fn no_password(text: &str, what: &str) {
    assert!(
        !text.contains(PASSWORD),
        "{what}: the password reached the output:\n{text}"
    );
    assert!(
        !text.contains("s3cretpw"),
        "{what}: a fragment of the password reached the output:\n{text}"
    );
}

fn record_line(text: &str) -> serde_json::Value {
    let line = text
        .lines()
        .find(|l| l.starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON record line in:\n{text}"));
    serde_json::from_str(line).unwrap_or_else(|e| panic!("record is not JSON ({e}): {line}"))
}

// ---------------------------------------------------------------------------
// The mode and the arguments.
// ---------------------------------------------------------------------------

#[test]
fn refuses_a_missing_or_foreign_mode_namespace_or_name() {
    let c = Case::new("args");
    for (args, needle) in [
        (vec![], "usage"),
        (vec!["--now", "boss", "boss"], "--dry-run and --for-real"),
        (vec!["--dry-run", "kube-system", "boss"], "namespace"),
        (vec!["--dry-run", "boss-dev", "boss"], "boss-dev"),
        (vec!["--dry-run", "boss", "Boss"], "database name"),
        (vec!["--dry-run", "boss", "bo;ss"], "database name"),
        (vec!["--dry-run", "boss"], "usage"),
    ] {
        let (rc, text) = c.run(&args);
        assert_eq!(rc, 2, "{args:?} was not refused:\n{text}");
        assert!(
            text.contains(needle),
            "{args:?}: expected `{needle}` in:\n{text}"
        );
        assert!(
            c.calls().is_empty(),
            "a refused argument set reached kubectl:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The archive must not be the live database.
// ---------------------------------------------------------------------------

/// The Secret names `algedonic`; asking to sweep from `algedonic` is
/// refused before the archive or the forge is read, and the password
/// is nowhere in the refusal.
#[test]
fn refuses_the_live_database_as_the_archive_and_never_prints_the_password() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("live-db");
    for mode in ["--dry-run", "--for-real"] {
        let (rc, text) = c.run(&[mode, "boss", "algedonic"]);
        assert_eq!(rc, 2, "{mode}: the live database was not refused:\n{text}");
        no_password(&text, mode);
        contains_all(
            &text,
            &[
                "REFUSED",
                "boss@postgres.boss.svc.cluster.local:5432/algedonic",
                "live system of record",
                "Nothing was changed",
            ],
            "the live-db refusal",
        );
        assert!(
            !c.calls().iter().any(|w| w.contains(&"psql".to_string())),
            "{mode}: the archive was read before the live-db bound:\n{text}"
        );
        assert_eq!(c.forge_heads().len(), 6, "{mode}: the forge changed");
    }
}

/// A Secret that cannot be read is a bound that cannot be evaluated —
/// a refusal, never a pass.
#[test]
fn an_unreadable_secret_is_a_refusal() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("secret-unreadable");
    let (rc, text) = c.run_env(
        &["--dry-run", "boss", "boss"],
        &[("STUB_SECRET_UNREADABLE", "1".into())],
    );
    assert_eq!(rc, 2, "an unreadable Secret was not a refusal:\n{text}");
    contains_all(&text, &["cannot read Secret", "Forbidden"], "the refusal");
}

// ---------------------------------------------------------------------------
// The dry run: the decision, the verdict first, nothing pushed.
// ---------------------------------------------------------------------------

#[test]
fn dry_run_decides_like_the_sweep_prints_the_verdict_first_and_pushes_nothing() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("dry-run");
    let before = c.forge_heads();
    let (rc, text) = c.run_merged(&["--dry-run", "boss", "boss"]);
    assert_eq!(rc, 0, "the dry run did not exit 0:\n{text}");
    no_password(&text, "dry run");

    // THE VERDICT IS THE FIRST LINE of the merged stream, and it carries
    // every count; the record is the second.
    let first = text.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "sweep-archive-branches: --dry-run archive=boss recorded=6 planned=2 by_archive=2 by_pr=0 deleted=0 moved=1 unrecorded=2 live=0 gone=2",
        "the verdict is not the first line:\n{text}"
    );
    let second = text.lines().nth(1).unwrap_or_default();
    assert!(
        second.starts_with('{'),
        "the record is not the second line:\n{text}"
    );
    let record = record_line(&text);
    assert_eq!(record["verb"], "sweep-archive-branches");
    assert_eq!(record["dry_run"], true);
    assert_eq!(record["archive_db"], "boss");
    assert_eq!(
        record["live_db"],
        "boss@postgres.boss.svc.cluster.local:5432/algedonic"
    );
    assert_eq!(record["cars_read"], 5);
    let names = |key: &str| -> Vec<String> {
        record["branches"][key]
            .as_array()
            .unwrap_or_else(|| panic!("branches.{key} missing in {record}"))
            .iter()
            .map(|b| {
                b.get("branch")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| b.as_str().unwrap())
                    .to_string()
            })
            .collect()
    };
    assert_eq!(
        names("delete"),
        ["feat/landed-at-head", "feat/rerail-original"],
        "the planned set"
    );
    assert_eq!(names("deleted"), Vec::<String>::new());
    assert_eq!(names("moved"), ["feat/moved-after-boarding"]);
    assert_eq!(names("no_head"), ["feat/claimed-without-a-head"]);
    assert_eq!(names("unclaimed"), ["feat/nobody-claims"]);
    assert_eq!(names("live"), Vec::<String>::new());
    // GONE IS A COUNT (backlog 2c10a25d): the two claims whose branches
    // are not on the forge are counted, and no list names them.
    assert_eq!(record["gone"], 2);
    assert!(
        record["branches"].get("gone").is_none(),
        "the record still lists gone branches by name: {record}"
    );
    // Every archive-planned entry says so.
    assert!(
        record["branches"]["delete"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["evidence"] == "archive"),
        "the archive's evidence is not named: {record}"
    );
    assert_eq!(record["by_archive"], 2);
    assert_eq!(record["by_pr"], 0);
    // The moved line names both heads, the way sweep_guard's Moved does.
    let moved = &record["branches"]["moved"][0];
    assert_eq!(moved["current"], c.head("feat/moved-after-boarding"));
    assert_ne!(moved["recorded"], moved["current"]);
    assert_eq!(moved["car"], "22222222-aaaa-4aaa-8aaa-222222222222");
    // The rerail original is deleted on the car's rerail_origins entry,
    // and the record says which car.
    let rerail = &record["branches"]["delete"][1];
    assert_eq!(rerail["car"], "55555555-aaaa-4aaa-8aaa-555555555555");
    assert_eq!(rerail["rerail_origin"], true);

    // The per-branch lines come AFTER, and name what an operator would act on.
    contains_all(
        &text,
        &[
            "would delete feat/landed-at-head",
            "would delete feat/rerail-original",
            "moved feat/moved-after-boarding",
            "no head feat/claimed-without-a-head",
            "unrecorded feat/nobody-claims",
            "kept",
            "Nothing was changed",
        ],
        "the per-branch lines",
    );
    assert!(
        !text.contains("feat/gone-already"),
        "a gone branch is nothing to act on, and its name carries nothing — it is counted, never named:\n{text}"
    );

    // NOTHING WAS PUSHED: the forge holds exactly what it held.
    assert_eq!(c.forge_heads(), before, "the dry run changed the forge");
    // ONE psql round trip, read-only, against the ARCHIVE.
    let calls = c.calls();
    let psql: Vec<&Vec<String>> = calls
        .iter()
        .filter(|w| w.contains(&"psql".to_string()))
        .collect();
    assert_eq!(psql.len(), 1, "the archive was read {} times", psql.len());
    let w = psql[0];
    assert!(
        w.contains(&"SET default_transaction_read_only = on".to_string()),
        "the archive read is not in a read-only session: {w:?}"
    );
    let d = w
        .iter()
        .position(|x| x == "-d")
        .expect("psql names its database");
    assert_eq!(
        w[d + 1],
        "boss",
        "the read went to the wrong database: {w:?}"
    );
    assert_eq!(w[1], "boss", "the exec was not in namespace boss: {w:?}");
}

// ---------------------------------------------------------------------------
// The real run: the planned set goes, each read back; the rest stay.
// ---------------------------------------------------------------------------

#[test]
fn for_real_deletes_exactly_the_planned_set_and_reads_each_back() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("for-real");
    let (rc, text) = c.run_merged(&["--for-real", "boss", "boss"]);
    assert_eq!(rc, 0, "the real run did not exit 0:\n{text}");
    no_password(&text, "real run");
    let first = text.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "sweep-archive-branches: --for-real archive=boss recorded=6 planned=2 by_archive=2 by_pr=0 deleted=2 moved=1 unrecorded=2 live=0 gone=2",
        "the verdict is not the first line:\n{text}"
    );
    let record = record_line(&text);
    assert_eq!(record["dry_run"], false);
    assert_eq!(record["deleted"], 2);
    let deleted: Vec<&str> = record["branches"]["deleted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["branch"].as_str().unwrap())
        .collect();
    assert_eq!(deleted, ["feat/landed-at-head", "feat/rerail-original"]);
    assert!(
        record["branches"]["deleted"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["read_back"] == "absent"),
        "every deletion is read back: {record}"
    );
    contains_all(
        &text,
        &[
            "deleted feat/landed-at-head",
            "deleted feat/rerail-original",
            "moved feat/moved-after-boarding",
        ],
        "the per-branch lines",
    );
    let mut heads = c.forge_heads();
    heads.sort();
    assert_eq!(
        heads,
        [
            "feat/claimed-without-a-head",
            "feat/moved-after-boarding",
            "feat/nobody-claims",
            "main",
        ],
        "the forge holds the wrong set after the sweep"
    );
}

/// A branch name is data the forge handed back, and git admits `;`,
/// `$(`, and a backtick in one (measured: `git check-ref-format
/// --branch 'feat/x;y$(z)'` passes). A deletable branch whose name is
/// outside the shape the verb hands to git is kept and named, and no
/// push is issued for it — the forge still holds it afterwards.
#[test]
fn a_branch_name_outside_the_safe_shape_is_kept_and_never_reaches_git() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("unsafe-name");
    let unsafe_name = "feat/x;y$(z)";
    git(&c.tree, &["checkout", "-q", "-b", unsafe_name, "main"]);
    write_file(&c.tree.join("unsafe.txt"), "x");
    git(&c.tree, &["add", "."]);
    git(
        &c.tree,
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "unsafe"],
    );
    let head = git(&c.tree, &["rev-parse", "HEAD"]);
    git(&c.tree, &["push", "-q", "forgejo", unsafe_name]);
    git(&c.tree, &["checkout", "-q", "main"]);
    write_file(
        &c.answers.join("merged_cars"),
        &format!(
            "{}\n",
            serde_json::json!([{
                "id": "66666666-aaaa-4aaa-8aaa-666666666666",
                "branch": unsafe_name, "boarded_head": head, "rerail_origins": []
            }])
        ),
    );
    let (rc, text) = c.run_merged(&["--for-real", "boss", "boss"]);
    assert_eq!(rc, 0, "the run did not exit 0:\n{text}");
    let first = text.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "sweep-archive-branches: --for-real archive=boss recorded=1 planned=0 by_archive=0 by_pr=0 deleted=0 moved=0 unrecorded=5 live=0 gone=0 unsafe=1",
        "the verdict does not name the unsafe branch:\n{text}"
    );
    contains_all(
        &text,
        &["unsafe name feat/x;y$(z)", "kept"],
        "the per-branch line",
    );
    assert!(
        c.forge_heads().iter().any(|h| h == unsafe_name),
        "the unsafe branch was deleted: {:?}",
        c.forge_heads()
    );
    assert_eq!(c.forge_heads().len(), 7, "the forge changed");
}

// ---------------------------------------------------------------------------
// The forge's URL never reaches the output.
// ---------------------------------------------------------------------------

/// The converged checkout's `forgejo` remote may carry a token in its
/// URL. A failed ls-remote is a refusal that carries git's words — with
/// the URL's userinfo redacted, so the token can never land on a packet.
#[test]
fn an_unreadable_forge_is_a_refusal_that_never_prints_the_remote_url() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("forge-unreadable");
    git(
        &c.tree,
        &[
            "remote",
            "set-url",
            "forgejo",
            "http://david:forge-t0ken-xyz@127.0.0.1:1/david/boss.git",
        ],
    );
    let (rc, text) = c.run(&["--dry-run", "boss", "boss"]);
    assert_eq!(rc, 2, "an unreadable forge was not a refusal:\n{text}");
    assert!(
        !text.contains("forge-t0ken-xyz"),
        "the remote's token reached the output:\n{text}"
    );
    assert!(
        !text.contains("david:"),
        "the remote's userinfo reached the output:\n{text}"
    );
    contains_all(
        &text,
        &["REFUSED", "forgejo", "Nothing was changed"],
        "the refusal",
    );
}

/// The archive that cannot be read is a refusal too — and psql's words
/// ride the refusal with the password scrubbed.
#[test]
fn an_unreadable_archive_is_a_refusal() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("archive-unreadable");
    let (rc, text) = c.run_env(
        &["--dry-run", "boss", "boss"],
        &[("STUB_PSQL_FAIL", PASSWORD.into())],
    );
    assert_eq!(rc, 2, "an unreadable archive was not a refusal:\n{text}");
    no_password(&text, "archive refusal");
    contains_all(
        &text,
        &["REFUSED", "cannot read the archive", "Nothing was changed"],
        "the refusal",
    );
    assert_eq!(c.forge_heads().len(), 6, "the forge changed");
}

// ---------------------------------------------------------------------------
// The second evidence: pull-request ancestry (backlog 2c10a25d).
// ---------------------------------------------------------------------------

/// `feat/nobody-claims` is named by no archive car, but its head is an
/// ancestor of `refs/pull/7/head`, and the forge's main carries
/// `… (#7)`: it landed with train #7 and is planned, evidence
/// `pull-request`, pr 7. The same head is ALSO in the unmerged PR 9 —
/// merged wins. `feat/only-in-open-pr` is in PR 9 alone: not evidence,
/// still unrecorded. Then the real run deletes it, leased to the head
/// that proved ancestry, and reads it back.
#[test]
fn a_head_a_merged_pull_request_contains_is_planned_and_one_only_in_an_open_pr_is_not() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("pr-ancestry");
    git(
        &c.seed,
        &["checkout", "-q", "-b", "feat/only-in-open-pr", "main"],
    );
    write_file(&c.seed.join("open.txt"), "open");
    git(&c.seed, &["add", "."]);
    git(
        &c.seed,
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "open"],
    );
    git(&c.seed, &["push", "-q", "forgejo", "feat/only-in-open-pr"]);
    // PR 9: an OPEN train carrying both branches (never merged).
    git(
        &c.seed,
        &["checkout", "-q", "--detach", "feat/only-in-open-pr"],
    );
    git(
        &c.seed,
        &[
            "-c",
            "commit.gpgsign=false",
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "train: consist 9",
            "feat/nobody-claims",
        ],
    );
    git(&c.seed, &["push", "-q", "forgejo", "HEAD:refs/pull/9/head"]);
    // PR 7: the MERGED train carrying feat/nobody-claims.
    c.pull_request(7, "feat/nobody-claims", true);
    let remotes_before = git(&c.tree, &["for-each-ref", "refs/remotes/"]);

    let (rc, text) = c.run_merged(&["--dry-run", "boss", "boss"]);
    assert_eq!(rc, 0, "the dry run did not exit 0:\n{text}");
    let first = text.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "sweep-archive-branches: --dry-run archive=boss recorded=6 planned=3 by_archive=2 by_pr=1 deleted=0 moved=1 unrecorded=2 live=0 gone=2",
        "the verdict does not carry the pull-request evidence:\n{text}"
    );
    let record = record_line(&text);
    let planned = record["branches"]["delete"].as_array().unwrap();
    let by_pr: Vec<&serde_json::Value> = planned
        .iter()
        .filter(|b| b["evidence"] == "pull-request")
        .collect();
    assert_eq!(
        by_pr.len(),
        1,
        "one head is proved by a pull request: {record}"
    );
    assert_eq!(by_pr[0]["branch"], "feat/nobody-claims");
    assert_eq!(by_pr[0]["pr"], 7, "merged #7 wins over open #9: {record}");
    assert_eq!(
        by_pr[0]["head"],
        c.head("feat/nobody-claims"),
        "the head that proved ancestry is the one the delete is leased to"
    );
    let unclaimed: Vec<&str> = record["branches"]["unclaimed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["branch"].as_str().unwrap())
        .collect();
    assert_eq!(
        unclaimed,
        ["feat/only-in-open-pr"],
        "a head only in an unmerged PR is not evidence"
    );
    contains_all(
        &text,
        &[
            "would delete feat/nobody-claims (landed with pull request #7",
            "unrecorded feat/only-in-open-pr",
        ],
        "the per-branch lines",
    );
    // The pull heads were fetched through the checkout's `forgejo`
    // remote into the verb's own namespace — not into refs/remotes,
    // which is the converge's.
    assert_eq!(record["remote"], "forgejo");
    let ns = git(
        &c.tree,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/sweep-archive-branches/pull/",
        ],
    );
    assert_eq!(
        ns.lines().collect::<Vec<_>>(),
        [
            "refs/sweep-archive-branches/pull/7",
            "refs/sweep-archive-branches/pull/9"
        ],
        "the pull heads are not in the verb's namespace"
    );
    assert_eq!(
        git(&c.tree, &["for-each-ref", "refs/remotes/"]),
        remotes_before,
        "the fetch wrote into refs/remotes"
    );

    let (rc, text) = c.run_merged(&["--for-real", "boss", "boss"]);
    assert_eq!(rc, 0, "the real run did not exit 0:\n{text}");
    let record = record_line(&text);
    assert_eq!(record["deleted"], 3);
    let deleted: Vec<(&str, &str)> = record["branches"]["deleted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["branch"].as_str().unwrap(),
                b["evidence"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        deleted,
        [
            ("feat/landed-at-head", "archive"),
            ("feat/rerail-original", "archive"),
            ("feat/nobody-claims", "pull-request"),
        ]
    );
    contains_all(
        &text,
        &["deleted feat/nobody-claims (landed with pull request #7"],
        "the deleted line",
    );
    let mut heads = c.forge_heads();
    heads.sort();
    assert_eq!(
        heads,
        [
            "feat/claimed-without-a-head",
            "feat/moved-after-boarding",
            "feat/only-in-open-pr",
            "main",
        ]
    );
}

/// THE GUARD AT DELETE TIME. The evidence vouches for one head; the
/// forge is asked to delete the branch ONLY IF it still points there
/// (`--force-with-lease=refs/heads/<b>:<head>`). A stub git moves the
/// branch between the sweep's read and its push — the case car 23923b40
/// paid for — and the forge refuses: the branch survives at its new
/// head, recorded `failed` with the forge's reason, exit 1.
#[test]
fn a_branch_that_moves_between_the_read_and_the_delete_survives() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("lease");
    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    // The stub: real git, except that the FIRST `push … --delete` of
    // feat/landed-at-head is preceded by a commit landing on it.
    let moved_flag = c.bin.join("moved");
    write_exec(
        &c.bin.join("git"),
        &format!(
            r#"#!/usr/bin/env bash
case " $* " in
    *" --delete "*"refs/heads/feat/landed-at-head "*)
        if [ ! -e "{flag}" ]; then
            : > "{flag}"
            d=$(mktemp -d)
            {git} clone -q -o forgejo "{forge}" "$d/w" >/dev/null 2>&1
            {git} -C "$d/w" checkout -q feat/landed-at-head
            echo late > "$d/w/late.txt"
            {git} -C "$d/w" add . && {git} -C "$d/w" -c commit.gpgsign=false -c user.name=f -c user.email=f@example.invalid commit -q -m late
            {git} -C "$d/w" push -q forgejo feat/landed-at-head >/dev/null 2>&1
            rm -rf "$d"
        fi ;;
esac
exec "{git}" "$@"
"#,
            flag = moved_flag.display(),
            git = real_git,
            forge = c.forge.display(),
        ),
    );
    let (rc, text) = c.run_merged(&["--for-real", "boss", "boss"]);
    assert_eq!(rc, 1, "a refused delete is not exit 1:\n{text}");
    let first = text.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "sweep-archive-branches: --for-real archive=boss recorded=6 planned=2 by_archive=2 by_pr=0 deleted=1 moved=1 unrecorded=2 live=0 gone=2 failed=1",
        "the verdict does not carry the failure:\n{text}"
    );
    let record = record_line(&text);
    let failed = &record["branches"]["failed"][0];
    assert_eq!(failed["branch"], "feat/landed-at-head");
    assert!(
        failed["reason"].as_str().unwrap().contains("stale info"),
        "the forge's lease refusal is not the recorded reason: {record}"
    );
    contains_all(&text, &["FAILED feat/landed-at-head"], "the failed line");
    assert!(
        c.forge_heads().iter().any(|h| h == "feat/landed-at-head"),
        "the moved branch was deleted: {:?}",
        c.forge_heads()
    );
    assert!(
        !c.forge_heads().iter().any(|h| h == "feat/rerail-original"),
        "the unmoved planned branch was not deleted: {:?}",
        c.forge_heads()
    );
}

// ---------------------------------------------------------------------------
// Gone is a count (backlog 2c10a25d, defect 1).
// ---------------------------------------------------------------------------

/// Measured: 1,014 gone names made a 184 KB record, and the runner kept
/// 102,400 bytes — the plan was cut off the packet. With 1,000 gone
/// claims and 50 forge heads the record must stay under 100,000 bytes,
/// by construction: gone is a number.
#[test]
fn gone_is_a_count_and_the_record_stays_under_the_runners_cap() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("gone-count");
    // 50 heads: the same commit under 50 more names, one push.
    let mut args: Vec<String> = vec!["push".into(), "-q".into(), "forgejo".into()];
    for i in 0..45 {
        args.push(format!("main:refs/heads/feat/orphan-{i:03}"));
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    git(&c.seed, &argv);
    assert_eq!(c.forge_heads().len(), 51, "50 heads and main");
    // 1,000 cars whose branches are not on the forge.
    let cars: Vec<serde_json::Value> = (0..1000)
        .map(|i| {
            serde_json::json!({
                "id": format!("{i:08x}-aaaa-4aaa-8aaa-{i:012x}"),
                "branch": format!("fix/a-long-branch-name-that-landed-months-ago-and-was-swept-{i:04}"),
                "boarded_head": "0123456789abcdef0123456789abcdef01234567",
                "rerail_origins": []
            })
        })
        .collect();
    write_file(
        &c.answers.join("merged_cars"),
        &format!("{}\n", serde_json::Value::Array(cars)),
    );
    let (rc, text) = c.run_merged(&["--dry-run", "boss", "boss"]);
    assert_eq!(rc, 0, "the dry run did not exit 0:\n{text}");
    let first = text.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "sweep-archive-branches: --dry-run archive=boss recorded=1000 planned=0 by_archive=0 by_pr=0 deleted=0 moved=0 unrecorded=50 live=0 gone=1000"
    );
    let line = text.lines().nth(1).unwrap_or_default();
    assert!(line.starts_with('{'), "the record is the second line");
    assert!(
        line.len() < 100_000,
        "the record is {} bytes — over the runner's cap, the plan would be cut",
        line.len()
    );
    let record = record_line(&text);
    assert_eq!(record["gone"], 1000);
    assert!(record["branches"].get("gone").is_none());
    assert_eq!(
        record["branches"]["unclaimed"].as_array().unwrap().len(),
        50
    );
}

// ---------------------------------------------------------------------------
// Live cars are named, not candidates (backlog 2c10a25d, defect 2).
// ---------------------------------------------------------------------------

/// Two open cars in the live system of record name `feat/landed-at-head`
/// (which the archive would plan) and `feat/nobody-claims` (which PR
/// ancestry would call landed). Both are `live`: named, counted, never
/// unrecorded, never planned — a follow-up car branched from a landed
/// head is live work. The live record is read through the reader as a
/// read-scoped actor, with the exact listing path.
#[test]
fn a_live_cars_branch_is_named_live_and_never_a_candidate() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("live");
    c.pull_request(7, "feat/nobody-claims", true);
    c.set_live(&[
        (
            "77777777-aaaa-4aaa-8aaa-777777777777",
            "feat/landed-at-head",
        ),
        ("88888888-aaaa-4aaa-8aaa-888888888888", "feat/nobody-claims"),
        (
            "99999999-aaaa-4aaa-8aaa-999999999999",
            "feat/not-on-the-forge-yet",
        ),
    ]);
    let (rc, text) = c.run_merged(&["--for-real", "boss", "boss"]);
    assert_eq!(rc, 0, "the real run did not exit 0:\n{text}");
    let first = text.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "sweep-archive-branches: --for-real archive=boss recorded=6 planned=1 by_archive=1 by_pr=0 deleted=1 moved=1 unrecorded=1 live=2 gone=2",
        "the verdict does not name the live cars:\n{text}"
    );
    let record = record_line(&text);
    let live: Vec<(&str, &str)> = record["branches"]["live"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["branch"].as_str().unwrap(), b["car"].as_str().unwrap()))
        .collect();
    assert_eq!(
        live,
        [
            (
                "feat/landed-at-head",
                "77777777-aaaa-4aaa-8aaa-777777777777"
            ),
            ("feat/nobody-claims", "88888888-aaaa-4aaa-8aaa-888888888888"),
        ]
    );
    assert_eq!(record["live"], 2);
    contains_all(
        &text,
        &[
            "live feat/landed-at-head (open car 77777777",
            "live feat/nobody-claims (open car 88888888",
            "deleted feat/rerail-original",
        ],
        "the per-branch lines",
    );
    assert!(
        !text.contains("unrecorded feat/nobody-claims")
            && !text.contains("would delete feat/landed"),
        "a live branch was listed as a candidate:\n{text}"
    );
    let mut heads = c.forge_heads();
    heads.sort();
    assert_eq!(
        heads,
        [
            "feat/claimed-without-a-head",
            "feat/landed-at-head",
            "feat/moved-after-boarding",
            "feat/nobody-claims",
            "main",
        ],
        "a live branch was deleted"
    );
    // The reader: once, the exact listing, as the read-scoped actor.
    let calls = c.calls();
    let reads: Vec<&Vec<String>> = calls
        .iter()
        .filter(|w| w.first().is_some_and(|x| x == "boss-sor-read"))
        .collect();
    assert_eq!(
        reads.len(),
        1,
        "the live record was read {} times",
        reads.len()
    );
    assert_eq!(
        reads[0][1],
        "/api/jobs?kind=ship-a-change&status=open&limit=200"
    );
    assert!(
        reads[0][2].contains("\"role\":\"audit-readonly\""),
        "the live read is not as a read-scoped actor: {:?}",
        reads[0]
    );
}

/// An unreadable live record cannot tell live from stale, so it is a
/// refusal — the reader absent, the reader refusing (the way the real
/// one does with no identity), an answer of the wrong shape, or a
/// listing cut at its limit — and nothing is pushed.
#[test]
fn an_unreadable_live_record_is_a_refusal() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("live-unreadable");
    let before = c.forge_heads();
    // (a) the reader is not there
    let (rc, text) = c.run_env(
        &["--for-real", "boss", "boss"],
        &[(
            "BOSS_SWEEP_SOR_READ",
            c.bin.join("no-such-reader").display().to_string(),
        )],
    );
    assert_eq!(rc, 2, "an absent reader was not a refusal:\n{text}");
    contains_all(
        &text,
        &["REFUSED", "no-such-reader", "live", "Nothing was changed"],
        "the absent-reader refusal",
    );
    // (b) the reader refuses
    let (rc, text) = c.run_env(
        &["--for-real", "boss", "boss"],
        &[(
            "STUB_LIVE_FAIL",
            "BOSS_JOBS_URL is not set, and there is no safe default".into(),
        )],
    );
    assert_eq!(rc, 2, "a refusing reader was not a refusal:\n{text}");
    contains_all(
        &text,
        &["REFUSED", "BOSS_JOBS_URL is not set", "Nothing was changed"],
        "the refusing-reader refusal",
    );
    // (c) not a listing
    write_file(&c.live, "[]\n");
    let (rc, text) = c.run(&["--for-real", "boss", "boss"]);
    assert_eq!(rc, 2, "a wrong-shaped answer was not a refusal:\n{text}");
    contains_all(
        &text,
        &["REFUSED", "Nothing was changed"],
        "the shape refusal",
    );
    // (d) a listing cut at its limit: total says more than was returned
    write_file(
        &c.live,
        r#"{"data":[{"id":"77777777-aaaa-4aaa-8aaa-777777777777","kind":"ship-a-change","status":"open","metadata":{"branch":"feat/landed-at-head"}}],"total":201,"limit":200,"offset":0}
"#,
    );
    let (rc, text) = c.run(&["--for-real", "boss", "boss"]);
    assert_eq!(rc, 2, "a cut listing was not a refusal:\n{text}");
    contains_all(
        &text,
        &["REFUSED", "201", "Nothing was changed"],
        "the cut-listing refusal",
    );
    assert_eq!(c.forge_heads(), before, "a refusal changed the forge");
}

// ---------------------------------------------------------------------------
// The verb file.
// ---------------------------------------------------------------------------

#[test]
fn the_verb_file_is_a_mutating_forge_verb_with_the_three_params() {
    let path = repo_root().join("infra/ops/verbs/sweep-archive-branches.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}", "{2}", "{3}"]));
    let params = v["params"].as_array().unwrap();
    assert_eq!(params.len(), 3);
    assert_eq!(params[0]["name"], "mode");
    assert_eq!(
        params[0]["one_of"],
        serde_json::json!(["--dry-run", "--for-real"])
    );
    assert_eq!(params[1]["name"], "namespace");
    assert_eq!(params[1]["pattern"], "^boss(-[a-z0-9]+)*$");
    assert_eq!(params[2]["name"], "archive_db");
    assert_eq!(params[2]["pattern"], "^[a-z][a-z0-9_]{0,62}$");
    assert_eq!(v["timeout"], 300);
    let about = v["about"].as_str().unwrap();
    assert!(
        about.starts_with("MUTATING"),
        "about opens with MUTATING: {about}"
    );
    assert!(about.contains("David"), "about names who authorized it");
    assert!(about.contains("dfd83788"), "about names the packet");
    assert!(
        about.contains("--dry-run"),
        "about says the dry run is the default way in"
    );
    // The second source and the two defects it was rebuilt for.
    for needle in ["2c10a25d", "refs/pull/", "live", "gone is a COUNT"] {
        assert!(about.contains(needle), "about lacks `{needle}`: {about}");
    }
}

/// `boss orient`'s ORPHANS tail points at this verb, so the operator
/// reading 50 orphans is told the door rather than left to delete by
/// hand.
#[test]
fn orient_points_orphans_at_the_verb() {
    let src =
        std::fs::read_to_string(repo_root().join("crates/orchestrators/boss-cli/src/orient.rs"))
            .unwrap();
    assert!(
        src.contains("boss ops forge sweep-archive-branches --wait -- --dry-run boss <archive-db>"),
        "orient.rs does not name the sweep verb in its ORPHANS section"
    );
}
