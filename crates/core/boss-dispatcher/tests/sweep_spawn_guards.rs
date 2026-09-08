//! Every daily spawner asks before it spawns, and every guard it asks
//! with actually exists.
//!
//! 0517387b: the maintenance-sweep spawn rules fired unconditionally —
//! `schedule` + `jobs.spawn` and no `when` — so an obligation nobody
//! could discharge accumulated one packet per day (5 open
//! cluster-conformance sweeps when measured). The guard is rule DATA
//! (`NOT open_job_exists(kind, subject)`), which is exactly why a test
//! must pin it: nothing else connects a TOML `when` string to the Rust
//! helper it names, and a rule added without a guard regresses
//! silently. Both directions:
//!
//! 1. Every rule that spawns a `maintenance-sweep` carries a dedup
//!    guard naming its own subject — a new sweep rule without one
//!    fails here BY NAME.
//! 2. Every helper referenced by any `when` in rules.toml resolves in
//!    the one resolver the runners bind (`InventoryHelpers`). Called
//!    with empty args: a KNOWN helper refuses with a helper error, an
//!    unknown one is `UnknownHelper` — no HTTP happens either way, so
//!    the pin runs without a jobs-api.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver};
use boss_dispatcher::rules::helpers_inventory::InventoryHelpers;
use boss_dispatcher::rules::registry::parse_raw;

fn rules_toml() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/dispatcher/rules.toml"
    ))
    .expect("read rules.toml")
}

/// The subject a spawn rule passes, unquoted. Spawn args are expr
/// strings, so a literal subject arrives as `"\"disk-headroom\""`.
fn literal(arg: &str) -> Option<&str> {
    arg.strip_prefix('"')?.strip_suffix('"')
}

#[tokio::test(flavor = "multi_thread")]
async fn every_sweep_spawner_guards_on_its_own_subject() {
    let raw = parse_raw(&rules_toml()).expect("rules.toml parses");
    let mut checked = 0;
    for rule in &raw.rules {
        for step in &rule.do_steps {
            if step.handler != "jobs.spawn" {
                continue;
            }
            let Some(kind) = step.args.get("kind").and_then(|k| literal(k)) else {
                continue;
            };
            if kind != "maintenance-sweep" {
                continue;
            }
            checked += 1;
            let subject = step
                .args
                .get("subject")
                .and_then(|s| literal(s))
                .unwrap_or_else(|| {
                    panic!("rule {}: sweep spawn without a literal subject", rule.name)
                });
            let when = rule.when.as_deref().unwrap_or_else(|| {
                panic!(
                    "rule {}: spawns a maintenance-sweep with NO `when` guard — \
                     this is 0517387b regressing; add \
                     when = 'NOT open_job_exists(\"maintenance-sweep\", \"{subject}\")'",
                    rule.name
                )
            });
            let expected = format!("open_job_exists(\"maintenance-sweep\", \"{subject}\")");
            assert!(
                when.contains(&expected),
                "rule {}: `when` guard does not dedup on this rule's own subject: {when}",
                rule.name
            );
        }
    }
    assert!(
        checked >= 7,
        "expected the seven daily sweep spawners, found {checked} — \
         if sweeps moved, move this pin with them"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_helper_a_when_guard_names_resolves() {
    let raw = parse_raw(&rules_toml()).expect("rules.toml parses");
    let helpers = InventoryHelpers::new("http://unused.invalid", "http://unused.invalid");
    let mut seen = std::collections::BTreeSet::new();
    for rule in &raw.rules {
        let Some(when) = &rule.when else { continue };
        // Helper calls are `name(`; identifiers without a paren are
        // payload bindings, not helpers.
        let bytes = when.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_lowercase() || bytes[i] == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'(') {
                    seen.insert((when[start..i].to_string(), rule.name.clone()));
                }
            } else {
                i += 1;
            }
        }
    }
    assert!(!seen.is_empty(), "no helper calls found in any when guard");
    for (name, rule) in &seen {
        if name == "NOT" || name == "not" {
            continue;
        }
        // Any other refusal (missing arg) or even a success proves
        // the name dispatches — no HTTP happened with empty args.
        if let Err(EvalError::UnknownHelper(n)) = helpers.call(name, &[]) {
            panic!("rule {rule}: when-guard names helper `{n}` that no resolver knows")
        }
    }
}
