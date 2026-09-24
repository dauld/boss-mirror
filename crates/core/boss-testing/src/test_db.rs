//! `TestDb` — per-test Postgres database with the Boss schema loaded.
//!
//! Each `TestDb::new()` call creates a fresh, randomly-named database
//! (`test_boss_<uuid>`) as a copy of a schema TEMPLATE, and returns a
//! connection pool.
//!
//! The schema — the per-module `infra/postgres/schema/` files, via the
//! `SCHEMA_FILES` list `build.rs` generates from that directory — is
//! loaded once per server, into `boss_tmpl_<fingerprint>`. Everything
//! after that is `CREATE DATABASE … TEMPLATE`, which copies files
//! instead of replaying 86 DDL scripts behind a cluster-wide lock. See
//! [`ensure_template`] for the measurement that motivated it and the
//! two properties that make reusing a template safe.
//!
//! Before any of that, every `TestDb::new` checks that the compiled
//! `SCHEMA_FILES` still describes the schema directory on disk, and
//! REFUSES if it does not — see [`assert_compiled_schema_is_current`].
//! A compiled list that is one migration behind does not announce
//! itself: the test that drops a column, renames a table or adds a
//! constraint simply passes, against the old schema, which is a false
//! green on exactly the change a DB-backed test exists to cover.
//!
//! On `Drop`, the database's name is handed to this process's reaper
//! thread, which drops it on a runtime of its own — so it outlives the
//! test's runtime — and drops everything queued together, so a batch
//! shares one forced checkpoint. See [`release_scratch_database`]. What
//! is still queued when the process exits is reclaimed by the stamped
//! orphan sweep, which runs on the same thread.
//!
//! ## Prerequisites
//!
//! - A reachable Postgres instance with a role that has `CREATEDB`.
//! - The admin URL (connection string pointing at the `postgres`
//!   database or any existing database) via the
//!   `BOSS_TEST_POSTGRES_ADMIN_URL` environment variable, or the default
//!   `postgres://boss:boss@127.0.0.1/postgres`.
//!
//! ## Usage
//!
//! ```ignore
//! #[tokio::test(flavor = "multi_thread")]
//! async fn my_integration_test() {
//!     let db = boss_testing::TestDb::new().await;
//!     sqlx::query("INSERT INTO accounts (id, name) VALUES ($1, $2)")
//!         .bind("account-1")
//!         .bind("Test Account")
//!         .execute(&db.pool)
//!         .await
//!         .unwrap();
//!     // ... exercise code under test against db.pool ...
//! }
//! ```

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender};

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const DEFAULT_ADMIN_URL: &str = "postgres://boss:boss@127.0.0.1/postgres";

/// The escape hatch, named so it cannot be set by accident.
const ALLOW_PRODUCTION_ENV: &str = "BOSS_TEST_ALLOW_PRODUCTION_SERVER";

/// Every scratch database starts with this.
/// How many times to reach for the admin connection before giving up.
///
/// Three, and the shape of the retry matters more than the number: a
/// lost connection is retried by RECONNECTING, never by reissuing on
/// the dead socket. One transient is what this is for; three failures
/// in a row is a server that is actually gone, and pretending
/// otherwise would turn a broken CI database into a slow one.
const CONNECT_ATTEMPTS: u32 = 3;

/// Is this the database going away, or the database saying no?
///
/// The distinction is the whole point. A rejected statement is a
/// defect in the change under test and must fail loudly on the first
/// try; a closed socket is weather, and failing on it reds a train
/// carrying other people's work. Before this existed the two produced
/// the same panic and an agent had to read the message to tell them
/// apart — which on 2026-08-16 meant an hour of archaeology to
/// conclude that a car was innocent.
///
/// Transport failures and the two SQLSTATEs that mean "not now, ask
/// again": 53300 too_many_connections, 57P03 cannot_connect_now.
/// 08006 connection_failure is included for the same reason.
fn is_transient(e: &sqlx::Error) -> bool {
    match e {
        // `expected to read 5 bytes, got 0 bytes at EOF` — the one
        // that actually happened — arrives here.
        sqlx::Error::Io(_) => true,
        sqlx::Error::Protocol(_) => true,
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
        sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(db) => {
            matches!(db.code().as_deref(), Some("53300" | "57P03" | "08006"))
        }
        // Everything else — RowNotFound, ColumnDecode, a syntax error
        // reaching us as Database with another code — is the change
        // under test being wrong, and must not be retried into
        // looking flaky.
        _ => false,
    }
}

async fn connect_admin(opts: &PgConnectOptions, admin_url: &str) -> PgConnection {
    let mut last = String::new();
    for attempt in 1..=CONNECT_ATTEMPTS {
        match PgConnection::connect_with(opts).await {
            Ok(c) => return c,
            Err(e) if is_transient(&e) && attempt < CONNECT_ATTEMPTS => {
                eprintln!(
                    "TestDb: admin connect failed (attempt {attempt}/{CONNECT_ATTEMPTS}): {e}"
                );
                last = e.to_string();
                tokio::time::sleep(std::time::Duration::from_millis(200 * u64::from(attempt)))
                    .await;
            }
            Err(e) => panic!("connecting to admin db at {admin_url}: {e}"),
        }
    }
    panic!("connecting to admin db at {admin_url} after {CONNECT_ATTEMPTS} attempts: {last}");
}

const SCRATCH_PREFIX: &str = "test_boss_";

/// The name for a scratch database a test creates for itself — the ONE
/// shape the orphan sweep understands: `test_boss_<secs>_<tag><hex>`.
///
/// A test that needs a database `TestDb` cannot hand it (an empty one
/// for `migrate.sh` to fill, the target of a `switch-instance-database`
/// run) still creates it on the shared server, and every `TestDb::new`
/// in every other test process sweeps that server. `is_orphan` reads a
/// name with no stamp as ancient — right for the legacy shape, and
/// fatal for a hand-rolled one: `test_boss_switch_<pid>` and
/// `test_boss_mig_<hex>` were both dropped mid-test the moment no
/// session was connected (2026-09-19, backlog 2a056500; reproduced on
/// demand: `FATAL: database "test_boss_mig_09977214ad4e" does not
/// exist — It seems to have just been dropped or renamed`). Naming
/// through here gives the database the same TTL as TestDb's own; the
/// tag keeps it identifiable in `pg_database` when it is left behind.
pub fn scratch_database_name(tag: &str) -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    format!("{SCRATCH_PREFIX}{}_{tag}{}", now_secs(), &suffix[..12])
}

/// How old a scratch database must be before another test process will
/// drop it. Generous on purpose: the point is to reclaim yesterday's
/// litter, not to race a suite that is running right now. A slow
/// DB-backed test is minutes; nothing legitimately holds a scratch
/// database for half an hour.
const ORPHAN_TTL_SECS: u64 = 1800;

/// Seconds since the epoch, for stamping a scratch database's name.
///
/// This is deliberately NOT the `clock.now()` every other "now" in BOSS
/// routes through, and it is not what `infra/lint/no-wallclock.sh`
/// forbids (that lint is about `Utc::now` stamping audit_log with
/// wallclock in a service). Nothing here reaches audit_log: it is test
/// harness bookkeeping about when a throwaway database was created, and
/// a sim clock would make the age meaningless.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The creation stamp encoded in a scratch database's name, if it has
/// one. `test_boss_<secs>_<hex>` → `Some(secs)`.
///
/// Names from before the stamp existed (`test_boss_<hex>`) have no
/// underscore after the prefix and return `None`. Those are treated as
/// ancient, which is right: every scratch database created since this
/// change carries a stamp, so an unstamped one is by construction left
/// over from an older process that is no longer running.
fn stamp_of(db_name: &str) -> Option<u64> {
    // Both litter-producing prefixes stamp the same way. A COMPLETED
    // template (`boss_tmpl_<fingerprint>`) matches neither and so is
    // never a sweep candidate — that is the point of it, it is meant to
    // outlive the process that built it.
    let rest = db_name
        .strip_prefix(BUILDING_PREFIX)
        .or_else(|| db_name.strip_prefix(SCRATCH_PREFIX))?;
    // `split_once` is what separates the two shapes: the stamped name
    // has an underscore here and the legacy one does not, so a legacy
    // suffix that happens to be all digits cannot be misread as a date.
    let (secs, suffix) = rest.split_once('_')?;
    if suffix.is_empty() {
        return None;
    }
    secs.parse::<u64>().ok()
}

