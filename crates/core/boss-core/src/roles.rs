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

/// True for a role that may read and must never write — the ONE
/// predicate for "read-only role" (backlog 07e797b4, 2026-09-25). The
/// gateway refuses every method but GET/HEAD/OPTIONS from a session
/// whose role answers true here, before any upstream sees the request,
/// because about 110 upstream write routes authorized no caller and the
/// guest session carries this role.
///
/// `audit-readonly` alone today. Design 2830b6b7 adds a `visitor` role
/// and reuses THIS function for it rather than growing a second list:
/// a role joins the read-only set here, and every edge that asks
/// inherits it.
pub fn is_read_only_role(role: &str) -> bool {
    role == AUDIT_READONLY_ROLE
}

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

    /// The read-only set is audit-readonly and nothing else today: not
    /// the roles that merely READ everything (platform-admin, a seeded
    /// executive), not break-glass, not an empty or unknown code — the
    /// gateway refuses every write from a role in this set, so a false
    /// positive would lock a writer out at the edge.
    #[test]
    fn only_audit_readonly_is_a_read_only_role() {
        seed_executive_set();
        assert!(is_read_only_role(AUDIT_READONLY_ROLE));
        for role in [
            PLATFORM_ADMIN_ROLE,
            BREAK_GLASS_ROLE,
            "ceo",
            "service-tech",
            "",
            "Audit-Readonly",
        ] {
            assert!(!is_read_only_role(role), "{role:?} is not read-only");
        }
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
}
