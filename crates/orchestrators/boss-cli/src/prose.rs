//! Prose that reaches a verb through argv, and the one door that keeps
//! the shell out of it.
//!
//! WHY. CLAUDE.md §Doors carries the warning "prose with backticks does
//! not survive argv" and scopes it to `boss gate --park-*`. The hazard
//! is not specific to `--park-*`: it belongs to every flag that carries
//! a sentence, and the operator surface now has several — `boss triage
//! --evidence`, `boss fold --change`, `boss design --markdown`, `boss
//! prove --verified`, `boss dispatch --summary`, `boss job file
//! --title`. Measured instance, 2026-09-19 (backlog 2376b89e): a triage
//! of f3e091f0 was typed inside DOUBLE quotes with a backticked word in
//! it, and the record now reads "Ordering trap confirmed:  is required
//! of every rule" — the word gone, the sentence still grammatical, a
//! precise constraint inverted into nothing. It was caught only because
//! bash printed "why: command not found"; a real command, or a
//! redirected stderr, and the record would be confidently wrong with
//! nothing to read it by. Worse, the triage step was already completed
//! by then, so the step API refuses a metadata write and the correction
//! had to ride `PATCH /api/jobs/{id}/metadata` as an annotation BESIDE
//! the wrong text rather than replacing it.
//!
//! ## What a verb can and cannot detect
//!
//! This is the finding, and it points the other way from the obvious
//! fix. The packet proposed refusing prose containing an unescaped
//! backtick, at the verb. That refusal cannot work, and shipping it
//! would be worse than shipping nothing, because it fires on exactly
//! the spellings that are FINE and never on the one that hurts:
//!
//! - single quotes round the value: the backtick arrives INTACT, and
//!   the record is what the operator typed. This is the door.
//! - a backslash-escaped backtick: same, intact and correct.
//! - double quotes, or no quotes: the shell runs the substitution and
//!   what arrives is a string with a hole in it. No backtick remains.
//!
//! The shell performs command substitution BEFORE the process starts.
//! By the time argv exists the backtick and its contents are gone, and
//! the substitution's exit status is discarded, so nothing about it is
//! visible to the child. A literal-backtick check would therefore
//! refuse every correct caller and pass every broken one.
//!
//! Nor is the hole provable. An empty substitution leaves an artifact —
//! a doubled space, a sentence that reads short — but a doubled space
//! is also just typing, and a false refusal on legitimate prose would
//! cost more than the defect it hunts. A substitution that produced
//! OUTPUT leaves no artifact at all. The one unambiguous case is a
//! value that came out EMPTY, which is refused below.
//!
//! ## So the mechanism is to remove the shell, not to inspect its
//! ## leavings
//!
//! `boss design --markdown-file` already exists for the same reason one
//! flag over (backlog 1763d5af): long prose belongs in a file, and a
//! file's bytes never pass through word expansion. This module is that
//! door, shared — a verb declares its text flag and a `-file` twin,
//! calls [`text_or_file`], and the operator with a paragraph to record
//! writes it to a file and passes the path. Single quotes stay correct
//! for a short sentence, and are now the documented rule for every
//! prose flag rather than a `--park-*` footnote.

use std::path::Path;

use anyhow::{Context, Result, bail};

/// The prose a verb was given: the text flag, or the file flag that
/// reads it with no shell in the path. Exactly one is expected — clap
/// holds them exclusive and requires one — and the refusals here are
/// for what clap cannot say.
///
/// `flag` is the text flag's spelling (`--evidence`); `file_flag` is
/// the door named in every refusal, so the operator reads the fix
/// rather than the diagnosis.
pub(crate) fn text_or_file(
    flag: &str,
    file_flag: &str,
    text: Option<String>,
    file: Option<&Path>,
) -> Result<String> {
    let prose = match (text, file) {
        (_, Some(path)) => std::fs::read_to_string(path)
            .with_context(|| format!("{file_flag}: reading {}", path.display()))?,
        (Some(text), None) => {
            // A path handed to the text flag is the 1763d5af defect,
            // and now that this flag has a file door the refusal can
            // name it. Same predicate design.rs refuses a doc body
            // with — one definition of "this value is a path".
            if crate::design::path_shaped(&text, |p| Path::new(p).is_file()) {
                bail!(
                    "{flag} takes the text itself, and {:?} is a path, not prose — pass \
                     `{file_flag} <PATH>` to read it from that file. A packet recorded \
                     with a path for its prose reaches the reader with nothing to read.",
                    text.trim()
                );
            }
            text
        }
        (None, None) => bail!("{flag} (or `{file_flag} <PATH>`) is required"),
    };
    // A file ends with a newline; a sentence does not. An editor's
    // final newline should not change what the record holds.
    let prose = prose.trim_end().to_string();
    if prose.trim().is_empty() {
        bail!(
            "{flag} is empty — nothing would be recorded. If a command substitution ate \
             it, quote the text with SINGLE quotes or pass it through `{file_flag} <PATH>`: \
             backticks inside double quotes are run by the shell before this verb sees them."
        );
    }
    Ok(prose)
}

