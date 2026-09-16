//! `infra/forge/switch-instance-database.sh` is RUN, not read — against
//! a stubbed `kubectl` that records every argv it receives, keeps the
//! Secret's value in a file so a patch is visible to the read-back, and
//! answers the psql / pg_dump / nats reads with canned rows — so every
//! verdict below is one the script actually reached. Nothing here
//! touches a cluster or a database.
//!
//! WHY THE VERB EXISTS (backlog 063dba4e; design e652c7c6, David
//! 2026-09-16 "Let's do Option 3"). Prod restarts from an EMPTY database
//! seeded by the platform bundle and the Algedonic tenant, and the
//! brewery's database becomes an archive. Every service reads
//! BOSS_POSTGRES_URL from Secret boss-secrets key database-url, so a
//! fresh instance is a NEW database in the same postgres and the
//! Secret naming it — four kubectl round trips a person could type,
//! with nothing recording what the old database held, whether the
//! converge was held, or which packet asked. So it is an ops verb on
//! the forge, the retire-second-stack shape: `--dry-run` prints every
//! bound, `--for-real` acts only while the converge is HELD.
//!
//! What each case pins:
//!
//!   * THE PASSWORD NEVER APPEARS IN ANY OUTPUT, on any path — dry run,
//!     real run, every refusal. The Secret's value is parsed in a
//!     variable and every line printed names user@host:port/db only.
//!   * THE REAL RUN REFUSES WITHOUT THE HOLD, and says how to hold; the
//!     dry run reports the missing hold as what the real run would
//!     refuse, and still prints the rest of the plan.
//!   * THE SNAPSHOT COMES BEFORE THE CHANGE: a failed or too-small dump
//!     leaves the Secret and postgres untouched, and says so.
//!   * A TARGET THAT EXISTS WITH TABLES IS REFUSED; absent is created
//!     with `OWNER boss`; present-and-empty is reused, not re-created.
//!   * THE SECRET PATCH HAS THE ARGV SHAPE THE BRIEF NAMED (`patch
//!     secret boss-secrets --type merge -p …`) and its decoded value is
//!     the old URL with only the database segment changed; the
//!     read-back is the verdict.
//!   * THE FLIP IS READ OFF THE REF THE RELEASE WILL BUILD: boss-init's
//!     PGDATABASE literal on main must name the new database, or the
//!     release would converge the schema into the old one.
//!   * THE RECORD carries old/new names, the snapshot path and size,
//!     and the NATS positions (or `unmeasured`, never a fake).
//!   * THE VERB FILE serves the forge, is MUTATING, names David, takes
//!     mode/namespace/new_db, and declares the timeout the brief asked.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/forge/switch-instance-database.sh";
const PASSWORD: &str = "s3cretpw0123";
const OLD_URL: &str = "postgres://boss:s3cretpw0123@postgres.boss.svc.cluster.local:5432/boss";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// A git checkout standing in for the forge's: `infra/cluster/manifests/
/// boss.yaml` carrying boss-init's PGDATABASE literal and
/// `infra/cluster/instances.toml` naming the namespace, committed, so
/// `git show <ref>:<path>` reads them the way the script reads main.
fn fixture_tree(dir: &Path, pgdatabase: &str) {
    let manifests = dir.join("infra/cluster/manifests");
    std::fs::create_dir_all(&manifests).unwrap();
    write_file(
        &manifests.join("boss.yaml"),
        &format!(
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: boss\n  namespace: boss\nspec:\n  template:\n    spec:\n      initContainers:\n        - name: boss-init\n          env:\n            - {{name: PGHOST, value: postgres}}\n            - {{name: PGUSER, value: boss}}\n            - {{name: PGDATABASE, value: {pgdatabase}}}\n"
        ),
    );
    write_file(
        &dir.join("infra/cluster/instances.toml"),
        "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant_repo = \"david/algedonic-llc\"\ntenant_ref = \"main\"\nsim = false\nhostname = \"boss.algedonic.dev\"\n\n[playground]\nnamespace = \"boss-playground\"\ntenant_dir = \"examples/brewery\"\nsim = true\nhostname = \"playground.algedonic.dev\"\n",
    );
    let git = |args: &[&str]| {
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
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "fixture",
    ]);
}

