//! The builder rules must not steer a builder into a command shape the
//! worktree-isolation guard refuses, and must say, before the first
//! rule that shows a command, what shape passes instead.
//!
//! MEASURED 2026-09-23 (backlog 68535488), across that day's builders
//! (runs 27bfbdac, 2c623d2e, 161944b2 and others). In an isolated
//! worktree the harness's guard refuses, as "too complex to verify", a
//! python3 heredoc, `cat > file <<EOF`, any command after a heredoc, a
//! compound command naming git or looping over boss-api, and `$((...))`
//! arithmetic in a loop. The rules showed exactly those shapes — a
//! `&&` chain of two git commands in rule 1, a `git show … | grep`
//! rehearsal in rule 7 — and every builder rediscovered the same way
//! round: write the script with the Write tool into the scratch dir and
//! run it by absolute path, which always passes. Rule 0 says so once,
//! and this pin holds every command the rules show to that shape.
//!
//! What it reads is the document as authored: every backticked span
//! outside rule 0 (rule 0 NAMES the refused shapes, so it is the one
//! place they may appear). A span that is a heredoc or `$((` is
//! refused wherever it stands; a span that names `git` or `boss-api`
//! must be ONE command — no `&&`, `||`, `;` or `|` — because that is
//! the class the guard refuses. A span carrying `complete` or `eval` as
//! a whole word is refused even as one command, because the guard reads
//! either as a command runner (backlog a057aa5d). Script CONTENT that is not git (the
//! gate command, a probe's `case` guard) is written into a file by the
//! rule that shows it, so it is not an inline command and not judged.

use boss_testing::repo_root;

const RULES: &str = "infra/platform/documents/builder-rules.md";

fn rules() -> String {
    std::fs::read_to_string(repo_root().join(RULES)).expect("the builder rules are readable")
}

/// The text of rule 0: from its number to rule 1's.
fn rule_zero(text: &str) -> Option<&str> {
    let start = text.find("\n0. ")? + 1;
    let end = text[start..].find("\n1. ")? + start;
    Some(&text[start..end])
}

/// Every backticked span, in order. The document carries no fenced
/// block, so an odd index after splitting on the backtick is a span.
fn spans(text: &str) -> Vec<&str> {
    assert!(
        !text.contains("```"),
        "{RULES} gained a fenced block — this pin reads backticked spans only \
         and must learn to read fences before it can judge one"
    );
    text.split('`').skip(1).step_by(2).collect()
}

#[test]
fn rule_zero_says_how_to_run_more_than_one_command_before_any_rule_shows_one() {
    let text = rules();
    let zero = rule_zero(&text).unwrap_or_else(|| {
        panic!(
            "{RULES} must open with a rule 0 — before rule 1, the first to show a \
             command — saying that anything longer than one plain command is \
             written with the Write tool and run by absolute path (backlog 68535488)"
        )
    });
    for phrase in ["Write tool", "absolute path", "scratch dir", "heredoc"] {
        assert!(
            zero.contains(phrase),
            "rule 0 of {RULES} must say `{phrase}` — it is the whole of the \
             workaround every builder of 2026-09-23 rediscovered: {zero}"
        );
    }
}

/// The words the worktree guard reads as a command runner wherever they
/// stand as a whole word of a command, quoted alone or not. MEASURED
/// 2026-09-26 by the triage of backlog a057aa5d (run 9bdcfaab, one Bash
/// call each, inside an isolated worktree): `boss step complete --help`,
/// `boss step 'complete' --help`, `echo complete foo`, `echo foo complete`
/// and `echo eval foo` were refused; `echo 'the step is complete'` (the
/// word inside a longer quoted string), `boss job file --help`, `boss
/// triage --help`, `boss rerail --help` and `boss gate … --dry-run` passed.
const RUNNER_WORDS: [&str; 2] = ["complete", "eval"];

/// A span's shell words: whitespace splits them except inside single or
/// double quotes, and the quotes themselves are dropped — so `'complete'`
/// is the word `complete`, while `'the step is complete'` is one word.
fn shell_words(span: &str) -> Vec<String> {
    let (mut words, last, _) = span.chars().fold(
        (Vec::new(), String::new(), None::<char>),
        |(mut words, mut word, quote), c| match (quote, c) {
            (None, '\'' | '"') => (words, word, Some(c)),
            (Some(q), _) if c == q => (words, word, None),
            (None, _) if c.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                (words, word, None)
            }
            _ => {
                word.push(c);
                (words, word, quote)
            }
        },
    );
    if !last.is_empty() {
        words.push(last);
    }
    words
}

/// Whether a command span carries a runner word as a whole shell word —
/// the shape the guard refuses. Quotes around the word alone do not hide
/// it (`boss step 'complete'` was refused); the word inside a longer
/// quoted string, or glued to more text (`status=complete`), is a
/// different word.
fn carries_a_runner_word(span: &str) -> bool {
    shell_words(span)
        .iter()
        .any(|w| RUNNER_WORDS.contains(&w.as_str()))
}

