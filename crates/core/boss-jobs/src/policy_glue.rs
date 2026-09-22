//! Glue between `boss-policy` (resource-agnostic) and `boss-jobs`
//! (domain-specific). The policy engine emits intent; this module
//! realises that intent against the Job shape.
//!
//! The Territory arm below consumes the `Subject` trait's `kind()`
//! + `id()` methods (docs/architecture-decisions.md §Primitives &
//! information architecture); the enum remains the storage shape,
//! consumed through the trait's bridge impl.

use boss_core::job::Job;
// Subject trait comes in through the full path on
// `territory_matches`'s impl bound; no top-level `use` is needed,
// and aliasing would clash with the enum of the same name that the
// tests still construct directly.
use boss_policy_client::{Scope, User};

/// Does this `job` fall within `scope` for `user`?
///
/// Used after a successful `policy.check()` to verify the *specific*
/// target row is inside the caller's scope. `list_with_predicate`
/// handles the collection case at the repository layer; this one
/// handles single-row reads and writes.
pub fn scope_matches(user: &User, scope: &Scope, job: &Job) -> bool {
    match scope {
        Scope::None => false,
        Scope::All => true,
        Scope::Self_ => job.owner_id == user.id,
        Scope::Territory => territory_matches(user, &job.subject),
        Scope::Team => user.id == job.owner_id || user.direct_report_ids.contains(&job.owner_id),
        Scope::Department(d) => user.department.as_deref() == Some(d.as_str()),
    }
}

/// Territory scope check over a Subject, keyed by kind.
///
/// The only kinds whose id maps into `territory_account_ids` are
/// `account` and `employee`. Others deny:
///
/// - `asset` would require an Assets → Account resolver to find the
///   owning account; v1 pessimistically denies to avoid a cross-
///   service hop on every check. Service/refurb jobs on Systems are
///   normally Department-scoped anyway.
/// - `purchase_order` and `campaign` aren't territory-scoped.
/// - `vendor` — procurement sees all vendors (or by account-team
///   membership, a separate scope). Territory is customer-facing.
/// - `custom` — opaque; deny by default.
fn territory_matches(user: &User, subject: &impl boss_core::primitives::Subject) -> bool {
    match subject.kind() {
        "account" | "employee" => {
            let target = subject.id();
            user.territory_account_ids.iter().any(|p| p == target)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    // `Subject` is the storage shape and the most compact way to
    // assemble test fixtures.
    use boss_core::job::{JobId, Priority, Subject};

    fn user(id: &str, role: &str) -> User {
        User {
            id: id.to_string(),
            role: role.to_string(),
            access_tier: boss_policy_client::AccessTier::User,
            territory_account_ids: vec!["p-1".into(), "p-2".into()],
            direct_report_ids: vec!["emp-2".into(), "emp-3".into()],
            department: Some("service".into()),
        }
    }

    fn job(owner_id: &str, subject: Subject) -> Job {
        Job {
            id: JobId::new(),
            kind: "field-service".into(),
            workflow_version: 1,
            subject,
            title: "Test".into(),
            owner_id: owner_id.to_string(),
            status: boss_core::job::JobStatus::Open,
            priority: Priority::Standard,
            opened_on: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            opened_at: None,
            due_on: None,
            closed_on: None,
            metadata: serde_json::Value::Null,
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        }
    }

    #[test]
    fn self_matches_owner() {
        let u = user("emp-1", "service-tech");
        let j = job("emp-1", Subject::new("asset", "SYS-1"));
        assert!(scope_matches(&u, &Scope::Self_, &j));

        let other = job("emp-2", Subject::new("asset", "SYS-1"));
        assert!(!scope_matches(&u, &Scope::Self_, &other));
    }

    #[test]
    fn territory_matches_account_in_list() {
        let u = user("emp-1", "sales-rep");
        let inside = job("emp-other", Subject::new("account", "p-1"));
        let outside = job("emp-other", Subject::new("account", "p-99"));
        assert!(scope_matches(&u, &Scope::Territory, &inside));
        assert!(!scope_matches(&u, &Scope::Territory, &outside));
    }

    #[test]
    fn territory_denies_system_subject_in_v1() {
        // Cross-service resolver not wired yet; must deny conservatively.
        let u = user("emp-1", "sales-rep");
        let j = job("emp-other", Subject::new("asset", "SYS-5"));
        assert!(!scope_matches(&u, &Scope::Territory, &j));
    }

    #[test]
    fn team_matches_self_plus_reports() {
        let u = user("emp-mgr", "service-mgr");
        assert!(scope_matches(
            &u,
            &Scope::Team,
            &job("emp-mgr", Subject::new("account", "p-1"))
        ));
        assert!(scope_matches(
            &u,
            &Scope::Team,
            &job("emp-2", Subject::new("account", "p-1"))
        ));
        assert!(!scope_matches(
            &u,
            &Scope::Team,
            &job("emp-outsider", Subject::new("account", "p-1"))
        ));
    }

    #[test]
    fn department_matches_user_department() {
        let u = user("emp-1", "service-mgr");
        let j = job("emp-other", Subject::new("asset", "SYS-1"));
        assert!(scope_matches(&u, &Scope::Department("service".into()), &j));
        assert!(!scope_matches(&u, &Scope::Department("sales".into()), &j));
    }

    #[test]
    fn all_always_matches() {
        let u = user("emp-1", "ceo");
        let j = job("emp-other", Subject::new("weird", "x"));
        assert!(scope_matches(&u, &Scope::All, &j));
    }

    #[test]
    fn none_never_matches() {
        let u = user("emp-1", "anyone");
        let j = job("emp-1", Subject::new("account", "p-1"));
        assert!(!scope_matches(&u, &Scope::None, &j));
    }
}
