//! Every executable a test writes goes through
//! `boss_testing::scratch::write_exec`. An inline `std::fs::write` +
//! `set_permissions(0o755)` still carries the ETXTBSY race that
//! `write_exec` was rebuilt to remove, so this pin greps the crates
//! tree and refuses a new one by `file:line`.
//!
//! THE CLASS (backlog 6eaef658, 2026-09-18; this pin is 62fc6f0c).
//! Linux refuses to exec a file any process holds open for writing.
//! The writer closes its descriptor before the exec — and the exec
//! still lost, once in a full run, because the holder was not the
//! writer: a child spawned by a SIBLING test thread inherits every open
//! descriptor of the process until its own exec, so a write descriptor
//! open at the instant any other test spawns is briefly open in that
//! child too. Reproduced 8/8 with four spawner threads; `write_exec`
//! now streams the body through a child that has exited before the
//! call returns, and was 0/20 red after. Every site that writes an
//! executable WITHOUT it — `std::fs::write` then `set_permissions`,
//! `File::create` then `write!`, `std::fs::copy` then chmod — is the
//! same race, waiting for the run where two spawns line up. Twenty-six
//! such lines were measured on 2026-09-19 (grep for `from_mode` with an
//! executable octal under crates/), six of them private `write_exec`
//! copies that predate the shared one (CLAUDE.md §9a: a fact that
//! lives twice).
//!
//! THE RULE. A line under `crates/` that sets an executable bit from
//! this process — `from_mode`, `set_mode` or `OpenOptions::mode` with
//! an octal whose owner digit has the execute bit — is refused, except in
//! `boss-testing/src/scratch.rs` (the one definition), or when the
//! line or the comment line directly above it carries
//! `mode-bits-ok: <reason>`, a declaration of why THIS chmod is not an
//! executable this process then runs (a directory made traversable, a
//! downloaded binary renamed over the CLI and never exec'd here). The
//! marker needs a reason of more than one word — a bare marker is a
//! rubber stamp, the same rule `a-fixture-path-cannot-be-a-literal`
//! holds its `shared-tmp-ok` to.
//!
//! NO REFUSED SHAPE IS SPELLED IN THIS FILE. The needles are split
//! from the digit that makes them executable, and the fixtures below
//! assemble them at runtime, so this file is scanned by the scanner it
//! proves — no self-exclusion, which would be a hole exactly where the
//! next author is working.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// The one place an executable bit is set from a test process.
const THE_DEFINITION: &str = "crates/core/boss-testing/src/scratch.rs";

/// The exemption marker; its reason follows the colon.
const MARKER: &str = "mode-bits-ok:";

/// Each way std sets a mode from this process, split before the octal
/// digits so the refused shape is never spelled here whole.
const NEEDLES: &[&str] = &["from_mode(0o", "set_mode(0o", ".mode(0o"];

/// Does `line` set an executable bit? Any of the needles followed by an
/// octal literal whose first (owner) digit has the execute bit set —
/// `0o7xx`, `0o5xx`, `0o1xx`, `0o3xx` — is an executable; `0o600` and
/// `0o644` are not what this pin is about.
fn sets_an_exec_bit(line: &str) -> bool {
    NEEDLES.iter().any(|needle| {
        line.match_indices(needle).any(|(at, _)| {
            line[at + needle.len()..]
                .chars()
                .next()
                .is_some_and(|d| matches!(d, '1' | '3' | '5' | '7'))
        })
    })
}

/// Is the hit declared? The marker with a reason of more than one word,
/// on the line itself or on the comment line directly above it.
fn is_declared(line: &str, above: Option<&str>) -> bool {
    let carries = |l: &str| {
        l.find(MARKER)
            .map(|at| l[at + MARKER.len()..].split_whitespace().count() >= 2)
            .unwrap_or(false)
    };
    carries(line) || above.is_some_and(|a| a.trim_start().starts_with("//") && carries(a))
}

/// Every `file:line` under `root` (relative to it) that sets an
/// executable bit without going through the definition or a declared
/// reason. Pure over the tree so the fixtures below can drive it.
fn undeclared_exec_writes(root: &Path) -> Vec<String> {
    let mut offenders = Vec::new();
    for path in rust_files_under(&root.join("crates")) {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        if rel == THE_DEFINITION {
            continue;
        }
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let above = i.checked_sub(1).map(|j| lines[j]);
            if sets_an_exec_bit(line) && !is_declared(line, above) {
                offenders.push(format!("  {rel}:{}\n      {}", i + 1, line.trim()));
            }
        }
    }
    offenders
}

