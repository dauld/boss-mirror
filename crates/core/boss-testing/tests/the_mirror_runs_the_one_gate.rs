//! The public mirror's CI runs THE gate — `infra/gate.sh`, full mode —
//! and not a second list of checks beside it.
//!
//! WHY THE MIRROR RUNS ANYTHING AGAIN (backlog 2328c95e, and David's
//! feedback 5565fe33 "Github CI checks no longer running, stuck on
//! yellow"). From 2026-09-08 (design 7b59af2c) the mirror carried only
//! CodeQL and Scorecard, both `build-mode: none`, so a green publish PR
//! meant static analysis found nothing and said NOTHING about whether
//! the tree builds — while a reader of a public repository reasonably
//! reads a green check as the opposite. Measured 2026-09-22 on PR 241:
//! three check runs, all CodeQL. The mirror is public, so its runner
//! minutes cost nothing; what it costs is a second definition to keep
//! green, which is what this file refuses.
//!
//! CLAUDE.md §9a. The GitHub workflow and the cluster gate cannot be
//! one file — one is GitHub Actions YAML, the other a pod spec — so the
//! ENVIRONMENT lives twice (toolchain, Postgres, bun) and the CHECKS
//! live once, in the script both invoke. That was the shape of the
//! mirror's ci.yml before its deletion (it ran `infra/gate.sh`), and of
//! the forge's `test` job before design 128b5496; the pins below are
//! the ones `gate_sh.rs` carried for it then, restored.

use boss_testing::repo_root;

const MIRROR_CI: &str = ".github/workflows/ci.yml";

fn mirror_ci() -> String {
    let path = repo_root().join(MIRROR_CI);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} — the public mirror has no workflow that builds or tests \
             the tree, so its green check means only that CodeQL found nothing \
             (backlog 2328c95e)",
            path.display()
        )
    })
}

/// Every `run:` command in the workflow, single-line and block form,
/// trimmed. Comments are YAML's, not the shell's, so they are dropped:
/// a comment that MENTIONS `cargo test` is prose, not a second gate.
fn run_lines(yml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut block_indent: Option<usize> = None;
    for line in yml.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if let Some(bi) = block_indent {
            if t.is_empty() || indent > bi {
                if !t.is_empty() && !t.starts_with('#') {
                    out.push(t.to_string());
                }
                continue;
            }
            block_indent = None;
        }
        let body = t.strip_prefix("- ").unwrap_or(t);
        if let Some(cmd) = body.strip_prefix("run:") {
            let cmd = cmd.trim();
            if cmd == "|" || cmd == ">" || cmd == "|-" || cmd == ">-" {
                block_indent = Some(indent);
            } else if !cmd.is_empty() {
                out.push(cmd.to_string());
            }
        }
    }
    out
}

/// The mirror runs the one definition, in FULL mode. `--quick`,
/// `--lint`, `--auto` and `-p` each run a subset, and a subset under a
/// green badge is the defect this workflow exists to end.
#[test]
fn the_mirror_ci_runs_the_full_gate() {
    let runs = run_lines(&mirror_ci());
    let gate: Vec<&String> = runs
        .iter()
        .filter(|r| r.contains("infra/gate.sh"))
        .collect();
    assert!(
        !gate.is_empty(),
        "{MIRROR_CI} never runs infra/gate.sh — the mirror's checks have forked \
         away from the one gate definition; run lines: {runs:?}"
    );
    for line in gate {
        for subset in [
            "--quick",
            "--lint",
            "--auto",
            "-p ",
            "--roster",
            "--self-test",
        ] {
            assert!(
                !line.contains(subset),
                "{MIRROR_CI} runs `{line}` — `{subset}` is a subset of the gate, \
                 and the mirror's green must mean the full gate passed"
            );
        }
    }
}

/// Environment setup stays in the workflow; checks do not. An inline
/// check beside the script call is the two-definition state §9a names.
#[test]
fn the_mirror_ci_has_no_inline_second_definition() {
    let runs = run_lines(&mirror_ci());
    let inline_checks = [
        "cargo clippy",
        "cargo test",
        "cargo fmt",
        "cargo build",
        "infra/lint/",
        "bun run test",
        "bun run build",
        "bun run typecheck",
    ];
    for line in &runs {
        for needle in inline_checks {
            assert!(
                !line.contains(needle),
                "{MIRROR_CI} runs `{line}` beside infra/gate.sh — `{needle}` is a \
                 check, and a check here is a second definition of the gate; move \
                 it into the script"
            );
        }
    }
}

/// The DB-backed suites reach Postgres the way the cluster gate's do:
/// through `BOSS_TEST_POSTGRES_ADMIN_URL`, at a service container that
/// hosts no database named `boss`, so TestDb's production-server guard
/// holds without the override (`BOSS_TEST_ALLOW_PRODUCTION_SERVER`
/// exists for a disposable server that DOES host one, and this one
/// need not).
#[test]
fn the_mirror_ci_gives_the_suites_a_scratch_postgres() {
    let yml = mirror_ci();
    for needle in [
        "BOSS_TEST_POSTGRES_ADMIN_URL",
        "image: postgres:",
        "POSTGRES_DB: postgres",
    ] {
        assert!(
            yml.contains(needle),
            "{MIRROR_CI} is missing `{needle}` — without a scratch Postgres the \
             gate's `fixture` and DB-backed `test` checks red on the runner, not \
             on the tree"
        );
    }
    assert!(
        !yml.contains("BOSS_TEST_ALLOW_PRODUCTION_SERVER"),
        "{MIRROR_CI} sets BOSS_TEST_ALLOW_PRODUCTION_SERVER — the service container \
         hosts no `boss` database, so the guard needs no override; setting it anyway \
         disarms the one check that tells a scratch server from a deployment"
    );
}

/// Every action pinned to a full commit sha, as codeql.yml and
/// scorecard.yml already are: a tag moves under you, and Scorecard's
/// Pinned-Dependencies check scores the repository on exactly this.
#[test]
fn the_mirror_ci_pins_every_action_to_a_sha() {
    let yml = mirror_ci();
    for line in yml.lines() {
        let t = line.trim().trim_start_matches("- ");
        let Some(spec) = t.strip_prefix("uses:") else {
            continue;
        };
        let spec = spec.split('#').next().unwrap_or("").trim();
        let sha = spec.rsplit('@').next().unwrap_or("");
        assert!(
            spec.contains('@') && sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
            "{MIRROR_CI}: `{}` is not pinned to a 40-hex commit sha",
            line.trim()
        );
    }
}

#[test]
fn run_lines_reads_both_forms_and_drops_comments() {
    let yml = "steps:\n  - run: infra/gate.sh\n  - name: x\n    run: |\n      # cargo test is prose here\n      set -eu\n      echo hi\n  - name: y\n    run: echo bye\n";
    assert_eq!(
        run_lines(yml),
        vec!["infra/gate.sh", "set -eu", "echo hi", "echo bye"]
    );
}
