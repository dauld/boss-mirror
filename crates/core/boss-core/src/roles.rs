//! Role groupings shared across services.
//!
//! Executive role codes are tenant-defined. At startup each service
//! loads the `employee` Class registry and seeds an in-process cache
//! with the codes whose `metadata.is_executive` is true; subsequent
//! `is_executive` / `has_global_read` checks read from that cache
//! synchronously. Services that haven't initialised the cache treat
//! every role as non-executive — `platform-admin` and `audit-readonly`
//! are the only roles that grant global read in that state.
//!
//! The executive set is Class-registry-driven, not a hardcoded
//! `ceo|coo|cto|cfo` list: tenants pick their own executive roles via
//! the Class registry without forking core. The seeding helper lives
//! in [`crate::roles::load_executive_roles_from_classes`].

use std::collections::HashSet;
use std::sync::OnceLock;

static EXECUTIVE_ROLES: OnceLock<HashSet<String>> = OnceLock::new();

/// Initialise the in-process executive-role cache. Idempotent —
/// repeated calls after the first are no-ops. Call once at service
/// startup with the codes whose Class metadata flags
/// `is_executive = true`. Pre-init, `is_executive` returns false
/// for every role.
pub fn init_executive_roles(roles: impl IntoIterator<Item = String>) {
    let set: HashSet<String> = roles.into_iter().collect();
    let _ = EXECUTIVE_ROLES.set(set);
}

/// True if `role` is in the executive cache. Returns false if the
/// cache hasn't been seeded — services that depend on this gate
/// must call [`init_executive_roles`] at startup.
pub fn is_executive(role: &str) -> bool {
    EXECUTIVE_ROLES
        .get()
        .map(|set| set.contains(role))
        .unwrap_or(false)
}

/// Platform-admin role — the operator who owns the BOSS deployment
/// itself. On the OSS quickstart this is the bootstrap admin email;
/// on real tenants it's whoever holds the keys to the box.
pub const PLATFORM_ADMIN_ROLE: &str = "platform-admin";

/// Audit-readonly role — the OSS playground's anonymous-bind role
/// and the seeded `emp-audit` external-auditor login. Has Read on
/// every projection resource via the policy defaults; never has
/// any write/mutate verb. Treated as a global-read role here so
/// admin-ish gates that expose status data (integration providers,
/// gateway perf, etc.) don't reject anonymous OSS visitors.
pub const AUDIT_READONLY_ROLE: &str = "audit-readonly";

/// Break-glass role — the emergency session minted by the gateway's
/// hardware-key WebAuthn ceremony (docs/design/break-glass-is-a-key-
/// you-hold.md). Deliberately NARROW (Q4): it carries exactly the
/// three emergency levers — deploy rollback, merge approval, auth
/// administration — and is not platform-admin. It must never join
/// [`has_global_read`]: an emergency key is a door key, not a data
/// key.
pub const BREAK_GLASS_ROLE: &str = "break-glass";

/// True for any role that has full read across the deployment —
/// the platform admin, audit-readonly, or any role the tenant has
/// flagged executive via `metadata.is_executive = true`.
pub fn has_global_read(role: &str) -> bool {
    role == PLATFORM_ADMIN_ROLE || role == AUDIT_READONLY_ROLE || is_executive(role)
}

/// The roles allowed to administer the gateway's auth surface
/// (onboard local credentials, issue resets) — named, never derived.
///
/// - `platform-admin`: the deploy superuser, who onboards the first
///   users and holds every registry write in core's policy defaults.
/// - `break-glass`: auth administration is one of its three named
///   levers; without it a lockout emergency could not repair the door
///   it came in through.
///
/// This set used to be `has_global_read` plus break-glass (backlog
/// 34242f9a, 2026-09-25). Global read is a READ grant, and inheriting
/// it handed a write to two members that were never meant to hold
/// one: `audit-readonly`, the role `POST /api/auth/guest` mints for
/// any anonymous visitor and whose own contract says it never writes;
/// and every tenant-flagged executive, whose `is_executive` flag
/// means "reads everything" and was never a grant to overwrite any
/// credential — the platform-admin's included, which makes onboard a
/// path from a tenant role to the deploy superuser.
pub const AUTH_ADMINISTRATOR_ROLES: [&str; 2] = [PLATFORM_ADMIN_ROLE, BREAK_GLASS_ROLE];

