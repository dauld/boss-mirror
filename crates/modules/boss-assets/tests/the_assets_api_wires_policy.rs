//! The production binary builds the assets surface WITH a policy
//! client (backlog 54bf2e1e, 2026-09-26).
//!
//! `None` is the test path — the asset-event gate in `http.rs` is
//! `if let Some(ref policy) = state.policy`, so it allows — and the
//! binary passed it, so the Update-on-asset check never ran in
//! production, whatever the handler's comment said. The same defect
//! boss-people had until 8cdad84c, pinned the same way
//! (`the_people_api_wires_policy.rs`): this reads the binary's source,
//! because the wiring is the one place a port-level test cannot reach.

const BINARY: &str = include_str!("../src/bin/boss_assets_api.rs");

#[test]
fn the_assets_api_binary_never_builds_its_state_without_policy() {
    let code: Vec<&str> = BINARY
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let unwired: Vec<&&str> = code.iter().filter(|l| l.contains("policy: None")).collect();
    assert!(
        unwired.is_empty(),
        "boss_assets_api.rs builds its state with no policy: {unwired:?}"
    );
    let source = code.join("\n");
    // The sim is admitted through the bypass wrapper, as people and
    // ledger do; a bare ReqwestPolicyClient would 403 the brewery sim.
    assert!(
        source.contains("SimBypassPolicyClient::from_env("),
        "boss_assets_api.rs does not wire SimBypassPolicyClient::from_env(..)"
    );
    assert!(
        source.contains("ReqwestPolicyClient::new("),
        "boss_assets_api.rs does not build a ReqwestPolicyClient"
    );
}
