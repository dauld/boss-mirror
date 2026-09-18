//! The platform owner, for every packet a verb files — a car, a
//! gate-run, a design doc, the conductor's alarms (backlog 3c23662d,
//! design 42277636). The port and the WHY are
//! `boss_core::platform_owner`; this module is only how a verb that
//! knows the jobs base reaches the people registry, and the one line it
//! prints when the registry refuses.
//!
//! WHERE THE PEOPLE API IS. A verb has `BOSS_JOBS_URL` and nothing
//! else. The system of record is several services on one address
//! (design 28d2bed9, boss-jobs-internal.yaml), each on the port
//! boss-ports assigns it, so the people door is the jobs base with its
//! port moved to `boss_ports::prod("people")` — the rule the shell doors
//! apply in infra/forge/probe-bin/sor-routes.sh (`sor_base_on_port`).
//! `BOSS_PEOPLE_URL` names it outright when set (a launcher, a test).
//! The host is never this module's to choose: a verb that named its own
//! host could read a different stack and get a well-formed answer about
//! different data (CLAUDE.md §Doors).

use boss_core::platform_owner::{PlatformOwner, owner_for_filing};
use boss_people_client::ReqwestPlatformOwner;

pub(crate) const PEOPLE_ENV: &str = "BOSS_PEOPLE_URL";

/// `base` with its port swapped and its host kept:
/// `scheme://host[:port][/…]` → `scheme://host:PORT`.
pub(crate) fn on_port(base: &str, port: u16) -> String {
    let (scheme, rest) = base.split_once("://").unwrap_or(("http", base));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => format!("[{}]", v6.split(']').next().unwrap_or_default()),
        None => authority.split(':').next().unwrap_or_default().to_string(),
    };
    format!("{scheme}://{host}:{port}")
}

/// The people door for a verb: `people_env` when it names one, else the
/// jobs base re-pointed at the people port.
pub(crate) fn people_base_from(people_env: Option<String>, jobs_base: &str) -> String {
    people_env
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| on_port(jobs_base, boss_ports::prod("people")))
}

/// A resolver over the people door the jobs base implies.
pub(crate) fn resolver(jobs_base: &str) -> ReqwestPlatformOwner {
    ReqwestPlatformOwner::new(people_base_from(std::env::var(PEOPLE_ENV).ok(), jobs_base))
}

/// The owner a verb files with: the port's answer, or NOBODY with the
/// refusal said once on stderr. Filing goes ahead either way — the jobs
/// API resolves the kind's `owner_role` from the same registry or
/// refuses by name — because a verb that fell silent for want of an
/// owner would be the failure mode this repo forbids.
pub(crate) async fn for_filing(port: &dyn PlatformOwner) -> String {
    owner_for_filing(port, |e| {
        eprintln!("boss: {e}; filing with no owner named — the jobs API resolves the kind's owner_role, or refuses");
    })
    .await
}

/// [`for_filing`] for a hand verb that has only the resolved jobs base.
pub(crate) async fn for_filing_at(jobs_base: &str) -> String {
    for_filing(&resolver(jobs_base)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The people door keeps the host and moves the port — the same
    /// rule the shell doors apply, pinned here for the Rust side.
    #[test]
    fn the_people_door_is_the_jobs_host_on_the_people_port() {
        let people = boss_ports::prod("people");
        assert_eq!(
            on_port("http://10.20.0.34:7900", people),
            format!("http://10.20.0.34:{people}")
        );
        assert_eq!(
            on_port(
                "http://boss-jobs-internal.boss.svc.cluster.local:7900/",
                people
            ),
            format!("http://boss-jobs-internal.boss.svc.cluster.local:{people}")
        );
        assert_eq!(
            on_port("https://sor.example", 7500),
            "https://sor.example:7500"
        );
        assert_eq!(on_port("http://[::1]:7900/api", 7500), "http://[::1]:7500");
    }

    /// An explicit people URL wins; blank is unset.
    #[test]
    fn an_explicit_people_url_wins_and_blank_is_unset() {
        assert_eq!(
            people_base_from(Some("http://people:1/".into()), "http://jobs:7900"),
            "http://people:1"
        );
        assert_eq!(
            people_base_from(Some("  ".into()), "http://jobs:7900"),
            on_port("http://jobs:7900", boss_ports::prod("people"))
        );
    }

    /// A refusal files with nobody; an answer files with the owner.
    #[tokio::test]
    async fn a_refusal_files_with_nobody() {
        use boss_core::platform_owner::{Fixed, NOBODY, Nobody};
        assert_eq!(for_filing(&Nobody).await, NOBODY);
        assert_eq!(for_filing(&Fixed("emp-x".into())).await, "emp-x");
    }
}
