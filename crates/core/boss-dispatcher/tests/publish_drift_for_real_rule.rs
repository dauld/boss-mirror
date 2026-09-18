//! The clean `publish-drift --check` files its own `--for-real`
//! (backlog a1d3c762, measured in retro 27fad542: 23 publish-workflow
//! ops-requests hand-scripted one per kind on 2026-09-18 13:4x, after a
//! hand wait for boss-gcp's checkout — the `--check` half rides
//! check-publish-drift-on-boss-gcp-converge-moved on the parked car
//! f1b2822a; this rule is the `--for-real` half).
//!
//! Pinned over the shipped rule directory, not a copy: the rule fires
//! on an ops-request closing `answered` and hands `ops.judge` a
//! declaration whose every part is checked HERE, at compile time of the
//! tree, rather than the first time a real check closes — the regex
//! compiles with the three named groups the `when` reads, the `when`
//! parses as boss-expr and answers the four cases the verb can print,
//! and the follow-on's args are words the verb file admits (once that
//! file is in the tree; until the publish-drift car lands the cross
//! check names what it is waiting for).

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};
use serde_json::json;

mod common;

const RULE: &str = "publish-drift-for-real-on-clean-check";
const HANDLER: &str = "ops.judge";

fn shipped_rules() -> Registry {
    common::authored_registry()
}

/// The close marker in the shape jobs-api's three emit sites produce.
fn request_closed(outcome: &str) -> serde_json::Value {
    json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "closed_on": "2026-09-18",
        "kind": "ops-request",
        "outcome": outcome,
        "title": "publish-drift --check on boss-gcp — its checkout moved",
        "subject_id": "boss-gcp",
        "parent_step_id": null,
    })
}

fn arg<'a>(args: &'a [(String, Value)], k: &str) -> Option<&'a str> {
    args.iter()
        .find(|(n, _)| n == k)
        .and_then(|(_, v)| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
}

/// The one `ops.judge` invocation this rule hands the handler on an
/// answered ops-request.
fn judge_args(reg: &Registry, outcome: &str) -> Vec<Vec<(String, Value)>> {
    match_event(reg, "jobs.job.closed", &request_closed(outcome), &NoHelpers)
        .matched
        .iter()
        .filter(|h| h.rule_name == RULE)
        .flat_map(|h| h.invocations.iter())
        .filter(|i| i.handler == HANDLER)
        .map(|i| i.args.clone())
        .collect()
}

#[test]
fn the_rule_hands_ops_judge_a_declaration_it_can_run() {
    let reg = shipped_rules();
    let invocations = judge_args(&reg, "answered");
    assert_eq!(
        invocations.len(),
        1,
        "{RULE}: exactly one ops.judge do; got {invocations:?}"
    );
    let a = &invocations[0];

    assert_eq!(arg(a, "verb"), Some("publish-drift"));
    assert_eq!(arg(a, "then_verb"), Some("publish-drift"));
    assert_eq!(arg(a, "then_host"), Some("boss-gcp"));
    assert_eq!(arg(a, "then_args"), Some("--for-real"));

    // The pattern compiles, and names the groups the predicate reads.
    // It is written `[0-9]+` because the arg crosses two string lexers
    // (TOML, then boss-expr's, which reads `\d` as `d`).
    let pattern = arg(a, "verdict_pattern").expect("verdict_pattern");
    assert!(
        !pattern.contains('\\'),
        "no backslash survives boss-expr's lexer: {pattern}"
    );
    let re = regex::Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
    let names: Vec<_> = re.capture_names().flatten().collect();
    assert_eq!(names, ["n", "m", "k"], "{pattern}");

    // The verb's own verdict line (infra/gcp/publish-drift.sh, `$NAME:
    // would publish $n_ahead, skipped $n_equal equal, refused $n_refused`
    // plus a parenthetical) matches; its --for-real line does not — the
    // follow-on's answer must not be a check's.
    let check = "publish-drift: would publish 3, skipped 20 equal, refused 0 (checkout cb053ed6, 23 kind(s), packet 11111111)";
    let caps = re
        .captures(check)
        .expect("the check's verdict line matches");
    assert_eq!((&caps["n"], &caps["m"], &caps["k"]), ("3", "20", "0"));
    assert!(
        !re.is_match("publish-drift: published 3, skipped 20 equal, refused 0 (checkout cb053ed6, 23 kind(s), packet f0000000)"),
        "the --for-real answer is not a check"
    );
    assert!(
        !re.is_match(
            "not yet: checkout at cb053ed6, main at 9a1b2c3d — nothing compared, nothing published"
        ),
        "a not-yet is not a verdict"
    );

    // The predicate is boss-expr over the groups, and says what the
    // packet asked: file when nothing is refused AND something is ahead.
    let when_src = arg(a, "when").expect("when");
    let when = boss_dispatcher::rules::expr::parse(when_src)
        .unwrap_or_else(|e| panic!("when {when_src:?}: {e}"));
    let holds = |groups: serde_json::Value| {
        boss_dispatcher::rules::expr::eval(
            &when,
            &boss_dispatcher::rules::expr::Context {
                payload: &groups,
                helpers: &NoHelpers,
            },
        )
        .unwrap_or_else(|e| panic!("when {when_src:?} over {groups}: {e}"))
    };
    assert_eq!(
        holds(json!({"n": 3, "m": 20, "k": 0})),
        Value::Bool(true),
        "clean, ahead"
    );
    assert_eq!(
        holds(json!({"n": 3, "m": 19, "k": 1})),
        Value::Bool(false),
        "a refusal"
    );
    assert_eq!(
        holds(json!({"n": 0, "m": 23, "k": 0})),
        Value::Bool(false),
        "nothing ahead"
    );
    assert_eq!(
        holds(json!({"n": 0, "m": 22, "k": 1})),
        Value::Bool(false),
        "both"
    );
}

