//! `boss-ports` — single source of truth for service port
//! assignments.
//!
//! The 7060/7250 collision (commits `bb60c58` + `8bf0f0a`) was
//! caused by port assignments living in three places — the
//! bare-metal deploy script's arrays, each binary's
//! `unwrap_or(<port>)` default, and each consumer's
//! `BOSS_<X>_URL` default. They had to stay in sync by hand;
//! they didn't, and jobs-api silently routed `policy.check()`
//! into the simulator's port (returning 404 → mapped to Deny →
//! empty pages everywhere).
//!
//! Single fix: this crate. Every service binary reads its bind
//! port from [`prod`] or [`scratch`]; every consumer reads its
//! upstream URL via [`url`]. The container's config generator
//! (`infra/oss-quickstart/generate-configs.sh`) reads the table
//! from `boss-ports-list` at container start, and the launcher's
//! roster (`services-launcher.sh`) is pinned to this table by
//! `launcher_roster_agreement` below.
//!
//! ## Adding a new service
//!
//! 1. Add a [`PortSpec`] entry below.
//! 2. Bump the binary's `unwrap_or` to call
//!    `boss_ports::prod("<name>")` instead of a literal.
//! 3. Add the binary to `services-launcher.sh`'s `SERVICES` —
//!    the pin below names the one you forgot.
//!
//! ## Per-environment overrides
//!
//! `BOSS_<NAME>_PORT` and `BOSS_<NAME>_URL` env vars still take
//! precedence — this crate sets the *default*, not the policy.

#![forbid(unsafe_code)]

/// Port-table row. `scratch` is `Some(prod + 1000)` for paired
/// services that have an isolated scratch counterpart on a
/// `+1000` offset; `None` for solo prod-only services.
#[derive(Debug, Clone, Copy)]
pub struct PortSpec {
    pub name: &'static str,
    pub prod: u16,
    pub scratch: Option<u16>,
}

/// Paired services — both a prod and a scratch instance.
pub const PAIRED: &[PortSpec] = &[
    PortSpec {
        name: "shipping",
        prod: 7100,
        scratch: Some(8100),
    },
    PortSpec {
        name: "messages",
        prod: 7200,
        scratch: Some(8200),
    },
    PortSpec {
        name: "inventory",
        prod: 7300,
        scratch: Some(8300),
    },
    PortSpec {
        name: "commerce",
        prod: 7400,
        scratch: Some(8400),
    },
    PortSpec {
        name: "people",
        prod: 7500,
        scratch: Some(8500),
    },
    PortSpec {
        name: "assets",
        prod: 7600,
        scratch: Some(8600),
    },
    PortSpec {
        name: "catalog",
        prod: 7750,
        scratch: Some(8750),
    },
    PortSpec {
        name: "calendar",
        prod: 7860,
        scratch: Some(8860),
    },
    PortSpec {
        name: "jobs",
        prod: 7900,
        scratch: Some(8900),
    },
];