/// Is this scratch database old enough for another process to drop?
///
/// Pure so the age decision is testable without a server — the same
/// reason `production_refusal` above is pure.
fn is_orphan(db_name: &str, now: u64, ttl_secs: u64) -> bool {
    match stamp_of(db_name) {
        // `saturating_sub` matters: a database stamped in the future
        // (clock skew between two machines sharing one Postgres) yields
        // 0, so it is left alone rather than dropped underneath its
        // owner.
        Some(created) => now.saturating_sub(created) > ttl_secs,
        None => true,
    }
}

/// Should `TestDb` refuse to run against this server?
///
/// THE INCIDENT THIS EXISTS FOR (2026-08-14). The admin URL defaults to
/// `127.0.0.1`, and a `kubectl port-forward` opened minutes earlier for
/// an unrelated read makes `127.0.0.1` the PRODUCTION cluster database.
/// A `cargo test --workspace` then created 582 scratch databases on the
/// production volume, filled the Longhorn PVC, and crashed Postgres into
/// recovery. It self-recovered in about forty seconds with no data lost
/// — 5,612 jobs, 388,260 audit rows, 24,367 messages all intact — but
/// that was luck, not design.
///
/// The failure is silent and inverted: nothing about running a test
/// suite suggests it will write to production, and the address that
/// makes it dangerous is the same address that makes it correct on a
/// laptop. So host and port cannot distinguish the two, and neither can
/// the database name — both are `postgres`.
///
/// What DOES distinguish them is what else lives on the server. A
/// scratch instance has no reason to hold a database called `boss`;
/// every BOSS deployment does. That single question separates the
/// cluster and boss-gcp (which must never be tested against) from a
/// throwaway `initdb` and from the dev container's sidecar, whose
/// POSTGRES_DB is `postgres`.
///
/// Pure so the decision is testable without a server, which is the
/// whole difficulty with guards like this one.
pub(crate) fn production_refusal(server_has_boss_db: bool, override_set: bool) -> Option<String> {
    if !server_has_boss_db || override_set {
        return None;
    }
    Some(format!(
        "REFUSING to create a test database: this server already hosts a database named \
         `boss`, which means it is a BOSS deployment and not a scratch instance.\n\n\
         TestDb creates a database per test. On 2026-08-14 that ran against the production \
         cluster through a forgotten `kubectl port-forward` — 582 scratch databases, a full \
         volume, and Postgres in recovery. The address is not the tell, because a laptop's \
         own Postgres and a port-forwarded production one are both 127.0.0.1.\n\n\
         Point BOSS_TEST_POSTGRES_ADMIN_URL at a scratch instance (the boss-dev pod's \
         sidecar, or a local `initdb` on a non-standard port), or close the port-forward. \
         If this really is a disposable server that happens to hold a `boss` database, set \
         {ALLOW_PRODUCTION_ENV}=1 and say why in the commit."
    ))
}

// `SCHEMA_FILES: &[(&str, &str)]` — the per-module schema files, in
// manifest apply order, each paired with its SQL. Generated by
// `build.rs` from infra/postgres/schema/*.sql, which is the one
// definition of the list; `migrate.sh` reads the same file at runtime.
// `include_str!` needs a literal path at compile time, which is why the
// build script emits the list rather than `test_db.rs` reading the
// manifest directly. `schema_sql` concatenates the entries so
// `new_without` can skip a module, mirroring migrate.sh's `--without`.
include!(concat!(env!("OUT_DIR"), "/schema_files.rs"));

// The migration ORDER and the one fingerprint function, shared with
// `build.rs` by inclusion so the two readers cannot disagree. They did
// once, over the width of an integer, and it redded a train — see the
// file's own header.
include!("../schema_order.rs");

/// Where `infra/postgres/schema` is, found from the RUNNING checkout.
///
/// Deliberately NOT `env!("CARGO_MANIFEST_DIR")`. That is baked in when
/// the crate is compiled and names the checkout that COMPILED it —
/// which in the failure this guards is a different checkout than the one
/// running the test, so the comparison would be a stale list against its
/// own stale directory, agreeing with itself. Cargo sets
/// `CARGO_MANIFEST_DIR` in the test PROCESS's environment too, and that
/// one names the crate under test here and now.
fn locate_schema_dir() -> Option<PathBuf> {
    let start = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    start
        .ancestors()
        .map(|dir| dir.join("infra/postgres/schema"))
        .find(|candidate| candidate.is_dir())
}

/// The ordered `(name, sql)` list as the directory holds it right now.
///
/// Same derivation as `build.rs`, same sort key, from the same file —
/// the directory is the definition and every reader derives the list
/// independently rather than consulting a copy.
fn read_schema_dir(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".sql"))
        .collect();
    names.sort_by_key(|name| schema_sort_key(name));
    names
        .into_iter()
        .map(|file| {
            let sql = std::fs::read_to_string(dir.join(&file))
                .map_err(|e| format!("reading {}: {e}", dir.join(&file).display()))?;
            Ok((file.strip_suffix(".sql").unwrap_or(&file).to_string(), sql))
        })
        .collect()
}

/// Does the schema list compiled into this binary still describe the
/// directory on disk? `None` when it does; otherwise the whole story.
///
/// Three readings are compared, not two:
/// - `SCHEMA_FILES_FINGERPRINT` — what `build.rs` read,
/// - `SCHEMA_FILES` — what rustc actually linked,
/// - `disk` — what is there now, in the checkout being tested.
///
/// The first pair disagreeing means the generator and the compiler saw
/// different files; the second means the artifact belongs to a
/// different tree or a different moment. Both end the same way: the
/// schema about to be applied is not the schema under test.
fn schema_disagreement(dir: &Path, disk: &[(String, String)]) -> Option<String> {
    let compiled_fp = schema_set_fingerprint(SCHEMA_FILES.iter().copied());
    let disk_fp =
        schema_set_fingerprint(disk.iter().map(|(name, sql)| (name.as_str(), sql.as_str())));
    if compiled_fp == disk_fp && compiled_fp == SCHEMA_FILES_FINGERPRINT {
        return None;
    }

    let compiled_names: Vec<&str> = SCHEMA_FILES.iter().map(|(name, _)| *name).collect();
    let disk_names: Vec<&str> = disk.iter().map(|(name, _)| name.as_str()).collect();
    let only_compiled: Vec<&str> = compiled_names
        .iter()
        .filter(|name| !disk_names.contains(name))
        .copied()
        .collect();
    let only_disk: Vec<&str> = disk_names
        .iter()
        .filter(|name| !compiled_names.contains(name))
        .copied()
        .collect();
    let changed: Vec<&str> = SCHEMA_FILES
        .iter()
        .filter_map(|(name, sql)| {
            disk.iter()
                .find(|(disk_name, _)| disk_name == name)
                .filter(|(_, disk_sql)| disk_sql != sql)
                .map(|_| *name)
        })
        .collect();
    let reordered = only_compiled.is_empty()
        && only_disk.is_empty()
        && changed.is_empty()
        && compiled_names != disk_names;

    let list = |label: &str, names: &[&str]| {
        if names.is_empty() {
            String::new()
        } else {
            format!("  {label}: {}\n", names.join(", "))
        }
    };
    let generator_note = if compiled_fp != SCHEMA_FILES_FINGERPRINT {
        "  build.rs and rustc disagreed: the generated list and the SQL compiled\n  \
         into it came from different readings of the directory.\n"
    } else {
        ""
    };

    Some(format!(
        "boss-testing: the schema compiled into this test binary is not the schema on disk.\n\
         \n\
         Every TestDb would load the compiled one, so a migration under test can be\n\
         silently ABSENT and its assertions evaluated against the old schema. That is a\n\
         false green on exactly the change a DB-backed test exists to cover, so this\n\
         refuses instead.\n\
         \n\
         compiled: {compiled_count} files, fingerprint {compiled_fp:#018x} \
         (build.rs read {SCHEMA_FILES_FINGERPRINT:#018x})\n\
         on disk:  {disk_count} files, fingerprint {disk_fp:#018x}  {dir}\n\
         {only_compiled_line}{only_disk_line}{changed_line}{reordered_line}{generator_note}\
         \n\
         Known cause, measured on the dev pod 2026-09-10: infra/dev-shared-target.sh points\n\
         every worktree on a machine at ONE CARGO_TARGET_DIR, and cargo's unit hash for\n\
         boss-testing is identical in each, so the rlib and build.rs's OUT_DIR are one\n\
         shared artifact owned by whichever checkout built last. This checkout is then\n\
         reported `Finished` with nothing recompiled, against its neighbour's schema list.\n\
         \n\
         Fix: `touch crates/core/boss-testing/build.rs` and rebuild, or give this checkout\n\
         its own CARGO_TARGET_DIR.\n",
        compiled_count = compiled_names.len(),
        disk_count = disk_names.len(),
        dir = dir.display(),
        only_compiled_line = list("only in the compiled list", &only_compiled),
        only_disk_line = list("only on disk", &only_disk),
        changed_line = list("same name, different SQL", &changed),
        reordered_line = if reordered {
            "  same files, different apply ORDER\n"
        } else {
            ""
        },
    ))
}

