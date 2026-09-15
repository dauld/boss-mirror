//! The daily recheck fires the probe handler with `scope = "failing"`;
//! the arrival rule still fires it with no scope.

use boss_dispatcher::rules::expr::{Expr, Value};
use boss_dispatcher::rules::registry::Registry;

mod common;

/// The whole authored directory, read the way the seed reads it.
fn shipped_rules() -> Registry {
    common::authored_registry()
}

#[test]
fn the_daily_recheck_is_a_scheduled_run_of_the_probe_handler_scoped_to_failing_cars() {
    let reg = shipped_rules();
    let rule = reg
        .rules()
        .iter()
        .find(|r| r.name == "recheck-failing-probes-daily")
        .expect("the recheck rule is declared");
    assert!(
        rule.schedule().is_some(),
        "a daily timer, not an event rule"
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
