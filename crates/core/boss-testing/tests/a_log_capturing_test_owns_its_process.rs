//! A test that captures `tracing` output owns its process.
//!
//! `tracing::subscriber::set_default` installs a subscriber for one
//! THREAD, while `tracing` keeps callsite interest and the dynamic
//! max level in PROCESS-global state. `cargo test` runs a binary's
//! tests on many threads at once, so a sibling test that reaches the
//! same `error!` callsite with no subscriber on its thread can leave
//! that callsite resolved against no subscriber for everyone — and
//! the capturing test reads a log that is missing lines, or empty.
//!
//! MEASURED, not reasoned: `boss-jobs::station_boot_quarantine` held
//! that shape beside four siblings and failed 1 run in 25 (0 in 60
//! alone); it reddened train #281 on 2026-09-09 with six green-gated
//! cars aboard. The same shape stood in `boss-core/src/publisher.rs`
//! — three capturing tests in the crate's lib binary, beside every
//! other `#[cfg(test)]` in the crate — until backlog 2c257761
//! (2026-09-18). The fix that holds is the one `boss-jobs` took: the
//! capturing test is the ONLY test in its `tests/*.rs` binary (Rust
//! gives one process per file there), and the subscriber goes in with
//! `set_global_default`, which is safe precisely because nothing runs
//! beside it.
//!
//! Two rules, pinned across every crate:
//!
//!   1. `subscriber::set_default` appears in no Rust source at all —
//!      there is no test binary in which a thread-scoped subscriber is
//!      not a coin.
//!   2. `set_global_default` appears only in a file under `tests/`
//!      that declares exactly one test.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
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
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Code, not prose: a line that is a comment is the explanation of the
/// rule, not an instance of the shape.
fn code_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .map(|(i, l)| (i + 1, l))
}

fn test_count(text: &str) -> usize {
    code_lines(text)
        .filter(|(_, l)| {
            let t = l.trim();
            t == "#[test]" || t.starts_with("#[tokio::test")
        })
        .count()
}

/// Built in halves so this file's own text is not an instance.
const THREAD_SCOPED: &str = concat!("subscriber::", "set_default");
const PROCESS_SCOPED: &str = concat!("set_global", "_default");

#[test]
fn the_count_reads_attributes_and_not_prose() {
    let two = "#[test]\nfn a() {}\n#[tokio::test(flavor = \"current_thread\")]\nasync fn b() {}\n";
    assert_eq!(test_count(two), 2);
    let prose = "//! #[test] quoted in a doc\n// #[tokio::test] in a comment\nfn none() {}\n";
    assert_eq!(test_count(prose), 0);
    let mention = format!("//! prose naming {PROCESS_SCOPED}\nfn f() {{}}\n");
    assert!(
        !code_lines(&mention).any(|(_, l)| l.contains(PROCESS_SCOPED)),
        "a comment naming the call is the explanation, not an instance"
    );
}

#[test]
fn a_tracing_capture_is_global_and_alone_in_its_binary() {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    files.sort();
    let mut offenders = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(f).unwrap_or_default();
        for (n, line) in code_lines(&text) {
            if line.contains(THREAD_SCOPED) {
                offenders.push(format!(
                    "{rel}:{n}: `{THREAD_SCOPED}` scopes the subscriber to one \
                     thread while the callsite cache is per-process; a sibling \
                     test empties the capture 1 run in 25"
                ));
            }
        }
        if code_lines(&text).any(|(_, l)| l.contains(PROCESS_SCOPED)) {
            let in_tests = rel.contains("/tests/");
            let tests = test_count(&text);
            if !in_tests || tests != 1 {
                offenders.push(format!(
                    "{rel}: `{PROCESS_SCOPED}` in a file that is {} — a \
                     captured log is deterministic only when the capturing test \
                     is the sole test in its own `tests/*.rs` binary",
                    if in_tests {
                        format!("under tests/ but declares {tests} tests")
                    } else {
                        "not a tests/*.rs binary".to_string()
                    }
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{} log-capturing test(s) do not own their process (train #281, backlog \
         2c257761; the shape that holds is boss-jobs/tests/station_boot_log.rs):\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}