/// Refuse to load a schema that is not the one on disk.
///
/// Runs once per process — the answer cannot change while the binary
/// does not — and panics on disagreement so the refusal reaches the
/// test as a failure rather than a line in a log nobody reads. A panic
/// leaves the cell unset, so every later `TestDb::new` refuses too
/// instead of the first one carrying the whole message.
///
/// It does NOT refuse when the directory cannot be found or read:
/// absence is not disagreement, and a test binary run outside a checkout
/// has nothing to compare. That case warns and proceeds.
fn assert_compiled_schema_is_current() {
    static CHECKED: OnceLock<()> = OnceLock::new();
    CHECKED.get_or_init(|| match locate_schema_dir() {
        None => eprintln!(
            "TestDb: infra/postgres/schema not found above {:?} — the compiled schema \
             list could not be verified against disk",
            std::env::var_os("CARGO_MANIFEST_DIR")
        ),
        Some(dir) => match read_schema_dir(&dir) {
            Err(e) => eprintln!(
                "TestDb: {e} — the compiled schema list could not be verified against disk"
            ),
            Ok(disk) => {
                if let Some(report) = schema_disagreement(&dir, &disk) {
                    panic!("{report}");
                }
            }
        },
    });
}

/// Concatenate the schema files in manifest order, omitting any whose name
/// contains an entry in `without`.
fn schema_sql(without: &[&str]) -> String {
    SCHEMA_FILES
        .iter()
        .filter(|(name, _)| !without.iter().any(|w| name.contains(w)))
        .map(|(_, sql)| *sql)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Serializes the TEMPLATE BUILD across concurrent test processes,
/// because the schema creates a cluster-global role. See
/// [`ensure_template`].
const SCHEMA_LOAD_LOCK: i64 = 0x_b055_10ad;

/// Completed schema templates. Content-addressed: the name carries a
/// fingerprint of the exact SQL inside, so a schema change produces a
/// different name and a stale template can never be reused.
const TEMPLATE_PREFIX: &str = "boss_tmpl_";

/// A template part-way through its schema load. Renamed to
/// `TEMPLATE_PREFIX + fingerprint` only once the load has succeeded, so
/// a process killed mid-build leaves litter under this prefix rather
/// than a half-populated template that every later test would copy.
const BUILDING_PREFIX: &str = "boss_tmpl_building_";

/// A stable fingerprint of the schema SQL, for naming its template.
///
/// FNV-1a, from `schema_order.rs` — one implementation, because this
/// function used to hold a second copy of the same five lines and a hash
/// that lives twice can drift (CLAUDE.md §9a).
fn schema_fingerprint(schema: &str) -> String {
    let hash = fnv1a(FNV1A_OFFSET, schema.as_bytes());
    format!("{TEMPLATE_PREFIX}{hash:016x}")
}

pub struct TestDb {
    pub pool: PgPool,
    db_name: String,
    admin_url: String,
}

impl TestDb {
    /// Create a fresh database, load the full Boss schema into it, and
    /// return a pool. Panics on any setup failure — tests that need a
    /// DB can't meaningfully continue without one, so fail loud.
    pub async fn new() -> Self {
        Self::new_with(&[]).await
    }

    /// Like [`new`](Self::new) but omits the schema files whose name
    /// contains any entry in `without` (mirrors migrate.sh's
    /// `--without`). Lets a test prove the core + a subset of modules
    /// bootstrap with, e.g., the ledger absent: `new_without(&["ledger"])`.
    pub async fn new_without(without: &[&str]) -> Self {
        Self::new_with(without).await
    }

    async fn new_with(without: &[&str]) -> Self {
        // Before anything is created: is the schema about to be applied
        // the schema in this checkout? Refuses loudly if not, because
        // the alternative is a test that passes against the old one.
        assert_compiled_schema_is_current();

        let admin_url = std::env::var("BOSS_TEST_POSTGRES_ADMIN_URL")
            .unwrap_or_else(|_| DEFAULT_ADMIN_URL.to_string());

        // The stamp is what lets a later process tell litter from live
        // work without asking whether a test is still running.
        let db_name = scratch_database_name("");

        let admin_opts = PgConnectOptions::from_str(&admin_url)
            .unwrap_or_else(|e| panic!("parsing BOSS_TEST_POSTGRES_ADMIN_URL: {e}"));

        let mut admin = connect_admin(&admin_opts, &admin_url).await;

        // Before creating anything: is this a BOSS deployment? Asked on
        // the admin connection we already hold, so it costs one query
        // and no extra connection.
        let has_boss_db: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'boss')")
                .fetch_one(&mut admin)
                .await
                .unwrap_or_else(|e| panic!("probing for a production database: {e}"));
        if let Some(reason) = production_refusal(
            has_boss_db,
            std::env::var(ALLOW_PRODUCTION_ENV).as_deref() == Ok("1"),
        ) {
            panic!("{reason}");
        }

        // Reclaim earlier runs' litter — on the reaper thread, never on
        // this test's path. See `sweep_orphans`.
        reap(Reap::Sweep {
            admin_url: admin_url.clone(),
        });

        // The schema is loaded ONCE, into a template, and every test
        // database after that is a copy of it. See `ensure_template`.
        let schema = schema_sql(without);
        let template = schema_fingerprint(&schema);
        ensure_template(&mut admin, &admin_url, &admin_opts, &schema, &template).await;

        // Retried, because the failure here is usually not about this
        // statement. On 2026-08-16 this line killed train 47 with
        // `CREATE DATABASE …: expected to read 5 bytes, got 0 bytes at
        // EOF` — the server closing the socket — and took a car that
        // passed the entire gate locally down with it (70487141).
        let create = format!(r#"CREATE DATABASE "{db_name}" TEMPLATE "{template}""#);
        for attempt in 1..=CONNECT_ATTEMPTS {
            match admin.execute(create.as_str()).await {
                Ok(_) => break,
                Err(e) if is_transient(&e) && attempt < CONNECT_ATTEMPTS => {
                    eprintln!(
                        "TestDb: CREATE DATABASE {db_name} lost the connection \
                         (attempt {attempt}/{CONNECT_ATTEMPTS}): {e}"
                    );
                    // The connection is the thing that broke; a retry on
                    // it would fail the same way forever.
                    admin = connect_admin(&admin_opts, &admin_url).await;
                }
                // 3D000 undefined_database: the template was dropped
                // between our check and this statement — an operator
                // clearing `boss_tmpl_*` by hand while a suite runs.
                // Rebuild it and try once more rather than failing a
                // test over someone else's housekeeping.
                Err(e) if is_missing_template(&e) && attempt < CONNECT_ATTEMPTS => {
                    eprintln!("TestDb: template {template} vanished; rebuilding");
                    ensure_template(&mut admin, &admin_url, &admin_opts, &schema, &template).await;
                }
                Err(e) => panic!("CREATE DATABASE {db_name} TEMPLATE {template}: {e}"),
            }
        }

        // Test sessions write audit_log events directly (via
        // PgAuditWriter / seed helpers) without running the projection
        // pipeline that populates the soft-FK parent tables (accounts,
        // vendors, …). The `audit_log_check_refs` BEFORE INSERT trigger
        // would therefore reject every invoice / vendor-invoice event.
        // Disable the check at the DB level — the same escape hatch
        // bundle-import uses (`audit_log.ref_check = 'off'`, see
        // boss-rebuild). The soft-FK *integrity scan*
        // (`check_audit_log_integrity`) runs independently and stays
        // under test.
        admin
            .execute(
                format!(r#"ALTER DATABASE "{db_name}" SET audit_log.ref_check = 'off'"#).as_str(),
            )
            .await
            .unwrap_or_else(|e| panic!("disable ref_check on {db_name}: {e}"));

        // Close admin connection before opening the test DB.
        drop(admin);

        let test_opts = admin_opts.clone().database(&db_name);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(test_opts)
            .await
            .unwrap_or_else(|e| panic!("connecting to test db {db_name}: {e}"));

        // No schema load here: the database arrived with the schema
        // already in it, copied from the template.

        Self {
            pool,
            db_name,
            admin_url,
        }
    }

    /// Database name, primarily for debugging.
    pub fn name(&self) -> &str {
        &self.db_name
    }

    /// Connection URL for this scratch database.
    ///
    /// `pool` covers everything in-process; this exists for the tests
    /// that need to hand the database to a SUBPROCESS — a binary whose
    /// exit code is the behaviour under test can only be checked by
    /// running it.
    ///
    /// Derived from the admin URL with the database segment swapped, so
    /// host, port, credentials and query parameters are whatever the
    /// harness itself connected with. Never a second source of truth.
    pub fn url(&self) -> String {
        with_database(&self.admin_url, &self.db_name)
    }
}

/// `admin_url` pointed at `db_name` instead of whatever database it
/// named. Everything else — scheme, credentials, host, port, query
/// parameters — is carried through untouched.
pub(crate) fn with_database(admin_url: &str, db_name: &str) -> String {
    let (scheme, rest) = admin_url
        .split_once("://")
        .unwrap_or(("postgres", admin_url));
    let (authority_and_path, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    // Split at the FIRST '/', which ends the authority. An admin URL
    // that names no database has none, and then the authority is the
    // whole of it.
    let authority = authority_and_path
        .split_once('/')
        .map_or(authority_and_path, |(a, _)| a);
    match query {
        Some(q) => format!("{scheme}://{authority}/{db_name}?{q}"),
        None => format!("{scheme}://{authority}/{db_name}"),
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let db_name = std::mem::take(&mut self.db_name);
        let admin_url = std::mem::take(&mut self.admin_url);
        if db_name.is_empty() {
            return;
        }
        release_scratch_database(&admin_url, &db_name);
    }
}

/// Hand a scratch database its owner is finished with to the reaper,
/// which drops it — `WITH (FORCE)`, so a pool still closing cannot hold
/// it — without the caller waiting.
///
/// THE LEAK THIS REPLACES (backlog 9e1ea321, 2026-09-23). `Drop` used to
/// `handle.spawn` the drop onto the test's own runtime, and a
/// `#[tokio::test]` runtime is torn down the moment the test function
/// returns, so the task was cancelled before it ran. Every scratch
/// database leaked, always; the 30-minute orphan sweep was the only
/// thing that ever reclaimed one. Measured on the dev pod's harness
/// Postgres that day: 423 `test_boss_*` databases holding 6.2 GB, and
/// 1,423 forced checkpoints in five hours against 47 timed ones.
///
/// For a test that creates a database `TestDb` cannot hand it (named
/// through [`scratch_database_name`]), this is the drop to use: a test
/// that issues its own `DROP DATABASE` waits for a checkpoint of the
/// whole server, and pays for one alone.
pub fn release_scratch_database(admin_url: &str, db_name: &str) {
    reap(Reap::Release {
        admin_url: admin_url.to_string(),
        db_name: db_name.to_string(),
    });
}

/// Work for the reaper thread.
enum Reap {
    /// Reclaim this server's stamped orphans — once per process.
    Sweep { admin_url: String },
    /// Drop a database whose owner has let it go.
    Release { admin_url: String, db_name: String },
}

/// How many `DROP DATABASE`s the reaper has in flight at once.
///
/// WHY CONCURRENT, NOT SERIAL (backlog 9e1ea321). Every `DROP DATABASE`
/// ends in `RequestCheckpoint(IMMEDIATE | FORCE | WAIT)` — Postgres must
/// make the checkpointer forget the dead files — and a checkpoint on a
/// busy server flushes and fsyncs everything dirty on it: about 40 s
/// each on the dev pod while this was written, the checkpointer in
/// `DataFileSync` and io pressure at 24% full. But every request that
/// arrives while one checkpoint runs is satisfied by the NEXT one, so
/// drops issued together share it. Measured on that server with empty
/// databases, 2026-09-23: 16 serial drops forced 16 checkpoints and 16
/// concurrent drops forced 2; 8 serial forced 15 (other sweepers were
/// dropping too) and 8 concurrent forced 2.
///
/// Eight, not more: each drop holds a connection, and a gate's test
/// binary can already hold dozens against the sidecar's default
/// `max_connections` of 100.
const REAP_WIDTH: usize = 8;

/// Queue work for the reaper thread, starting it on first use.
///
/// A thread with its OWN runtime is the whole fix: it is not torn down
/// when a test's runtime is, so a drop queued in `Drop` actually runs.
/// Process-global because the thing it serves is — every `TestDb` in a
/// test binary shares one server, and one queue is what lets their
/// drops be batched. Best-effort throughout: if the thread cannot start
/// or the queue is gone, the database waits for the orphan sweep, which
/// is exactly where every database went before this existed.
fn reap(work: Reap) {
    static REAPER: OnceLock<Option<Sender<Reap>>> = OnceLock::new();
    let reaper = REAPER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        match std::thread::Builder::new()
            .name("testdb-reaper".to_string())
            .spawn(move || run_reaper(rx))
        {
            Ok(_) => Some(tx),
            Err(e) => {
                eprintln!(
                    "TestDb: no reaper thread ({e}); scratch databases will wait for the orphan sweep"
                );
                None
            }
        }
    });
    if let Some(tx) = reaper {
        let _ = tx.send(work);
    }
}

/// The reaper's loop: block for work, take everything else already
/// queued with it, and drop it all together.
fn run_reaper(rx: Receiver<Reap>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!(
                "TestDb: the reaper has no runtime ({e}); scratch databases will wait for the orphan sweep"
            );
            return;
        }
    };
    let mut swept: HashSet<String> = HashSet::new();
    while let Ok(first) = rx.recv() {
        let mut released: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut sweeps: Vec<String> = Vec::new();
        for work in std::iter::once(first).chain(rx.try_iter()) {
            match work {
                Reap::Release { admin_url, db_name } => {
                    released.entry(admin_url).or_default().push(db_name);
                }
                Reap::Sweep { admin_url } => {
                    if swept.insert(admin_url.clone()) {
                        sweeps.push(admin_url);
                    }
                }
            }
        }
        rt.block_on(async {
            for (admin_url, names) in released {
                drop_together(&admin_url, names, true).await;
            }
            for admin_url in sweeps {
                sweep_orphans(&admin_url, now_secs()).await;
            }
        });
    }
}

