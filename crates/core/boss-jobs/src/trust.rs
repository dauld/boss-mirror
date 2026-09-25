//! Who an operator-machinery door admits — ONE definition.
//!
//! Five doors (`agent_runs`, `cadence`, `credentials`, `delivery`,
//! `surface_opens`) each carried their own copy of `is_trusted`, and on
//! 2026-09-15 one of them learned that its READS must also admit the
//! auditor tier — the tier the recorded-probe reader carries
//! (the unattended prove door: `audit-readonly` at `auditor`,
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

/// Machinery: an operator-tier caller, and nothing else. Every WRITE
/// on an operator door asks this and nothing wider.
///
/// A caller that sent NO `x-boss-user` is not machinery (backlog
/// e84de48e; David, 2026-09-25: "Agreed on not trusting requests
/// without the identity header"). This used to admit `role == guest`
/// — the user the extractor makes of silence — on the theory that
/// only a loopback sibling or a test harness arrives headerless. The
/// theory was false at the door that mattered: the gateway strips
/// x-boss-* and sets it only inside a session, so a sessionless route
/// reached these doors headerless and was read as trusted. Every
/// internal caller signs as its own `automation:<x>` at operator tier
/// (the dispatcher, the conductor, the cadence loop, the gateway's
/// own writes, the sim); one that does not is refused, loudly.
pub fn is_trusted(user: &User) -> bool {
    user.access_tier == AccessTier::Operator
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
    fn an_operator_does_both() {
        let u = user("platform-admin", AccessTier::Operator);
        assert!(is_trusted(&u));
        assert!(can_read(&u));
    }

    /// A REQUEST WITH NO IDENTITY IS NOT TRUSTED (backlog e84de48e;
    /// David, 2026-09-25). The user is built by the real extractor
    /// from a request carrying no `x-boss-user`, so the pin holds the
    /// two halves of the defect together: whatever `CurrentUser` makes
    /// of silence, neither door may admit it. Until this, it made
    /// `role=guest` and `is_trusted` admitted `guest` by name — so a
    /// sessionless route through the gateway (which strips x-boss-*
    /// and sets it only for a session) read fifteen operator surfaces
    /// as trusted machinery.
    #[tokio::test]
    async fn a_request_without_the_identity_header_is_refused_on_both() {
        use axum::extract::FromRequestParts;
        let (mut parts, _) = axum::http::Request::builder()
            .uri("/api/agents")
            .body(())
            .unwrap()
            .into_parts();
        let boss_policy_client::CurrentUser(anonymous) =
            boss_policy_client::CurrentUser::from_request_parts(&mut parts, &())
                .await
                .unwrap_or_else(|_| panic!("the extractor refused a headerless request"));
        assert!(!can_read(&anonymous), "{anonymous:?}");
        assert!(!is_trusted(&anonymous), "{anonymous:?}");
    }
}