/// One fixture: the stub kubectl, its answer files, the Secret's value
/// in a file, a hold file path, a backup directory and a tree.
struct Case {
    root: PathBuf,
    bin: PathBuf,
    answers: PathBuf,
    secret: PathBuf,
    log: PathBuf,
    created: PathBuf,
    hold: PathBuf,
    backups: PathBuf,
    tree: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("switch-instance-database-{name}"));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let answers = root.join("answers");
        std::fs::create_dir_all(&answers).unwrap();
        let secret = root.join("secret.url");
        write_file(&secret, OLD_URL);
        let log = root.join("argv.log");
        let created = root.join("created.log");
        let hold = root.join("converge-hold");
        let backups = root.join("backups");
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        fixture_tree(&tree, "algedonic");

        // The target as the maintenance database sees it: absent by
        // default; `db_exists` answers the datname row, `db_tables` the
        // table count in the target.
        write_file(&answers.join("db_exists"), "");
        write_file(&answers.join("db_tables"), "0\n");
        write_file(
            &answers.join("jsz"),
            r#"{"account_details":[{"stream_detail":[{"name":"BOSS_EVENTS","state":{"messages":120,"first_seq":1,"last_seq":120},"consumer_detail":[{"name":"dispatcher-job-opened","delivered":{"stream_seq":120},"ack_floor":{"stream_seq":118},"num_pending":0}]}]}]}"#,
        );

        // The stub kubectl. Every call's argv is appended to $STUB_LOG
        // (one word per line, `=== call ===` between calls). `get
        // secret` prints the file's URL base64-encoded the way the API
        // does; `patch secret` decodes `.data."database-url"` from the
        // -p document into the same file, so the script's read-back is
        // honest; `exec … pg_dump` streams $STUB_DUMP_BYTES of
        // incompressible bytes (or fails on STUB_PG_DUMP_FAIL); `exec …
        // psql` answers by the `-- switch:<tag>` on its last -c and
        // records a create; `exec … nats` prints the jsz answer.
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
if [ "${words[2]:-}" = patch ] && [ "${words[3]:-}" = secret ]; then
    [ -n "${STUB_PATCH_FAIL:-}" ] && { echo 'Error from server: the server could not patch' >&2; exit 1; }
    p=""
    for ((i=0; i<${#words[@]}; i++)); do [ "${words[$i]}" = -p ] && p="${words[$((i+1))]}"; done
    printf '%s' "$p" | jq -r '.data."database-url"' | base64 -d > "$STUB_SECRET"
    echo 'secret/boss-secrets patched'
    exit 0
fi
if [ "${words[2]:-}" = exec ]; then
    case "${words[3]:-}" in
        sts/nats)
            [ -n "${STUB_NATS_FAIL:-}" ] && { echo 'error: unable to upgrade connection: container not found ("nats")' >&2; exit 1; }
            cat "$STUB_ANSWERS/jsz"; exit 0 ;;
        sts/postgres) ;;
        *) echo "stub kubectl: unexpected workload ${words[3]:-}" >&2; exit 1 ;;
    esac
    case "${words[7]:-}" in
        pg_dump)
            [ -n "${STUB_PG_DUMP_FAIL:-}" ] && { echo 'pg_dump: error: connection to server failed' >&2; exit 1; }
            head -c "${STUB_DUMP_BYTES:-2000000}" /dev/urandom; exit 0 ;;
        psql)
            last="${words[${#words[@]}-1]}"
            tag=$(printf '%s' "$last" | sed -n 's/^-- switch:\([a-z_]*\).*/\1/p' | head -1)
            [ -n "$tag" ] || { echo "stub kubectl: no switch tag in the last argv word" >&2; exit 1; }
            case "$tag" in
                create_db)
                    printf '%s\n' "$last" >> "$STUB_CREATED"
                    [ -n "${STUB_CREATE_FAIL:-}" ] && { echo 'ERROR:  permission denied to create database' >&2; exit 1; }
                    echo 'CREATE DATABASE'; exit 0 ;;
            esac
            [ -f "$STUB_ANSWERS/$tag" ] || { echo "stub kubectl: ERROR:  no answer for $tag" >&2; exit 1; }
            cat "$STUB_ANSWERS/$tag"; exit 0 ;;
    esac
