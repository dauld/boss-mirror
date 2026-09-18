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
//! 2. Every helper referenced by any `when` in the rule registry resolves in
//!    the one resolver the runners bind (`InventoryHelpers`). Called
//!    with empty args: a KNOWN helper refuses with a helper error, an
//!    unknown one is `UnknownHelper` — no HTTP happens either way, so
//!    the pin runs without a jobs-api.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver};
use boss_dispatcher::rules::helpers_inventory::InventoryHelpers;
use boss_dispatcher::rules::registry::{RawRegistry, parse_raw_path};

fn rules() -> RawRegistry {
    parse_raw_path(boss_testing::dispatcher_rules_dir()).expect("parse the rule directory")
}

/// The subject a spawn rule passes, unquoted. Spawn args are expr
/// strings, so a literal subject arrives as `"\"disk-headroom\""`.
fn literal(arg: &str) -> Option<&str> {
    arg.strip_prefix('"')?.strip_suffix('"')
}

#[tokio::test(flavor = "multi_thread")]
async fn every_sweep_spawner_guards_on_its_own_subject() {
    let raw = rules();
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
    // FIVE since 2026-09-18, down from six: the sixth was
    // `maintenance-sweep-build-caches-daily`, retired by backlog 18df96c4.
    // Its measurement was a `disk-report` on the forge, which cannot see
    // a cargo target dir on the dev pod, and its question is answered
    // hourly by the maintenance-dev-scratch-reclaim packet on the host
    // where the caches live (stale_targets_reclaimed, targets_removed_mib,
    // worktrees_kept_*) — where no ops-runner runs. Before that, SIX
    // since 2026-09-10, down from seven: `maintenance-sweep-doc-status-
    // daily`, retired with the design-doc flush pipeline (backlog
    // f5da586c) — its whole content was the drifted-status report, and a
    // packet's status IS its status. The five are cluster-conformance,
    // converge-lag, disk, empty-decisions and image-freshness.
    //
    // The floor is what this pin is for: a sweep rule that loses its
    // guard still spawns, so it is still counted here and still fails
    // the assertions above BY NAME. A rule that disappears fails only
    // here — which is why lowering the number is a deliberate act with
    // a reason attached, not a number nudged until green.
    //
    // AND IT FAILS HERE AFTER CI HAS BEEN SPENT, which is backlog
    // 709e480b: this number went stale in a DIFFERENT car from the
    // retirement, so the contradiction existed only on the assembled
    // tree and the first reader was a gate. The line below is the same
    // six, declared in a shape the consist check reads against the rule
    // directory before a train boards
    // (`infra/lint/an-expectation-names-a-rule-the-tree-declares.sh`) —
    // a comment the machine checks, not a comment asking the next
    // person (CLAUDE.md §9a). It must move when this number moves; that
    // is the point, and the lint names the file and line when it does
    // not.
    //
    // rule-registry-pin: maintenance-sweep-*-daily = 5
    assert!(
        checked >= 5,
        "expected the five daily sweep spawners (cluster-conformance, \
         converge-lag, disk, empty-decisions, image-freshness), found \
         {checked} — a sweep went missing. If one \
         was retired on purpose, lower this pin AND say which and why, \
         the way the build-caches sweep's retirement is recorded above; if \
         not, a sweep stopped running and nobody noticed."
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_helper_a_when_guard_names_resolves() {
    let raw = rules();
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
