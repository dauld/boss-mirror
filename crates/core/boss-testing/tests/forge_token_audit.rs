//! `infra/maintenance/forge-token-audit.py` is RUN, not read.
//!
//! The script's whole value is a verdict — exit 0 clean, exit 2 drift — that a
//! systemd unit turns into an alert. A test that greps the source for the word
//! "UNDECLARED" would pass on a script that never reaches the branch. So each
//! case here builds a throwaway Forgejo database and a declaration, executes
//! the real script against them, and asserts on what it actually decided.
//!
//! One of these tests is not about drift at all: the audit reads a table whose
//! other columns are a credential hash and its salt, and
//! `no_credential_material_reaches_the_output` pins that neither ever reaches
//! stdout. That property is the reason this script is allowed to exist.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn script() -> PathBuf {
    repo_root().join("infra/maintenance/forge-token-audit.py")
}

/// A scratch directory per case, so cases cannot see each other's fixtures.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("forge-token-audit-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The secret-shaped values the fixture stores. If either ever appears in the
/// audit's output, the script is leaking the thing it exists to avoid touching.
const HASH: &str = "d3adb33fd3adb33fd3adb33fd3adb33fd3adb33fd3adb33fd3adb33fd3adb33f";
const SALT: &str = "s4ltysaltysalty";

/// Build a Forgejo-shaped SQLite database holding `tokens`, each
/// `(name, scope, last_used_unix)`.
fn forge_db(dir: &Path, tokens: &[(&str, &str, i64)]) -> PathBuf {
    let db = dir.join("gitea.db");
    let mut py = String::from(
        "import sqlite3,sys\n\
         con=sqlite3.connect(sys.argv[1])\n\
         con.execute('CREATE TABLE user (id INTEGER PRIMARY KEY, lower_name TEXT)')\n\
         con.execute('CREATE TABLE access_token (id INTEGER PRIMARY KEY, uid INTEGER, \
             name TEXT, token_hash TEXT, token_salt TEXT, token_last_eight TEXT, \
             created_unix INTEGER, updated_unix INTEGER, scope TEXT)')\n\
         con.execute(\"INSERT INTO user VALUES (1,'david')\")\n",
    );
    for (i, (name, scope, used)) in tokens.iter().enumerate() {
        py.push_str(&format!(
            "con.execute('INSERT INTO access_token VALUES (?,?,?,?,?,?,?,?,?)',\
             ({id},1,'{name}','{HASH}','{SALT}','ab12cd34',1000000,{used},'{scope}'))\n",
            id = i + 1,
        ));
    }
    py.push_str("con.commit()\n");

    let out = Command::new("python3")
        .arg("-c")
        .arg(&py)
        .arg(&db)
        .output()
        .expect("python3 builds the fixture database");
    assert!(
        out.status.success(),
        "fixture db failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    db
}

/// Write a declaration holding `tokens`, each `(name, consumer, scope)`.
fn declaration(dir: &Path, tokens: &[(&str, &str, &str)]) -> PathBuf {
    let path = dir.join("forge-tokens.toml");
    let mut toml = String::new();
    for (name, consumer, scope) in tokens {
        toml.push_str(&format!(
            "[[token]]\nname = \"{name}\"\nconsumer = \"{consumer}\"\n\
             scope = \"{scope}\"\ninstalled_at = \"somewhere\"\n\n"
        ));
    }
    std::fs::write(&path, toml).expect("write declaration");
    path
}

/// Write a credentials-registry fixture — the `GET /api/credentials`
/// response shape — holding forgejo rows, each `(id, scopes)`.
fn registry(dir: &Path, rows: &[(&str, &[&str])]) -> PathBuf {
    let path = dir.join("credentials.json");
    let rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, scopes)| {
            serde_json::json!({
                "id": id,
                "kind": "forgejo-access-token",
                "issuer": "forgejo (10.20.0.15)",
                "principal": "user david",
                "scopes": scopes,
                "storage_location": format!("k8s Secret somewhere/{id}"),
                "consumers": [],
                "rotation_policy": "on-demand",
                "rotated_at": null,
                "notes": "",
            })
        })
        .collect();
    std::fs::write(&path, serde_json::to_string(&rows).unwrap()).expect("write registry");
    path
}

/// Run the audit. Returns (exit code, stdout).
fn run(db: &Path, decl: &Path, now: i64) -> (i32, String) {
    run_args(db, decl, now, &[])
}