fi
echo "stub kubectl: unexpected argv: $*" >&2
exit 1
"#,
        );
        Case {
            root,
            bin,
            answers,
            secret,
            log,
            created,
            hold,
            backups,
            tree,
        }
    }

    fn hold(&self) {
        write_file(&self.hold, "switch-instance-database\n");
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
            .env("STUB_CREATED", &self.created)
            .env("BOSS_CONVERGE_HOLD", &self.hold)
            .env("BOSS_SWITCH_BACKUP_DIR", &self.backups)
            .env("BOSS_SWITCH_TREE", &self.tree)
            .env("BOSS_SWITCH_MAIN_REF", "HEAD")
            // The floor is 1 MiB on the forge (the prod database is
            // GBs); the stub streams 2 MB by default.
            .env("BOSS_SWITCH_MIN_SNAPSHOT_BYTES", "1048576");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("switch-instance-database.sh runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
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

    fn secret_now(&self) -> String {
        std::fs::read_to_string(&self.secret).unwrap_or_default()
    }

    fn created(&self) -> String {
        std::fs::read_to_string(&self.created).unwrap_or_default()
    }

    fn snapshots(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.backups)
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default()
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

// ---------------------------------------------------------------------------
// The mode and the arguments.
// ---------------------------------------------------------------------------

#[test]
fn refuses_a_missing_or_foreign_mode_namespace_or_name() {
    let c = Case::new("args");
    for (args, needle) in [
        (vec![], "usage"),
        (
            vec!["--now", "boss", "algedonic"],
            "--dry-run and --for-real",
        ),
        (vec!["--dry-run", "kube-system", "algedonic"], "namespace"),
        (vec!["--dry-run", "boss-dev", "algedonic"], "boss-dev"),
        (vec!["--dry-run", "boss", "Algedonic"], "database name"),
        (vec!["--dry-run", "boss", "alge;donic"], "database name"),
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
// The dry run.
// ---------------------------------------------------------------------------

/// Held, target absent, dump rehearsal fine, flip landed: every bound
/// passes and the plan is printed — the Secret untouched, no database
/// created, no snapshot written, and the password nowhere.
#[test]
fn dry_run_prints_every_bound_and_changes_nothing() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("dry-run");
    c.hold();
    let (rc, text) = c.run(&["--dry-run", "boss", "algedonic"]);
    assert_eq!(rc, 0, "the dry run did not exit 0:\n{text}");
    no_password(&text, "dry run");
    contains_all(
        &text,
        &[
            "DRY RUN",
            "hold: HELD",
            "boss@postgres.boss.svc.cluster.local:5432/boss", // the Secret, redacted
            "target: algedonic is absent — would CREATE DATABASE algedonic OWNER boss",
            "flip: boss-init PGDATABASE on HEAD (",
            ") names algedonic",
            "tenant_repo = \"david/algedonic-llc\"",
            "would snapshot boss to",
            "would patch Secret boss-secrets key database-url to boss@postgres.boss.svc.cluster.local:5432/algedonic",
            "nats: BOSS_EVENTS",
            "does NOT roll the pod",
            "Nothing was changed",
        ],
        "the dry run's plan",
    );
    assert_eq!(c.secret_now(), OLD_URL, "a dry run patched the Secret");
    assert!(c.created().is_empty(), "a dry run created a database");
    assert!(
        c.snapshots().is_empty(),
        "a dry run wrote a snapshot: {:?}",
        c.snapshots()
    );
    // The rehearsal is schema-only, and no call mutates.
    let calls = c.calls();
    let dump = calls
        .iter()
        .find(|w| w.contains(&"pg_dump".to_string()))
        .expect("the dry run rehearses the dump");
    assert!(
        dump.contains(&"--schema-only".to_string()),
        "the rehearsal is not schema-only: {dump:?}"
    );
    assert!(
        !calls.iter().any(|w| w.contains(&"patch".to_string())),
        "a dry run called patch"
    );
    // Every psql the dry run issues is a read in a read-only session.
    for w in calls.iter().filter(|w| w.contains(&"psql".to_string())) {
        assert!(
            w.contains(&"SET default_transaction_read_only = on".to_string()),
            "a dry-run psql without the read-only SET: {w:?}"
        );
    }
}

/// Without the hold the dry run still prints the plan, but names the
/// hold as the bound the real run would refuse on, and how to hold.
#[test]
fn dry_run_without_the_hold_says_the_real_run_would_refuse() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("dry-run-unheld");
    let (rc, text) = c.run(&["--dry-run", "boss", "algedonic"]);
    assert_eq!(rc, 2, "an unheld dry run must not read as a pass:\n{text}");
    no_password(&text, "unheld dry run");
    contains_all(
        &text,
        &[
            "hold: NOT held",
            "hold-converge",
            "DRY RUN",
            "would REFUSE",
            "Nothing was changed",
        ],
        "the unheld dry run",
    );
    assert_eq!(c.secret_now(), OLD_URL);
    assert!(c.created().is_empty());
}

// ---------------------------------------------------------------------------
// The real run's refusals — each leaves the Secret and postgres alone.
// ---------------------------------------------------------------------------

#[test]
fn the_real_run_refuses_without_the_hold() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real-unheld");
    let (rc, text) = c.run(&["--for-real", "boss", "algedonic"]);
    assert_eq!(rc, 2, "not refused:\n{text}");
    no_password(&text, "unheld real run");
    contains_all(
        &text,
        &[
            "REFUSED",
            "converge is not held",
            "hold-converge",
            "Nothing was changed",
        ],
        "the refusal",
    );
    assert_eq!(c.secret_now(), OLD_URL, "the Secret was patched");
    assert!(c.created().is_empty(), "a database was created");
    assert!(c.snapshots().is_empty(), "a snapshot was written");
}

#[test]
fn the_real_run_refuses_when_the_secret_already_names_the_target() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real-same");
    c.hold();
    let (rc, text) = c.run(&["--for-real", "boss", "boss"]);
    assert_eq!(rc, 2, "not refused:\n{text}");
    no_password(&text, "same-name run");
    contains_all(
        &text,
        &["already names boss", "Nothing was changed"],
        "the refusal",
    );
    assert!(c.created().is_empty());
}

