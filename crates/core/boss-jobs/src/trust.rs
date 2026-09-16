//! Who an operator-machinery door admits — ONE definition.
//!
//! Five doors (`agent_runs`, `cadence`, `credentials`, `delivery`,
//! `surface_opens`) each carried their own copy of `is_trusted`, and on
//! 2026-09-15 one of them learned that its READS must also admit the
//! auditor tier — the tier the recorded-probe reader carries
//! (`infra/forge/run-car-probe.sh`: `audit-readonly` at `auditor`,
//! signing as `automation:run-car-probe-reader`) — because without it
//! no car about that surface could ever be proved through
//! `boss-sor-read`, the one reader a probe may use. The other four did
//! not learn it. Measured 2026-09-16 (backlog 839335b7) as that reader
//! against the system of record: `/api/delivery/policy/default`,
//! `/api/credentials`, `/api/cadence/rules` and
//! `/api/cadence/rules/{name}/last-firing` answered 403; the agent-run
//! surfaces 200. A car about the cadence, the delivery policy or the
//! credential registry was unprovable, silently.
//!
//! A fact that lives five times gets collapsed (CLAUDE.md §9a): the two
//! predicates live here and the doors import them.

use boss_policy_client::{AccessTier, User};

/// Machinery: an operator-tier caller, or a trusted internal one — the
/// extractor defaults to `role=guest` when no `x-boss-user` header
/// arrived, i.e. a loopback sibling or a test harness. The gateway
/// always injects the header for external requests, so a browser
/// session never lands in the trusted-internal path. Every WRITE on
/// an operator door asks this and nothing wider.
pub fn is_trusted(user: &User) -> bool {
    user.role == "guest" || user.access_tier == AccessTier::Operator
}

/// A READ on an operator door admits one more caller than a write: the
/// auditor tier — the door `/api/events/*` already opens to it, and
/// the tier the recorded-probe reader carries. The gateway's guest
/// session is NOT this: it arrives as `audit-readonly` at USER tier
/// (`POST /api/auth/guest`) and stays refused.
pub fn can_read(user: &User) -> bool {
    is_trusted(user) || user.access_tier == AccessTier::Auditor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(role: &str, tier: AccessTier) -> User {
        User {
            id: "x".into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        }
    }

    #[test]
    fn the_probe_reader_reads_and_cannot_write() {
        let reader = user("audit-readonly", AccessTier::Auditor);
        assert!(can_read(&reader));
        assert!(!is_trusted(&reader));
    }

    #[test]
    fn the_guest_session_is_refused_on_both() {
        let guest_session = user("audit-readonly", AccessTier::User);
        assert!(!can_read(&guest_session));
        assert!(!is_trusted(&guest_session));
    }

    #[test]
    fn an_operator_and_a_headerless_sibling_do_both() {
        for u in [
            user("platform-admin", AccessTier::Operator),
            user("guest", AccessTier::User),
        ] {
            assert!(is_trusted(&u), "{}", u.role);
            assert!(can_read(&u), "{}", u.role);
        }
    }
}