/// MEASURED 2026-09-23 (backlog 83081e2b, runs 21976a09 and a2fcdc4f):
/// the guard refuses a SINGLE `boss step complete <packet> --step <s>
/// --field-file …` as running a string, because it reads `complete` as a
/// command runner, so that verb goes through a script. Car 83081e2b then
/// wrote the wider claim that EVERY boss verb is refused even as one
/// command and named `boss job file` and `boss gate` beside it; only the
/// one verb had been measured, and the triage of a057aa5d measured the
/// other two passing (see `RUNNER_WORDS`). A rule that overstates costs
/// every builder a script per verb, so rule 0 names the trigger words and
/// the one verb that carries one, and not the wider claim.
#[test]
fn rule_zero_names_the_words_that_send_a_boss_verb_to_a_script() {
    let text = rules();
    let zero = rule_zero(&text).unwrap_or_default();
    assert!(
        zero.contains("`boss step complete"),
        "rule 0 of {RULES} must name `boss step complete` as a command written \
         to a script first — the worktree guard refuses it even as one command \
         (backlog 83081e2b): {zero}"
    );
    for word in RUNNER_WORDS {
        assert!(
            zero.contains(&format!("`{word}`")),
            "rule 0 of {RULES} must name the word `{word}` — a command carrying it \
             as a whole word is what the guard refuses, whatever the verb \
             (backlog a057aa5d): {zero}"
        );
    }
    let lower = zero.to_lowercase();
    for overstated in ["verb even as one command", "boss` verb even as one"] {
        assert!(
            !lower.contains(overstated),
            "rule 0 of {RULES} claims every boss verb is refused even as one \
             command (`{overstated}`); only a command carrying `complete` or \
             `eval` was measured refused, and `boss job file` and `boss gate` \
             were measured passing (backlog a057aa5d): {zero}"
        );
    }
    for verb in ["boss job file", "boss gate"] {
        assert!(
            zero.contains(&format!("`{verb}`")),
            "rule 0 of {RULES} must name `{verb}` as a verb that runs as one plain \
             command, measured passing (backlog a057aa5d) — without the name a \
             builder generalises from `boss step complete` and scripts every verb: \
             {zero}"
        );
    }
}

/// A command the rules show outside rule 0 must not carry a runner word:
/// the guard refuses it as one command, so it belongs in a script, and
/// rule 0 is the one place the word may stand (backlog a057aa5d).
#[test]
fn no_command_the_rules_show_carries_a_word_the_guard_reads_as_a_runner() {
    let text = rules();
    let zero = rule_zero(&text).unwrap_or_default();
    let judged = text.replacen(zero, "", 1);
    let refused: Vec<String> = spans(&judged)
        .into_iter()
        .filter(|span| carries_a_runner_word(span))
        .map(|span| format!("`{span}`"))
        .collect();
    assert!(
        refused.is_empty(),
        "{RULES} shows a command carrying `complete` or `eval` as a whole word, \
         which the worktree guard refuses even as ONE command — show it as a \
         script written with the Write tool and run by absolute path (rule 0, \
         backlog a057aa5d):\n  {}",
        refused.join("\n  ")
    );
}

#[test]
fn a_runner_word_is_judged_as_a_whole_word_the_way_the_guard_measured() {
    // Refused as measured (a057aa5d): the word alone, quoted alone or not.
    assert!(carries_a_runner_word("boss step complete <p> --step s"));
    assert!(carries_a_runner_word("boss step 'complete' --help"));
    assert!(carries_a_runner_word("echo eval foo"));
    // Passed as measured: the word inside a longer quoted string, and the
    // verbs with no runner word at all.
    assert!(!carries_a_runner_word("echo 'the step is complete'"));
    assert!(!carries_a_runner_word(
        "boss gate b --dry-run --hold 'we complete it later and eval nothing'"
    ));
    assert!(!carries_a_runner_word("boss job file --help"));
    assert!(!carries_a_runner_word("boss rerail <car> --finish"));
    // Not measured, and a different word by the guard's own reading.
    assert!(!carries_a_runner_word("--field status=complete"));
}

#[test]
fn no_command_the_rules_show_is_a_shape_the_worktree_guard_refuses() {
    let text = rules();
    let zero = rule_zero(&text).unwrap_or_default();
    let judged = text.replacen(zero, "", 1);
    let mut refused = Vec::new();
    for span in spans(&judged) {
        if span.contains("<<") || span.contains("$((") {
            refused.push(format!("`{span}` — a heredoc or $((...)) arithmetic"));
            continue;
        }
        let names_git = span
            .split_whitespace()
            .any(|w| w == "git" || w == "boss-api");
        let compound = ["&&", "||", ";", "|"].iter().any(|op| span.contains(op));
        if names_git && compound {
            refused.push(format!("`{span}` — a compound command that names git"));
        }
    }
    assert!(
        refused.is_empty(),
        "{RULES} shows command shapes the worktree-isolation guard refuses — \
         write each as one plain command per call, or as a script written with \
         the Write tool and run by absolute path (rule 0, backlog 68535488):\n  {}",
        refused.join("\n  ")
    );
}