/// Did this fail because the template database is not there?
///
/// 3D000 `undefined_database`. Split out from [`is_transient`] on
/// purpose: this is not weather and it is not a defect in the change
/// under test, it is a third thing — someone removed a cache — and it
/// has its own recovery, which is to rebuild rather than to wait.
fn is_missing_template(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("3D000"))
}

/// Build the schema template if this server does not already have it.
///
/// THE MEASUREMENT THIS EXISTS FOR (2026-08-26). Every DB-backed test
/// used to create an empty database and replay all 86 schema files into
/// it — and, because the schema creates a cluster-global role, those
/// replays were serialized behind one advisory lock for the whole
/// server. So the test suite was neither CPU-bound nor even
/// Postgres-throughput-bound: it was bound on a single queue that every
/// one of ~494 DB-backed tests had to pass through one at a time. PR
/// #116's CI ran 50+ minutes on 16 cores at load average 2.38 — about
/// 15% utilisation — and adding cores to the gate did not move it,
/// because cores were never the constraint.
///
/// So the schema is applied ONCE, to a template, and each test database
/// is `CREATE DATABASE … TEMPLATE` — Postgres copying files rather than
/// planning and executing DDL. The lock is still held here, for the
/// build, which is the only place the cluster-global CREATE ROLE now
/// runs; the copies need no lock at all because they touch nothing
/// shared.
///
/// ROLES ARE CLUSTER-GLOBAL, AND THE SCHEMA CREATES ONE.
/// `111-gateway-audit-events.sql` creates the `boss_gateway_audit` login
/// role inside a DO block that swallows `duplicate_object`. That is the
/// right guard for a re-run and the wrong one for a RACE: two
/// simultaneous loads both see no such role, both issue CREATE ROLE, and
/// the loser is refused by the shared catalog index with
/// `unique_violation` (pg_authid_rolname_index) — a different SQLSTATE,
/// so the DO block does not catch it and the load panics. It surfaced as
/// `test` failing on train after train in August 2026 while every car
/// passed alone, because parallelism decides whether it fires. The
/// migration itself cannot be fixed: applied migrations are hashed, and
/// editing one takes production down at the next deploy (2026-08-13, an
/// hour with no system of record, over a COMMENT in this very file).
///
/// TWO PROPERTIES MAKE REUSE SAFE.
///
/// *Content addressing.* The name is a fingerprint of the exact SQL, so
/// a schema change cannot be served a stale template — it asks for a
/// different name. `new_without` falls out of this for free: a
/// different `without` set is different SQL and gets its own template.
///
/// *Build-then-rename.* The schema loads into `boss_tmpl_building_*`
/// and is renamed to its final name only after the load succeeds.
/// Without that, a process killed mid-load would leave a database with
/// the right name and half the tables, and every later test on that
/// server would copy it — a corruption that survives the process that
/// caused it. The rename is atomic; the abandoned build is swept as
/// litter like any other.
async fn ensure_template(
    admin: &mut PgConnection,
    admin_url: &str,
    admin_opts: &PgConnectOptions,
    schema: &str,
    template: &str,
) {
    if database_exists(admin, template).await {
        return;
    }

    // Serialize the build. The lock lives on its own connection so it
    // is released by closing that connection even if we panic between
    // here and the unlock.
    let lock = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url)
        .await
        .unwrap_or_else(|e| panic!("connecting for the schema lock: {e}"));
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(SCHEMA_LOAD_LOCK)
        .execute(&lock)
        .await
        .unwrap_or_else(|e| panic!("taking the schema-load lock: {e}"));

    // Re-check under the lock: whoever held it before us was very
    // likely building the same template, and the whole point is that
    // the load happens once.
    let outcome = if database_exists(admin, template).await {
        Ok(())
    } else {
        build_template(admin, admin_opts, schema, template).await
    };

    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(SCHEMA_LOAD_LOCK)
        .execute(&lock)
        .await;
    lock.close().await;

    // Panic AFTER releasing the lock, so a schema that cannot load
    // fails this test instead of hanging every other process on the
    // server behind a lock nobody will release.
    outcome.unwrap_or_else(|e| panic!("building schema template {template}: {e}"));
}

