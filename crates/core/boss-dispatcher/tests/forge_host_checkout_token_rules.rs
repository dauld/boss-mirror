//! The forge host's checkout token is rotated by the broker and
//! revoked only after the host records delivery (design 1c90d183,
//! David 2026-09-26; backlog c4cbc6b5).
//!
//! Two rule files declare it, because one rule has one trigger and the
//! rotation needs two: the scope step (`credential-rotation`) starts it,
//! and the host's `delivered` step (`credential-delivery`) lets the
//! revoke run. Both hand the SAME declaration to the same handler — the
//! Secret, the issuer account, the scopes, the proving repo, the
//! delivery mode and the spared tokens — so the declaration lives twice
//! and is pinned equal here (CLAUDE.md §9a). A delivery firing that read
//! a different Secret would judge the host's last eight against the
//! wrong value, and one that lost `delivery = "off-host"` would be
//! refused by the handler on every firing, quietly, on the dead-letter
//! shelf.
//!
//! The `spare` arg is a third copy of a fact: the tokens another
//! consumer is DECLARED to hold, which this rotation must never revoke
//! (David's Q2 answer: the one automatic exception). Its sources are the
//! forge token inventory's attributed rows and the other forgejo broker
//! credentials, whose instances carry `<secret_name>-<packet>` names.
//! Pinned to both, so a newly attributed token or a new broker
//! credential names the rule that must learn it.

use std::collections::BTreeSet;

use boss_dispatcher::rules::registry::{RawRegistry, RawRule, parse_raw_path};
use boss_testing::{dispatcher_rules_dir, repo_root};

const ROTATE: &str = "broker-rotates-the-forge-host-checkout-token";
const DELIVERED: &str = "broker-revokes-the-forge-host-checkout-token-on-delivery";
const HANDLER: &str = "credential.rotate.forgejo";
const CREDENTIAL: &str = "forge-host-checkout-token";

fn rules() -> RawRegistry {
    parse_raw_path(dispatcher_rules_dir()).expect("parse the rule directory")
}

fn rule<'a>(reg: &'a RawRegistry, name: &str) -> &'a RawRule {
    reg.rules
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("rule {name} is in the authored directory"))
}

/// An arg's expr source `"\"boss\""` as the bare literal `boss`.
fn literal(rule: &RawRule, key: &str) -> String {
    let raw = rule.do_steps[0]
        .args
        .get(key)
        .unwrap_or_else(|| panic!("{} declares no `{key}`", rule.name));
    raw.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{}: `{key}` is not a string literal: {raw}", rule.name))
        .to_string()
}

#[test]
fn one_declaration_is_fired_by_the_scope_step_and_by_the_hosts_delivery() {
    let reg = rules();
    let rotate = rule(&reg, ROTATE);
    let delivered = rule(&reg, DELIVERED);
    assert_eq!(
        rotate.on_event.as_deref(),
        Some("step.done.credential-rotation")
    );
    assert_eq!(
        delivered.on_event.as_deref(),
        Some("step.done.credential-delivery"),
        "the second firing is the host's `delivered` step, and only it"
    );
    let when = format!("subject_id = \"{CREDENTIAL}\"");
    for r in [rotate, delivered] {
        assert_eq!(
            r.when.as_deref(),
            Some(when.as_str()),
            "{} must fire for this credential's packets only",
            r.name
        );
        assert_eq!(r.do_steps.len(), 1, "{}: one handler", r.name);
        assert_eq!(r.do_steps[0].handler, HANDLER, "{}", r.name);
    }
    assert_eq!(
        rotate.do_steps[0].args, delivered.do_steps[0].args,
        "{ROTATE} and {DELIVERED} must hand the handler one declaration"
    );

    // The declaration itself, as design 1c90d183 decided it (D4, D5).
    assert_eq!(literal(rotate, "secret_namespace"), "boss");
    assert_eq!(literal(rotate, "secret_name"), CREDENTIAL);
    assert_eq!(literal(rotate, "secret_key"), "token");
    assert_eq!(literal(rotate, "forge_user"), "david");
    assert_eq!(literal(rotate, "scopes"), "write:repository");
    assert_eq!(literal(rotate, "verify_repo"), "david/boss");
    assert_eq!(
        literal(rotate, "delivery"),
        "off-host",
        "the host takes the value on its next pass, so the revoke waits for its record"
    );
}

#[test]
fn the_spared_tokens_are_every_other_declared_holder() {
    let reg = rules();
    let got: BTreeSet<String> = literal(rule(&reg, ROTATE), "spare")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let inventory: toml::Value = toml::from_str(
        &std::fs::read_to_string(repo_root().join("infra/platform/forge-tokens.toml"))
            .expect("the forge token inventory"),
    )
    .expect("the inventory parses");
    let mut want: BTreeSet<String> = inventory["token"]
        .as_array()
        .expect("[[token]] rows")
        .iter()
        .filter(|t| t["consumer"].as_str() != Some("UNKNOWN"))
        .map(|t| t["name"].as_str().expect("a name").to_string())
        .collect();
    for r in &reg.rules {
        let Some(step) = r.do_steps.first() else {
            continue;
        };
        if step.handler != HANDLER {
            continue;
        }
        let secret = literal(r, "secret_name");
        if secret != CREDENTIAL {
            want.insert(format!("{secret}-*"));
        }
    }
    assert!(
        want.contains("boss-gcp"),
        "control: the conductor's token is the inventory's one attributed row: {want:?}"
    );
    assert_eq!(
        got, want,
        "{ROTATE}'s `spare` must name every token another consumer is declared to hold \
         (infra/platform/forge-tokens.toml rows with a consumer, and every other forgejo \
         broker credential's instances) — David's Q2: those are never this rotation's to revoke"
    );
}
