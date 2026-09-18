//! The platform owner — the ONE person the platform's own packets are
//! filed to, read from the people registry, never written into code.
//!
//! WHY (backlog 3c23662d, design 42277636 first wave, audit H6). On
//! 2026-09-18 fourteen production sites carried `"emp-david"` as the
//! `owner_id` of every packet the platform files — every car (Tier-1
//! `boss_jobs::car`), gate-run, design doc, conductor alarm, estate,
//! cadence, sensor and DNS alarm, the forge watchdog's alert and the
//! nightly install-smoke red — and the bootstrap walk signed its
//! synthetic approvals `"emp-cto"`. Each was a fact about ONE deployment
//! in code every deployment runs: on the playground the id names
//! nobody, on an OSS install nobody, and the day Algedonic LLC hires a
//! second operator (decided 2026-09-16) every alarm still lands on the
//! first one unless fourteen files are edited in step. The measured
//! line: 16 literals → 0, one definition here.
//!
//! WHAT THE OWNER IS. The first active hire holding
//! [`crate::roles::PLATFORM_ADMIN_ROLE`], by hire date — on prod that is
//! David, on the playground the bootstrap admin, on an OSS install
//! whoever the operator hired first. Ordered by hire date and not by id
//! so a second admin joining later changes nothing about who answers
//! for the platform's packets, and so the answer is the same from every
//! process that asks. `BOSS_PLATFORM_OWNER` is the explicit override a
//! launcher or unit may carry; it wins without a registry read.
//!
//! NO DEFAULT. When neither the registry nor the environment names
//! anyone, the caller gets a NAMED refusal ([`PlatformOwnerError`]),
//! never a literal. A filer then writes [`NOBODY`] as the packet's
//! `owner_id`: the jobs API's admission (Q7, `owner_resolution.rs`)
//! resolves an unowned packet to the kind's `owner_role` holder from the
//! same registry, or refuses the create by name — either way a person is
//! READ, and silence is refused exactly like failure (CLAUDE.md, mostly
//! sure vs. absolutely sure).
//!
//! This is the PORT. Tier-1 code names the trait; the resolution over
//! the people HTTP API lives with the people client
//! (`boss_people_client::ReqwestPlatformOwner`), which is Tier 2 —
//! exactly the hexagonal seam that keeps the core free of a domain
//! service it must not depend on. [`first_hire`] is the pure half of
//! that resolution and lives here so the ordering rule is one function
//! any adapter reuses.

use async_trait::async_trait;
use chrono::NaiveDate;

/// The explicit override: an employee id a launcher or unit names so no
/// registry read is needed (an OSS quickstart before the roster exists,
/// a unit that must file while the people service is dark).
pub const PLATFORM_OWNER_ENV: &str = "BOSS_PLATFORM_OWNER";

/// What a filer writes as `owner_id` when the port refused: nobody.
/// Empty is automation-shaped to the jobs API
/// (`boss_jobs::owner_resolution::is_automation_shaped`), so admission
/// resolves the kind's `owner_role` holder from the registry or refuses
/// the create with the Q7 reason. Never a person's id.
pub const NOBODY: &str = "";

/// The named refusal. Each variant says what was asked and what would
/// answer it, because a refusal that names neither sends an operator
/// to re-derive what the system already knew.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlatformOwnerError {
    /// The registry answered and nobody active holds the role.
    #[error(
        "no platform owner: no active employee holds `{role}` in the people registry \
         and {PLATFORM_OWNER_ENV} is unset — hire one, or name one in the environment"
    )]
    NoHolder { role: &'static str },
    /// The registry could not be read at all.
    #[error(
        "platform owner unresolvable: the people registry did not answer ({0}) and \
         {PLATFORM_OWNER_ENV} is unset"
    )]
    Unreachable(String),
}

/// Port: who owns the platform's packets.
#[async_trait]
pub trait PlatformOwner: Send + Sync {
    /// The platform owner's employee id, or a named refusal. Never a
    /// default.
    async fn platform_owner(&self) -> Result<String, PlatformOwnerError>;
}

/// The override the environment carries, if it names anyone. Pure over
/// the value so the precedence is testable without an env write (env
/// writes are `unsafe` under edition 2024 and racy across the test
/// runner).
pub fn override_from(env: Option<String>) -> Option<String> {
    env.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// [`override_from`] over the live environment.
pub fn env_override() -> Option<String> {
    override_from(std::env::var(PLATFORM_OWNER_ENV).ok())
}

/// One holder of the role, as the registry answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub id: String,
    pub hire_date: Option<NaiveDate>,
}

/// The first hire among the holders: earliest `hire_date` wins; a holder
/// with no hire date sorts after every dated one; ties break on id so
/// the answer is the same from every process. `None` for no holders.
pub fn first_hire(holders: &[Holder]) -> Option<String> {
    holders
        .iter()
        .min_by(|a, b| {
            // A dated hire precedes an undated one: `None` last.
            match (a.hire_date, b.hire_date) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.id.cmp(&b.id))
        })
        .map(|h| h.id.clone())
}

