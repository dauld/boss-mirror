//! The recheck fires the probe handler with `scope = "failing"` on an
//! HOURLY schedule; the arrival rule still fires it with no scope.
//!
//! WHY THE CADENCE IS PINNED HERE AND NOT JUST AUTHORED (backlog
//! a9dafed6, 2026-09-19). The rule was daily, and the events its probes
//! wait on are not: a car became provable within an hour or two of its
//! convergence and then waited up to 24 hours for anyone to look. The
//! interval is the whole point of this rule, so a test reads it rather
//! than only asking that a schedule exists — the previous shape of this
//! file asserted `schedule().is_some()`, which a daily and an hourly
//! rule satisfy identically.

use boss_core::calendar::Cadence;
use boss_dispatcher::rules::expr::{Expr, Value};
use boss_dispatcher::rules::registry::Registry;

mod common;

/// The whole authored directory, read the way the seed reads it.
fn shipped_rules() -> Registry {
    common::authored_registry()
}

#[test]
fn the_recheck_is_an_hourly_run_of_the_probe_handler_scoped_to_failing_cars() {
    let reg = shipped_rules();
    let rule = reg
        .rules()
        .iter()
        .find(|r| r.name == "recheck-failing-probes-hourly")
        .expect("the recheck rule is declared");
    let schedule = rule.schedule().expect("a timer, not an event rule");
    assert_eq!(
        schedule.cadence,
        Cadence::Hourly,
        "the recheck runs at the rate the events its probes wait on fire — \
         the finest of them is hourly (a9dafed6)"
    );
    let step = rule.do_steps.first().expect("one handler");
    assert_eq!(step.handler, "jobs.run-car-probes");
    let scope = step
        .args
        .iter()
        .find(|(k, _)| k == "scope")
        .map(|(_, e)| e.clone())
        .expect("names its scope");
    assert_eq!(scope, Expr::Literal(Value::String("failing".into())));
}

/// The name the rule was retired from. A file under the old name would
/// mean two rules filing the same ops-requests — the recheck hourly and
/// again daily — so the rename is asserted as a removal, not only as an
/// addition.
#[test]
fn no_rule_still_claims_the_daily_cadence_the_recheck_left() {
    let reg = shipped_rules();
    assert!(
        !reg.rules()
            .iter()
            .any(|r| r.name == "recheck-failing-probes-daily"),
        "the daily rule is retired by deleting its file (infra/dispatcher/rules/README.md)"
    );
}
