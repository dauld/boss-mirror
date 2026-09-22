//! View filters, on top of the shared `boss-expr` DSL.
//!
//! One consumer of that DSL among several; the roster lives in the
//! DSL's own header, where a test holds it to the manifests that
//! decide it (`the_header_names_every_crate_that_depends_on_the_dsl`).
//! An ordinal here would be a second, unpinned copy of that count —
//! this one said `third` while there were four (backlog f1bfc954).
//! Reusing the DSL rather than inventing a filter language is the
//! whole reason this phase needs no sandbox:
//! the grammar has no loops, no recursion and no Turing-completeness,
//! so an operator-authored predicate provably terminates. Q3 of the
//! review deferred agent-written code precisely because *that* needs
//! infrastructure for running user code safely; this does not.

use boss_expr::{Context, Expr, NoHelpers, Value};

use crate::error::ViewsError;

/// Parse a filter, rejecting anything malformed.
///
/// Called on save so a bad expression fails for its author, at the
/// moment they wrote it — not later, for whoever opens the View.
pub fn compile(filter: &str) -> Result<Option<Expr>, ViewsError> {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    boss_expr::parse(trimmed)
        .map(Some)
        .map_err(|e| ViewsError::InvalidFilter(e.to_string()))
}

/// Evaluate a compiled filter against one row.
///
/// A row that errors — an identifier the payload doesn't carry, a
/// comparison across types — is treated as **not matching** rather
/// than failing the whole View. One malformed row in ten thousand
/// should not blank the surface, and the alternative (propagate) makes
/// a View's success depend on the shape of data it did not choose.
///
/// The DSL requires strict booleans, so a filter that evaluates to a
/// string or an int does not match either; it is not truthy-coerced.
pub fn matches(expr: &Expr, row: &serde_json::Value) -> bool {
    let ctx = Context {
        payload: row,
        helpers: &NoHelpers,
    };
    matches!(boss_expr::eval(expr, &ctx), Ok(Value::Bool(true)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_filter_compiles_to_none() {
        assert!(compile("").unwrap().is_none());
        assert!(compile("   ").unwrap().is_none());
    }

    #[test]
    fn malformed_filter_is_rejected_at_compile() {
        let err = compile("status =").unwrap_err();
        assert!(matches!(err, ViewsError::InvalidFilter(_)));
    }

    #[test]
    fn matches_on_a_simple_comparison() {
        let e = compile("status = \"open\"").unwrap().unwrap();
        assert!(matches(&e, &json!({"status": "open"})));
        assert!(!matches(&e, &json!({"status": "closed"})));
    }

    #[test]
    fn combines_with_and_or_not() {
        let e = compile("status = \"open\" AND priority = \"high\"")
            .unwrap()
            .unwrap();
        assert!(matches(&e, &json!({"status": "open", "priority": "high"})));
        assert!(!matches(&e, &json!({"status": "open", "priority": "low"})));
    }

    #[test]
    fn a_row_missing_the_field_does_not_match_and_does_not_explode() {
        // The failure mode this guards: one row without `status`
        // taking down the whole View.
        let e = compile("status = \"open\"").unwrap().unwrap();
        assert!(!matches(&e, &json!({"other": 1})));
    }

    #[test]
    fn a_non_boolean_filter_does_not_match() {
        // `title` evaluates to a string. The DSL refuses truthy
        // coercion, and so does this: no accidental "everything
        // matches" from a filter that forgot its comparison.
        let e = compile("title").unwrap().unwrap();
        assert!(!matches(&e, &json!({"title": "anything"})));
    }

    #[test]
    fn nested_paths_resolve() {
        let e = compile("subject.subject_kind = \"account\"")
            .unwrap()
            .unwrap();
        assert!(matches(
            &e,
            &json!({"subject": {"subject_kind": "account"}})
        ));
        assert!(!matches(&e, &json!({"subject": {"subject_kind": "asset"}})));
    }
}