#[test]
fn the_real_run_refuses_a_target_that_exists_with_tables() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real-nonempty");
    c.hold();
    write_file(&c.answers.join("db_exists"), "algedonic\n");
    write_file(&c.answers.join("db_tables"), "37\n");
    let (rc, text) = c.run(&["--for-real", "boss", "algedonic"]);
    assert_eq!(rc, 2, "not refused:\n{text}");
    no_password(&text, "non-empty target");
    contains_all(
        &text,
        &["algedonic exists and holds 37 table", "Nothing was changed"],
        "the refusal",
    );
    assert_eq!(c.secret_now(), OLD_URL);
    assert!(c.created().is_empty());
    assert!(
        c.snapshots().is_empty(),
        "the snapshot ran before the target bound"
    );
}

#[test]
fn the_real_run_refuses_when_the_flip_has_not_landed() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real-noflip");
    c.hold();
    // main still says PGDATABASE=boss: the release would converge the
    // schema into the OLD database while the services read the new one.
    fixture_tree(&c.root.join("tree-unflipped"), "boss");
    let (rc, text) = c.run_env(
        &["--for-real", "boss", "algedonic"],
        &[(
            "BOSS_SWITCH_TREE",
            c.root.join("tree-unflipped").display().to_string(),
        )],
    );
    assert_eq!(rc, 2, "not refused:\n{text}");
    no_password(&text, "unflipped run");
    contains_all(
        &text,
        &[
            "PGDATABASE",
            "names boss, not algedonic",
            "Nothing was changed",
        ],
        "the refusal",
    );
    assert_eq!(c.secret_now(), OLD_URL);
    assert!(c.created().is_empty());
    assert!(c.snapshots().is_empty());
}