/// True for exactly the roles in [`AUTH_ADMINISTRATOR_ROLES`].
pub fn can_administer_auth(role: &str) -> bool {
    AUTH_ADMINISTRATOR_ROLES.contains(&role)
}

/// Visitor role — the anonymous guest on an install offering BASIC
/// guest access, the OSS default (design 2830b6b7, decided 2026-09-25:
/// "the default for the OSS repo be guest accounts with only basic
/// access, whatever that means for that install"). It reads what the
/// install grants the role as policy data and nothing else: it is not
/// in [`has_global_read`], so the name-keyed gates on that (the events
/// tail, the people scope routes) refuse it without an edit of their
/// own.
///
/// Named `visitor` and NOT `guest`: `guest` is the role the policy
/// extractor gives a request that arrived with no `x-boss-user` header
/// — a loopback sibling or a test harness — and `boss-jobs` `trust.rs`
/// and boss-messages admit that string for WRITES. A browser session
/// carrying it would pass those doors.
pub const VISITOR_ROLE: &str = "visitor";

/// The roles that read and never write — the two a guest session can
/// carry. `audit-readonly` is the system-audit read (the external
/// auditor, and a guest on an instance that opts in to it);
/// `visitor` is the basic guest. Every read-only guard asks
/// [`is_read_only_floor`] rather than naming a role, because three such
/// guards once keyed on the NAME `audit-readonly` and a new guest role
/// added only to policy would have passed all three (sim-clock control,
/// the simulator, surface opens).
///
/// `guest` is deliberately not here: it is the no-header trusted-
/// internal sentinel, not a session, and the doors that refuse it keep
/// their own check beside this one.
pub const READ_ONLY_FLOOR_ROLES: [&str; 2] = [AUDIT_READONLY_ROLE, VISITOR_ROLE];

/// True for exactly the roles in [`READ_ONLY_FLOOR_ROLES`] — the ONE
/// predicate for "read-only role". The gateway refuses every method but
/// GET/HEAD/OPTIONS from a session whose effective role answers true
/// here, before any upstream sees the request, because about 110
/// upstream write routes authorized no caller and a guest session
/// carries one of these roles (backlog 07e797b4, 2026-09-25). That edge
/// landed asking an `audit-readonly`-only predicate of its own the same
/// day this list gained `visitor`; the two were merged into this one so
/// a Basic guest is refused at the edge like an Audit guest, and a role
/// joining the floor here is refused there with no edit.
pub fn is_read_only_floor(role: &str) -> bool {
    READ_ONLY_FLOOR_ROLES.contains(&role)
}

/// The role a session acts as when it carries none: [`VISITOR_ROLE`],
/// the least access, where it used to be `audit-readonly` — the widest
/// read. A session reaching a backend without a role is a defect
/// somewhere upstream, and a defect should not widen what a stranger
/// reads (design 2830b6b7).
pub fn effective_role(role: Option<&str>) -> &str {
    role.unwrap_or(VISITOR_ROLE)
}

// ---------------------------------------------------------------------------
// Broad-account-access role set
// ---------------------------------------------------------------------------
//
// Roles allowed to see every account's next-best-actions / risk-score
// watchlist without being on the account's territory-rep or team-member
// list. Sits between `is_executive` (C-suite tenure) and ordinary role
// gates — covers VPs + ops/sales/service managers that need cross-
// account visibility for triage.
//
// Same OnceLock + Class-registry shape as `EXECUTIVE_ROLES`. Pre-init
// fallback: the union of the two pre-D5 hardcoded lists from
// `boss-accounts` (next_actions + risk_scores), so services that
// haven't called `init_broad_account_access_roles` keep the original
// behavior. Tenants flag a role broad-access by setting
// `metadata.broad_account_access = true` on the employee Class +
// calling `boss_classes_client::seed_broad_account_access_role_cache`
// at startup.