/// Every `*.rs` under `dir`, recursively, sorted.
fn rust_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            out.extend(rust_files_under(&path));
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// A line that chmods a path executable, assembled at runtime so this
/// file does not spell the shape it refuses.
fn chmod_line(mode: &str) -> String {
    format!(
        "    std::fs::set_permissions(&p, std::fs::Permissions::{}{mode})).unwrap();",
        NEEDLES[0]
    )
}

#[test]
fn the_shape_check_reads_an_executable_bit_and_not_a_private_mode() {
    for mode in ["755", "775", "700", "555", "111"] {
        assert!(
            sets_an_exec_bit(&chmod_line(mode)),
            "0o{mode} sets an execute bit and must be read as one"
        );
    }
    for mode in ["600", "644", "666", "000"] {
        assert!(
            !sets_an_exec_bit(&chmod_line(mode)),
            "0o{mode} sets no execute bit — a token file or a lock is not this class"
        );
    }
    let via_set_mode = format!("        perms.{}755);", NEEDLES[1]);
    let via_open_options = format!(
        "    OpenOptions::new().write(true){}755).open(&p)",
        NEEDLES[2]
    );
    assert!(sets_an_exec_bit(&via_set_mode) && sets_an_exec_bit(&via_open_options));
    assert!(
        !sets_an_exec_bit("    boss_testing::write_exec(&p, body);"),
        "the door is not a finding"
    );
}

#[test]
fn a_marker_needs_a_reason_and_sits_on_the_line_or_directly_above() {
    let hit = chmod_line("755");
    assert!(
        is_declared(
            &hit,
            Some("    // mode-bits-ok: a directory the second uid must traverse")
        ),
        "a reasoned marker on the comment line above declares the hit"
    );
    assert!(
        is_declared(
            &format!("{hit} // mode-bits-ok: a directory, not a file"),
            None
        ),
        "a reasoned marker on the line itself declares the hit"
    );
    assert!(
        !is_declared(&hit, Some("    // mode-bits-ok:")),
        "a bare marker is a rubber stamp, not a declaration"
    );
    assert!(
        !is_declared(&hit, Some("    // mode-bits-ok: directory")),
        "a one-word reason is not a reason"
    );
    assert!(
        !is_declared(
            &hit,
            Some("    let bin = root.join(\"bin\"); // mode-bits-ok: a directory here")
        ),
        "a marker on a line of CODE above does not reach down — it covers its own line only"
    );
}

/// The walker on a synthetic tree: the definition is skipped by its
/// path, a declared hit is skipped by its marker, and every other hit
/// is named by file and line.
#[test]
fn an_undeclared_executable_write_is_named_by_file_and_line() {
    let root = boss_testing::scratch_dir("exec-write-pin");
    let def = root.join(THE_DEFINITION);
    boss_testing::create_dir(def.parent().expect("a parent"));
    boss_testing::write_file(
        &def,
        &format!("pub fn write_exec() {{\n{}\n}}\n", chmod_line("755")),
    );
    let tests = root.join("crates/core/boss-testing/tests");
    boss_testing::create_dir(&tests);
    boss_testing::write_file(
        &tests.join("declared.rs"),
        &format!(
            "fn a() {{\n    // mode-bits-ok: a directory the second uid traverses\n{}\n}}\n",
            chmod_line("755")
        ),
    );
    boss_testing::write_file(
        &tests.join("inline.rs"),
        &format!(
            "fn stub() {{\n    std::fs::write(&p, body).unwrap();\n{}\n}}\n",
            chmod_line("755")
        ),
    );
    let offenders = undeclared_exec_writes(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        offenders.len(),
        1,
        "exactly the inline site is named — not the definition, not the declared one:\n{}",
        offenders.join("\n")
    );
    assert!(
        offenders[0].contains("crates/core/boss-testing/tests/inline.rs:3"),
        "the verdict names file and line, so nobody re-derives them:\n{}",
        offenders[0]
    );
}

/// THE RATCHET. The real tree has no inline executable write outside
/// the definition; a car that adds one is named here before it is a
/// `Text file busy` on a gate that has nothing to do with it.
#[test]
fn every_executable_under_crates_is_written_by_write_exec() {
    let root = repo_root();
    assert!(
        root.join(THE_DEFINITION).exists(),
        "the one definition moved — update THE_DEFINITION, do not widen the exclusion"
    );
    let offenders = undeclared_exec_writes(&root);
    assert!(
        offenders.is_empty(),
        "{} line(s) set an executable bit from this process without going through \
         boss_testing::write_exec. A file this process held open for writing is briefly \
         open in every child a sibling test thread spawns until that child's exec, and \
         the exec of the just-written file then loses with ETXTBSY (backlog 6eaef658, \
         reproduced 8/8). Route the write through boss_testing::write_exec, or — when the \
         path is not an executable this process runs (a directory, a binary renamed over \
         the CLI) — say why on the line or the comment above it: `{MARKER} <reason>`.\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
