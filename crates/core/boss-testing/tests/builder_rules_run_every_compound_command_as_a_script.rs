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
//! the class the guard refuses. Script CONTENT that is not git (the
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

/// MEASURED 2026-09-23 (backlog 83081e2b, runs 21976a09 and a2fcdc4f):
/// the guard also refuses a SINGLE `boss step complete <packet> --step
/// <s> --field-file …` as running a string — it reads `complete` as a
/// command runner — so two builders rediscovered that a boss verb goes
/// through a script too. Rule 0 named git, boss-api loops and heredocs,
/// and no boss verb, so "one plain command" read as covering them.
#[test]
fn rule_zero_names_the_boss_verbs_that_run_from_a_script() {
    let text = rules();
    let zero = rule_zero(&text).unwrap_or_default();
    for verb in ["boss step complete", "boss job file", "boss gate"] {
        assert!(
            zero.contains(&format!("`{verb}")),
            "rule 0 of {RULES} must name `{verb}` among the commands written to a \
             script first — the worktree guard refuses a boss verb even as one \
             command, and builders rediscover it without the name (backlog 83081e2b): \
             {zero}"
        );
    }
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