static BROAD_ACCOUNT_ACCESS_ROLES: OnceLock<HashSet<String>> = OnceLock::new();

/// Default broad-account-access role set when the Class-registry cache
/// hasn't been seeded. Union of the two pre-D5 hardcoded lists from
/// `boss-accounts/src/{account_next_actions,account_risk_scores}.rs`
/// — dedupes the drift the punch list flagged.
const DEFAULT_BROAD_ACCOUNT_ACCESS_ROLES: &[&str] = &[
    "ceo",
    "cto",
    "coo",
    "cfo",
    "controller",
    "vp-sales",
    "sales-mgr",
    "vp-service",
    "service-mgr",
];

/// Initialise the broad-account-access role cache. Idempotent —
/// repeated calls after the first are no-ops. Call once at service
/// startup with the codes whose Class metadata flags
/// `broad_account_access = true`. Pre-init, [`has_broad_account_access`]
/// falls back to [`DEFAULT_BROAD_ACCOUNT_ACCESS_ROLES`].
pub fn init_broad_account_access_roles(roles: impl IntoIterator<Item = String>) {
    let set: HashSet<String> = roles.into_iter().collect();
    let _ = BROAD_ACCOUNT_ACCESS_ROLES.set(set);
}

/// True if `role` is in the broad-account-access cache (or, pre-init,
/// in the default set). Platform-admin + audit-readonly also qualify
/// since they hold global read.
pub fn has_broad_account_access(role: &str) -> bool {
    if has_global_read(role) {
        return true;
    }
    match BROAD_ACCOUNT_ACCESS_ROLES.get() {
        Some(set) => set.contains(role),
        None => DEFAULT_BROAD_ACCOUNT_ACCESS_ROLES.contains(&role),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The executive-role cache is process-global. To keep tests
    // hermetic we drive a single initialisation up-front; the
    // OnceLock means later `init_executive_roles` calls in this
    // test module are no-ops.
    fn seed_executive_set() {
        init_executive_roles(["ceo", "coo", "cto", "cfo"].into_iter().map(String::from));
    }

    #[test]
    fn executive_roles_pulled_from_init() {
        seed_executive_set();
        assert!(is_executive("ceo"));
        assert!(is_executive("cto"));
        assert!(!is_executive("service-tech"));
        assert!(!is_executive(""));
    }

    #[test]
    fn has_global_read_covers_admin_audit_and_seeded_executives() {
        seed_executive_set();
        assert!(has_global_read("ceo"));
        assert!(has_global_read("cto"));
        assert!(has_global_read(PLATFORM_ADMIN_ROLE));
        assert!(has_global_read(AUDIT_READONLY_ROLE));
        assert!(!has_global_read("service-tech"));
        assert!(!has_global_read("admin")); // legacy "admin" is not platform-admin
    }

    /// Q4 (break-glass-is-a-key-you-hold): the emergency role is
    /// NARROW. If it ever gains global read, every "admin-ish" gate
    /// keyed on `has_global_read` silently widens the emergency key
    /// into a data key.
    #[test]
    fn break_glass_never_has_global_read() {
        seed_executive_set();
        assert!(!has_global_read(BREAK_GLASS_ROLE));
    }

    /// Auth administration is one of break-glass's three levers; the
    /// gateway's admin gates ask this instead of `has_global_read`.
    #[test]
    fn break_glass_can_administer_auth_without_global_read() {
        seed_executive_set();
        assert!(can_administer_auth(BREAK_GLASS_ROLE));
        assert!(can_administer_auth(PLATFORM_ADMIN_ROLE));
        assert!(!can_administer_auth("service-tech"));
    }

    /// Backlog 34242f9a (2026-09-25). `audit-readonly` is the role
    /// the guest endpoint mints for any anonymous visitor, and its own
    /// contract above says it never writes. It held global read, and
    /// auth administration used to inherit that set whole — so a
    /// guest could overwrite any local credential. Global read is not
    /// write authority; this pins the two apart.
    #[test]
    fn audit_readonly_cannot_administer_auth() {
        seed_executive_set();
        assert!(has_global_read(AUDIT_READONLY_ROLE), "the read half stays");
        assert!(!can_administer_auth(AUDIT_READONLY_ROLE));
    }

    /// Same defect, second member: a tenant-flagged executive's
    /// `is_executive` is a READ flag, and onboard overwrites ANY
    /// credential — the platform-admin's included — so inheriting it
    /// would let a tenant role make itself the deploy superuser.
    #[test]
    fn a_seeded_executive_cannot_administer_auth() {
        seed_executive_set();
        assert!(has_global_read("ceo"), "the read half stays");
        for role in ["ceo", "coo", "cto", "cfo"] {
            assert!(
                !can_administer_auth(role),
                "{role} must not administer auth"
            );
        }
    }

    /// The admitted set is named, not derived: exactly these two.
    #[test]
    fn auth_administrators_are_named_explicitly() {
        seed_executive_set();
        for role in ["", "guest", "admin", "smoke-tester", "owner"] {
            assert!(
                !can_administer_auth(role),
                "{role:?} must not administer auth"
            );
        }
        assert_eq!(
            AUTH_ADMINISTRATOR_ROLES,
            [PLATFORM_ADMIN_ROLE, BREAK_GLASS_ROLE]
        );
    }

    /// Design 2830b6b7 (decided 2026-09-25): the read-only floor is
    /// the two roles a guest can carry and nothing else. A role that
    /// merely READS everything (platform-admin, a seeded executive) is
    /// not on it, nor is break-glass, nor `guest` — that string is the
    /// no-header trusted-internal sentinel, and every door that asks
    /// this predicate keeps its own `guest` check beside it. The
    /// gateway refuses every write from a role on the floor (07e797b4),
    /// so a false positive here locks a writer out at the edge.
    #[test]
    fn the_read_only_floor_is_audit_readonly_and_visitor() {
        seed_executive_set();
        assert_eq!(READ_ONLY_FLOOR_ROLES, [AUDIT_READONLY_ROLE, VISITOR_ROLE]);
        assert!(is_read_only_floor(AUDIT_READONLY_ROLE));
        assert!(is_read_only_floor(VISITOR_ROLE));
        for role in [
            PLATFORM_ADMIN_ROLE,
            BREAK_GLASS_ROLE,
            "guest",
            "ceo",
            "service-tech",
            "",
            "Visitor",
            "Audit-Readonly",
        ] {
            assert!(!is_read_only_floor(role), "{role:?} is not the floor");
        }
    }

    /// `visitor` is a NEW name, not a second spelling of `guest` — the
    /// no-header sentinel `trust.rs` and boss-messages admit for writes.
    /// And it is outside global read: the OSS guest reads only what the
    /// install grants it as policy rows, so the name-keyed gates on
    /// `has_global_read` (the events tail, people `bootstrap_by_email`)
    /// refuse it with no edit of their own.
    #[test]
    fn a_visitor_is_not_the_headerless_guest_and_has_no_global_read() {
        seed_executive_set();
        assert_eq!(VISITOR_ROLE, "visitor");
        assert_ne!(VISITOR_ROLE, "guest");
        assert!(!has_global_read(VISITOR_ROLE));
        assert!(!has_broad_account_access(VISITOR_ROLE));
        assert!(!can_administer_auth(VISITOR_ROLE));
    }

    /// A session that reaches a backend with no role acts as the LEAST
    /// access, not the widest read: the fallback used to be
    /// `audit-readonly`, Read on every shipped resource (design 2830b6b7).
    #[test]
    fn a_session_without_a_role_acts_as_a_visitor() {
        assert_eq!(effective_role(None), VISITOR_ROLE);
        assert_eq!(effective_role(Some("service-tech")), "service-tech");
        assert_eq!(
            effective_role(Some(AUDIT_READONLY_ROLE)),
            AUDIT_READONLY_ROLE
        );
    }
}
