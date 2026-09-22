//! The builder rules must name every web check the gate's web-suite
//! phase runs, so a frontend car runs what will judge it.
//!
//! MEASURED 2026-09-22 (backlog 3566f5b4). A car's first gate went red
//! on six checks, and FIVE of them were mocked specs under
//! `apps/web/tests/mocked/`. Rule 10 mentioned mocked specs only as a
//! reason to have node_modules and never gave the command; the command
//! a builder does reach for, `bun test src/`, is `test:unit` and reads
//! nothing under `tests/`. Two minutes of `bun run test:mocked` would
//! have named all five.
//!
//! CLAUDE.md §9a: the roster is not retyped here. `infra/gate.sh`'s
//! web-suite check is the definition, and this test reads the `bun run
//! <script>` names out of it — so a phase added there and not written
//! into the rules fails HERE, by name, rather than on a builder's
//! first gate.

use boss_testing::repo_root;

fn gate_web_suite_scripts() -> Vec<String> {
    let gate = std::fs::read_to_string(repo_root().join("infra/gate.sh")).expect("infra/gate.sh");
    let line = gate
        .lines()
        .find(|l| l.contains("check \"web-suite"))
        .expect("infra/gate.sh runs a web-suite check");
    let scripts: Vec<String> = line
        .split("bun run ")
        .skip(1)
        .map(|rest| {
            rest.split([' ', '\'', '"', '&'])
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        !scripts.is_empty(),
        "the web-suite check must name its scripts as `bun run <name>`: {line}"
    );
    scripts
}

#[test]
fn the_rules_name_every_script_the_gates_web_suite_runs() {
    let rules =
        std::fs::read_to_string(repo_root().join("infra/platform/documents/builder-rules.md"))
            .expect("the builder rules are readable");
    for script in gate_web_suite_scripts() {
        assert!(
            rules.contains(&format!("bun run {script}")),
            "the builder rules must tell a frontend car to run `bun run {script}` — \
             the gate's web-suite phase does, and five mocked specs redded a car \
             on 2026-09-22 because the rules never named it"
        );
    }
}
