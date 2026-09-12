//! The daily recheck fires the probe handler with `scope = "failing"`;
//! the arrival rule still fires it with no scope.

use boss_dispatcher::rules::expr::{Expr, Value};
use boss_dispatcher::rules::registry::Registry;

fn shipped_rules() -> Registry {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/dispatcher/rules"
    );
    let mut toml = String::new();
    for entry in std::fs::read_dir(dir).expect("rules dir") {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "toml") {
            toml.push_str(&std::fs::read_to_string(&p).unwrap());
            toml.push('\n');
        }
    }
    Registry::from_toml(&toml).expect("the shipped rules parse together")
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