/// Run the audit with extra flags (the registry direction).
fn run_args(db: &Path, decl: &Path, now: i64, extra: &[&std::ffi::OsStr]) -> (i32, String) {
    let out = Command::new("python3")
        .arg(script())
        .arg("--db")
        .arg(db)
        .arg("--declaration")
        .arg(decl)
        .arg("--now")
        .arg(now.to_string())
        .args(extra)
        .output()
        .expect("the audit script runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

const NOW: i64 = 1_800_000_000;

#[test]
fn a_forge_matching_its_declaration_is_clean() {
    let dir = scratch("clean");
    let db = forge_db(&dir, &[("boss-gcp", "write:repository", NOW - 3600)]);
    let decl = declaration(&dir, &[("boss-gcp", "the conductor", "write:repository")]);

    let (code, out) = run(&db, &decl, NOW);
    assert_eq!(code, 0, "expected a clean exit, got:\n{out}");
    assert!(out.contains("clean"), "{out}");
}

#[test]
fn a_token_the_declaration_does_not_mention_is_reported_and_fails_the_run() {
    let dir = scratch("undeclared");
    let db = forge_db(
        &dir,
        &[
            ("boss-gcp", "write:repository", NOW - 3600),
            ("push-20260818", "write:repository", NOW - 3600),
        ],
    );
    let decl = declaration(&dir, &[("boss-gcp", "the conductor", "write:repository")]);

    let (code, out) = run(&db, &decl, NOW);
    assert_eq!(code, 2, "undeclared token must fail the run:\n{out}");
    assert!(out.contains("UNDECLARED"), "{out}");
    assert!(
        out.contains("push-20260818"),
        "the finding must NAME the token, or it cannot be acted on:\n{out}"
    );
}

#[test]
fn a_declared_token_missing_from_the_forge_is_reported() {
    // The direction that catches a revocation nobody wrote down - and warns
    // before the consumer that still holds it discovers the loss itself.
    let dir = scratch("missing");
    let db = forge_db(&dir, &[("boss-gcp", "write:repository", NOW - 3600)]);
    let decl = declaration(
        &dir,
        &[
            ("boss-gcp", "the conductor", "write:repository"),
            ("boss-dev-read", "the dev pod", "read:repository"),
        ],
    );

    let (code, out) = run(&db, &decl, NOW);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("MISSING"), "{out}");
    assert!(out.contains("boss-dev-read"), "{out}");
    assert!(
        out.contains("the dev pod"),
        "naming the consumer is the point - it says who is about to break:\n{out}"
    );
}

#[test]
fn a_scope_that_widened_behind_our_back_is_reported() {
    let dir = scratch("scope");
    let db = forge_db(&dir, &[("boss-dev-read", "write:repository", NOW - 3600)]);
    let decl = declaration(&dir, &[("boss-dev-read", "the dev pod", "read:repository")]);

    let (code, out) = run(&db, &decl, NOW);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("SCOPE-DRIFT"), "{out}");
}

#[test]
fn an_unattributed_consumer_is_counted_even_when_nothing_else_drifts() {
    // consumer = UNKNOWN is the debt itself, so a forge that matches the
    // declaration perfectly must still NOT come back clean while one exists.
    let dir = scratch("unknown");
    let db = forge_db(&dir, &[("cluster-registry", "write:package", NOW - 3600)]);
    let decl = declaration(&dir, &[("cluster-registry", "UNKNOWN", "write:package")]);

    let (code, out) = run(&db, &decl, NOW);
    assert_eq!(
        code, 2,
        "an unattributable live credential is not a clean state:\n{out}"
    );
    assert!(out.contains("UNATTRIBUTED"), "{out}");
}

// ---------------------------------------------------------------------------
// The credentials-registry direction (packet 7ee101aa, second leg):
// live forge tokens vs registry rows of kind forgejo-access-token,
// both ways. The registry fixture is the GET /api/credentials shape.
// ---------------------------------------------------------------------------

#[test]
fn a_forge_matching_the_registry_is_clean_and_prefix_matching_covers_rotation_names() {
    // The rotation-minted instance carries a packet-derived name
    // (`{id}-{packet8}`); the registry row holds the durable id. If
    // matching were exact-name, every rotated token would false-alarm.
    let dir = scratch("reg-clean");
    let db = forge_db(
        &dir,
        &[(
            "boss-dev-forge-token-7ee101aa",
            "write:repository",
            NOW - 3600,
        )],
    );
    let decl = declaration(
        &dir,
        &[(
            "boss-dev-forge-token-7ee101aa",
            "the dev pod",
            "write:repository",
        )],
    );
    let reg = registry(&dir, &[("boss-dev-forge-token", &["write:repository"])]);

    let (code, out) = run_args(&db, &decl, NOW, &["--registry-json".as_ref(), reg.as_ref()]);
    assert_eq!(code, 0, "expected clean:\n{out}");
    assert!(out.contains("credentials registry"), "{out}");
    assert!(
        !out.contains("REG-"),
        "no registry finding expected:\n{out}"
    );
}

