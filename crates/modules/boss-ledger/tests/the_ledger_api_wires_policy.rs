//! The production binary builds the ledger surface with the REAL policy
//! engine (backlog 7048afa8, 2026-09-26).
//!
//! `LedgerApiState.policy` is a required client, so the type system
//! already refuses a surface with none. What it cannot refuse is a
//! binary handed an allow-all test client — `PermissivePolicyClient` or
//! `FakePolicyClient` would compile, and would open the company's books
//! exactly as the old `policy: None` did. This reads the binary's
//! source, because the wiring is the one place a port-level test cannot
//! reach — the shape of boss-people's `the_people_api_wires_policy`.

const BINARY: &str = include_str!("../src/bin/boss_ledger_api.rs");

fn code_lines() -> impl Iterator<Item = &'static str> {
    BINARY.lines().filter(|l| !l.trim_start().starts_with("//"))
}

#[test]
fn the_ledger_api_binary_wires_the_policy_engine() {
    let state = BINARY
        .split("LedgerApiState {")
        .nth(1)
        .and_then(|rest| rest.split("};").next())
        .expect("boss_ledger_api.rs no longer builds a LedgerApiState");
    let policy = state
        .split("policy:")
        .nth(1)
        .expect("the LedgerApiState literal names no policy field");
    assert!(
        policy.contains("ReqwestPolicyClient"),
        "the ledger surface is not wired to the policy engine: {policy}"
    );
}

#[test]
fn the_ledger_api_binary_never_wires_an_allow_all_client() {
    let open: Vec<&str> = code_lines()
        .filter(|l| {
            ["PermissivePolicyClient", "FakePolicyClient", "policy: None"]
                .iter()
                .any(|c| l.contains(c))
        })
        .collect();
    assert!(
        open.is_empty(),
        "boss_ledger_api.rs wires a client that allows every read: {open:?}"
    );
}
