//! Every ops verb whose recorded output a dispatcher rule JUDGES
//! declares its own `timeout` in its verb file.
//!
//! WHY (backlog 8fcd25c4, measured 2026-09-26). `boss ops boss-gcp
//! disk-report` (ops-request b21ddeb3) printed its per-directory lists
//! and was killed at 30 s — exit 124 — before its verdict line.
//! `infra/ops/verbs/disk-report.json` declared no timeout, so the
//! ops-runner's default (`OPS_TIMEOUT`, 30 s) applied, and a car earlier
//! that day had added one-level-down scans that take longer than that on
//! boss-gcp; the forge had answered in 6.5 s. The judge that reads the
//! verdict (`maintenance.sweep.judge`, rule judge-disk-headroom-sweep-
//! on-report-answered) then correctly refused a killed verb's output, so
//! the disk-headroom sweep got no reading at all.
//!
//! The default is a number chosen for a one-line read (`df`, `uptime`).
//! A verb whose output a rule turns into a completion or an alert is
//! doing a measurement, and how long that measurement may take is a
//! reviewed fact about the verb, like its argv — so it is written where
//! the verb is, not inherited from the runner.
//!
//! WHICH VERBS. Derived from the rule registry, not listed here: a
//! rule action whose `args` carry a `verb` names the ops verb whose
//! answered output that handler reads (`maintenance.sweep.judge`,
//! `ops.judge`, `jobs.complete_linked_step` with a `verdict_pattern`).
//! A new judge rule on a verb with no timeout fails this test by name.

use boss_testing::{dispatcher_rules_dir, repo_root};
use std::collections::BTreeMap;

/// The ops-runner's default, read out of the runner itself so this
/// file does not carry a second copy of the number.
fn runner_default_timeout() -> u64 {
    let runner = std::fs::read_to_string(repo_root().join("infra/ops/ops-runner.sh"))
        .expect("infra/ops/ops-runner.sh is readable");
    runner
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("OPS_TIMEOUT=\"${OPS_TIMEOUT:-")
                .and_then(|rest| rest.strip_suffix("}\""))
        })
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| {
            panic!("ops-runner.sh no longer sets OPS_TIMEOUT=\"${{OPS_TIMEOUT:-<n>}}\"")
        })
}

/// Every verb a rule judges, with the rules that judge it.
fn judged_verbs() -> BTreeMap<String, Vec<String>> {
    let mut judged: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let dir = dispatcher_rules_dir();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    for path in files {
        let body = std::fs::read_to_string(&path).expect("rule file is readable");
        let doc: toml::Value =
            toml::from_str(&body).unwrap_or_else(|e| panic!("{} is not TOML: {e}", path.display()));
        for rule in doc
            .get("rule")
            .and_then(|r| r.as_array())
            .into_iter()
            .flatten()
        {
            let name = rule
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string();
            for action in rule
                .get("do")
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
            {
                // Rule args are expressions: a string literal is quoted.
                if let Some(verb) = action
                    .get("args")
                    .and_then(|a| a.get("verb"))
                    .and_then(|v| v.as_str())
                {
                    let verb = verb.trim().trim_matches('"').to_string();
                    judged.entry(verb).or_default().push(name.clone());
                }
            }
        }
    }
    judged
}

#[test]
fn every_verb_a_rule_judges_declares_its_own_timeout() {
    let default = runner_default_timeout();
    let judged = judged_verbs();
    // Not vacuous: the verb this pin was written for is among them.
    assert!(
        judged.contains_key("disk-report"),
        "no rule judges disk-report any longer — the derivation read nothing it was written for: {judged:?}"
    );
    let mut missing = Vec::new();
    for (verb, rules) in &judged {
        let path = repo_root().join(format!("infra/ops/verbs/{verb}.json"));
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "rule(s) {rules:?} judge verb {verb}, which has no verb file at {}: {e}",
                path.display()
            )
        });
        let spec: serde_json::Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()));
        match spec.get("timeout").and_then(|t| t.as_u64()) {
            Some(t) if t > 0 => {}
            other => missing.push(format!(
                "{verb} (judged by {rules:?}) declares timeout {other:?}; without one the runner kills it at {default}s and the judge reads a killed verb"
            )),
        }
    }
    assert!(
        missing.is_empty(),
        "every verb whose output a rule judges must declare a positive integer `timeout` in infra/ops/verbs/<verb>.json:\n  {}",
        missing.join("\n  ")
    );
}

/// The number itself: disk-report's scans on boss-gcp outlasted the
/// runner's default, so its own timeout must exceed that default.
#[test]
fn disk_report_is_given_longer_than_the_runners_default() {
    let default = runner_default_timeout();
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/disk-report.json"))
            .expect("the verb file is readable"),
    )
    .expect("the verb file is JSON");
    let t = spec["timeout"].as_u64().unwrap_or(0);
    assert!(
        t > default,
        "disk-report was killed at the {default}s default on boss-gcp (ops-request b21ddeb3); its own timeout is {t}"
    );
}
