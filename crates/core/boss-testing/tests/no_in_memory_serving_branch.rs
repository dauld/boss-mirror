//! No service binary carries an in-memory serving branch (backlog
//! be793304, a deletion measured rather than guessed).
//!
//! Every `*-api` binary in a crate with a `postgres` feature declares
//! `required-features = ["postgres"]`, so cargo refuses to build it
//! without the feature — which made every `#[cfg(not(feature =
//! "postgres"))]` arm in those binaries, and the
//! `require_postgres_or_explicit_inmemory` guard they called, code that
//! could not run. `BOSS_ALLOW_INMEMORY` was set by nothing in the tree,
//! and infra/check-binary-build-coverage.sh classified reaching the
//! guard as a defect. A branch whose sanctioned outcome is "never
//! reached" is deleted, not kept; this pins the deletion so the branch
//! does not grow back one `cfg(not(...))` at a time.

use boss_testing::repo_root;
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules")
            {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_service_binary_has_an_in_memory_arm() {
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let mut offenders = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().to_string();
        let is_bin = rel.contains("/src/bin/") || rel.ends_with("/src/main.rs");
        let text = std::fs::read_to_string(f).unwrap_or_default();
        if is_bin && text.contains(r#"cfg(not(feature = "postgres"))"#) {
            offenders.push(format!(
                "{rel}: a `cfg(not(feature = \"postgres\"))` arm in a binary"
            ));
        }
        // Code, not prose: the env read, the fn, or a call. History
        // notes that NAME the deleted guard are how the deletion
        // explains itself and are allowed.
        if rel.contains("/tests/") {
            continue;
        }
        if text.contains(r#"env::var("BOSS_ALLOW_INMEMORY""#)
            || text.contains("fn require_postgres_or_explicit_inmemory")
            || text.contains("require_postgres_or_explicit_inmemory(")
        {
            offenders.push(format!(
                "{rel}: reads or calls the deleted in-memory override"
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "the in-memory serving branch is deleted (be793304); it is growing back here:\n  {}",
        offenders.join("\n  ")
    );
}
