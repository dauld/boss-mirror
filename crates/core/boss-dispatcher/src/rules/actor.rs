//! The dispatcher's actor identity for downstream API calls.
//!
//! ONE DEFINITION, in core. Per the rule-as-actor model in the
//! dispatcher design doc: every dispatcher-fired write names the RULE
//! as actor, with `executed_by = automation:dispatcher` distinct from
//! `actor`, so the audit log says which registry row caused the write.
//!
//! This lived in `boss-dispatcher-handlers::handlers::common` until the
//! rules runner itself needed to write (the dead-letter annotation in
//! [`super::dead_letter`]), which would have made it a second copy of an
//! identity — and an identity that drifts is a policy refusal that reads
//! as missing data (CLAUDE.md §9a: collapse it if you can). `common.rs`
//! re-exports this function, so every handler call site is unchanged.
//!
//! `role`/`access_tier` are the operator pair the jobs API's policy gate
//! expects for a platform write; a narrower identity answers 200 with a
//! smaller world instead of erroring.

/// Build the `x-boss-user` header value for a dispatcher-side write
/// attributed to `rule_name`.
pub fn dispatcher_actor_header(rule_name: &str) -> String {
    serde_json::json!({
        "id": format!("rule:{}", rule_name),
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_actor_is_the_rule_that_fired() {
        let v: serde_json::Value =
            serde_json::from_str(&dispatcher_actor_header("inspect-empty-decisions-sweep"))
                .expect("header is JSON");
        assert_eq!(v["id"], "rule:inspect-empty-decisions-sweep");
        assert_eq!(v["role"], "platform-admin");
        assert_eq!(v["access_tier"], "operator");
    }
}