/// Solo services — prod only. Registry services + the simulator
/// + ledger / ml / content.
pub const SOLO: &[PortSpec] = &[
    // search — the global search read surface (boss-search). Core:
    // it reads subjects/jobs/audit_log, which every deployment has.
    PortSpec {
        name: "search",
        prod: 7960,
        scratch: None,
    },
    // views — the View registry + the endpoint that runs one
    // (boss-views). Core: a View reads subjects/jobs/audit_log, which
    // every deployment has, and the registry is the personal rung of
    // the extensibility ladder rather than anything tenant-shaped.
    PortSpec {
        name: "views",
        prod: 7961,
        scratch: None,
    },
    // `simulator` hosts the /simulator UX — the SPA bundle + the
    // /simulator/api/* control+status surface (boss-simulator service).
    PortSpec {
        name: "simulator",
        prod: 7010,
        scratch: None,
    },
    // `sim-control` is the brewery-sim DAEMON's localhost-only control +
    // telemetry server (NOT gateway-proxied). boss-simulator (7010)
    // proxies to it for the Cockpit telemetry + behavior-config get/set.
    // Inert unless the daemon is running (demo mode).
    PortSpec {
        name: "sim-control",
        prod: 7011,
        scratch: None,
    },
    // `clock` runs on port 7060. The Clock service is the single
    // authority for "what time is it" — services hold a `ClockClient`
    // and call `clock.now()` instead of `Utc::now()`. Production runs
    // the wall-clock mode; demo runs the sim mode (advances `sim_clock`).
    PortSpec {
        name: "clock",
        prod: 7060,
        scratch: None,
    },
    PortSpec {
        name: "ml",
        prod: 7070,
        scratch: None,
    },
    PortSpec {
        name: "ledger",
        prod: 7080,
        scratch: None,
    },
    PortSpec {
        name: "content",
        prod: 7090,
        scratch: None,
    },
    PortSpec {
        name: "policy",
        prod: 7250,
        scratch: None,
    },
    PortSpec {
        name: "classes",
        prod: 7800,
        scratch: None,
    },
    PortSpec {
        name: "locations",
        prod: 7820,
        scratch: None,
    },
    PortSpec {
        name: "subject-kinds",
        prod: 7830,
        scratch: None,
    },
    PortSpec {
        name: "products",
        prod: 7840,
        scratch: None,
    },
    PortSpec {
        name: "campaigns",
        prod: 7845,
        scratch: None,
    },
    PortSpec {
        name: "customers",
        prod: 7855,
        scratch: None,
    },
    // 7880 was `observability` until 2026-09-23: boss-observability
    // retired as superseded-by (backlog 467175e7, car B), its cross-VM
    // view by a region of the IT world map.
    // Audit-log read surface. boss-events owns the audit_log table
    // and the tail/stream/export router; pre-2026-06 the router
    // was mounted into boss-people-api for convenience. Split out
    // into its own service so audit_log access is a first-class
    // tier-1 surface, not parasitic on people-api.
    PortSpec {
        name: "events",
        prod: 7150,
        scratch: None,
    },
    // Accounts + account_notes + account_team + account_next_actions
    // + account_risk_scores + support_cases. boss-accounts hosted
    // 6 routers under boss-people-api until 2026-06 when this
    // dedicated service split out. Mirrors the pattern every
    // other core domain follows.
    PortSpec {
        name: "accounts",
        prod: 7550,
        scratch: None,
    },
    // Dispatch service: subscribes to jobs.step.* NATS events and
    // auto-assigns ready Steps to role-matched Employees.
    // Health-only HTTP surface; the dispatcher's only outputs are
    // PUTs to jobs-api. Lives at port 7950.
    PortSpec {
        name: "dispatcher",
        prod: 7950,
        scratch: None,
    },
    // The gateway — the human door, and since backlog 240e03f3
    // (2026-09-19) a row on the LAN machine door too, so a recorded
    // probe can ask what it answers a caller with no session
    // (`boss-gateway-read`). 4443 is what boss-gateway listened on
    // from the start (BOSS_LISTEN's default, boss.yaml's
    // containerPort, the quickstart's "SPA: http://localhost:4443");
    // it lives here now so the manifest, sor-ports.env and the
    // gateway's own default are one fact
    // (the_machine_door_carries_every_read_surface.rs).
    PortSpec {
        name: "gateway",
        prod: 4443,
        scratch: None,
    },
];

/// All known services (paired + solo). Iteration order is
/// stable: paired first, then solo.
pub fn all() -> impl Iterator<Item = &'static PortSpec> {
    PAIRED.iter().chain(SOLO.iter())
}

/// Look up the prod port for `name`. Panics on unknown names —
/// every call site is a hardcoded service identifier so this is
/// a programmer error, not a runtime concern.
pub fn prod(name: &str) -> u16 {
    all()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("boss_ports::prod: unknown service '{name}'"))
        .prod
}

/// Look up the scratch port for `name`. Returns `None` for
/// solo services. Panics on unknown names.
pub fn scratch(name: &str) -> Option<u16> {
    all()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("boss_ports::scratch: unknown service '{name}'"))
        .scratch
}

/// Default localhost URL for a service in prod (e.g.
/// `http://127.0.0.1:7250` for `policy`). Consumers default
/// `BOSS_<NAME>_URL` to this; env-var override still wins.
pub fn url(name: &str) -> String {
    format!("http://127.0.0.1:{}", prod(name))
}