#[test]
fn the_real_run_refuses_when_the_snapshot_fails_or_is_too_small() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real-dump-fails");
    c.hold();
    let (rc, text) = c.run_env(
        &["--for-real", "boss", "algedonic"],
        &[("STUB_PG_DUMP_FAIL", "1".into())],
    );
    assert_eq!(rc, 2, "not refused:\n{text}");
    no_password(&text, "failed dump");
    contains_all(
        &text,
        &["snapshot", "pg_dump", "Nothing was changed"],
        "the refusal",
    );
    assert_eq!(c.secret_now(), OLD_URL);
    assert!(c.created().is_empty());
    assert!(
        c.snapshots().is_empty(),
        "a failed snapshot left a file: {:?}",
        c.snapshots()
    );

    let c = Case::new("real-dump-small");
    c.hold();
    let (rc, text) = c.run_env(
        &["--for-real", "boss", "algedonic"],
        &[("STUB_DUMP_BYTES", "4096".into())],
    );
    assert_eq!(rc, 2, "not refused:\n{text}");
    no_password(&text, "small dump");
    contains_all(
        &text,
        &["smaller than", "1048576", "Nothing was changed"],
        "the refusal",
    );
    assert_eq!(c.secret_now(), OLD_URL);
    assert!(c.created().is_empty());
    assert!(
        c.snapshots().is_empty(),
        "a too-small snapshot was kept: {:?}",
        c.snapshots()
    );
}

#[test]
fn a_secret_that_cannot_be_read_is_a_refusal_not_a_pass() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("secret-unreadable");
    c.hold();
    let (rc, text) = c.run_env(
        &["--for-real", "boss", "algedonic"],
        &[("STUB_SECRET_UNREADABLE", "1".into())],
    );
    assert_eq!(rc, 2, "not refused:\n{text}");
    contains_all(
        &text,
        &["boss-secrets", "Forbidden", "Nothing was changed"],
        "the refusal",
    );
    assert!(c.created().is_empty());
    assert!(c.snapshots().is_empty());
}

// ---------------------------------------------------------------------------
// The real run.
// ---------------------------------------------------------------------------

