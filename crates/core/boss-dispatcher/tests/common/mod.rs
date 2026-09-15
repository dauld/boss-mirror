//! The shipped dispatcher-rule registry, loaded the way the dispatcher
//! boots it — ONCE, for every test that reads rules from the table.
//!
//! WHY THIS FILE (backlog 488cadca). `infra/dispatcher/rules/` is the
//! registry's definition and `dispatcher_rules` is DERIVED from it at
//! boot by `seed_authored_rules` (the collapse, 41ba00cd). A rule change
//! is a `version` bump in its file and writes no migration, so a fresh
//! TestDb holds only the LAST MIGRATED version of each rule. A test that
//! called `load_active_rules` alone was therefore a pin on stale data:
//! feedback_obligation_rules.rs pinned `complete-feedback-branch-on-car-
//! merged` at the migration's v3 while the file said v4, and passed. It
//! was fixed by hand on #370 with a private `shipped_registry` helper;
//! rule_payload_contract.rs had the same shape. The helper lives here
//! now, and `infra/lint/rule-tests-seed-the-directory.sh` names any test
//! file that calls `load_active_rules` without `seed_authored_rules`.
//!
//! Provides:
//! - `shipped_raw_rules(db)` — seed the directory, then read the active
//!   rows back: the raw shape, for a test that walks `RawRule`s
//! - `shipped_registry(db)` — the same, compiled to a `Registry`
//! - `authored_registry()` — the whole directory as a `Registry`, read
//!   the way the seed reads it, for a test that needs no database
//! - `authored_rule(name)` — ONE rule's file as a `Registry`, for a
//!   test about a single row's selection that needs no database
//!
//! Where the directory IS is not defined here: `boss_testing::
//! dispatcher_rules_dir()` is the one definition, shared with the
//! handlers crate (backlog 94f150f9).

#![allow(dead_code)]

use boss_dispatcher::rules::registry::{RawRegistry, Registry, load_active_rules, parse_raw_dir};
use boss_dispatcher::rules::seed::seed_authored_rules;
use boss_testing::{TestDb, dispatcher_rules_dir};

/// Seed the authored directory over whatever the migrations left, then
/// read the active rows back — the dispatcher's own boot order.
pub async fn shipped_raw_rules(db: &TestDb) -> RawRegistry {
    seed_authored_rules(&db.pool, dispatcher_rules_dir())
        .await
        .expect("seed the authored rule directory");
    load_active_rules(&db.pool)
        .await
        .expect("load active rules from dispatcher_rules")
}

/// `shipped_raw_rules`, compiled: the registry the runner matches on.
pub async fn shipped_registry(db: &TestDb) -> Registry {
    Registry::from_raw(shipped_raw_rules(db).await).expect("the shipped rows parse")
}

/// The whole authored directory, compiled, without a database — read
/// through `parse_raw_dir`, so the seed's own file checks (one rule per
/// file named for it, a non-empty `why`) apply here too. Until
/// 2026-09-14 four selection tests each carried a 14-line copy of a
/// concatenate-every-toml loop around the directory literal (94f150f9).
pub fn authored_registry() -> Registry {
    let dir = dispatcher_rules_dir();
    let raw = parse_raw_dir(&dir)
        .unwrap_or_else(|e| panic!("parse the rule directory {}: {e}", dir.display()));
    Registry::from_raw(raw).expect("the shipped rules parse together")
}

/// One authored rule, read from its file. The single-file read keeps a
/// selection test's `hits.len() == 1` honest — the whole registry has
/// other rules on the same topic — while the text is still the file the
/// dispatcher boots from, not a copy of it.
pub fn authored_rule(name: &str) -> Registry {
    let path = dispatcher_rules_dir().join(format!("{name}.toml"));
    let toml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read rule file {}: {e}", path.display()));
    Registry::from_toml(&toml)
        .unwrap_or_else(|e| panic!("rule file {} parses: {e}", path.display()))
}