/// Default localhost URL for a service in scratch. Returns
/// `None` for solo services that have no scratch counterpart.
pub fn scratch_url(name: &str) -> Option<String> {
    scratch(name).map(|p| format!("http://127.0.0.1:{p}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list bin classifies by array membership and EXPECTS every
    /// PAIRED spec to carry a scratch port — a solo-shaped spec
    /// parked in PAIRED panics `boss-ports-list --paired` mid-stream
    /// and every consumer (generate-configs) sees a
    /// truncated table. Exactly this shipped once: campaigns
    /// (scratch: None) landed in PAIRED and docker's generated
    /// config rendered `http_bind = "0.0.0.0:"`, wedging the stack.
    #[test]
    fn paired_specs_carry_scratch_ports_solo_specs_do_not() {
        for s in PAIRED {
            assert!(
                s.scratch.is_some(),
                "{} is in PAIRED without a scratch port — move it to SOLO",
                s.name
            );
        }
        for s in SOLO {
            assert!(
                s.scratch.is_none(),
                "{} is in SOLO with a scratch port — move it to PAIRED",
                s.name
            );
        }
    }

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in all() {
            assert!(seen.insert(s.name), "duplicate service name: {}", s.name);
        }
    }

    #[test]
    fn ports_are_unique_across_prod_and_scratch() {
        let mut seen: std::collections::HashMap<u16, &'static str> =
            std::collections::HashMap::new();
        for s in all() {
            if let Some(prev) = seen.insert(s.prod, s.name) {
                panic!(
                    "duplicate prod port {} on services {} and {}",
                    s.prod, prev, s.name,
                );
            }
            if let Some(scr) = s.scratch
                && let Some(prev) = seen.insert(scr, s.name)
            {
                panic!(
                    "duplicate scratch port {} on services {} and {}",
                    scr, prev, s.name,
                );
            }
        }
    }

    #[test]
    fn lookups_resolve() {
        assert_eq!(prod("policy"), 7250);
        assert_eq!(prod("jobs"), 7900);
        assert_eq!(prod("gateway"), 4443);
        assert_eq!(scratch("jobs"), Some(8900));
        assert_eq!(scratch("policy"), None);
        assert_eq!(url("policy"), "http://127.0.0.1:7250");
        assert_eq!(
            scratch_url("commerce"),
            Some("http://127.0.0.1:8400".into())
        );
    }

    #[test]
    fn policy_pinned_to_7250() {
        // Pin policy to 7250 so a default-vs-config drift can't
        // silently re-introduce the 7060 collision this port moved
        // off of.
        assert_eq!(prod("policy"), 7250);
    }
}

#[cfg(test)]
mod launcher_roster_agreement {
    use super::{PAIRED, SOLO};

    /// The container launcher (`infra/oss-quickstart/services-launcher.sh`,
    /// the pod CMD on every cluster deploy) starts the binaries its
    /// `SERVICES` array names — a roster this registry does not
    /// generate. Until 2026-09-18 this module pinned the bare-metal
    /// deploy script's fallback arrays to the registry instead, while
    /// the LIVE roster was the unpinned one: views, search, ml and the
    /// simulator reached this table and never reached the launcher, so
    /// every container deploy 502'd on /system/os-map, /api/search/*,
    /// /api/ml/* and /simulator with the binaries sitting in the image
    /// (aab30bbf). The bare-metal path is deleted (backlog e109bd71,
    /// design 42277636); this is the same fact-lives-twice pin, aimed at
    /// the roster that runs.
    ///
    /// The binary for a registry name is `boss-<name>-api`, or
    /// `boss-<name>` for the three non-`-api` services. `sim-control` is
    /// the one legitimate absence: it is the sim daemon's
    /// embedded port, alive only while the daemon runs, not a binary.
    /// The launcher may carry binaries with no port row (the event
    /// relay, the sim daemon, the gateway) — only `boss-*-api` lines
    /// are checked in the reverse direction.
    const LAUNCHER: &str = "infra/oss-quickstart/services-launcher.sh";

    fn launcher_services() -> Vec<String> {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../../{LAUNCHER}"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let start = text.find("SERVICES=(").expect("SERVICES=( in the launcher");
        let rest = &text[start..];
        let end = rest.find("\n)").expect("SERVICES array terminator");
        rest[..end]
            .lines()
            .filter_map(|l| {
                let l = l.trim();
                l.strip_prefix('"')
                    .and_then(|l| l.strip_suffix('"'))
                    .map(str::to_string)
            })
            .collect()
    }

    #[test]
    fn every_port_registry_service_is_launched() {
        let launched = launcher_services();
        assert!(
            launched.len() >= 20,
            "scraped only {} SERVICES lines from {LAUNCHER}",
            launched.len()
        );
        let missing: Vec<String> = PAIRED
            .iter()
            .chain(SOLO.iter())
            .filter(|s| s.name != "sim-control")
            .filter(|s| {
                !launched.iter().any(|b| {
                    b == &format!("boss-{}-api", s.name) || b == &format!("boss-{}", s.name)
                })
            })
            .map(|s| s.name.to_string())
            .collect();
        assert!(
            missing.is_empty(),
            "in the port registry but not in {LAUNCHER}'s SERVICES (the binary would sit in the image and every request 502): {missing:?}"
        );
    }

    #[test]
    fn every_launched_api_binary_has_a_port_row() {
        let stray: Vec<String> = launcher_services()
            .into_iter()
            .filter(|b| b.starts_with("boss-") && b.ends_with("-api"))
            .filter(|b| {
                let name = &b["boss-".len()..b.len() - "-api".len()];
                !PAIRED.iter().chain(SOLO.iter()).any(|s| s.name == name)
            })
            .collect();
        assert!(
            stray.is_empty(),
            "{LAUNCHER} launches an -api binary with no row in the port registry (it would bind a port nothing routes to): {stray:?}"
        );
    }
}