/// Held, flip landed, target absent: snapshot, create, patch, read
/// back, record — in that order, with the password nowhere and the pod
/// not rolled.
#[test]
fn the_real_run_snapshots_creates_repoints_and_records() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real");
    c.hold();
    let (rc, text) = c.run(&["--for-real", "boss", "algedonic"]);
    assert_eq!(rc, 0, "the real run failed:\n{text}");
    no_password(&text, "real run");

    // The snapshot: one gzip file, named for namespace and old database.
    let snaps = c.snapshots();
    assert_eq!(snaps.len(), 1, "one snapshot: {snaps:?}");
    let name = snaps[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("boss-boss-") && name.ends_with(".sql.gz"),
        "snapshot name: {name}"
    );
    let bytes = std::fs::metadata(&snaps[0]).unwrap().len();
    assert!(bytes >= 1_048_576, "snapshot is {bytes} bytes");
    assert!(
        text.contains(&name),
        "the record names the snapshot:\n{text}"
    );
    assert!(
        text.contains(&format!("{bytes} bytes")),
        "the record carries the size:\n{text}"
    );

    // The create: exactly one, OWNER boss, the pattern-checked name.
    assert!(
        c.created().contains("CREATE DATABASE algedonic OWNER boss"),
        "the create: {}",
        c.created()
    );
    assert_eq!(
        c.created()
            .lines()
            .filter(|l| l.starts_with("CREATE DATABASE"))
            .count(),
        1,
        "exactly one create"
    );

    // The patch: the argv shape, and the decoded value is the old URL
    // with only the database segment changed.
    let calls = c.calls();
    let patch = calls
        .iter()
        .find(|w| w.get(2).is_some_and(|x| x == "patch"))
        .expect("a patch call");
    assert_eq!(
        &patch[..7],
        &[
            "-n",
            "boss",
            "patch",
            "secret",
            "boss-secrets",
            "--type",
            "merge"
        ],
        "patch argv: {patch:?}"
    );
    assert_eq!(patch[7], "-p");
    let doc: serde_json::Value = serde_json::from_str(&patch[8]).expect("the patch is JSON");
    let keys: Vec<&String> = doc["data"].as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["database-url"], "the patch touches one key");
    assert_eq!(
        c.secret_now(),
        "postgres://boss:s3cretpw0123@postgres.boss.svc.cluster.local:5432/algedonic"
    );

    // Order: the snapshot before the create before the patch.
    let idx = |word: &str| {
        calls
            .iter()
            .position(|w| w.contains(&word.to_string()))
            .unwrap_or_else(|| panic!("no call carrying {word}"))
    };
    let dump = calls
        .iter()
        .position(|w| {
            w.contains(&"pg_dump".to_string()) && !w.contains(&"--schema-only".to_string())
        })
        .expect("the real dump");
    let create = calls
        .iter()
        .position(|w| w.iter().any(|x| x.starts_with("-- switch:create_db")))
        .expect("the create");
    assert!(dump < create, "the snapshot precedes the create");
    assert!(create < idx("patch"), "the create precedes the patch");
    // The read-back after the patch is a second `get secret`.
    let gets: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, w)| w.get(2).is_some_and(|x| x == "get"))
        .map(|(i, _)| i)
        .collect();
    assert!(
        gets.iter().any(|&i| i > idx("patch")),
        "no read-back after the patch: {gets:?}"
    );
    // The dump's shape: the same exec door the census uses, --no-owner.
    let d = &calls[dump];
    assert_eq!(
        &d[..7],
        &["-n", "boss", "exec", "sts/postgres", "-c", "postgres", "--"],
        "dump argv: {d:?}"
    );
    assert!(d.contains(&"--no-owner".to_string()));
    assert!(d.contains(&"-d".to_string()) && d.contains(&"boss".to_string()));

    contains_all(
        &text,
        &[
            "OK — Secret boss-secrets in boss now names boss@postgres.boss.svc.cluster.local:5432/algedonic",
            "old database boss stays in postgres as the archive",
            "does NOT roll the pod",
            "release-converge",
            "nats: BOSS_EVENTS",
            "dispatcher-job-opened",
        ],
        "the real run's record",
    );
    // The record line is one JSON document a reader of the packet can
    // parse, last on stdout.
    let record = text
        .lines()
        .find(|l| l.starts_with("{\"verb\":\"switch-instance-database\""))
        .expect("a JSON record line");
    let rec: serde_json::Value = serde_json::from_str(record).unwrap();
    assert_eq!(rec["old_db"], "boss");
    assert_eq!(rec["new_db"], "algedonic");
    assert_eq!(rec["namespace"], "boss");
    assert_eq!(rec["snapshot_bytes"], bytes);
    assert_eq!(rec["nats"]["streams"][0]["name"], "BOSS_EVENTS");
    assert_eq!(rec["nats"]["streams"][0]["consumers"][0]["ack_floor"], 118);
    assert_eq!(rec["created"], true);
}

