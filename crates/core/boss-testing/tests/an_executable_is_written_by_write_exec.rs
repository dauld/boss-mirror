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
//! TWO MORE SHAPES, NO CHMOD (backlog eed361e0, 2026-09-26). A
//! `std::fs::copy` keeps the source's execute bit, and a `std::fs::write`
//! over a file already executable keeps its own — both hold the file
//! open in this process, and neither spells an octal. The live site was
//! `undeclared_objects_sh.rs`: a copy, then a raw write, of a `0755`
//! derivation its lint execs as `"$DERIVE" --list`. A grep cannot know a
//! runtime mode, so the rule reads the one static sign there is: a copy
//! or a write whose call names a `.sh` path literal. Measured on the
//! tree at 17b7e526: 4 of the 55 lines naming `fs::copy`, and 3
//! `fs::write` calls — each a script run as `bash <path>`, sourced, or
//! a new `0644` file, and so declared. The third
//! shape is `set_permissions` handed ANOTHER file's `permissions()` —
//! a copy's mode carried over (`a_verb_declares_the_hosts_it_serves_sh`
//! `copy_dir`): 1 site. Each goes through `copy_exec` or carries the
//! marker. A copy whose path is a variable (`DERIVE_REL`) is not seen;
//! the pin narrows the class, it does not close it.
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

/// The std calls that write a file's bytes from this process, split
/// from their paren so the refused shape is never spelled here whole.
const WRITERS: &[&str] = &["fs::copy", "fs::write"];

/// A script path, as a string literal ends one: the only static sign a
/// line-grep has that the file it writes will be executable.
const SCRIPT_TAIL: &str = ".sh";

/// How many lines one call may span before the scan stops looking for
/// its closing `;` — rustfmt breaks a long call over three or four.
const CALL_SPAN: usize = 5;

/// Does the call starting on `lines[i]` copy or write a SCRIPT — a
/// `.sh` path literal anywhere in the call? The mode bit such a copy
/// keeps, or such a write finds already set, is invisible to
/// `sets_an_exec_bit`, and the write holds the file open in this
/// process (backlog eed361e0, 2026-09-26). A comment line is not a call.
fn writes_a_script(lines: &[&str], i: usize) -> bool {
    let line = lines[i];
    if line.trim_start().starts_with("//") {
        return false;
    }
    let script = format!("{SCRIPT_TAIL}\"");
    WRITERS.iter().any(|w| {
        let needle = format!("{w}(");
        line.find(&needle).is_some_and(|at| {
            let mut call = line[at..].to_string();
            for next in lines.iter().skip(i + 1).take(CALL_SPAN - 1) {
                if call.contains(';') {
                    break;
                }
                call.push_str(next);
            }
            call.contains(&script)
        })
    })
}

/// The first argument of the call `needle` opens on `line`, trimmed —
/// `None` when the call's arguments do not start on this line.
fn first_arg<'a>(line: &'a str, needle: &str) -> Option<&'a str> {
    let at = line.find(needle)? + needle.len();
    let rest = &line[at..];
    let end = rest.find([',', ')'])?;
    Some(rest[..end].trim()).filter(|a| !a.is_empty())
}

/// Does `lines[i]` set a path's permissions to ANOTHER path's — a
/// `metadata(<from>)…permissions()` read on this line or the few above,
/// handed to `set_permissions(<to>, …)`? That is `std::fs::copy` then
/// chmod with no octal in sight, and it carries a source's execute bit
/// onto a copy this process wrote (the `copy_dir` in
/// `a_verb_declares_the_hosts_it_serves_sh.rs`, eed361e0). The same
/// path on both sides is a mode read back and changed — an octal on
/// that line is `sets_an_exec_bit`'s business, not this.
fn copies_another_files_mode(lines: &[&str], i: usize) -> bool {
    let line = lines[i];
    if line.trim_start().starts_with("//") {
        return false;
    }
    let setter = format!("{}(", "set_permissions");
    let Some(to) = first_arg(line, &setter) else {
        return false;
    };
    let reader = format!("{}(", "metadata");
    lines[i.saturating_sub(3)..=i].iter().any(|l| {
        l.contains(".permissions()") && first_arg(l, &reader).is_some_and(|from| from != to)
    })
}

