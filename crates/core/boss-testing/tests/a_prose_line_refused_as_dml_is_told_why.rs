//! A refusal aimed at PROSE says so, and says what to write instead.
//!
//! MEASURED 2026-09-22 (backlog c68104cf, from the builder of 3d18b741).
//! `infra/lint/api-path-bypass-smell.sh` refuses a DML phrase that is not
//! in a read position — a correct rule, and not one this file touches.
//! What it did not do was distinguish a STATEMENT from a SENTENCE ABOUT
//! one. Rewriting `a-verb-declares-the-hosts-it-serves.sh`, the builder
//! wrote the WHY comment every change here carries:
//!
//! ```text
//! # UNTIL 2026-09-20 THIS READ THE `INSERT INTO nodes` rows in schema.
//! ```
//!
//! and was refused. The lexer strips ordinary shell comments before any
//! classification (`api-path-bypass-smell.sh`, the `#` case in its awk
//! lexer), so a plain comment never reaches the classifier at all — which
//! is why a first reading of this packet concluded it did not reproduce.
//! That comment was inside the file's `python3 - <<'PY'` heredoc, whose
//! opener is not a text tool, and a heredoc body is classified off its
//! opener. So the line WAS code by the rule, and the rule was right.
//!
//! The cost was never the refusal; it was that the refusal's advice —
//! "put it in a read position the classifier knows: a grep/awk/sed/echo/
//! printf invocation, or a *_PATTERN variable" — is unfollowable for a
//! sentence. You cannot pipe a comment through grep. The repair is to
//! reword the prose (`the `nodes` seed rows`), and nothing said so.
//!
//! WHAT IS PINNED HERE. A reported line whose first non-blank character
//! is `#` gets a second line naming the cause and the repair. The rule
//! is unchanged: the line is still reported, the lint still exits 1, and
//! the control below proves a real write gets no such excuse-shaped hint.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch repo root holding a COPY of `infra/lint/` (the lint derives
/// its root from its own location, and scans `infra/` under it), the
/// paths its allowlist names, and whatever fixtures a test plants.
fn planted_root(name: &str) -> PathBuf {
    let root = scratch_dir(name);
    let src = repo_root().join("infra/lint");
    let dst = root.join("infra/lint");
    std::fs::create_dir_all(dst.parent().expect("infra/"))
        .unwrap_or_else(|e| panic!("create {}: {e}", dst.display()));
    let status = Command::new("cp")
        .arg("-r")
        .arg(&src)
        .arg(&dst)
        .status()
        .unwrap_or_else(|e| panic!("cp {} {}: {e}", src.display(), dst.display()));
    assert!(status.success(), "cp of infra/lint into the scratch root");

    // The allowlist refuses an entry naming a path that is gone
    // (infra/lint/lib/allowlist.sh), and that check runs before any
    // scanning. Plant each path as an empty file so the lint reaches the
    // classifier — the allowlist's CONTENT is pinned elsewhere.
    for rel in allowlisted_paths() {
        let path = root.join(&rel);
        std::fs::create_dir_all(path.parent().expect("a parent"))
            .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
        write_file(&path, "");
    }
    root
}

/// The allowlist's paths, read from the lint itself — never retyped
/// here, which is the same rule the lint's own allowlist is held to
/// (CLAUDE.md §9a).
fn allowlisted_paths() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join("infra/lint/api-path-bypass-smell.sh"))
        .expect("infra/lint/api-path-bypass-smell.sh");
    let body = text
        .split_once("\nALLOWLIST=(")
        .expect("the lint declares ALLOWLIST=(")
        .1
        .split_once("\n)")
        .expect("the ALLOWLIST array closes")
        .0;
    let paths: Vec<String> = body
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"'))
        .filter_map(|l| l.split_once("::"))
        .map(|(path, _)| path.to_string())
        .collect();
    assert!(
        !paths.is_empty(),
        "read 0 allowlist paths out of the lint — the ALLOWLIST shape moved"
    );
    paths
}

/// Run the planted copy of the lint and return (exit code, stdout+stderr).
fn run_lint(root: &Path) -> (i32, String) {
    let out = Command::new("bash")
        .arg(root.join("infra/lint/api-path-bypass-smell.sh"))
        .current_dir(root)
        .output()
        .expect("run the planted api-path-bypass-smell.sh");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The packet's own line, in the shape that produced it: a WHY comment
/// inside a heredoc whose opener is not a text tool.
#[test]
fn a_comment_refused_as_dml_is_told_it_is_prose_and_what_to_write() {
    let root = planted_root("api-path-bypass-prose");
    write_exec(
        &root.join("infra/a-verb-declares-its-hosts.sh"),
        "#!/usr/bin/env bash\npython3 - <<'PY'\n# UNTIL 2026-09-20 THIS READ THE `INSERT INTO nodes` rows in schema.\nprint(1)\nPY\n",
    );

    let (code, out) = run_lint(&root);
    assert_eq!(
        code, 1,
        "the rule is unchanged: a DML phrase in write position is still refused\n{out}"
    );
    assert!(
        out.contains("a-verb-declares-its-hosts.sh:3:"),
        "the finding still names the file and line\n{out}"
    );
    assert!(
        out.contains("that line is PROSE"),
        "the refusal must name the cause — a reader who edited a comment is told \
         they edited a comment, not handed advice about grep invocations\n{out}"
    );
    assert!(
        out.contains("reword"),
        "the refusal must name the repair a sentence can actually follow: reword it. \
         You cannot pipe a comment through grep.\n{out}"
    );
}

/// The control, because a hint that fires on everything says nothing: a
/// real write is reported with no prose hint attached to it.
#[test]
fn a_real_write_is_refused_without_the_prose_hint() {
    let root = planted_root("api-path-bypass-real-write");
    write_exec(
        &root.join("infra/a-real-write.sh"),
        "#!/usr/bin/env bash\npsql -c \"INSERT INTO nodes (id) VALUES ('w-1')\"\n",
    );

    let (code, out) = run_lint(&root);
    assert_eq!(code, 1, "a psql -c write is refused\n{out}");
    assert!(
        out.contains("a-real-write.sh:2:"),
        "the finding names the file and line\n{out}"
    );
    assert!(
        !out.contains("that line is PROSE"),
        "a statement is not prose — the hint must not soften a real write\n{out}"
    );
}