/// A target that exists and is EMPTY is reused: no create, the rest
/// unchanged.
#[test]
fn an_existing_empty_target_is_reused_not_recreated() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real-empty-exists");
    c.hold();
    write_file(&c.answers.join("db_exists"), "algedonic\n");
    write_file(&c.answers.join("db_tables"), "0\n");
    let (rc, text) = c.run(&["--for-real", "boss", "algedonic"]);
    assert_eq!(rc, 0, "failed:\n{text}");
    no_password(&text, "reuse run");
    assert!(c.created().is_empty(), "an empty target was re-created");
    assert!(text.contains("algedonic exists and is empty"), "{text}");
    assert!(c.secret_now().ends_with("/algedonic"));
    let record = text
        .lines()
        .find(|l| l.starts_with("{\"verb\":\"switch-instance-database\""))
        .expect("a JSON record line");
    let rec: serde_json::Value = serde_json::from_str(record).unwrap();
    assert_eq!(rec["created"], false);
}

/// NATS positions the verb cannot read are recorded as `unmeasured`
/// with the reason — never faked, never a refusal (the switch does not
/// depend on them).
#[test]
fn unreadable_nats_positions_are_recorded_as_unmeasured() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real-nats-dark");
    c.hold();
    let (rc, text) = c.run_env(
        &["--for-real", "boss", "algedonic"],
        &[("STUB_NATS_FAIL", "1".into())],
    );
    assert_eq!(rc, 0, "a dark NATS must not refuse the switch:\n{text}");
    no_password(&text, "nats-dark run");
    assert!(text.contains("nats: unmeasured"), "{text}");
    let record = text
        .lines()
        .find(|l| l.starts_with("{\"verb\":\"switch-instance-database\""))
        .expect("a JSON record line");
    let rec: serde_json::Value = serde_json::from_str(record).unwrap();
    assert_eq!(rec["nats"]["measured"], false);
    assert!(
        rec["nats"]["reason"]
            .as_str()
            .unwrap()
            .contains("container not found")
    );
}

/// A patch that fails after the snapshot and the create is a FAILURE
/// (exit 1), not a refusal: the record states what stands — the
/// snapshot, the created database — and that the Secret is unchanged.
#[test]
fn a_failed_patch_exits_1_and_states_what_stands() {
    if !has("jq") {
        eprintln!("skipping: jq not on PATH");
        return;
    }
    let c = Case::new("real-patch-fails");
    c.hold();
    let (rc, text) = c.run_env(
        &["--for-real", "boss", "algedonic"],
        &[("STUB_PATCH_FAIL", "1".into())],
    );
    assert_eq!(rc, 1, "expected a failure exit:\n{text}");
    no_password(&text, "failed patch");
    contains_all(
        &text,
        &[
            "FAILED",
            "the Secret still names boss@postgres.boss.svc.cluster.local:5432/boss",
            "the snapshot at",
            "CREATE DATABASE algedonic OWNER boss stands",
        ],
        "the failure record",
    );
    assert_eq!(c.secret_now(), OLD_URL);
}

// ---------------------------------------------------------------------------
// The declaration.
// ---------------------------------------------------------------------------

