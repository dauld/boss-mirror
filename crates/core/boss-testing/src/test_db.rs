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
//! On `Drop`, the database is dropped via a
//! best-effort background task — if that fails (test process killed,
//! runtime already shut down), the random name prefix makes orphans
//! easy to find and clean up administratively.
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

use std::str::FromStr;

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
/// FNV-1a. Not a security primitive and not trying to be — the only
/// property required is that the same SQL yields the same name and
/// different SQL does not, across processes and across runs, with no
/// dependency added to a crate that every test in the workspace links.
fn schema_fingerprint(schema: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in schema.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
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
        let admin_url = std::env::var("BOSS_TEST_POSTGRES_ADMIN_URL")
            .unwrap_or_else(|_| DEFAULT_ADMIN_URL.to_string());

        let suffix = Uuid::new_v4().simple().to_string();
        // The stamp is what lets a later process tell litter from live
        // work without asking whether a test is still running.
        let db_name = format!("{SCRATCH_PREFIX}{}_{}", now_secs(), &suffix[..12]);

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

        // Reclaim earlier runs' litter before adding to it. See
        // `sweep_orphans` for why this happens here rather than on Drop.
        sweep_orphans(&mut admin, now_secs()).await;

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
        // Best-effort cleanup. If we're inside a tokio runtime we
        // schedule a background drop of the database. If not, the
        // orphan persists and must be cleaned by `DROP DATABASE
        // test_boss_*` administratively.
        let db_name = std::mem::take(&mut self.db_name);
        let admin_url = std::mem::take(&mut self.admin_url);
        if db_name.is_empty() {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                drop_database(&admin_url, &db_name).await;
            });
        }
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
/// WHY CLEANUP CANNOT LIVE ONLY ON `Drop` (filed f22369c4). `Drop`
/// schedules the drop with `handle.spawn`, but a `#[tokio::test]`
/// runtime is torn down the moment the test function returns — so the
/// spawned task usually never runs, and the database survives. Measured
/// 2026-08-15: a scratch instance created at 16:30Z held 196 databases
/// and 3.0G by 17:15Z, and two ordinary test runs while writing this
/// added two more. The `Drop` path is kept because it does work when
/// the runtime outlives the handle; it is simply not sufficient alone.
///
/// So a process cleans up after its predecessors rather than after
/// itself: one query per test PROCESS, not per test.
///
/// Two conditions, and both are needed. Age alone would race a suite
/// running elsewhere against the same server; "no active connections"
/// alone would race the gap between `CREATE DATABASE` and the pool's
/// first connect in a parallel process. Requiring both means a database
/// must be older than the TTL AND have nobody attached — a freshly
/// created one fails the age test no matter what its connections are
/// doing.
///
/// Best-effort throughout: a concurrent sweeper may drop a row between
/// our SELECT and our DROP, and that is fine. Never panics — a test
/// must not fail because someone else's litter would not go away.
async fn sweep_orphans(admin: &mut PgConnection, now: u64) {
    // Two shapes of litter, one query. `boss_tmpl_building_%` is a
    // template whose schema load never finished; a COMPLETED template
    // (`boss_tmpl_<fingerprint>`) matches neither pattern and is
    // deliberately kept — it is the cache that makes the suite fast,
    // it is content-addressed so it cannot go stale, and it costs one
    // schema-sized database per schema version on the server.
    let candidates: Vec<String> = match sqlx::query_scalar(
        "SELECT d.datname FROM pg_database d \
         WHERE (d.datname LIKE 'test\\_boss\\_%' OR d.datname LIKE 'boss\\_tmpl\\_building\\_%') \
           AND NOT EXISTS (SELECT 1 FROM pg_stat_activity a WHERE a.datname = d.datname)",
    )
    .fetch_all(&mut *admin)
    .await
    {
        Ok(rows) => rows,
        Err(_) => return,
    };

    for name in candidates
        .into_iter()
        .filter(|n| is_orphan(n, now, ORPHAN_TTL_SECS))
    {
        // Quote the identifier; never interpolate it anywhere a name
        // could be read as SQL. These come from pg_database, but the
        // habit is the point.
        let _ = admin
            .execute(format!(r#"DROP DATABASE IF EXISTS "{name}""#).as_str())
            .await;
    }
}

async fn drop_database(admin_url: &str, db_name: &str) {
    let Ok(opts) = PgConnectOptions::from_str(admin_url) else {
        return;
    };
    let Ok(mut conn) = PgConnection::connect_with(&opts).await else {
        return;
    };
    // Terminate other sessions holding connections to the test db,
    // otherwise DROP DATABASE fails with "database is being accessed
    // by other users". WITH (FORCE) would also work on Postgres 13+.
    let _ = conn
        .execute(
            format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                 WHERE datname = '{db_name}' AND pid <> pg_backend_pid()"
            )
            .as_str(),
        )
        .await;
    let _ = conn
        .execute(format!(r#"DROP DATABASE IF EXISTS "{db_name}""#).as_str())
        .await;
}

#[cfg(test)]
mod generated_schema_list {
    include!("../schema_order.rs");
    use super::SCHEMA_FILES;

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
    /// `OUT_DIR` artifact. Reading the directory here at run time means
    /// that if the build script's `rerun-if-changed` coverage ever
    /// regresses, the real schema meets the stale generated list and
    /// this fails — with the same symptom it always guarded against,
    /// caught at the same place.
    #[test]
    fn generated_list_matches_the_schema_directory() {
        let schema_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/postgres/schema");
        let mut expected: Vec<String> = std::fs::read_dir(&schema_dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", schema_dir.display()))
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        // The SAME key build.rs uses, from the same file, so this test
        // cannot pass while the two disagree — which is what it is for.
        expected.sort_by_key(|n| schema_sort_key(n));
        let expected: Vec<String> = expected
            .iter()
            .map(|n| n.trim_end_matches(".sql").to_string())
            .collect();

        let actual: Vec<&str> = SCHEMA_FILES.iter().map(|(name, _)| *name).collect();
        assert!(
            !actual.is_empty(),
            "SCHEMA_FILES is empty — every TestDb would load an empty schema"
        );
        assert_eq!(
            actual, expected,
            "generated SCHEMA_FILES is stale against infra/postgres/schema/*.sql"
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
    use super::{ORPHAN_TTL_SECS, is_orphan, stamp_of};

    const NOW: u64 = 1_786_800_000;

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