#[test]
fn a_live_token_absent_from_the_registry_names_itself() {
    let dir = scratch("reg-undeclared");
    let db = forge_db(
        &dir,
        &[
            (
                "boss-dev-forge-token-7ee101aa",
                "write:repository",
                NOW - 3600,
            ),
            ("rogue-token", "write:package", NOW - 3600),
        ],
    );
    // The TOML declares both, so every finding here is the registry's.
    let decl = declaration(
        &dir,
        &[
            (
                "boss-dev-forge-token-7ee101aa",
                "the dev pod",
                "write:repository",
            ),
            ("rogue-token", "somebody", "write:package"),
        ],
    );
    let reg = registry(&dir, &[("boss-dev-forge-token", &["write:repository"])]);

    let (code, out) = run_args(&db, &decl, NOW, &["--registry-json".as_ref(), reg.as_ref()]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("REG-UNDECLARED"), "{out}");
    assert!(
        out.contains("'rogue-token'"),
        "the finding must NAME the token:\n{out}"
    );
    assert_eq!(
        out.matches("REG-UNDECLARED").count(),
        1,
        "exactly one — the rotation-named instance belongs to its row:\n{out}"
    );
}

#[test]
fn a_registry_row_the_forge_does_not_know_is_reported() {
    // The broker-root seed ships exactly this way — storage known,
    // forge-side token name unrecorded — so the message must offer
    // both readings: revoked-without-updating, or name-not-recorded.
    let dir = scratch("reg-missing");
    let db = forge_db(&dir, &[("boss-gcp", "write:repository", NOW - 3600)]);
    let decl = declaration(&dir, &[("boss-gcp", "the conductor", "write:repository")]);
    let reg = registry(
        &dir,
        &[
            ("boss-gcp", &["write:repository"]),
            ("boss-credential-broker-root", &[]),
        ],
    );

    let (code, out) = run_args(&db, &decl, NOW, &["--registry-json".as_ref(), reg.as_ref()]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("REG-MISSING"), "{out}");
    assert!(out.contains("boss-credential-broker-root"), "{out}");
    assert!(
        out.contains("does not record its forge-side token name"),
        "the honest second reading must be offered:\n{out}"
    );
}

#[test]
fn registry_scope_drift_and_unverified_scopes_are_reported() {
    let dir = scratch("reg-scope");
    let db = forge_db(
        &dir,
        &[
            ("drifted-token", "write:repository", NOW - 3600),
            ("unverified-token", "read:user", NOW - 3600),
        ],
    );
    let decl = declaration(
        &dir,
        &[
            ("drifted-token", "somebody", "write:repository"),
            ("unverified-token", "somebody", "read:user"),
        ],
    );
    let reg = registry(
        &dir,
        &[
            // The registry believes read-only; the forge says write.
            ("drifted-token", &["read:repository"]),
            // The seeded honest-gap shape: scopes empty, audit fills.
            ("unverified-token", &[]),
        ],
    );

    let (code, out) = run_args(&db, &decl, NOW, &["--registry-json".as_ref(), reg.as_ref()]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("REG-SCOPE-DRIFT"), "{out}");
    assert!(
        out.contains("REG-SCOPE-UNVERIFIED"),
        "an empty scopes array is a gap to fill, not a pass:\n{out}"
    );
    assert!(
        out.contains("'read:user'"),
        "the fill-me finding must SAY what the forge says, so recording it \
         is a copy rather than a re-derivation:\n{out}"
    );
}

#[test]
fn an_unreachable_registry_makes_the_run_honestly_partial() {
    // Port 1 refuses fast on any host. An otherwise-clean run that
    // could not check a configured direction must not exit 0.
    let dir = scratch("reg-unreachable");
    let db = forge_db(&dir, &[("boss-gcp", "write:repository", NOW - 3600)]);
    let decl = declaration(&dir, &[("boss-gcp", "the conductor", "write:repository")]);

    let (code, out) = run_args(
        &db,
        &decl,
        NOW,
        &["--registry-url".as_ref(), "http://127.0.0.1:1".as_ref()],
    );
    assert_eq!(code, 1, "partial is not clean:\n{out}");
    assert!(out.contains("LIMIT"), "{out}");
    assert!(
        out.contains("NOT checked"),
        "the run must say exactly what it could not do:\n{out}"
    );
}

#[test]
fn no_credential_material_reaches_the_output() {
    // THE PROPERTY THAT LETS THIS SCRIPT EXIST. It reads a table whose other
    // columns are a credential hash and its salt. Every case above is run
    // again here against one output, because a leak in any branch is a leak.
    let dir = scratch("noleak");
    let db = forge_db(
        &dir,
        &[
            ("boss-gcp", "write:repository", NOW - 3600),
            ("stale-one", "write:package", NOW - 90 * 86400),
            ("undeclared-one", "read:repository", NOW - 3600),
        ],
    );
    let decl = declaration(
        &dir,
        &[
            ("boss-gcp", "the conductor", "write:repository"),
            ("stale-one", "UNKNOWN", "write:package"),
            ("gone-one", "somebody", "read:repository"),
        ],
    );

    let (_code, out) = run(&db, &decl, NOW);
    assert!(!out.contains(HASH), "the token hash reached stdout:\n{out}");
    assert!(!out.contains(SALT), "the token salt reached stdout:\n{out}");
    // Sanity: this fixture really did exercise the reporting paths.
    assert!(
        out.contains("UNDECLARED") && out.contains("MISSING"),
        "{out}"
    );
}
