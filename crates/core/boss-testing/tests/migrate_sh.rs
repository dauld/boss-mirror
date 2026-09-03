//! Behavior of `infra/postgres/migrate.sh` — the only path schema takes
//! into a database.
//!
//! The runner's contract, pinned here:
//! - schema/*.sql sorted by NNN- prefix is the migration order; files not yet recorded
//!   in `schema_migrations` apply in order, each atomically with its
//!   record, so a re-run never re-applies and a failure records nothing.
//! - `--baseline` records without running, for databases that predate the
//!   runner (their tables already exist).
//! - `--without <name>` skips matching entries without recording them —
//!   the ledger-less bootstrap capability (`TestDb::new_without` mirrors it).
//! - An already-applied file whose content has changed fails the run by
//!   name: applied migrations are history, changes go in a new file.
//!
//! Each test creates its own scratch database (dropped at the end; a
//! panic can leak one — the `test_boss_mig_` prefix makes orphans easy to
//! find, same tradeoff as TestDb). The synthetic-schema tests copy the
//! script into a temp dir beside a tiny schema/ so they can grow, break,
//! and edit migrations without touching the repo's real schema files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::str::FromStr;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

const DEFAULT_ADMIN_URL: &str = "postgres://boss:boss@127.0.0.1/postgres";

fn admin_url() -> String {
    std::env::var("BOSS_TEST_POSTGRES_ADMIN_URL").unwrap_or_else(|_| DEFAULT_ADMIN_URL.to_string())
}

/// Swap the database name in a `postgres://...` URL. Assumes the URL's
/// last path segment is the database — true of the plain URLs this
/// harness runs with (see `DEFAULT_ADMIN_URL`, CI's service URL).
fn with_db(url: &str, db: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let cut = base.rfind('/').expect("admin url has no path segment");
    let mut out = format!("{}/{db}", &base[..cut]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    out
}

fn real_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/postgres/migrate.sh")
}

fn real_schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/postgres/schema")
}

async fn admin_conn() -> PgConnection {
    let opts = PgConnectOptions::from_str(&admin_url()).expect("parsing admin url");
    PgConnection::connect_with(&opts)
        .await
        .expect("connecting to admin db")
}

/// Create an empty scratch database and return `(name, url)`.
async fn scratch_db() -> (String, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let name = format!("test_boss_mig_{}", &suffix[..12]);
    let mut admin = admin_conn().await;
    admin
        .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
        .await
        .expect("CREATE DATABASE");
    let url = with_db(&admin_url(), &name);
    (name, url)
}

