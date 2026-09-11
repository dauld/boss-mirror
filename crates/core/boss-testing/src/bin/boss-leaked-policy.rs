//! `boss-leaked-policy` — the AST half of CLAUDE.md §9, as JSON on
//! stdout.
//!
//! `infra/codebase-metrics.sh` shells out to this to fill
//! `registry.code_branches_on_kind`, the field it had been recording as
//! `null` because a regex cannot tell a leaked workflow policy from the
//! dispatcher's own handler table. The rule lives in
//! `boss_testing::leaked_policy`; this is argument parsing and an exit
//! status.
//!
//! USAGE
//!   boss-leaked-policy [--repo DIR] [--scope PATH]... [--all-sites]
//!
//! `--all-sites` prints every site and the rung it landed on, one line
//! each, to STDERR — the door for reading the classification back by
//! hand, which is the only evidence this number is worth filing daily.
//! It is stderr rather than stdout so the JSON contract with
//! `codebase-metrics.sh` is unaffected by passing it.
//!
//! `--repo` defaults to the current directory, `--scope` to
//! `crates/core` and may be repeated. Both the vocabulary and the code
//! are read from `--repo`, so the answer is a function of the tree handed
//! in — which is what lets the cadence point this at an extracted
//! `git archive` and get a number belonging to the sha it is measuring.
//!
//! EXIT STATUS — the vocabulary `infra/lint/lib/git-answer.sh` defines,
//! so a caller can tell the two apart without parsing prose:
//!   0  it read the tree and here is the count
//!   2  the arguments were wrong
//!   3  it never measured — no scope, no registry vocabulary, or a file
//!      it could not parse. An INFRASTRUCTURE refusal, never a tree with
//!      no leaked branches in it.

use std::path::PathBuf;
use std::process::ExitCode;

/// "I could not answer." Distinct from 0 (measured, here is the count)
/// on purpose — `infra/lint/lib/git-answer.sh` carries the argument, and
/// four lints threw the distinction away on 2026-09-11.
const CANNOT_ANSWER: u8 = 3;

const USAGE: &str = "usage: boss-leaked-policy [--repo DIR] [--scope PATH]... [--all-sites]";

fn main() -> ExitCode {
    let mut repo: Option<PathBuf> = None;
    let mut scopes: Vec<String> = Vec::new();
    let mut all_sites = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--all-sites" => all_sites = true,
            "--repo" => match args.next() {
                Some(v) => repo = Some(PathBuf::from(v)),
                None => {
                    eprintln!("boss-leaked-policy: --repo needs a directory\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "--scope" => match args.next() {
                Some(v) => scopes.push(v),
                None => {
                    eprintln!("boss-leaked-policy: --scope needs a path\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("boss-leaked-policy: unknown argument '{other}'\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    let repo = repo.unwrap_or_else(|| PathBuf::from("."));
    if scopes.is_empty() {
        // §9 is about CORE code: "if you find yourself adding a
        // `match kind { … }` in core code, there's a registry you should
        // be using instead". The scope is an argument rather than a
        // constant so widening it later is a caller's decision and shows
        // up in the row's `scopes` field either way.
        scopes.push("crates/core".to_string());
    }
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();

    match boss_testing::leaked_policy::scan(&repo, &scope_refs) {
        Ok(report) => {
            if all_sites {
                for site in &report.sites {
                    eprintln!(
                        "{:38} {}:{} match {} [{}]",
                        site.class.key(),
                        site.file,
                        site.line,
                        site.scrutinee,
                        site.literals.join(" | ")
                    );
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&report.to_json())
                    .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            // The whole refusal, not a digest of it: the caller that
            // reduces this to "could not count" is the caller that makes
            // the next person re-derive it.
            eprintln!("boss-leaked-policy: CANNOT ANSWER — {e}");
            ExitCode::from(CANNOT_ANSWER)
        }
    }
}
