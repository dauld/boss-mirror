//! WHICH KINDS A DEPARTMENT'S WORK IS — the join behind
//! `GET /api/jobs?department=<code>`.
//!
//! A department's work kinds are declared on their WORKFLOW: the
//! tenant's rows declare `metadata.department` (`receive-an-inquiry`
//! and `receive-a-sponsorship` say `sales`, `publish-the-landing-page`
//! says `marketing`, `receive-a-payout` says `finance`; the platform
//! bundle declares none). So "the sales department's jobs" is the
//! packets of the kinds whose ACTIVE workflow row declares `sales` —
//! the current operating model, read from the registry.
//!
//! PLUS the packets that name the department themselves (backlog
//! 481d7939, 2026-09-23). The kinds every department has —
//! `department-retro`, `page-audit`, the `backlog-item`s an audit
//! files — are platform rows that declare no department because they
//! serve all of them; their packets carry `metadata.department`, and
//! the retro and page-audit schemas require it. Joined over kinds
//! alone they appeared on no department's view. A packet's own word
//! wins over its kind's (`crate::port::DepartmentFilter`), and both are
//! read by one rule (`carried`).
//!
//! Measured before this existed (backlog cc76f755, 2026-09-18):
//! `?department=sales` answered 1944 — the unfiltered total — because
//! the listing never read the parameter, and an unknown query param
//! is silently ignored. That is the wrong-target shape CLAUDE.md
//! §Doors names: a confident answer where an empty one was true. A
//! department nobody declares resolves to no kinds here, and the port
//! answers an empty kind set with zero packets, not with everything.
//!
//! Only `metadata.department` is read. The tenant's rows happen to put
//! the same word in `category`, but `category` is the registry's
//! DISPLAY grouping (the platform bundle's is `platform`, which is no
//! department), and one declaration is one fewer to keep in step.
//!
//! The same join answers `GET /api/departments/<code>/readiness`
//! (`readiness`, `http`; design 3613f0af, backlog 1dffde5d): a
//! department's protocols are the kinds declaring it, and everything
//! else the read reports — sensors, rules, the newest retro — hangs
//! off that kind set. `GET /api/departments` lists what the DEPARTMENTS
//! registry holds (`registry` — it was the employee Class drawer
//! until backlog 80a77466), which is what the weekly retro rule
//! iterates. Its one write, `POST /api/departments/batch`, lands a
//! tenant's declared roster (`declare`; backlog 7edf0e97).

pub mod declare;
pub mod http;
pub mod readiness;
pub mod registry;
pub mod rules;

use crate::registry::WorkflowSpec;

/// The workflow-metadata key a department is declared under.
pub const KEY: &str = "department";

/// The department a workflow row declares, if it declares one.
pub fn declared(spec: &WorkflowSpec) -> Option<&str> {
    carried(&spec.metadata)
}

/// The department a metadata document names under [`KEY`] — a
/// non-empty string, or nothing. ONE rule for a workflow row and for a
/// packet (`crate::port::DepartmentFilter`): an empty string or a
/// non-string names no department, so a packet whose step defaults
/// ship `department = ""` falls back to its kind's declaration instead
/// of leaving every department's view.
pub fn carried(metadata: &serde_json::Value) -> Option<&str> {
    metadata
        .get(KEY)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// The kinds whose row declares `code` — the set the listing narrows
/// to. Empty when nothing declares it, which the port reads as "no
/// packet", never as "no filter". Sorted and deduplicated so the
/// answer is the same whatever order the registry listed its rows in.
pub fn kinds_declaring(specs: &[WorkflowSpec], code: &str) -> Vec<String> {
    let mut kinds: Vec<String> = specs
        .iter()
        .filter(|s| declared(s) == Some(code))
        .map(|s| s.kind.clone())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: &str, metadata: serde_json::Value) -> WorkflowSpec {
        let mut s = WorkflowSpec::platform_seed(kind, kind, "platform", vec![], vec![]);
        s.metadata = metadata;
        s
    }

    #[test]
    fn a_department_is_the_kinds_whose_row_declares_it() {
        let specs = vec![
            spec(
                "receive-a-sponsorship",
                serde_json::json!({ "department": "sales" }),
            ),
            spec(
                "receive-an-inquiry",
                serde_json::json!({ "department": "sales" }),
            ),
            spec(
                "receive-a-payout",
                serde_json::json!({ "department": "finance" }),
            ),
            // The platform bundle: no department, and `category` is not
            // read as one.
            spec("pr-train", serde_json::json!({})),
            spec(
                "backlog-item",
                serde_json::json!({ "surfaces": ["system-design"] }),
            ),
        ];
        assert_eq!(
            kinds_declaring(&specs, "sales"),
            vec![
                "receive-a-sponsorship".to_string(),
                "receive-an-inquiry".to_string()
            ],
        );
        assert_eq!(
            kinds_declaring(&specs, "finance"),
            vec!["receive-a-payout".to_string()]
        );
    }

    #[test]
    fn a_department_nobody_declares_is_no_kinds_not_no_filter() {
        let specs = vec![spec(
            "receive-an-inquiry",
            serde_json::json!({ "department": "sales" }),
        )];
        assert!(kinds_declaring(&specs, "no-such-department-zz").is_empty());
        // `category` is a display grouping, not a declaration.
        assert!(kinds_declaring(&specs, "platform").is_empty());
    }

    #[test]
    fn a_declaration_that_is_not_a_word_is_no_declaration() {
        // An empty string or a non-string value declares nothing —
        // `metadata_defaults = { department = "" }` is the shape one
        // tenant workflow ships for a per-PACKET field of the same
        // name, and a row-level "" must not match a department "".
        let specs = vec![
            spec("a", serde_json::json!({ "department": "" })),
            spec("b", serde_json::json!({ "department": 7 })),
            spec("c", serde_json::json!({ "department": null })),
        ];
        assert!(kinds_declaring(&specs, "").is_empty());
        assert_eq!(declared(&specs[1]), None);
        assert_eq!(declared(&specs[2]), None);
    }
}
