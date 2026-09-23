//! The production binary builds every people surface WITH a policy
//! client (backlog 8cdad84c, 2026-09-23).
//!
//! `None` is the test path — the write gate allows — and the binary
//! passed it for as long as the gate existed, so the employee Update
//! gate never ran in production and nothing noticed: every handler test
//! wires its own policy, and the binary's `main` has no test. This
//! reads the binary's source, because the wiring is the one place a
//! port-level test cannot reach.

const BINARY: &str = include_str!("../src/bin/boss_people_api.rs");

#[test]
fn the_people_api_binary_never_builds_a_surface_without_policy() {
    let unwired: Vec<&str> = BINARY
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .filter(|l| l.contains("policy: None"))
        .collect();
    assert!(
        unwired.is_empty(),
        "boss_people_api.rs builds a surface with no policy: {unwired:?}"
    );
    // Each call is followed by the next `.merge(` of the router chain.
    for router in ["workflow_router(", "employee_changes_router("] {
        let call = BINARY
            .split(router)
            .nth(1)
            .and_then(|rest| rest.split(".merge(").next())
            .unwrap_or_else(|| panic!("{router} is no longer called here"));
        assert!(
            call.contains("Some(policy"),
            "{router} is called without the policy client: {call}"
        );
    }
}