/// The owner a filer writes: the port's answer, or [`NOBODY`] with the
/// refusal handed to `on_refusal` so it lands in a journal or on
/// stderr — never swallowed, never a literal. One helper so every filer
/// makes the same choice the same way.
pub async fn owner_for_filing(
    port: &dyn PlatformOwner,
    on_refusal: impl FnOnce(&PlatformOwnerError),
) -> String {
    match port.platform_owner().await {
        Ok(id) => id,
        Err(e) => {
            on_refusal(&e);
            NOBODY.to_string()
        }
    }
}

/// In-memory adapter: a fixed owner. For tests, and for a process that
/// resolved the owner once and hands it on.
pub struct Fixed(pub String);

#[async_trait]
impl PlatformOwner for Fixed {
    async fn platform_owner(&self) -> Result<String, PlatformOwnerError> {
        Ok(self.0.clone())
    }
}

/// In-memory adapter: nobody holds the role. For tests of the refusal
/// path.
pub struct Nobody;

#[async_trait]
impl PlatformOwner for Nobody {
    async fn platform_owner(&self) -> Result<String, PlatformOwnerError> {
        Err(PlatformOwnerError::NoHolder {
            role: crate::roles::PLATFORM_ADMIN_ROLE,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Option<NaiveDate> {
        Some(NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap())
    }

    fn holder(id: &str, hire: Option<NaiveDate>) -> Holder {
        Holder {
            id: id.into(),
            hire_date: hire,
        }
    }

    /// The earliest hire owns the platform, whatever order the registry
    /// listed them in and whatever their ids sort like.
    #[test]
    fn the_earliest_hire_is_the_owner() {
        let rows = [
            holder("emp-zed", d("2026-09-16")),
            holder("emp-alpha", d("2026-09-17")),
        ];
        assert_eq!(first_hire(&rows).as_deref(), Some("emp-zed"));
        let reversed = [rows[1].clone(), rows[0].clone()];
        assert_eq!(first_hire(&reversed).as_deref(), Some("emp-zed"));
    }

    /// An undated hire never outranks a dated one; among undated, the
    /// id decides so the answer is deterministic across processes.
    #[test]
    fn an_undated_hire_sorts_last_and_ties_break_on_id() {
        let rows = [holder("emp-a", None), holder("emp-b", d("2026-09-17"))];
        assert_eq!(first_hire(&rows).as_deref(), Some("emp-b"));
        let undated = [holder("emp-b", None), holder("emp-a", None)];
        assert_eq!(first_hire(&undated).as_deref(), Some("emp-a"));
        let same_day = [
            holder("emp-b", d("2026-09-17")),
            holder("emp-a", d("2026-09-17")),
        ];
        assert_eq!(first_hire(&same_day).as_deref(), Some("emp-a"));
    }

    /// No holders is no owner — not a default.
    #[test]
    fn no_holders_is_no_owner() {
        assert_eq!(first_hire(&[]), None);
    }

    /// The override is a trimmed non-empty id; blank is unset.
    #[test]
    fn the_override_is_read_trimmed_and_blank_is_unset() {
        assert_eq!(
            override_from(Some("  emp-x \n".into())).as_deref(),
            Some("emp-x")
        );
        assert_eq!(override_from(Some("   ".into())), None);
        assert_eq!(override_from(None), None);
    }

    /// The refusal names the role and the override, so the operator
    /// reads both fixes off the message.
    #[test]
    fn the_refusal_names_the_role_and_the_override() {
        let e = PlatformOwnerError::NoHolder {
            role: crate::roles::PLATFORM_ADMIN_ROLE,
        };
        let s = e.to_string();
        assert!(s.contains("platform-admin"), "{s}");
        assert!(s.contains(PLATFORM_OWNER_ENV), "{s}");
        let u = PlatformOwnerError::Unreachable("connection refused".into()).to_string();
        assert!(
            u.contains("connection refused") && u.contains(PLATFORM_OWNER_ENV),
            "{u}"
        );
    }

    /// A filer writes the owner, or NOBODY with the refusal reported —
    /// the refusal is never swallowed and never becomes a literal.
    #[tokio::test]
    async fn a_filer_writes_the_owner_or_nobody_and_reports_the_refusal() {
        let mut reported = None;
        let id = owner_for_filing(&Fixed("emp-x".into()), |e| reported = Some(e.clone())).await;
        assert_eq!(id, "emp-x");
        assert_eq!(reported, None);

        let mut reported = None;
        let id = owner_for_filing(&Nobody, |e| reported = Some(e.clone())).await;
        assert_eq!(id, NOBODY);
        assert!(matches!(
            reported,
            Some(PlatformOwnerError::NoHolder { .. })
        ));
    }
}