/// Every `file:line` under `root` (relative to it) that sets an
/// executable bit, copies or writes a script, or carries another file's
/// mode, without going through the definition or a declared reason.
/// Pure over the tree so the fixtures below can drive it.
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
            let hit = sets_an_exec_bit(line)
                || writes_a_script(&lines, i)
                || copies_another_files_mode(&lines, i);
            if hit && !is_declared(line, above) {
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

/// A copy or a write naming a script path is read as one, over the
/// lines rustfmt breaks it into; a data file, a comment, and the door
/// itself are not.
#[test]
fn a_copied_or_written_script_is_read_and_a_data_file_is_not() {
    let copy = format!("{}(", WRITERS[0]);
    let write = format!("{}(", WRITERS[1]);
    let script = format!("\"infra/gate{SCRIPT_TAIL}\"");
    let one_line = format!("    std::{copy}root.join({script}), &dst).unwrap();");
    assert!(writes_a_script(&[one_line.as_str()], 0));
    let broken = [
        format!("    std::{copy}"),
        format!("        root.join({script}),"),
        "        &dst,".to_string(),
        "    )".to_string(),
        "    .unwrap();".to_string(),
    ];
    let broken: Vec<&str> = broken.iter().map(String::as_str).collect();
    assert!(
        writes_a_script(&broken, 0),
        "a call rustfmt broke over five lines is still one call"
    );
    let written = format!("    std::{write}dir.join(\"run{SCRIPT_TAIL}\"), body).unwrap();");
    assert!(writes_a_script(&[written.as_str()], 0));
    for miss in [
        format!("    std::{copy}root.join(\"verbs.json\"), &dst).unwrap();"),
        format!("    // std::{copy}root.join({script}), &dst) — prose"),
        format!("    boss_testing::copy_exec(&root.join({script}), &dst);"),
    ] {
        assert!(
            !writes_a_script(&[miss.as_str()], 0),
            "not a finding: {miss}"
        );
    }
    let next_statement = [
        format!("    std::{copy}&a, &b).unwrap();"),
        format!("    run({script});"),
    ];
    let next_statement: Vec<&str> = next_statement.iter().map(String::as_str).collect();
    assert!(
        !writes_a_script(&next_statement, 0),
        "the scan stops at the call's own semicolon"
    );
}

/// `set_permissions` handed ANOTHER file's permissions is a copy's
/// mode carried over; the same file's permissions changed is not.
#[test]
fn another_files_mode_is_read_and_a_files_own_is_not() {
    let set = format!("{}(", "set_permissions");
    let carried = [
        "    let mode = std::fs::metadata(&from).expect(\"metadata\").permissions();".to_string(),
        format!("    let _ = std::fs::{set}&to, mode);"),
    ];
    let carried: Vec<&str> = carried.iter().map(String::as_str).collect();
    assert!(copies_another_files_mode(&carried, 1));
    let inline = format!("    std::fs::{set}&to, std::fs::metadata(&from)?.permissions())?;");
    assert!(copies_another_files_mode(&[inline.as_str()], 0));
    let own = [
        "    let mut perm = std::fs::metadata(&path).unwrap().permissions();".to_string(),
        "    perm.readonly();".to_string(),
        format!("    std::fs::{set}&path, perm).unwrap();"),
    ];
    let own: Vec<&str> = own.iter().map(String::as_str).collect();
    assert!(
        !copies_another_files_mode(&own, 2),
        "a file's own mode, changed"
    );
    let method = [
        "    let mut perms = f.metadata()?.permissions();".to_string(),
        format!("    f.{set}perms)?;"),
    ];
    let method: Vec<&str> = method.iter().map(String::as_str).collect();
    assert!(
        !copies_another_files_mode(&method, 1),
        "an open file's own mode"
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
    boss_testing::write_file(
        &tests.join("copied.rs"),
        &format!(
            "fn a() {{\n    // mode-bits-ok: run as bash <path>, opened read-only\n    \
             std::{w}root.join(\"a{SCRIPT_TAIL}\"), &d).unwrap();\n    \
             std::{w}root.join(\"b{SCRIPT_TAIL}\"), &d).unwrap();\n}}\n",
            w = format!("{}(", WRITERS[0])
        ),
    );
    let offenders = undeclared_exec_writes(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        offenders.len(),
        2,
        "exactly the inline chmod and the undeclared copy are named — not the \
         definition, not the declared ones:\n{}",
        offenders.join("\n")
    );
    assert!(
        offenders[0].contains("crates/core/boss-testing/tests/copied.rs:4")
            && offenders[1].contains("crates/core/boss-testing/tests/inline.rs:3"),
        "the verdict names file and line, so nobody re-derives them:\n{}",
        offenders.join("\n")
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
        "{} line(s) set an executable bit, copy or write a script, or carry another \
         file's mode from this process without going through boss_testing::write_exec \
         or boss_testing::copy_exec. A file this process held open for writing is briefly \
         open in every child a sibling test thread spawns until that child's exec, and \
         the exec of the just-written file then loses with ETXTBSY (backlog 6eaef658, \
         reproduced 8/8; the copy shape is eed361e0). Route the write through \
         boss_testing::write_exec or copy_exec, or — when the path is not an executable \
         anything execs directly (a directory, a script only ever run as `bash <path>`, a \
         binary renamed over the CLI) — say why on the line or the comment above it: \
         `{MARKER} <reason>`.\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