/// Load the schema into a scratch name and rename it into place.
async fn build_template(
    admin: &mut PgConnection,
    admin_opts: &PgConnectOptions,
    schema: &str,
    template: &str,
) -> Result<(), sqlx::Error> {
    let suffix = Uuid::new_v4().simple().to_string();
    let building = format!("{BUILDING_PREFIX}{}_{}", now_secs(), &suffix[..12]);

    admin
        .execute(format!(r#"CREATE DATABASE "{building}""#).as_str())
        .await?;

    // The schema must load in the same session settings the old
    // per-test load had. `audit_log.ref_check` is set per database and
    // is NOT copied by `CREATE DATABASE … TEMPLATE` (per-database
    // settings are keyed by OID, and a copy gets a new one), so the
    // test databases still set it for themselves — see `new_with`.
    // Setting it here as well is what keeps the template build itself
    // faithful to the environment the schema used to load in.
    admin
        .execute(format!(r#"ALTER DATABASE "{building}" SET audit_log.ref_check = 'off'"#).as_str())
        .await?;

    // Scoped so the pool is closed — and every connection with it —
    // before the rename. `ALTER DATABASE … RENAME TO` is refused while
    // anyone is connected.
    //
    // On failure the `?` leaves the half-built database behind for the
    // sweeper rather than blocking on cleanup: it carries a stamp and
    // holds no connections, and it does NOT carry the template's name,
    // so nothing will ever copy it.
    {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(admin_opts.clone().database(&building))
            .await?;
        let loaded = sqlx::raw_sql(schema).execute(&pool).await;
        pool.close().await;
        loaded?;
    }

    rename_into_place(admin, &building, template).await
}

/// `ALTER DATABASE … RENAME TO`, retried past a backend that has not
/// finished exiting.
///
/// `PgPool::close` returns when the client sockets are closed, which is
/// not quite when Postgres has reaped the backends behind them. Rename
/// refuses (55006 `object_in_use`) while any session remains, so a
/// close-then-rename can lose that race — rarely, and only under load,
/// which is the worst way for it to fail: the whole suite would flake
/// on its very first test with a message about a database name nobody
/// recognises. Terminate the stragglers and give them a moment.
async fn rename_into_place(
    admin: &mut PgConnection,
    building: &str,
    template: &str,
) -> Result<(), sqlx::Error> {
    const RENAME_RETRIES: u32 = 4;
    let rename = format!(r#"ALTER DATABASE "{building}" RENAME TO "{template}""#);
    for attempt in 1..=RENAME_RETRIES {
        match admin.execute(rename.as_str()).await {
            Ok(_) => return Ok(()),
            Err(e) if is_in_use(&e) => {
                let _ = sqlx::query(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = $1 AND pid <> pg_backend_pid()",
                )
                .bind(building)
                .execute(&mut *admin)
                .await;
                tokio::time::sleep(std::time::Duration::from_millis(100 * u64::from(attempt)))
                    .await;
            }
            Err(e) => return Err(e),
        }
    }
    // The last word, and the one whose error the caller sees.
    admin.execute(rename.as_str()).await.map(|_| ())
}

/// 55006 `object_in_use` — someone is still connected to the database
/// this statement wants exclusive access to.
fn is_in_use(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("55006"))
}

/// Does a database with this exact name exist on the server?
async fn database_exists(admin: &mut PgConnection, name: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(name)
        .fetch_one(&mut *admin)
        .await
        .unwrap_or(false)
}

/// Drop scratch databases left behind by earlier test processes.
///
/// WHY CLEANUP CANNOT LIVE ONLY ON `Drop` (filed f22369c4). A process
/// killed mid-suite, a panic that aborts, and the releases still queued
/// when a test binary exits all leave databases nobody will drop.
/// Measured 2026-08-15: a scratch instance created at 16:30Z held 196
/// databases and 3.0G by 17:15Z. So a process also cleans up after its
/// predecessors — once per process per server, on the reaper thread.
///
/// Two conditions, and both are needed. Age alone would race a suite
/// running elsewhere against the same server; "no active connections"
/// alone would race the gap between `CREATE DATABASE` and the pool's
/// first connect in a parallel process. Requiring both means a database
/// must be older than the TTL AND have nobody attached — a freshly
/// created one fails the age test no matter what its connections are
/// doing. It is also why these drops are NOT `WITH (FORCE)`: a session
/// on an old database belongs to someone, and the drop yields to it.
///
/// OFF THE TEST'S PATH, AND ONE SWEEPER AT A TIME (backlog 9e1ea321).
/// This ran inline in `TestDb::new`, dropping serially, so a test's
/// setup waited out one forced checkpoint per orphan — the packet's
/// "DB-backed tests wait behind a sweep of hundreds" (a gate pod went
/// from 283 to 114 in fifteen seconds while its tests waited). And
/// every test process swept the same list at once: on 2026-09-23 four
/// sessions sat on one `DROP DATABASE`, three on its lock and one on
/// its checkpoint. Now it runs on the reaper, concurrently in windows,
/// under a session advisory lock taken with `pg_try_advisory_lock` — a
/// process that cannot take it skips, because someone else is already
/// doing the same work, and the lock dies with the sweeper's connection.
///
/// Best-effort throughout: a concurrent drop may remove a row between
/// our SELECT and our DROP, and that is fine. Never panics — a test
/// must not fail because someone else's litter would not go away.
async fn sweep_orphans(admin_url: &str, now: u64) {
    let Ok(opts) = PgConnectOptions::from_str(admin_url) else {
        return;
    };
    // Held for the whole sweep: the advisory lock lives on it.
    let Ok(mut admin) = PgConnection::connect_with(&opts).await else {
        return;
    };
    let sweeping: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(SWEEP_LOCK)
        .fetch_one(&mut admin)
        .await
        .unwrap_or(false);
    if !sweeping {
        return;
    }
    let orphans = orphan_candidates(&mut admin, now).await;
    drop_together(admin_url, orphans, false).await;
}

/// The databases a sweep at `now` would drop.
///
/// Two shapes of litter, one query. `boss_tmpl_building_%` is a
/// template whose schema load never finished; a COMPLETED template
/// (`boss_tmpl_<fingerprint>`) matches neither pattern and is
/// deliberately kept — it is the cache that makes the suite fast, it is
/// content-addressed so it cannot go stale, and it costs one
/// schema-sized database per schema version on the server.
///
/// NOT ONE WHOSE DROP IS ALREADY IN FLIGHT (backlog 9469bc69,
/// 2026-09-24). The sweep lock dies with the sweeper's connection, but
/// its `DROP DATABASE` backends do not: a test binary that exits
/// mid-sweep leaves them waiting on a slow forced checkpoint, connected
/// to `postgres` and so invisible to the `pg_stat_activity` test. Every
/// later sweeper issued the same drops again, until 77 of the server's
/// 100 connections were 11 copies each of drops on 7 databases.
///
/// The in-flight drop is read from `pg_locks`, not from the query text
/// in `pg_stat_activity`. `dropdb` takes an AccessExclusiveLock on the
/// database object before it does anything else and holds it through
/// the checkpoint to commit, and every drop queued behind it shows the
/// same lock ungranted — so the granted drop and all its duplicates are
/// there, as structure. The query text is a worse witness: it is
/// `<insufficient privilege>` for another role's session, absent when
/// `track_activities` is off, truncated at `track_activity_query_size`,
/// and would have to be pattern-matched against a quoted name.
/// AccessExclusive, not any lock: on a database object only a drop, a
/// rename or a `SET TABLESPACE` move takes it, so a `COMMENT`, a
/// template copy or a connect in progress does not hide litter, and a
/// database a rename or a move is working on is not one to touch either.
///
/// The reaper's own `Release` drops need nothing further: each names a
/// database only its owner releases, once, so they cannot duplicate one
/// another; and a released drop still waiting when its process exits is
/// excluded here like any other.
async fn orphan_candidates(admin: &mut PgConnection, now: u64) -> Vec<String> {
    let candidates: Vec<String> = sqlx::query_scalar(
        "SELECT d.datname FROM pg_database d \
         WHERE (d.datname LIKE 'test\\_boss\\_%' OR d.datname LIKE 'boss\\_tmpl\\_building\\_%') \
           AND NOT EXISTS (SELECT 1 FROM pg_stat_activity a WHERE a.datname = d.datname) \
           AND NOT EXISTS (SELECT 1 FROM pg_locks l \
                           WHERE l.locktype = 'object' \
                             AND l.classid = 'pg_database'::regclass \
                             AND l.objid = d.oid \
                             AND l.mode = 'AccessExclusiveLock')",
    )
    .fetch_all(&mut *admin)
    .await
    .unwrap_or_default();

    candidates
        .into_iter()
        .filter(|n| is_orphan(n, now, ORPHAN_TTL_SECS))
        .collect()
}

/// Serializes the orphan sweep across processes sharing a server. Not
/// [`SCHEMA_LOAD_LOCK`]: a sweep must never hold up a template build.
const SWEEP_LOCK: i64 = 0x_b055_5eed;

/// Drop these databases [`REAP_WIDTH`] at a time, so each window shares
/// its forced checkpoint instead of paying one per database.
async fn drop_together(admin_url: &str, names: Vec<String>, force: bool) {
    let Ok(opts) = PgConnectOptions::from_str(admin_url) else {
        return;
    };
    for window in names.chunks(REAP_WIDTH) {
        let mut in_flight = tokio::task::JoinSet::new();
        for name in window {
            let opts = opts.clone();
            let name = name.clone();
            in_flight.spawn(async move { drop_one(&opts, &name, force).await });
        }
        while in_flight.join_next().await.is_some() {}
    }
}

async fn drop_one(opts: &PgConnectOptions, db_name: &str, force: bool) {
    let Ok(mut conn) = PgConnection::connect_with(opts).await else {
        return;
    };
    // Quote the identifier; never interpolate it anywhere a name could
    // be read as SQL. These come from pg_database or from
    // `scratch_database_name`, but the habit is the point. `FORCE`
    // terminates the sessions of a database its owner released — the
    // pool's sockets may still be closing — where the old path issued a
    // separate pg_terminate_backend first.
    let with = if force { " WITH (FORCE)" } else { "" };
    let _ = conn
        .execute(format!(r#"DROP DATABASE IF EXISTS "{db_name}"{with}"#).as_str())
        .await;
}

#[cfg(test)]
mod generated_schema_list {
    use super::{
        SCHEMA_FILES, locate_schema_dir, read_schema_dir, schema_disagreement,
        schema_set_fingerprint, schema_sort_key,
    };

    /// The generated list must name the manifest's files, in order.
    ///
    /// This used to pin two hand-maintained lists to each other, after
    /// they drifted: `42-views.sql` and `43-event-facts.sql` reached
    /// the manifest and never reached the Rust copy, so every
    /// TestDb-backed test ran against a schema missing those tables.
    /// The failure reads as "relation does not exist" in a test that
    /// has nothing to do with schema loading — a long way from the
    /// cause.
    ///
    /// `build.rs` now generates the list from the schema DIRECTORY, so
    /// that drift is impossible by construction and the old equality
    /// test would be comparing the directory to itself. The test is
    /// kept, repurposed, because generation introduces a *new* failure
    /// the hand-maintained arrangement could not have: a stale or empty
    /// `OUT_DIR` artifact.
    ///
    /// It now asserts exactly what `TestDb::new` asserts, through the
    /// same function — which is the point. Before, the guard lived only
    /// here, in a `#[cfg(test)]` module of boss-testing itself, so a
    /// builder running `cargo test -p boss-jobs --features postgres`
    /// never executed it and a stale list reached every one of those
    /// tests unchecked. This is the pin; `assert_compiled_schema_is_current`
    /// is the refusal the rest of the workspace gets.
    #[test]
    fn generated_list_matches_the_schema_directory() {
        let dir = locate_schema_dir().expect("infra/postgres/schema is above this crate");
        let disk = read_schema_dir(&dir).expect("the schema directory reads");
        assert!(
            !SCHEMA_FILES.is_empty(),
            "SCHEMA_FILES is empty — every TestDb would load an empty schema"
        );
        if let Some(report) = schema_disagreement(&dir, &disk) {
            panic!("{report}");
        }
    }

    /// The fingerprint must separate lists that a naive concatenation
    /// would hash alike — a byte moved across an entry boundary is a
    /// different migration set, not the same one spelled differently.
    #[test]
    fn a_byte_moved_across_an_entry_boundary_is_a_different_schema() {
        assert_ne!(
            schema_set_fingerprint([("ab", "c")]),
            schema_set_fingerprint([("a", "bc")]),
            "length-prefixing is what keeps these apart"
        );
        assert_ne!(
            schema_set_fingerprint([("01-a", "x"), ("02-b", "y")]),
            schema_set_fingerprint([("02-b", "y"), ("01-a", "x")]),
            "apply ORDER is part of the schema, so it is part of the fingerprint"
        );
        assert_eq!(
            schema_set_fingerprint([("01-a", "x")]),
            schema_set_fingerprint([("01-a", "x")]),
            "the same list must fingerprint the same, in any process"
        );
    }

    /// The shape that redded train a20dd59f on 2026-09-09: a
    /// fourteen-digit prefix beside a twelve-digit one. Numeric order
    /// and string order disagree here, so a reader that overflows its
    /// integer and falls back to the string sorts them the other way
    /// round. Two files, one assertion, and the class cannot come back
    /// silently.
    #[test]
    fn a_wider_prefix_still_sorts_by_its_number() {
        let mut names = vec![
            "202609090025-a-flush-job-records-who-worked-it.sql".to_string(),
            "20260908234904-migration-prefixes-carry-seconds.sql".to_string(),
            "03-jobs.sql".to_string(),
            "reference-data.sql".to_string(),
        ];
        names.sort_by_key(|n| schema_sort_key(n));
        assert_eq!(
            names,
            vec![
                "03-jobs.sql".to_string(),
                "202609090025-a-flush-job-records-who-worked-it.sql".to_string(),
                "20260908234904-migration-prefixes-carry-seconds.sql".to_string(),
                "reference-data.sql".to_string(),
            ],
            "the numeric prefix decides, a wider one sorts later, and a file with no prefix sorts last"
        );
    }
}

/// The production guard's decision, exercised without a server —
/// which is the difficulty with guards like this: the dangerous case
/// is precisely the one you must not set up to test.
#[cfg(test)]
mod orphan_sweep {
    use super::{ORPHAN_TTL_SECS, is_orphan, scratch_database_name, stamp_of};

    const NOW: u64 = 1_786_800_000;

    /// THE RACE TWO TESTS HAD (2026-09-19, backlog 2a056500). A test
    /// that names its own scratch database `test_boss_switch_<pid>` or
    /// `test_boss_mig_<hex>` has written the LEGACY shape: no stamp, so
    /// `is_orphan` reads it as ancient and the sweep every
    /// `TestDb::new` runs drops it the moment no session is connected —
    /// which is every gap between two psql invocations. Reproduced on
    /// demand with the sweeper's query in a loop beside each test:
    /// `FATAL: database "test_boss_mig_09977214ad4e" does not exist —
    /// It seems to have just been dropped or renamed`, and the same for
    /// `test_boss_switch_1092622`. Under the full `--all-features` run
    /// the gaps are wide enough to lose 1-in-N; alone, nothing sweeps.
    #[test]
    fn a_hand_rolled_name_without_a_stamp_is_swept_at_once() {
        assert!(is_orphan("test_boss_switch_1092622", NOW, ORPHAN_TTL_SECS));
        assert!(is_orphan(
            "test_boss_mig_09977214ad4e",
            NOW,
            ORPHAN_TTL_SECS
        ));
    }

    /// The pin behind the door (CLAUDE.md §9a: a fact that lives twice
    /// gets an equality test). A `test_boss_` literal in any test file
    /// of this workspace is a name the sweep will read as ancient; the
    /// one place the prefix may be spelled is this module, and a test
    /// that needs its own database calls `scratch_database_name`.
    #[test]
    fn no_test_file_spells_the_scratch_prefix_itself() {
        let root = crate::repo_root().join("crates");
        let mut offenders = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read a crates dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs")
                    && !path.ends_with("boss-testing/src/test_db.rs")
                    && std::fs::read_to_string(&path)
                        .expect("read a source file")
                        .lines()
                        // A comment may quote the failure text; code
                        // may not spell the name.
                        .filter(|l| !l.trim_start().starts_with("//"))
                        .any(|l| l.contains("test_boss_"))
                {
                    offenders.push(path);
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these files spell a scratch database name by hand; the sweep reads an \
             unstamped test_boss_* name as litter and drops it mid-test — name it \
             through boss_testing::test_db::scratch_database_name instead:\n{}",
            offenders
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// The door: a tagged name from `scratch_database_name` is stamped
    /// like TestDb's own, so it lives its TTL and ages out like any
    /// other litter, and the tag still says which test made it.
    #[test]
    fn a_tagged_scratch_name_is_stamped_and_lives_its_ttl() {
        let name = scratch_database_name("switch");
        let stamp = stamp_of(&name).expect("a tagged scratch name carries a stamp");
        assert!(!is_orphan(&name, stamp, ORPHAN_TTL_SECS));
        assert!(!is_orphan(&name, stamp + ORPHAN_TTL_SECS, ORPHAN_TTL_SECS));
        assert!(is_orphan(
            &name,
            stamp + ORPHAN_TTL_SECS + 1,
            ORPHAN_TTL_SECS
        ));
        assert!(name.contains("_switch"), "the tag names the test: {name}");
        assert!(
            name.len() <= 63,
            "a Postgres identifier is at most 63 bytes: {name}"
        );
        assert_ne!(
            name,
            scratch_database_name("switch"),
            "two calls in one second must not collide"
        );
    }

    #[test]
    fn a_stamped_name_carries_its_creation_time() {
        assert_eq!(stamp_of("test_boss_1786800000_abc123def456"), Some(NOW));
    }

    #[test]
    fn a_legacy_unstamped_name_has_no_time() {
        // The shape TestDb wrote before the stamp existed.
        assert_eq!(stamp_of("test_boss_abc123def456"), None);
    }

    #[test]
    fn a_legacy_all_digit_suffix_is_not_mistaken_for_a_stamp() {
        // The uuid suffix is hex, so it can be all digits. Read as a
        // date this would be the year 5882 and the database would never
        // be swept; the underscore is what distinguishes the shapes.
        assert_eq!(stamp_of("test_boss_123456789012"), None);
        assert!(
            is_orphan("test_boss_123456789012", NOW, ORPHAN_TTL_SECS),
            "an unstamped legacy database is litter and must be swept"
        );
    }

    #[test]
    fn a_fresh_database_is_never_swept() {
        let fresh = format!("test_boss_{NOW}_abc123def456");
        assert!(!is_orphan(&fresh, NOW, ORPHAN_TTL_SECS));
        // Still live one second before the TTL expires.
        assert!(!is_orphan(&fresh, NOW + ORPHAN_TTL_SECS, ORPHAN_TTL_SECS));
    }

    #[test]
    fn a_database_older_than_the_ttl_is_swept() {
        let old = format!("test_boss_{NOW}_abc123def456");
        assert!(is_orphan(&old, NOW + ORPHAN_TTL_SECS + 1, ORPHAN_TTL_SECS));
    }

    #[test]
    fn a_future_stamp_is_left_alone() {
        // Clock skew between two machines sharing one Postgres must not
        // make a live database look infinitely old.
        let future = format!("test_boss_{}_abc123def456", NOW + 10_000);
        assert!(!is_orphan(&future, NOW, ORPHAN_TTL_SECS));
    }

    #[test]
    fn only_scratch_databases_are_candidates() {
        assert_eq!(stamp_of("boss"), None);
        assert_eq!(stamp_of("postgres"), None);
    }

    /// A build killed part-way leaves a stamped database that ages out
    /// like any other litter.
    #[test]
    fn an_abandoned_template_build_is_swept() {
        let building = format!("boss_tmpl_building_{NOW}_abc123def456");
        assert_eq!(stamp_of(&building), Some(NOW));
        assert!(!is_orphan(&building, NOW, ORPHAN_TTL_SECS));
        assert!(is_orphan(
            &building,
            NOW + ORPHAN_TTL_SECS + 1,
            ORPHAN_TTL_SECS
        ));
    }

    /// THE ONE THAT WOULD HURT. A finished template is the cache the
    /// whole suite reads; sweeping it would put the 86-file schema load
    /// back on every test. It carries no stamp, so it must not parse as
    /// litter — and `is_orphan`'s "unstamped means ancient" rule is
    /// exactly what would eat it if the prefixes ever overlapped.
    #[test]
    fn a_finished_template_is_never_litter() {
        assert_eq!(stamp_of("boss_tmpl_0123456789abcdef"), None);
        assert!(!"boss_tmpl_0123456789abcdef".starts_with("boss_tmpl_building_"));
        assert!(!"boss_tmpl_0123456789abcdef".starts_with("test_boss_"));
    }
}

/// The schema fingerprint decides which template a test gets, so its
/// only two properties — same SQL means same name, different SQL means
/// different name — are what stand between a test and a stale schema.
#[cfg(test)]
mod template_naming {
    use super::{TEMPLATE_PREFIX, schema_fingerprint};

    #[test]
    fn the_same_schema_names_the_same_template() {
        assert_eq!(
            schema_fingerprint("CREATE TABLE a (id int);"),
            schema_fingerprint("CREATE TABLE a (id int);")
        );
    }

    #[test]
    fn a_changed_schema_names_a_different_template() {
        // The case that matters: adding a migration must not be served
        // yesterday's template.
        let before = schema_fingerprint("CREATE TABLE a (id int);");
        let after = schema_fingerprint("CREATE TABLE a (id int);\nCREATE TABLE b (id int);");
        assert_ne!(before, after);
    }

    #[test]
    fn omitting_a_module_names_a_different_template() {
        // `new_without(&["ledger"])` produces different SQL, so it gets
        // its own template rather than one carrying the ledger tables.
        assert_ne!(
            schema_fingerprint("core;\nledger;"),
            schema_fingerprint("core;")
        );
    }

    #[test]
    fn a_template_name_is_a_legal_unquoted_identifier() {
        // It is interpolated into CREATE DATABASE. Keep it to
        // lowercase hex under the prefix so there is nothing to quote
        // and nothing to inject.
        let name = schema_fingerprint("anything");
        assert!(name.starts_with(TEMPLATE_PREFIX));
        let hex = &name[TEMPLATE_PREFIX.len()..];
        assert_eq!(hex.len(), 16, "{name}");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "{name}"
        );
    }
}

#[cfg(test)]
mod scratch_url {
    use super::{DEFAULT_ADMIN_URL, with_database};

    #[test]
    fn swaps_the_database_and_keeps_everything_else() {
        assert_eq!(
            with_database(DEFAULT_ADMIN_URL, "test_boss_1"),
            "postgres://boss:boss@127.0.0.1/test_boss_1"
        );
        assert_eq!(
            with_database("postgres://u:p@db.internal:15432/postgres", "test_boss_1"),
            "postgres://u:p@db.internal:15432/test_boss_1"
        );
    }

    #[test]
    fn keeps_query_parameters() {
        // sslmode and friends decide whether the subprocess can connect
        // at all — dropping them would fail somewhere far from here.
        assert_eq!(
            with_database(
                "postgres://boss@host/postgres?sslmode=require",
                "test_boss_1"
            ),
            "postgres://boss@host/test_boss_1?sslmode=require"
        );
    }

    #[test]
    fn an_admin_url_naming_no_database_still_yields_one() {
        assert_eq!(
            with_database("postgres://boss:boss@127.0.0.1", "test_boss_1"),
            "postgres://boss:boss@127.0.0.1/test_boss_1"
        );
    }
}

#[cfg(test)]
mod production_guard {
    use super::{ALLOW_PRODUCTION_ENV, production_refusal};

    #[test]
    fn a_server_holding_a_boss_database_is_refused() {
        let refusal = production_refusal(true, false).expect("a BOSS deployment is refused");
        // The message has to carry the way OUT, not just the way in —
        // the person reading it is mid-suite and needs the next action.
        assert!(refusal.contains("BOSS_TEST_POSTGRES_ADMIN_URL"));
        assert!(refusal.contains(ALLOW_PRODUCTION_ENV));
    }

    #[test]
    fn a_scratch_server_is_allowed() {
        // The dev container's sidecar (POSTGRES_DB=postgres) and a bare
        // `initdb` both land here, which is the point: the guard must
        // not make the safe path harder than the dangerous one.
        assert_eq!(production_refusal(false, false), None);
    }

    #[test]
    fn the_override_is_honoured_but_only_when_explicit() {
        assert_eq!(production_refusal(true, true), None);
        assert!(production_refusal(true, false).is_some());
    }
}

/// A sweep must not drop a database whose drop is already in flight.
///
/// THE PILE-UP THIS PINS (backlog 9469bc69, 2026-09-24). A sweeper's
/// advisory lock dies with its admin connection, and a short-lived test
/// binary exits mid-sweep while its `DROP DATABASE` backends are still
/// waiting on a slow forced checkpoint. The next process takes the lock,
/// sees the same databases — they still exist, and the dropping
/// backends are connected to `postgres`, not to them — and issues the
/// same drops again. Measured on the dev pod's harness Postgres
/// (the packet's `root_cause_2026_09_24`): 77 of 96 connections were
/// `DROP DATABASE` on only 7 databases, 11 identical drops each, and
/// every other test binary on the pod was refused with `too many
/// clients already`.
///
/// Against a real server, because the defect is in what the catalog
/// says while a drop waits. The drop is held in flight the cheap way —
/// an open transaction holding a conflicting lock on the database
/// (`COMMENT ON DATABASE` takes ShareUpdateExclusive) — so nothing here
/// waits on a checkpoint, and the drop is cancelled rather than let run.
#[cfg(test)]
mod an_in_flight_drop {
    use super::{
        DEFAULT_ADMIN_URL, ORPHAN_TTL_SECS, orphan_candidates, production_refusal,
        release_scratch_database, scratch_database_name, stamp_of,
    };
    use sqlx::postgres::PgConnectOptions;
    use sqlx::{Connection, Executor, PgConnection};
    use std::str::FromStr;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn a_database_whose_drop_is_in_flight_is_not_swept_again() {
        let admin_url = std::env::var("BOSS_TEST_POSTGRES_ADMIN_URL")
            .unwrap_or_else(|_| DEFAULT_ADMIN_URL.to_string());
        let opts = PgConnectOptions::from_str(&admin_url).expect("parsing the admin URL");
        let mut admin = PgConnection::connect_with(&opts)
            .await
            .expect("connecting to the admin database");
        let has_boss_db: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'boss')")
                .fetch_one(&mut admin)
                .await
                .expect("probing for a production database");
        if let Some(reason) = production_refusal(
            has_boss_db,
            std::env::var(super::ALLOW_PRODUCTION_ENV).as_deref() == Ok("1"),
        ) {
            panic!("{reason}");
        }

        let name = scratch_database_name("inflight");
        admin
            .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
            .await
            .expect("creating the scratch database");
        // A sweep that runs one TTL after the stamp: the name is litter.
        let later = stamp_of(&name).expect("a stamped name") + ORPHAN_TTL_SECS + 1;

        // Control: nobody touching it, so a sweep would take it.
        assert!(
            orphan_candidates(&mut admin, later).await.contains(&name),
            "an aged database nobody holds is a sweep candidate: {name}"
        );

        // Hold a lock DROP DATABASE must wait behind, in an open
        // transaction on a session connected to `postgres`.
        let mut holder = PgConnection::connect_with(&opts)
            .await
            .expect("connecting the holder");
        holder.execute("BEGIN").await.expect("BEGIN");
        holder
            .execute(format!(r#"COMMENT ON DATABASE "{name}" IS 'held by a test'"#).as_str())
            .await
            .expect("taking a lock on the database");
        // A lock that is not a drop's does not hide it: the exclusion
        // must key on the drop, or a connect-in-progress would too.
        assert!(
            orphan_candidates(&mut admin, later).await.contains(&name),
            "a database merely locked by someone is still a candidate: {name}"
        );

        // The drop in flight — a sweeper that has since exited looks
        // exactly like this to the server: a backend on `postgres`
        // waiting on the database's lock.
        let mut dropper = PgConnection::connect_with(&opts)
            .await
            .expect("connecting the dropper");
        let dropper_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut dropper)
            .await
            .expect("the dropper's pid");
        let drop_sql = format!(r#"DROP DATABASE IF EXISTS "{name}""#);
        let in_flight = tokio::spawn(async move {
            let _ = dropper.execute(drop_sql.as_str()).await;
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND NOT granted)",
            )
            .bind(dropper_pid)
            .fetch_one(&mut admin)
            .await
            .expect("reading pg_locks");
            if waiting {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the drop of {name} never queued on its lock"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let candidates = orphan_candidates(&mut admin, later).await;

        // Clean up before judging, so a red leaves nothing waiting: the
        // drop is cancelled, the lock released, and the database handed
        // to the reaper, which drops it off this test's path.
        let _ = sqlx::query("SELECT pg_cancel_backend($1)")
            .bind(dropper_pid)
            .execute(&mut admin)
            .await;
        let _ = tokio::time::timeout(Duration::from_secs(30), in_flight).await;
        let _ = holder.execute("ROLLBACK").await;
        let _ = holder.close().await;
        release_scratch_database(&admin_url, &name);

        assert!(
            !candidates.contains(&name),
            "{name} has a DROP DATABASE in flight, yet a second sweep would issue \
             another — the pile-up that filled the server (backlog 9469bc69)"
        );
    }
}

#[cfg(test)]
mod transient_tests {
    use super::is_transient;

    // The one that actually happened. sqlx renders a short read at EOF
    // as Error::Io, and train 47 died on it.
    #[test]
    fn a_closed_socket_is_weather() {
        let e = sqlx::Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "expected to read 5 bytes, got 0 bytes at EOF",
        ));
        assert!(is_transient(&e), "{e}");
    }

    #[test]
    fn a_protocol_break_and_a_dead_pool_are_weather() {
        assert!(is_transient(&sqlx::Error::Protocol("bad message".into())));
        assert!(is_transient(&sqlx::Error::PoolTimedOut));
        assert!(is_transient(&sqlx::Error::PoolClosed));
    }

    // The half that matters more: a real defect must fail on the first
    // try. Retrying it would spend three attempts turning a
    // reproducible failure into something that reads as flaky, which
    // is the failure mode this whole change exists to prevent —
    // pointed the other way.
    #[test]
    fn a_query_that_is_simply_wrong_is_not_retried() {
        assert!(!is_transient(&sqlx::Error::RowNotFound));
        assert!(!is_transient(&sqlx::Error::ColumnNotFound("nope".into())));
    }
}
