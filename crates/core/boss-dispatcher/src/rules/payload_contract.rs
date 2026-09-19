//! Does a rule only reference fields the event it binds actually carries?
//!
//! THE FAILURE THIS ANSWERS (filed cf7ae3b5, 2026-08-15). A rule can name
//! an identifier the event has never carried, and nothing catches it
//! until the rule fires in production. There it is not a quiet skip:
//! `expr::eval` returns `UnknownIdentifier`, the runner turns that into
//! `PredicateFailed` / `ArgFailed`, NAKs for redelivery, and the event
//! dead-letters after eight attempts.
//!
//! It has now happened twice on one topic. `spawn-car-on-sweep-remediated`
//! bound `title` against `jobs.job.closed`, which carried
//! id/closed_on/kind/outcome/parent_step_id and no title — eight WARN
//! redeliveries and a dead letter, seconds after a train merged. And one
//! of the three `jobs.job.closed` emit sites still omits `parent_step_id`
//! while a live rule gates on it (`da87e3a1`).
//!
//! WHY A ROSTER CHECK IS SOUND HERE. The evaluator's whole world is
//! `expr::Context { payload, helpers }`. Identifiers resolve against the
//! payload and nothing else — no ambient job, no system bindings — and
//! `expr::references` walks into function-call ARGUMENTS without emitting
//! the function's own name, so helpers cannot be mistaken for fields.
//! That makes "every identifier root must be a declared payload field" an
//! exact statement of what the evaluator will accept, not an approximation.
//!
//! IT IS A RATCHET, DELIBERATELY. `event_kinds.payload_fields` has existed
//! since migration 108 ("filled as consumers ... need it") and was empty
//! until this change. A kind with no declared roster is NOT checked, so
//! seeding one kind buys that kind's rules a gate without demanding a
//! complete inventory of every topic first — and a check that must be
//! total before it is useful never lands. Seed a roster, gain a gate.

use std::collections::BTreeSet;

use super::registry::{DoStep, Rule};
use boss_expr::{self as expr, Expr};

/// An identifier a rule references that its event does not declare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    /// Where in the rule it appears: `when`, or `do[].args.<name>`.
    pub location: String,
    /// The offending identifier, as the author wrote its root segment.
    pub identifier: String,
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} references `{}`", self.location, self.identifier)
    }
}

/// The root segments of every identifier path in `expr`.
///
/// Only the root matters: the payload supplies top-level keys, and a
/// dotted path like `metadata.subworkflow` traverses INTO the value of
/// `metadata`. Checking deeper would require declaring the shape of every
/// nested object, which `payload_fields` deliberately does not do (its
/// comment calls it a "flat field inventory").
fn identifier_roots(expr: &Expr) -> Vec<String> {
    expr::references(expr)
        .into_iter()
        .filter_map(|path| path.into_iter().next())
        .collect()
}

/// Identifiers `rule` references that `roster` does not declare.
///
/// An EMPTY roster means the kind has not declared its payload yet and
/// the rule is not checked — see the ratchet note above. Callers must
/// pass the roster only for the kind the rule actually binds.
pub fn unresolved_identifiers(rule: &Rule, roster: &BTreeSet<String>) -> Vec<Unresolved> {
    if roster.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut check = |e: &Expr, location: String| {
        for root in identifier_roots(e) {
            if !roster.contains(&root) {
                out.push(Unresolved {
                    location: location.clone(),
                    identifier: root,
                });
            }
        }
    };
    if let Some(when) = &rule.when {
        check(when, "when".to_string());
    }
    for DoStep { handler, args } in &rule.do_steps {
        for (name, e) in args {
            check(e, format!("do[{handler}].args.{name}"));
        }
    }
    out.dedup_by(|a, b| a == b);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::registry::{RawDoStep, RawRule, Rule};
    use std::collections::HashMap;

    fn roster(fields: &[&str]) -> BTreeSet<String> {
        fields.iter().map(|s| s.to_string()).collect()
    }

    fn rule(when: Option<&str>, args: &[(&str, &str)]) -> Rule {
        let raw = RawRule {
            name: "r".into(),
            on_event: Some("jobs.job.closed".into()),
            schedule: None,
            when: when.map(str::to_string),
            do_steps: vec![RawDoStep {
                handler: "jobs.spawn".into(),
                args: args
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
            }],
            delay: None,
            version: 1,
            why: None,
        };
        Rule::from_raw(raw).expect("test rule parses")
    }

    /// The exact incident: `title` bound against a payload without it.
    #[test]
    fn an_arg_naming_a_field_the_event_lacks_is_reported() {
        let found = unresolved_identifiers(
            &rule(None, &[("title", "title")]),
            &roster(&["id", "closed_on", "kind", "outcome", "parent_step_id"]),
        );
        assert_eq!(found.len(), 1, "one offending arg: {found:?}");
        assert_eq!(found[0].identifier, "title");
        assert!(
            found[0].location.contains("args.title"),
            "the location must name the arg, got {}",
            found[0].location
        );
    }

    #[test]
    fn a_predicate_naming_a_missing_field_is_reported() {
        let found = unresolved_identifiers(
            &rule(Some("parent_step_id != null"), &[]),
            &roster(&["id", "kind"]),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].identifier, "parent_step_id");
        assert_eq!(found[0].location, "when");
    }

    #[test]
    fn a_rule_that_only_names_declared_fields_is_clean() {
        let found = unresolved_identifiers(
            &rule(Some("outcome != null"), &[("title", "title")]),
            &roster(&["id", "outcome", "title"]),
        );
        assert!(found.is_empty(), "expected clean, got {found:?}");
    }

    /// Dotted paths traverse INTO a declared field; only the root is a
    /// payload key. `metadata.subworkflow` is legal when `metadata` is.
    #[test]
    fn a_dotted_path_is_checked_at_its_root_only() {
        let found = unresolved_identifiers(
            &rule(None, &[("kind", "metadata.subworkflow")]),
            &roster(&["metadata"]),
        );
        assert!(found.is_empty(), "expected clean, got {found:?}");

        let found = unresolved_identifiers(
            &rule(None, &[("kind", "meta.subworkflow")]),
            &roster(&["metadata"]),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].identifier, "meta");
    }

    /// A helper's NAME is not a payload field, and must not be reported;
    /// its arguments still are.
    #[test]
    fn a_helper_call_is_not_mistaken_for_a_field() {
        let found = unresolved_identifiers(
            &rule(Some("NOT open_restock_exists(part_sku)"), &[]),
            &roster(&["part_sku"]),
        );
        assert!(
            found.is_empty(),
            "the helper name is not a field; only its args are: {found:?}"
        );
    }

    /// The ratchet: a kind that has not declared a roster is not checked,
    /// which is what lets this land one topic at a time.
    #[test]
    fn an_undeclared_roster_checks_nothing() {
        let found = unresolved_identifiers(
            &rule(Some("anything_at_all != null"), &[("x", "whatever")]),
            &BTreeSet::new(),
        );
        assert!(found.is_empty(), "empty roster must not gate: {found:?}");
    }
}