/// [`text_or_file`] for a value that may legitimately be absent — a
/// car with no probe at all proves by event instead. `None` only when
/// NEITHER was given; once either is present every refusal above
/// applies, because a probe supplied badly is worse than no probe.
pub(crate) fn opt_text_or_file(
    flag: &str,
    file_flag: &str,
    text: Option<String>,
    file: Option<&Path>,
) -> Result<Option<String>> {
    if text.is_none() && file.is_none() {
        return Ok(None);
    }
    text_or_file(flag, file_flag, text, file).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::scratch::{scratch_dir, write_file};

    /// A PROBE IS SHELL, SO IT IS THE VALUE ARGV DAMAGES MOST
    /// (backlog 302bc2f2). The file door exists so a probe never has
    /// to survive a shell: what the file holds is what gets recorded,
    /// backslashes, quotes and all.
    #[test]
    fn a_probe_read_from_a_file_keeps_its_quoting_exactly() {
        let dir = scratch_dir("prose-probe-file");
        let probe = "n=$(git show HEAD:a.rs | grep -c 'pub enum StepAction' || true)\nexit 75\n";
        let path = dir.join("probe.sh");
        write_file(&path, probe);
        let got = opt_text_or_file("--park-probe", "--park-probe-file", None, Some(&path))
            .unwrap()
            .expect("a file was given");
        assert!(got.contains("'pub enum StepAction'"), "{got}");
        assert!(
            !got.contains(concat!(r"\", r#"""#)),
            "no backslash-quote may appear: {got}"
        );
        // The editor's trailing newline is not part of the probe.
        assert!(got.ends_with("exit 75"), "{got}");
    }

    /// Absent is absent: a car proving by EVENT passes neither flag,
    /// and that must not be an error.
    #[test]
    fn neither_flag_is_no_probe_rather_than_a_refusal() {
        assert!(
            opt_text_or_file("--park-probe", "--park-probe-file", None, None)
                .unwrap()
                .is_none()
        );
        // …but a probe supplied EMPTY is still refused: worse than none.
        assert!(
            opt_text_or_file(
                "--park-probe",
                "--park-probe-file",
                Some("   ".to_string()),
                None
            )
            .is_err()
        );
    }

    /// The finding, pinned as behaviour: prose containing a literal
    /// backtick is ACCEPTED. It has to be. A backtick that survived to
    /// argv was single-quoted or escaped — the two spellings that work
    /// — while the damaging spelling (double quotes) arrives with the
    /// backtick and its contents already removed by the shell. A
    /// literal-backtick refusal would therefore refuse every correct
    /// caller and pass every broken one. If this test is ever changed
    /// to expect a refusal, read the module doc first (backlog
    /// 2376b89e).
    #[test]
    fn backticked_prose_is_accepted_because_the_damaging_case_carries_no_backtick() {
        let survived = "Ordering trap confirmed: `why` is required of every rule";
        let eaten = "Ordering trap confirmed:  is required of every rule";
        assert!(survived.contains('`'), "single-quoted: the backtick lands");
        assert!(!eaten.contains('`'), "double-quoted: nothing lands");
        for prose in [survived, eaten] {
            let got = text_or_file("--evidence", "--evidence-file", Some(prose.into()), None)
                .expect("both are accepted; only one of them is what was meant");
            assert_eq!(got, prose);
        }
    }

    /// The door: the file's bytes, with no shell between them and the
    /// record. The file's final newline is the file's, not the
    /// sentence's.
    #[test]
    fn the_file_door_reads_the_bytes() {
        let dir = scratch_dir("boss-cli-prose-file");
        let path = dir.join("evidence.md");
        write_file(
            &path,
            "Ordering trap confirmed: `why` is required of every rule\n",
        );
        let got = text_or_file("--evidence", "--evidence-file", None, Some(&path))
            .expect("the file is read");
        assert_eq!(
            got, "Ordering trap confirmed: `why` is required of every rule",
            "the backtick survives and the file's final newline does not"
        );
    }

    /// A missing file names itself and the flag, rather than an errno.
    #[test]
    fn a_missing_file_names_the_path() {
        let dir = scratch_dir("boss-cli-prose-missing");
        let path = dir.join("nope.md");
        let err = text_or_file("--evidence", "--evidence-file", None, Some(&path))
            .expect_err("refused")
            .to_string();
        assert!(
            err.contains("--evidence-file") && err.contains("nope.md"),
            "{err}"
        );
    }

    /// An emptied value is the one artifact of this defect that is
    /// unambiguous — nothing at all would be recorded — so it is
    /// refused, and the refusal teaches the quoting rule.
    #[test]
    fn empty_prose_is_refused_and_names_the_quoting_rule() {
        for empty in ["", "   ", "\n"] {
            let err = text_or_file("--evidence", "--evidence-file", Some(empty.into()), None)
                .expect_err("refused")
                .to_string();
            assert!(err.contains("SINGLE quotes"), "{err}");
        }
    }

    /// A path in the text flag is the 1763d5af shape, and the refusal
    /// names this flag's own file door rather than `--markdown-file`.
    #[test]
    fn a_path_in_the_text_flag_names_this_flags_door() {
        let err = text_or_file(
            "--change",
            "--change-file",
            Some("docs/design/fold-change.md".into()),
            None,
        )
        .expect_err("refused")
        .to_string();
        assert!(
            err.contains("--change-file") && err.contains("fold-change.md"),
            "{err}"
        );
        // Prose that merely mentions a file is prose.
        text_or_file(
            "--change",
            "--change-file",
            Some("folded into architecture-decisions.md".into()),
            None,
        )
        .expect("accepted");
    }

    /// Neither flag given is a refusal that names both.
    #[test]
    fn neither_flag_is_refused() {
        let err = text_or_file("--evidence", "--evidence-file", None, None)
            .expect_err("refused")
            .to_string();
        assert!(
            err.contains("--evidence") && err.contains("--evidence-file"),
            "{err}"
        );
    }
}