/// THE PIN (CLAUDE.md §9a): the words the rule files as `then_args` are
/// words the verb file admits — `infra/ops/verbs/publish-drift.json`
/// declares `mode` with `one_of`, and the ops-runner refuses anything
/// else. The verb file rides the parked publish-drift car (f1b2822a);
/// until it is in the tree this pin says what it waits for, and the
/// moment it is, a `then_args` the verb refuses is named here.
#[test]
fn then_args_are_words_the_verb_file_admits() {
    let reg = shipped_rules();
    let a = judge_args(&reg, "answered").remove(0);
    let then_verb = arg(&a, "then_verb").expect("then_verb");
    let then_host = arg(&a, "then_host").expect("then_host");
    let path = boss_testing::repo_root().join(format!("infra/ops/verbs/{then_verb}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "not yet: {} is not in this tree (it rides the publish-drift car); the args cross-check waits for it",
            path.display()
        );
        return;
    };
    let spec: serde_json::Value = serde_json::from_str(&text).unwrap();
    let hosts: Vec<&str> = spec["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| h.as_str())
        .collect();
    assert!(
        hosts.contains(&then_host),
        "{then_verb} serves {hosts:?}, not {then_host}"
    );
    let params = spec["params"].as_array().cloned().unwrap_or_default();
    let words: Vec<&str> = arg(&a, "then_args")
        .unwrap_or("")
        .split_whitespace()
        .collect();
    assert!(
        words.len() <= params.len(),
        "{then_verb} takes at most {} arg(s), the rule files {words:?}",
        params.len()
    );
    for (i, w) in words.iter().enumerate() {
        let p = &params[i];
        if let Some(one_of) = p["one_of"].as_array() {
            assert!(
                one_of.iter().any(|o| o == w),
                "{then_verb} arg {} admits {one_of:?}, the rule files {w:?}",
                p["name"]
            );
        } else if let Some(pat) = p["pattern"].as_str() {
            assert!(
                regex::Regex::new(pat).unwrap().is_match(w),
                "{then_verb} arg {} wants {pat}, the rule files {w:?}",
                p["name"]
            );
        }
    }
}

/// The verb this rule judges is the verb the converge rule files, once
/// that rule is in the tree (it rides the same parked car): two rules
/// name one verb, and this names whichever drifts.
#[test]
fn the_judged_verb_is_the_verb_the_converge_rule_files() {
    let reg = shipped_rules();
    let a = judge_args(&reg, "answered").remove(0);
    let judged = arg(&a, "verb").expect("verb");
    let converge_moved = json!({
        "job_id": "c0000000-0000-0000-0000-000000000001",
        "step_id": "s-run",
        "kind": "task",
        "workflow_kind": "maintenance-boss-gcp-converge",
        "spec_slug": "run",
        "subject_kind": "custom",
        "subject_id": "boss-gcp",
        "assignee_id": null,
        "metadata": { "result": "ok", "converge_from": "aaaaaaaa", "converge_sha": "bbbbbbbb" }
    });
    let filed: Vec<String> = match_event(&reg, "step.done.task", &converge_moved, &NoHelpers)
        .matched
        .iter()
        .flat_map(|h| h.invocations.iter())
        .filter(|i| i.handler == "jobs.spawn" && arg(&i.args, "kind") == Some("ops-request"))
        .filter_map(|i| arg(&i.args, "metadata.verb").map(str::to_string))
        .collect();
    if filed.is_empty() {
        eprintln!(
            "not yet: no rule files an ops-request on boss-gcp's converge moving (check-publish-drift-on-boss-gcp-converge-moved rides the publish-drift car)"
        );
        return;
    }
    assert!(
        filed.iter().any(|v| v == judged),
        "{RULE} judges `{judged}` but the converge rule files {filed:?} — one verb, move both"
    );
}

/// A refused ops-request carries no verdict: nothing judges it. Nor
/// does any other kind closing `answered`.
#[test]
fn only_an_answered_ops_request_is_judged() {
    let reg = shipped_rules();
    assert!(
        judge_args(&reg, "refused").is_empty(),
        "a refusal is not a reading"
    );
    let mut other = request_closed("answered");
    other["kind"] = json!("pr-train");
    let n = match_event(&reg, "jobs.job.closed", &other, &NoHelpers)
        .matched
        .iter()
        .filter(|h| h.rule_name == RULE)
        .count();
    assert_eq!(n, 0, "only an ops-request carries a verb's answer");
}
