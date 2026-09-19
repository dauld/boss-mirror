//! The switch's SQL and dump, RUN against a real Postgres — the half
//! `switch_instance_database_sh.rs` cannot pin with a stub that answers
//! canned rows.
//!
//! A stub that echoes rows proves the script's shape; it proves nothing
//! about `pg_database` / `pg_tables` reads, `CREATE DATABASE … OWNER
//! boss`, or whether `pg_dump --no-owner` of the schema through a pipe
//! lands as a gzip the floor accepts. So here the stub `kubectl` does
//! one translation: it drops the `-n boss exec sts/postgres -c postgres
//! --` head the script sends to the cluster and runs the SAME psql /
//! pg_dump arguments against the harness's Postgres, with `-d <name>`
//! mapped onto that server (backlog 063dba4e; design e652c7c6).
//!
//! The Secret fixture names the scratch database `TestDb` created
//! (schema loaded) as the OLD database, so the snapshot is a dump of
//! the real schema; the NEW database is created for real on the same
//! server and dropped at the end. `TestDb` refuses a server hosting a
//! database named `boss` (test_db.rs), which is what a port-forward to
//! the cluster looks like from here — never against production.

use boss_testing::test_db::scratch_database_name;
use boss_testing::{TestDb, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/forge/switch-instance-database.sh";

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

/// `db.url()` with its database segment removed: `<base>/<name>` is a
/// URL onto any database on the harness's server.
fn base_url(db: &TestDb) -> String {
    let url = db.url();
    url[..url.rfind('/').expect("a database segment")].to_string()
}

fn psql(url: &str, sql: &str) -> String {
    let out = Command::new("psql")
        .arg(url)
        .args(["-X", "-q", "-At", "-v", "ON_ERROR_STOP=1", "-c", sql])
        .output()
        .expect("psql runs");
    assert!(
        out.status.success(),
        "psql {sql}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A committed tree whose boss.yaml names the target as boss-init's
/// PGDATABASE, so the flip bound passes and the SQL is what is judged.
fn fixture_tree(dir: &Path, pgdatabase: &str) {
    std::fs::create_dir_all(dir.join("infra/cluster/manifests")).unwrap();
    write_file(
        &dir.join("infra/cluster/manifests/boss.yaml"),
        &format!(
            "initContainers:\n  - name: boss-init\n    env:\n      - {{name: PGDATABASE, value: {pgdatabase}}}\n"
        ),
    );
    write_file(
        &dir.join("infra/cluster/instances.toml"),
        "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant_repo = \"david/algedonic-llc\"\ntenant_ref = \"main\"\n",
    );
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "."],
        vec![
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    ] {
        let out = Command::new("git")
            .args(&args)
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
    }
}

/// The translating stub: the Secret and the nats read are file-backed
/// as in the shell test; psql and pg_dump strip the exec head (asserted,
/// not skipped blind) and run against `$DB_BASE/<name>`.
fn write_stub(dir: &Path) -> PathBuf {
    let stub = dir.join("kubectl");
    write_exec(
        &stub,
        r#"#!/usr/bin/env bash
set -u
words=("$@")
if [ "${words[2]:-}" = get ] && [ "${words[3]:-}" = secret ]; then
    printf '%s' "$(cat "$STUB_SECRET")" | base64 -w0; exit 0
fi
if [ "${words[2]:-}" = patch ] && [ "${words[3]:-}" = secret ]; then
    p=""
    for ((i=0; i<${#words[@]}; i++)); do [ "${words[$i]}" = -p ] && p="${words[$((i+1))]}"; done
    printf '%s' "$p" | jq -r '.data."database-url"' | base64 -d > "$STUB_SECRET"
    echo 'secret/boss-secrets patched'; exit 0
fi
if [ "${words[2]:-}" = exec ] && [ "${words[3]:-}" = sts/nats ]; then
    echo 'error: unable to upgrade connection: container not found ("nats")' >&2; exit 1
fi
[ "$1 $2 $3 $4 $5 $6 $7" = "-n boss exec sts/postgres -c postgres --" ] \
    || { echo "stub kubectl: unexpected argv head: $*" >&2; exit 1; }
shift 7
tool="$1"; shift
db=""; args=()
while [ $# -gt 0 ]; do
    case "$1" in
        -U) shift 2 ;;
        -d) db="$2"; shift 2 ;;
        *) args+=("$1"); shift ;;
    esac
done
[ -n "$db" ] || { echo "stub kubectl: $tool without -d" >&2; exit 1; }
if [ "$tool" = pg_dump ]; then
    # On the forge pg_dump runs INSIDE the postgres container, so it
    # always matches the server. Here the harness's client may not
    # (bookworm's postgresql-client 15 against postgres:16, measured on
    # the dev pod and the gate image, 2026-09-16). Real pg_dump first;
    # on a version mismatch, a listing derived from the REAL catalog
    # stands in — one CREATE TABLE line per public table and the rows
    # of `companies` — and the path taken is recorded for the test.
    if err=$(pg_dump "$DB_BASE/$db" "${args[@]}" 2>&1 >"$STUB_DUMP_TMP"); then
        echo real > "$STUB_DUMP_PATH"; cat "$STUB_DUMP_TMP"; exit 0
    fi
    case "$err" in
        *"server version mismatch"*)
            echo substituted > "$STUB_DUMP_PATH"
            echo "-- substituted for pg_dump: $err"
            psql "$DB_BASE/$db" -X -q -At -v ON_ERROR_STOP=1 \
                -c "SELECT 'CREATE TABLE public.' || tablename || ' ();' FROM pg_tables WHERE schemaname = 'public' ORDER BY 1" \
                -c "SELECT 'COPY companies: ' || id || ' ' || name FROM companies"
            exit $? ;;
        *) printf '%s\n' "$err" >&2; exit 1 ;;
    esac
fi
exec "$tool" "$DB_BASE/$db" "${args[@]}"
"#,
    );
    stub
}

struct Run {
    code: i32,
    text: String,
}

/// The floor for these runs. The forge's 1 MiB floor is for a database
/// that is GBs; the harness's schema dumps to tens of KB, and the
/// substituted listing (see the stub) to a few KB gzipped.
const MIN_BYTES: &str = "1000";

fn run(dir: &Path, db: &TestDb, args: &[&str], tree: &str) -> Run {
    let stub = write_stub(dir);
    let out = Command::new("bash")
        .arg(repo_root().join(SCRIPT))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("BOSS_KUBECTL", stub.to_str().unwrap())
        .env("DB_BASE", base_url(db))
        .env("STUB_SECRET", dir.join("secret.url"))
        .env("STUB_DUMP_TMP", dir.join("dump.tmp"))
        .env("STUB_DUMP_PATH", dir.join("dump.path"))
        .env("BOSS_CONVERGE_HOLD", dir.join("hold"))
        .env("BOSS_SWITCH_BACKUP_DIR", dir.join("backups"))
        .env("BOSS_SWITCH_TREE", dir.join(tree))
        .env("BOSS_SWITCH_MAIN_REF", "HEAD")
        .env("BOSS_SWITCH_MIN_SNAPSHOT_BYTES", MIN_BYTES)
        .output()
        .expect("bash runs");
    Run {
        code: out.status.code().unwrap_or(-1),
        text: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// Dry run, then the real switch, then the same switch again, against
/// the harness's Postgres: the target is created for real with OWNER
/// boss and holds no table, the snapshot is a gzip of the real schema
/// the floor accepts, and the second run reuses the empty target.
#[tokio::test(flavor = "multi_thread")]
async fn the_switch_creates_a_real_empty_database_and_dumps_the_real_schema() {
    if !has("jq") || !has("pg_dump") {
        eprintln!("skipping: jq and pg_dump are needed on PATH");
        return;
    }
    let db = TestDb::new().await;
    let dir = scratch_dir("switch-instance-database-sql");
    let old = db.name().to_string();
    // TestDb's own naming, so an orphan is found by the same sweep AND
    // a live one is left alone by it. THE RACE THIS HAD (2026-09-19,
    // backlog 2a056500): `test_boss_switch_<pid>` carried the prefix and
    // no stamp, which the sweep every other test's `TestDb::new` runs
    // reads as ancient and drops as soon as no session is connected —
    // the gap between the script's psql invocations. Under the full
    // `--all-features` run this test redded 1-in-N (45 s under load
    // against 2 s alone made the gaps wide) with `FATAL: database
    // "test_boss_switch_1092622" does not exist — It seems to have just
    // been dropped or renamed`; reproduced on demand with the sweep's
    // query in a loop beside it.
    let new = scratch_database_name("switch");
    let password = "s3cretpw0123";
    write_file(
        &dir.join("secret.url"),
        &format!("postgres://boss:{password}@postgres.boss.svc.cluster.local:5432/{old}"),
    );
    write_file(&dir.join("hold"), "switch-instance-database\n");
    fixture_tree(&dir.join("tree"), &new);
    let base = base_url(&db);
    let admin = format!("{base}/postgres");

    // A row in the old database, so the dump carries data as well as
    // DDL and the "wrong database" floor has something to measure.
    sqlx::raw_sql("INSERT INTO companies (id, name) VALUES ('algedonic', 'Algedonic, LLC')")
        .execute(&db.pool)
        .await
        .expect("fixture row");

    // The dry run: every bound, against the real catalog.
    let r = run(&dir, &db, &["--dry-run", "boss", &new], "tree");
    assert_eq!(r.code, 0, "dry run:\n{}", r.text);
    assert!(
        !r.text.contains(password),
        "password in output:\n{}",
        r.text
    );
    assert!(
        r.text.contains(&format!(
            "target: {new} is absent — would CREATE DATABASE {new} OWNER boss"
        )),
        "{}",
        r.text
    );
    assert!(
        r.text.contains("DRY RUN — every bound passed"),
        "{}",
        r.text
    );
    assert_eq!(
        psql(
            &admin,
            &format!("SELECT count(*) FROM pg_database WHERE datname = '{new}'")
        ),
        "0",
        "a dry run created the database"
    );

    // The real switch. Every assertion after it is collected as a
    // verdict, and the created database is dropped whatever they say.
    let r = run(&dir, &db, &["--for-real", "boss", &new], "tree");
    let dump_path = std::fs::read_to_string(dir.join("dump.path")).unwrap_or_default();
    eprintln!("pg_dump leg: {}", dump_path.trim());
    let check = || -> Result<(), String> {
        if r.code != 0 {
            return Err(format!("real run exited {}:\n{}", r.code, r.text));
        }
        if r.text.contains(password) {
            return Err(format!("password in output:\n{}", r.text));
        }
        // Created for real, owned by boss, empty.
        let owner = psql(
            &admin,
            &format!("SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname = '{new}'"),
        );
        if owner != "boss" {
            return Err(format!("owner of {new} is '{owner}', not boss"));
        }
        let tables = psql(
            &format!("{base}/{new}"),
            "SELECT count(*) FROM pg_tables WHERE schemaname NOT IN ('pg_catalog', 'information_schema')",
        );
        if tables != "0" {
            return Err(format!("{new} holds {tables} tables"));
        }
        // The snapshot: one gzip, above the floor, carrying every
        // public table of the real schema and the fixture row.
        let snaps: Vec<PathBuf> = std::fs::read_dir(dir.join("backups"))
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        if snaps.len() != 1 {
            return Err(format!("snapshots: {snaps:?}"));
        }
        let bytes = std::fs::metadata(&snaps[0]).unwrap().len();
        if bytes < 1000 {
            return Err(format!("snapshot is {bytes} bytes"));
        }
        let count = |needle: &str| -> usize {
            let out = Command::new("sh")
                .args([
                    "-c",
                    &format!("gzip -dc '{}' | grep -c '{needle}'", snaps[0].display()),
                ])
                .output()
                .expect("gzip runs");
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .unwrap_or(0)
        };
        let tables_in_dump = count("CREATE TABLE");
        if tables_in_dump < 100 {
            return Err(format!(
                "the dump carries {tables_in_dump} CREATE TABLE statements"
            ));
        }
        if count("Algedonic, LLC") != 1 {
            return Err("the dump does not carry the fixture row".into());
        }
        // The Secret now names the new database, everything else intact.
        let secret = std::fs::read_to_string(dir.join("secret.url")).unwrap();
        let want = format!("postgres://boss:{password}@postgres.boss.svc.cluster.local:5432/{new}");
        if secret != want {
            return Err(format!("secret is {}", secret.replace(password, "***")));
        }
        Ok(())
    };
    let mut verdicts = vec![check()];

    // Again, with the Secret now naming the new database: refused as
    // "already names". Then with the Secret pointed back at the old
    // one: the empty target is reused, not re-created.
    let r2 = run(&dir, &db, &["--for-real", "boss", &new], "tree");
    verdicts.push(
        if r2.code == 2 && r2.text.contains(&format!("already names {new}")) {
            Ok(())
        } else {
            Err(format!("second run:\n{}", r2.text))
        },
    );
    write_file(
        &dir.join("secret.url"),
        &format!("postgres://boss:{password}@postgres.boss.svc.cluster.local:5432/{old}"),
    );
    let r3 = run(&dir, &db, &["--for-real", "boss", &new], "tree");
    verdicts.push(
        if r3.code == 0 && r3.text.contains(&format!("{new} exists and is empty")) {
            Ok(())
        } else {
            Err(format!("third run:\n{}", r3.text))
        },
    );

    // A non-empty target, judged by the real catalog: the schema-loaded
    // scratch database as the target, the maintenance db as the old.
    write_file(
        &dir.join("secret.url"),
        &format!("postgres://boss:{password}@postgres.boss.svc.cluster.local:5432/postgres"),
    );
    fixture_tree(&dir.join("tree-old"), &old);
    let r4 = run(&dir, &db, &["--for-real", "boss", &old], "tree-old");
    verdicts.push(
        if r4.code == 2
            && r4.text.contains(&format!("{old} exists and holds"))
            && r4.text.contains("table(s)")
            && !r4.text.contains(password)
        {
            Ok(())
        } else {
            Err(format!("non-empty target:\n{}", r4.text))
        },
    );

    let _ = Command::new("psql")
        .arg(&admin)
        .args(["-X", "-q", "-c", &format!("DROP DATABASE IF EXISTS {new}")])
        .status();
    for v in verdicts {
        v.unwrap();
    }
}
