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
//! What each case pins:
//!
//!   * THE DECISION IS THE SWEEP'S. A branch is deleted iff a closed,
//!     `merged` ship-a-change car in the archive names it (its own
//!     branch with `boarded_head`, or a `rerail_origins` entry with its
//!     head) AND the forge's current head equals the recorded head.
//!     Moved = kept and named; claimed with no head = kept and named;
//!     unclaimed = kept and named (a human's decision); gone = counted,
//!     nothing to do.
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
/// `forgejo` remote is that forge, the stub kubectl, the Secret's value
/// and the archive's answer in files.
struct Case {
    bin: PathBuf,
    answers: PathBuf,
    secret: PathBuf,
    log: PathBuf,
    forge: PathBuf,
    tree: PathBuf,
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
        let out = Command::new("bash")
            .arg("-c")
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
            .env("BOSS_SWEEP_TREE", &self.tree)
            .output()
            .expect("sweep-archive-branches.sh runs");
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
        "sweep-archive-branches: --dry-run archive=boss recorded=6 planned=2 deleted=0 moved=1 unrecorded=2 gone=2",
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
    assert_eq!(
        names("gone"),
        ["feat/gone-already", "feat/rerail-original-rerail"]
    );
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
        !text.contains("feat/gone-already\n") || text.contains("\"gone\""),
        "a gone branch earns no per-branch line"
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
        "sweep-archive-branches: --for-real archive=boss recorded=6 planned=2 deleted=2 moved=1 unrecorded=2 gone=2",
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
        "sweep-archive-branches: --for-real archive=boss recorded=1 planned=0 deleted=0 moved=0 unrecorded=5 gone=0 unsafe=1",
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
