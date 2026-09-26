//! Whose schedule a caller may read — ONE answer for the five
//! scheduling reads (backlog a621d091, 2026-09-25).
//!
//! Until this module the availability, assignment, shift-pattern and
//! week-grid reads took no caller at all. The gateway forwards
//! `/api/scheduling/*` for any session, so the guest session read
//! every employee's week, and a request with no `x-boss-user` — the
//! `/ics` traversal (1d9b7db7), or anything reaching the jobs port
//! directly — read the same.
//!
//! The rule is the jobs list's idiom: `CurrentUser`, then Read on a
//! resource ([`Resource::schedule`]) whose granted scope, as a
//! [`Predicate`], names the employees the caller may see. On top of
//! any grant, an employee reads their own schedule.
//!
//! A HEADERLESS caller is `anonymous` at role `guest` in
//! `CurrentUser`. `crate::trust::is_trusted` admits exactly that caller
//! on the operator-machinery doors (a loopback sibling), and it is
//! deliberately not consulted here (e84de48e is that decision): no
//! grant names `guest` on `schedule`, and an anonymous request is
//! nobody's self, so it is refused. So is the guest session, which
//! carries the read-only role and no employee id.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy_client::{PolicyClient, Predicate, Resource, User};

/// The employees whose schedule one caller may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readable {
    /// A grant at `all`.
    Everyone,
    /// Exactly these employees, never none: a caller who may read
    /// nobody's schedule is refused by [`readable`] instead, so a list
    /// read never answers an empty 200 that reads like "no schedule".
    Only(Vec<String>),
}

impl Readable {
    pub(crate) fn admits(&self, employee_id: &str) -> bool {
        match self {
            Self::Everyone => true,
            Self::Only(ids) => ids.iter().any(|i| i == employee_id),
        }
    }

    /// The employee set a read that names nobody runs under — `None`
    /// is everyone.
    pub(crate) fn only(&self) -> Option<&[String]> {
        match self {
            Self::Everyone => None,
            Self::Only(ids) => Some(ids),
        }
    }

    /// The refusal a read naming `named` gets, if it names an employee
    /// this caller may not see.
    pub(crate) fn refusal(&self, named: Option<&str>) -> Option<Response> {
        named.filter(|emp| !self.admits(emp)).map(outside)
    }

    /// The rows of an answer this caller may see, by each row's
    /// employee.
    pub(crate) fn keep<T>(&self, rows: Vec<T>, employee_of: impl Fn(&T) -> &str) -> Vec<T> {
        rows.into_iter()
            .filter(|r| self.admits(employee_of(r)))
            .collect()
    }
}

/// Who `user` may read, from the Read grant on `schedule` as a
/// predicate. `None` is nobody.
///
/// `Predicate::owner_allow_list` is the one translation of a grant
/// into an employee set, with one exception taken here: a DEPARTMENT
/// grant names nobody. Schedule rows carry no department, and the
/// shared translation reads `department:<d>` as all-or-nothing on the
/// caller's own department — for a schedule that would hand a
/// department manager every employee's week, company-wide. It fails
/// closed until the rows can answer the question.
pub(crate) fn from_predicate(predicate: &Predicate, user: &User) -> Option<Readable> {
    let granted = match predicate {
        Predicate::DepartmentIs { .. } => Some(Vec::new()),
        other => other.owner_allow_list(user),
    };
    let Some(mut ids) = granted else {
        return Some(Readable::Everyone);
    };
    if is_an_employee(user) && !ids.contains(&user.id) {
        ids.push(user.id.clone());
    }
    (!ids.is_empty()).then_some(Readable::Only(ids))
}

/// Whether this caller can be the employee a schedule belongs to: not
/// an anonymous visitor by id or by role — the headerless `anonymous`,
/// the guest session's address, or any role on the read-only floor
/// (`audit-readonly`, and `visitor` since design 2830b6b7). Asked of
/// `boss_core::roles`, the one list the calendar-token door on this
/// router asks too, so a role joining the floor is refused here with
/// no edit.
fn is_an_employee(user: &User) -> bool {
    !boss_core::roles::is_anonymous_visitor(&user.id, &user.role)
}

/// Ask policy who `user` may read, or the response that refuses.
/// A policy service that cannot answer refuses with 500: a gate that
/// cannot be asked is not a gate that passed.
pub(crate) async fn readable(policy: &dyn PolicyClient, user: &User) -> Result<Readable, Response> {
    let predicate = policy
        .scope_predicate(user, Resource::schedule())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response()
        })?;
    from_predicate(&predicate, user).ok_or_else(|| {
        (
            StatusCode::FORBIDDEN,
            format!(
                "reading a schedule takes a Read grant on `schedule`, or being its employee; \
                 {} (role {}) is neither",
                user.id, user.role
            ),
        )
            .into_response()
    })
}

/// The refusal of a read by id: generic, because the caller did not
/// name the employee and must not learn it from the refusal
/// (adversarial review, 2026-09-25).
pub(crate) fn not_in_scope() -> Response {
    (
        StatusCode::FORBIDDEN,
        "this assignment's schedule is outside your scope",
    )
        .into_response()
}

/// The refusal of a read that named `employee_id` itself — echoing
/// what the caller sent tells them nothing new.
fn outside(employee_id: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        format!("{employee_id}'s schedule is outside your scope"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::AccessTier;

    fn user(id: &str, role: &str, reports: &[&str]) -> User {
        User {
            id: id.into(),
            role: role.into(),
            access_tier: AccessTier::User,
            territory_account_ids: Vec::new(),
            direct_report_ids: reports.iter().map(|s| s.to_string()).collect(),
            department: Some("service".into()),
        }
    }

    #[test]
    fn no_grant_is_the_employee_themself() {
        let u = user("emp-1", "service-tech", &[]);
        assert_eq!(
            from_predicate(&Predicate::None, &u),
            Some(Readable::Only(vec!["emp-1".into()]))
        );
    }

    #[test]
    fn the_headerless_caller_and_the_guest_session_read_nobody() {
        for u in [
            user("anonymous", "guest", &[]),
            user("guest@algedonic.dev", "audit-readonly", &[]),
            user("guest@algedonic.dev", "visitor", &[]),
            // A read-only role is nobody's self even under a real id —
            // the seeded `emp-audit` login shares `audit-readonly`.
            user("emp-audit", "visitor", &[]),
        ] {
            assert_eq!(from_predicate(&Predicate::None, &u), None, "{}", u.id);
        }
    }

    #[test]
    fn a_team_grant_is_the_caller_and_their_reports() {
        let u = user("emp-mgr", "service-mgr", &["emp-1"]);
        let p = Predicate::OwnerIn {
            user_ids: vec!["emp-mgr".into(), "emp-1".into()],
        };
        let r = from_predicate(&p, &u).unwrap();
        assert!(r.admits("emp-1") && r.admits("emp-mgr"));
        assert!(!r.admits("emp-2"));
    }

    #[test]
    fn an_all_grant_is_everyone() {
        let u = user("emp-david", "platform-admin", &[]);
        assert_eq!(
            from_predicate(&Predicate::Unrestricted, &u),
            Some(Readable::Everyone)
        );
    }

    #[test]
    fn a_department_grant_does_not_widen_to_the_company() {
        // The caller sits in the granted department — the shared
        // translation would answer "everyone" here.
        let u = user("emp-mgr", "service-mgr", &[]);
        let p = Predicate::DepartmentIs {
            department: "service".into(),
        };
        assert_eq!(
            from_predicate(&p, &u),
            Some(Readable::Only(vec!["emp-mgr".into()]))
        );
    }
}