async fn drop_db(name: &str) {
    let mut admin = admin_conn().await;
    let _ = admin
        .execute(format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#).as_str())
        .await;
}

/// Run a migrate.sh (real or copied) against `db_url` with the given
/// script args, connecting via `-- psql <url>`.
///
/// Spawned via `bash <script>`, never by exec'ing the copy directly:
/// tests run as threads of one process, and a fork in thread A can
/// inherit thread B's still-open write-fd from `fs::copy` — exec'ing
/// B's file then fails ETXTBSY ("Text file busy", first struck on
/// train #222, `df69249e`). bash opens the script READ-ONLY, so the
/// race is impossible by construction.
fn run(script: &Path, args: &[&str], db_url: &str) -> Output {
    std::process::Command::new("bash")
        .arg(script)
        .args(args)
        .args(["--", "psql", db_url])
        .output()
        .unwrap_or_else(|e| panic!("spawning {}: {e}", script.display()))
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// A temp copy of the runner beside a synthetic schema/ dir the test can
/// mutate freely. `files` are `(filename, sql)`; the runner derives the
/// order from the `NNN-` prefixes, so there is no list to write.
struct Synthetic {
    dir: PathBuf,
}

impl Synthetic {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir =
            std::env::temp_dir().join(format!("boss-migrate-test-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(dir.join("schema")).expect("mkdir schema");
        // fs::copy preserves the exec bit on unix.
        fs::copy(real_script(), dir.join("migrate.sh")).expect("copying migrate.sh");
        let s = Self { dir };
        for (name, sql) in files {
            s.write(name, sql);
        }
        s
    }

    fn script(&self) -> PathBuf {
        self.dir.join("migrate.sh")
    }

    fn write(&self, name: &str, sql: &str) {
        fs::write(self.dir.join("schema").join(name), sql).expect("writing schema file");
    }
}

impl Drop for Synthetic {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

async fn connect(db_url: &str) -> PgConnection {
    let opts = PgConnectOptions::from_str(db_url).expect("parsing db url");
    PgConnection::connect_with(&opts)
        .await
        .expect("connecting to scratch db")
}

/// Recorded migration ids in application order.
async fn recorded(conn: &mut PgConnection) -> Vec<String> {
    sqlx::query("SELECT id FROM schema_migrations ORDER BY applied_at, id")
        .fetch_all(conn)
        .await
        .expect("reading schema_migrations")
        .into_iter()
        .map(|r| r.get::<String, _>("id"))
        .collect()
}

async fn recorded_row(conn: &mut PgConnection, id: &str) -> (String, String) {
    let row = sqlx::query(
        "SELECT checksum, applied_at::text AS applied_at FROM schema_migrations WHERE id = $1",
    )
    .bind(id)
    .fetch_one(conn)
    .await
    .unwrap_or_else(|e| panic!("no schema_migrations row for {id}: {e}"));
    (row.get("checksum"), row.get("applied_at"))
}

async fn table_exists(conn: &mut PgConnection, table: &str) -> bool {
    sqlx::query("SELECT to_regclass($1) IS NOT NULL AS present")
        .bind(table)
        .fetch_one(conn)
        .await
        .expect("to_regclass")
        .get("present")
}

/// The migration order as the runner computes it: every `*.sql` in the
/// schema dir, sorted by its `NNN-` prefix. Mirrors migrate.sh's
/// `sort -t- -k1,1n` and build.rs's sort key — if those three ever
/// disagree, the tests below apply a different schema than production.
fn migration_order(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("reading the schema dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sql"))
        .collect();
    names.sort_by_key(|n| {
        let num: u32 = n
            .split('-')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(u32::MAX);
        (num, n.clone())
    });
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_schema_applies_to_a_fresh_db_and_a_rerun_is_a_noop() {
    let (name, url) = scratch_db().await;

    let out = run(&real_script(), &[], &url);
    assert_ok(&out, "first run against a fresh db");

    let mut conn = connect(&url).await;
    let expected = migration_order(&real_schema_dir());
    let mut got = recorded(&mut conn).await;
    let mut want = expected.clone();
    got.sort();
    want.sort();
    assert_eq!(got, want, "every migration is recorded, nothing else");
    assert!(table_exists(&mut conn, "jobs").await, "03-jobs applied");
    assert!(
        table_exists(&mut conn, "audit_log").await,
        "02-events applied"
    );

    let before = recorded_row(&mut conn, &expected[0]).await;
    let out = run(&real_script(), &[], &url);
    assert_ok(&out, "second run against an up-to-date db");
    let after = recorded_row(&mut conn, &expected[0]).await;
    assert_eq!(before, after, "a re-run re-applies nothing");

    drop(conn);
    drop_db(&name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_unrecorded_entries_apply() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[("01-first.sql", "CREATE TABLE t1 (id int);")]);

    assert_ok(&run(&syn.script(), &[], &url), "applying 01-first");
    let mut conn = connect(&url).await;
    let first = recorded_row(&mut conn, "01-first.sql").await;

    // Dropping the file in IS the whole act of adding a migration —
    // there is no list to update, which is the point of the collapse.
    syn.write("02-second.sql", "CREATE TABLE t2 (id int);");
    assert_ok(
        &run(&syn.script(), &[], &url),
        "applying the newly-added migration",
    );

    assert!(table_exists(&mut conn, "t2").await, "02-second ran");
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-first.sql", "02-second.sql"],
        "both entries recorded, in order"
    );
    assert_eq!(
        first,
        recorded_row(&mut conn, "01-first.sql").await,
        "the already-applied entry was not touched"
    );

    drop(conn);
    drop_db(&name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn baseline_records_without_running() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[("01-first.sql", "CREATE TABLE t1 (id int);")]);

    assert_ok(&run(&syn.script(), &["--baseline"], &url), "--baseline");
    let mut conn = connect(&url).await;
    assert_eq!(recorded(&mut conn).await, vec!["01-first.sql"]);
    assert!(
        !table_exists(&mut conn, "t1").await,
        "--baseline must not execute the migration"
    );

    // A normal run now finds nothing pending: baseline marked it applied.
    assert_ok(&run(&syn.script(), &[], &url), "run after baseline");
    assert!(!table_exists(&mut conn, "t1").await);

    drop(conn);
    drop_db(&name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_migration_is_not_recorded_and_stops_the_run() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[
        ("01-ok.sql", "CREATE TABLE t1 (id int);"),
        ("02-broken.sql", "CREATE TALBE t2 (id int);"),
        ("03-after.sql", "CREATE TABLE t3 (id int);"),
    ]);

    let out = run(&syn.script(), &[], &url);
    assert!(!out.status.success(), "a broken migration fails the run");

    let mut conn = connect(&url).await;
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-ok.sql"],
        "the entry before the failure is recorded; the failure and what follows are not"
    );
    assert!(table_exists(&mut conn, "t1").await);
    assert!(
        !table_exists(&mut conn, "t3").await,
        "the run stopped at the failure"
    );

    // Fixing an entry that never applied is legitimate — re-run picks it
    // up along with the rest.
    syn.write("02-broken.sql", "CREATE TABLE t2 (id int);");
    assert_ok(
        &run(&syn.script(), &[], &url),
        "re-run after fixing the broken entry",
    );
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-ok.sql", "02-broken.sql", "03-after.sql"]
    );
    assert!(table_exists(&mut conn, "t3").await);

    drop(conn);
    drop_db(&name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_an_applied_migration_fails_the_next_run_by_name() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[("01-first.sql", "CREATE TABLE t1 (id int);")]);

    assert_ok(&run(&syn.script(), &[], &url), "applying 01-first");
    let mut conn = connect(&url).await;
    let original = recorded_row(&mut conn, "01-first.sql").await;

    syn.write(
        "01-first.sql",
        "CREATE TABLE t1 (id int);\n-- edited after apply\n",
    );
    let out = run(&syn.script(), &[], &url);
    assert!(
        !out.status.success(),
        "an applied migration whose content changed must fail the run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("01-first.sql"),
        "the failure names the drifted file; stderr was:\n{stderr}"
    );
    assert_eq!(
        original,
        recorded_row(&mut conn, "01-first.sql").await,
        "the recorded row keeps the checksum of what actually ran"
    );

    drop(conn);
    drop_db(&name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_skips_matching_entries_and_does_not_record_them() {
    let (name, url) = scratch_db().await;
    let syn = Synthetic::new(&[
        ("01-core.sql", "CREATE TABLE t_core (id int);"),
        ("02-ledger.sql", "CREATE TABLE t_ledger (id int);"),
    ]);

    assert_ok(
        &run(&syn.script(), &["--without", "ledger"], &url),
        "--without ledger",
    );
    let mut conn = connect(&url).await;
    assert!(table_exists(&mut conn, "t_core").await);
    assert!(
        !table_exists(&mut conn, "t_ledger").await,
        "the skipped module did not run"
    );
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-core.sql"],
        "a skipped entry is not recorded — a later full run still owes it"
    );

    assert_ok(
        &run(&syn.script(), &[], &url),
        "full run after a --without run",
    );
    assert!(table_exists(&mut conn, "t_ledger").await);
    assert_eq!(
        recorded(&mut conn).await,
        vec!["01-core.sql", "02-ledger.sql"]
    );

    drop(conn);
    drop_db(&name).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_db_that_predates_the_runner_is_refused_until_baselined() {
    let (name, url) = scratch_db().await;
    // The marker of a real pre-runner BOSS database: core tables exist,
    // no bookkeeping. audit_log is the sentinel the guard keys on.
    let mut conn = connect(&url).await;
    conn.execute("CREATE TABLE audit_log (id int)")
        .await
        .expect("creating sentinel table");

    let syn = Synthetic::new(&[("01-first.sql", "CREATE TABLE t1 (id int);")]);
    let out = run(&syn.script(), &[], &url);
    assert!(
        !out.status.success(),
        "a populated db with no schema_migrations must not be re-applied from scratch"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--baseline"),
        "the refusal tells the operator the way out; stderr was:\n{stderr}"
    );

    assert_ok(
        &run(&syn.script(), &["--baseline"], &url),
        "--baseline adopts the db",
    );
    assert_ok(
        &run(&syn.script(), &[], &url),
        "normal runs work once baselined",
    );

    drop(conn);
    drop_db(&name).await;
}

/// Two pods booting together race the schema converge — named as
/// RollingUpdate blocker #2 in boss.yaml's strategy comment. The whole
/// run now holds `pg_advisory_lock` on a dedicated connection and
/// computes its pending set AFTER acquisition, so the loser sees the
/// winner's committed bookkeeping and applies nothing. The lock tag
/// includes the database OID, so TestDb's per-test scratch databases
/// never serialize on each other — only real contenders for one
/// schema do.
///
/// Without the lock this fails loudly: both runs read an empty
/// ledger, race the same non-idempotent CREATE TABLE, and the loser
/// exits nonzero on duplicate DDL or the bookkeeping PK conflict.
#[tokio::test]
async fn two_concurrent_runs_apply_each_migration_exactly_once() {
    let synth = Synthetic::new(&[
        ("001-a.sql", "CREATE TABLE mig_race_a (id INT PRIMARY KEY);"),
        ("002-b.sql", "CREATE TABLE mig_race_b (id INT PRIMARY KEY);"),
        ("003-c.sql", "CREATE TABLE mig_race_c (id INT PRIMARY KEY);"),
    ]);
    let (name, url) = scratch_db().await;

    let script = synth.script();
    let (u1, u2) = (url.clone(), url.clone());
    let s1 = script.clone();
    let t1 = std::thread::spawn(move || run(&s1, &[], &u1));
    let t2 = std::thread::spawn(move || run(&script, &[], &u2));
    let o1 = t1.join().expect("run 1 thread");
    let o2 = t2.join().expect("run 2 thread");

    assert_ok(&o1, "concurrent run 1");
    assert_ok(&o2, "concurrent run 2");

    // "migrate.sh: applied A, already recorded R, of K migrations" —
    // totals across the pair prove nothing ran twice and nothing was
    // skipped: one run applied all three, the other recorded all
    // three as already done.
    let counts = |out: &Output| -> (u32, u32) {
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text
            .lines()
            .rev()
            .find(|l| l.contains("of 3 migrations"))
            .expect("summary line");
        let nums: Vec<u32> = line
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().unwrap())
            .collect();
        (nums[0], nums[1])
    };
    let (a1, r1) = counts(&o1);
    let (a2, r2) = counts(&o2);
    assert_eq!(a1 + a2, 3, "each migration applied exactly once");
    assert_eq!(r1 + r2, 3, "the loser saw the winner's bookkeeping");

    let opts = PgConnectOptions::from_str(&url).expect("parsing scratch url");
    let mut conn = PgConnection::connect_with(&opts)
        .await
        .expect("connecting to scratch db");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM schema_migrations")
        .fetch_one(&mut conn)
        .await
        .expect("counting bookkeeping rows");
    assert_eq!(rows, 3, "exactly one bookkeeping row per migration");

    drop(conn);
    drop_db(&name).await;
}