#[test]
fn the_verb_file_is_a_mutating_forge_verb_with_the_three_params() {
    let path = repo_root().join("infra/ops/verbs/switch-instance-database.json");
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
    assert_eq!(params[2]["name"], "new_db");
    // The runner matches a pattern with jq's `test` (ops-runner.sh), so
    // the patterns are judged by jq here too — the same engine.
    let matches = |pat: &str, value: &str| -> bool {
        let out = Command::new("jq")
            .args([
                "-n",
                "--arg",
                "v",
                value,
                "--arg",
                "p",
                pat,
                "$v | test($p)",
            ])
            .output()
            .expect("jq runs");
        String::from_utf8_lossy(&out.stdout).trim() == "true"
    };
    if has("jq") {
        let ns = params[1]["pattern"].as_str().unwrap();
        for ok in ["boss", "boss-playground"] {
            assert!(matches(ns, ok), "namespace pattern refuses {ok}");
        }
        for bad in ["kube-system", "Boss", "boss playground", "-boss", ""] {
            assert!(!matches(ns, bad), "namespace pattern admits {bad:?}");
        }
        let db = params[2]["pattern"].as_str().unwrap();
        for ok in ["algedonic", "boss", "boss_2026"] {
            assert!(matches(db, ok), "db pattern refuses {ok}");
        }
        for bad in ["Algedonic", "alge;donic", "-x", "a b", "postgres/x", ""] {
            assert!(!matches(db, bad), "db pattern admits {bad:?}");
        }
    }
    let about = v["about"].as_str().unwrap();
    assert!(
        about.starts_with("MUTATING"),
        "about opens with MUTATING: {about}"
    );
    assert!(about.contains("David"), "about names who authorized it");
    assert!(about.contains("063dba4e"), "about names the packet");
    assert!(about.contains("e652c7c6"), "about names the design");
    assert!(
        about.contains("does NOT roll"),
        "about says the roll is release-converge's"
    );
    // A whole-database pg_dump through docker+kubectl exceeds the
    // runner's 30 s default many times over.
    assert_eq!(v["timeout"], 900);
    let script = repo_root().join(SCRIPT);
    use std::os::unix::fs::PermissionsExt;
    assert!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o111 != 0);
}

/// §9a: the namespace/StatefulSet/container/user the script exec's
/// into are what the manifest declares, and the hold file it reads is
/// the one converge-hold.sh writes.
#[test]
fn the_target_and_the_hold_file_are_the_ones_the_tree_declares() {
    let src = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let manifest =
        std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss.yaml")).unwrap();
    let want = |s: &str| assert!(src.contains(s), "script lacks `{s}`");
    want("PG_WORKLOAD=\"sts/postgres\"");
    want("PG_CONTAINER=\"postgres\"");
    want("PG_USER=\"boss\"");
    want("NATS_WORKLOAD=\"sts/nats\"");
    want("NATS_CONTAINER=\"nats\"");
    assert!(manifest.contains("{name: POSTGRES_USER, value: boss}"));
    let sts = manifest
        .split("kind: StatefulSet\n")
        .find(|s| s.starts_with("metadata:\n  name: postgres\n"))
        .expect("boss.yaml declares StatefulSet postgres");
    assert!(
        sts.split("kind: ")
            .next()
            .unwrap()
            .contains("- name: postgres\n")
    );
    let nats = manifest
        .split("kind: StatefulSet\n")
        .find(|s| s.starts_with("metadata:\n  name: nats\n"))
        .expect("boss.yaml declares StatefulSet nats");
    assert!(
        nats.split("kind: ")
            .next()
            .unwrap()
            .contains("- name: nats\n")
    );
    assert!(nats.contains("8222"), "nats monitoring port is 8222");
    // The hold file: the same default converge-hold.sh and the runner use.
    let hold = std::fs::read_to_string(repo_root().join("infra/forge/converge-hold.sh")).unwrap();
    assert!(hold.contains("${BOSS_CONVERGE_HOLD:-/var/tmp/boss-converge-hold}"));
    want("${BOSS_CONVERGE_HOLD:-/var/tmp/boss-converge-hold}");
    // boss-init carries NO PGDATABASE literal since the sibling car
    // (fix/boss-init-reads-its-database-from-the-secret, 2026-09-16):
    // its database is derived from DATABASE_URL, the Secret this verb
    // repoints. The flip bound still READS the shape, so a literal that
    // ever returns is judged rather than ignored — and its absence reads
    // as "derived from the Secret", the state the tree declares.
    assert!(
        !manifest.contains("- {name: PGDATABASE, value: "),
        "boss-init must not name its database beside DATABASE_URL (two names for one fact)"
    );
    want("{name: PGDATABASE, value: ");
    want("derived from the Secret");
    // No trace: `set -x` would print the Secret's value.
    assert!(
        !src.lines()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("set -x")),
        "the script traces (set -x) — a trace prints the Secret"
    );
}
