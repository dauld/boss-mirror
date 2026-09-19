//! The reads this instance answers WITHOUT a session — a per-instance
//! declaration, defaulting to none.
//!
//! WHY (design 11e60367 Q1, decided 2026-09-18; backlog b4afd7b9).
//! Four demo landing-page reads — `/api/workflows[/*]`,
//! `/api/jobs/summary`, `/api/jobs/live`, `/api/events/public-tail` —
//! were pinned to `proxy::handle_public` in the route table, a gateway
//! CONSTANT. Measured on prod 2026-09-17 (audit O3): all four answered
//! without a session, on the company's own instance, whose landing
//! page reads none of them; Cloudflare Access was the only thing in
//! front. The set belongs to the tenant, not the binary: the public
//! demo tenant declares the four its landing page needs in its
//! tenant.toml (`[gateway] public_reads`), the company's manifest says
//! nothing, and nothing is what it gets.
//!
//! WHERE THE DECLARATION LIVES — the tenant contract, not
//! infra/cluster/instances.toml. instances.toml renders the FIVE
//! deployment parameters (namespace, tenant source, sim, hostname,
//! guest) into env on the boss container and is read by the shell at
//! converge; the gateway never sees the file. tenant.toml the gateway
//! ALREADY reads at boot, through `BOSS_TENANT_MANIFEST_TOML` (derived
//! from `BOSS_TENANT_DIR` by the launcher) and one parser,
//! `boss_core::tenant_manifest::TenantToml` — so the declaration rides
//! in the tenant directory the image or the `boss-tenant` ConfigMap
//! already delivers, with no new plumbing and no second reader. And
//! it IS tenant data: which reads a stranger may make is a fact about
//! what the tenant chose to show, the way `[modules]` is.
//!
//! WHAT A DECLARATION CANNOT DO. [`PUBLISHABLE`] is the closed table
//! of reads that CAN be public — the four known handlers, GET only.
//! A declared path outside it is refused at boot with the path named
//! ([`PublicReads::resolve`]): the manifest chooses among the doors
//! the gateway offers; it cannot open a new one, and it cannot make a
//! write public. Undeclared, each of the four is routed through the
//! session-gated proxy exactly like every other `/api` route and
//! answers 401 to a sessionless caller.
//!
//! AND THE READS THAT ARE PUBLIC ON EVERY INSTANCE, BY DESIGN
//! ([`PUBLIC_BY_DESIGN`], backlog 240e03f3, 2026-09-19). The hardening
//! inventory found two `handle_public` routes left in the route table
//! after the four moved here. Each was decided rather than left:
//! the calendar feed stays public, with its reason as a row here, so
//! the inventory reads ONE module to audit every sessionless door;
//! the observability health alias was not by design — the page that
//! reads it holds a session, as it does for every other
//! `/api/<service>/health` — and now meets the gate like them
//! (main.rs, `the_observability_health_alias_refuses_a_sessionless_caller`).
//! A by-design row is never an `/api` path: an `/api` read that a
//! stranger may make is a tenant's choice, and belongs in
//! [`PUBLISHABLE`] behind a declaration.

use std::sync::Arc;

use crate::AppState;
use crate::proxy::{self, ProxyConfig};

/// One read the gateway CAN answer without a session.
pub struct PublicRead {
    /// The path as a declaration spells it.
    pub path: &'static str,
    /// The matchers the route table registers for it: the bare path
    /// and, for a family, its sub-path wildcard.
    pub matchers: &'static [&'static str],
    /// The upstream that serves it.
    pub upstream: &'static ProxyConfig,
}

/// The closed table. `/api/workflows` is a family — the landing page
/// reads the list AND `/api/workflows/{kind}` for the step graph — so
/// one declaration covers both matchers.
pub static PUBLISHABLE: &[PublicRead] = &[
    PublicRead {
        path: "/api/workflows",
        matchers: &["/api/workflows", "/api/workflows/{*rest}"],
        upstream: &proxy::JOBS,
    },
    PublicRead {
        path: "/api/jobs/summary",
        matchers: &["/api/jobs/summary"],
        upstream: &proxy::JOBS,
    },
    PublicRead {
        path: "/api/jobs/live",
        matchers: &["/api/jobs/live"],
        upstream: &proxy::JOBS,
    },
    PublicRead {
        path: "/api/events/public-tail",
        matchers: &["/api/events/public-tail"],
        upstream: &proxy::EVENTS,
    },
];

/// One read that is sessionless on EVERY instance, and why. No
/// declaration can close it, so the reason has to be one that holds
/// for every tenant — which is what `why` is for, and what the
/// hardening inventory reads.
pub struct PublicByDesign {
    /// The route-table matcher.
    pub matcher: &'static str,
    /// The upstream that serves it.
    pub upstream: &'static ProxyConfig,
    /// Why no session can be asked for here.
    pub why: &'static str,
}

/// The closed table of by-design public reads. GET only, like
/// [`PUBLISHABLE`]; never an `/api` path (pinned below).
pub static PUBLIC_BY_DESIGN: &[PublicByDesign] = &[PublicByDesign {
    matcher: "/ics/{*rest}",
    upstream: &proxy::JOBS,
    why: "the calendar feed: a calendar client subscribes by URL and cannot hold a \
          session cookie, so the 256-bit token in the path IS the credential, minted \
          per owner and validated by boss-jobs-api on every read; an undeclared \
          instance answering 401 here would silently empty every subscribed calendar",
}];

/// The resolved declaration: which of [`PUBLISHABLE`] this instance
/// answers without a session. Built once at boot; the route table
/// asks it per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicReads {
    declared: Vec<&'static str>,
}

impl PublicReads {
    /// No sessionless read at all — the default, and what a manifest
    /// with no `[gateway] public_reads` line resolves to.
    pub fn none() -> Self {
        Self {
            declared: Vec::new(),
        }
    }

    /// Resolve a manifest's `public_reads` against the table. Every
    /// declared path must be one of [`PUBLISHABLE`]'s; the first that
    /// is not is the error, named, with the paths that would have
    /// been accepted — a boot refusal has to say what to fix. A
    /// duplicate declaration is not an error; it declares the same
    /// door twice.
    pub fn resolve(declared: &[String]) -> Result<Self, String> {
        let mut out: Vec<&'static str> = Vec::new();
        for path in declared {
            match PUBLISHABLE.iter().find(|p| p.path == path.trim()) {
                Some(p) => {
                    if !out.contains(&p.path) {
                        out.push(p.path);
                    }
                }
                None => {
                    let allowed = PUBLISHABLE
                        .iter()
                        .map(|p| p.path)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "[gateway] public_reads names `{path}`, which is not a read the gateway \
                         can answer without a session; the declarable reads are: {allowed}. A \
                         declaration chooses among these; it cannot open a new door or make a \
                         write public."
                    ));
                }
            }
        }
        Ok(Self { declared: out })
    }

    /// Is this [`PUBLISHABLE`] entry declared public here?
    pub fn is_public(&self, read: &PublicRead) -> bool {
        self.declared.contains(&read.path)
    }

    /// The declared paths, in table order — for the boot log line.
    pub fn paths(&self) -> Vec<&'static str> {
        PUBLISHABLE
            .iter()
            .filter(|p| self.is_public(p))
            .map(|p| p.path)
            .collect()
    }
}

/// Register every sessionless read in the route table. The by-design
/// rows first, GET only, on every instance. Then the four, each by
/// what this instance declared: declared → GET through the sessionless
/// proxy, every other method through the gated one (the same
/// MethodRouter, or a POST to `/api/workflows` would answer 405 — the
/// strict matcher shadows `/api/jobs/{*rest}`); undeclared → every
/// method gated, exactly like the routes around it. Both branches
/// register the same matchers, so the route table's SHAPE does not
/// depend on the declaration — only which proxy answers a GET.
pub fn mount(app: axum::Router<Arc<AppState>>, reads: &PublicReads) -> axum::Router<Arc<AppState>> {
    let app = PUBLIC_BY_DESIGN.iter().fold(app, |app, read| {
        let upstream = read.upstream;
        app.route(
            read.matcher,
            axum::routing::get(move |s, r| proxy::handle_public(s, r, upstream)),
        )
    });
    PUBLISHABLE.iter().fold(app, |app, read| {
        let upstream = read.upstream;
        let public = reads.is_public(read);
        read.matchers.iter().fold(app, |app, matcher| {
            if public {
                app.route(
                    matcher,
                    axum::routing::get(move |s, r| proxy::handle_public(s, r, upstream))
                        .post(move |s, r| proxy::handle(s, r, upstream))
                        .put(move |s, r| proxy::handle(s, r, upstream))
                        .delete(move |s, r| proxy::handle(s, r, upstream)),
                )
            } else {
                app.route(
                    matcher,
                    axum::routing::any(move |s, r| proxy::handle(s, r, upstream)),
                )
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    /// The default when the key is absent: NONE. A company's instance
    /// whose manifest never mentions public reads answers every /api
    /// read with 401 to a sessionless caller.
    #[test]
    fn an_empty_declaration_is_no_public_route() {
        let r = PublicReads::resolve(&[]).unwrap();
        assert_eq!(r, PublicReads::none());
        assert!(r.paths().is_empty());
        for p in PUBLISHABLE {
            assert!(!r.is_public(p), "{} must not be public by default", p.path);
        }
    }

    /// The demo tenant's four — exactly those, no more.
    #[test]
    fn the_demo_tenants_four_resolve_to_exactly_those() {
        let r = PublicReads::resolve(&strings(&[
            "/api/workflows",
            "/api/jobs/summary",
            "/api/jobs/live",
            "/api/events/public-tail",
        ]))
        .unwrap();
        assert_eq!(
            r.paths(),
            vec![
                "/api/workflows",
                "/api/jobs/summary",
                "/api/jobs/live",
                "/api/events/public-tail"
            ]
        );
        for p in PUBLISHABLE {
            assert!(r.is_public(p), "{} was declared", p.path);
        }
    }

    /// A subset is a subset: declaring one leaves the other three
    /// gated.
    #[test]
    fn a_partial_declaration_leaves_the_rest_gated() {
        let r = PublicReads::resolve(&strings(&["/api/jobs/live"])).unwrap();
        assert_eq!(r.paths(), vec!["/api/jobs/live"]);
        let workflows = PUBLISHABLE
            .iter()
            .find(|p| p.path == "/api/workflows")
            .unwrap();
        assert!(!r.is_public(workflows));
    }

    /// A declaration cannot open a door the table does not offer — a
    /// write, a sub-path spelled on its own, a read that carries
    /// per-actor cost. The refusal names the offending path and the
    /// paths that would have been accepted, because it is what an
    /// operator reads in the boot log.
    #[test]
    fn a_path_outside_the_table_is_refused_by_name() {
        for bad in [
            "/api/jobs",
            "/api/agent-runs",
            "/api/workflows/some-kind",
            "/api/snapshot",
            "/api/auth/login",
        ] {
            let err = PublicReads::resolve(&strings(&["/api/jobs/live", bad]))
                .expect_err("an unknown path must refuse");
            assert!(err.contains(bad), "the refusal must name `{bad}`: {err}");
            assert!(
                err.contains("/api/events/public-tail"),
                "the refusal must list what is declarable: {err}"
            );
        }
    }

    /// A BY-DESIGN ROW SAYS WHY, AND IS NEVER AN /api PATH. The reason
    /// is what the hardening inventory reads; an `/api` read a stranger
    /// may make is a tenant's choice and belongs in [`PUBLISHABLE`]
    /// behind a declaration, not here where no declaration can close
    /// it. And the two tables cannot overlap: a matcher registered
    /// twice panics axum at boot, which is the wrong place to learn it.
    #[test]
    fn a_by_design_row_carries_its_reason_and_is_never_an_api_path() {
        assert!(
            !PUBLIC_BY_DESIGN.is_empty(),
            "the by-design table is empty — if the calendar feed moved, the rule moved with it"
        );
        for row in PUBLIC_BY_DESIGN {
            assert!(
                row.why.len() > 40,
                "{} is public by design without a reason an inventory can read",
                row.matcher
            );
            assert!(
                !row.matcher.starts_with("/api/") && row.matcher != "/api",
                "{} is an /api read public on every instance — that is a tenant's \
                 declaration (PUBLISHABLE), not a design",
                row.matcher
            );
            for p in PUBLISHABLE {
                assert!(
                    !p.matchers.contains(&row.matcher),
                    "{} is in both tables — one matcher, one row",
                    row.matcher
                );
            }
        }
    }

    /// Declaring the same read twice is one door, not an error and
    /// not two routes (axum panics on a duplicate matcher).
    #[test]
    fn a_duplicate_declaration_is_one_door() {
        let r = PublicReads::resolve(&strings(&["/api/jobs/live", "/api/jobs/live"])).unwrap();
        assert_eq!(r.paths(), vec!["/api/jobs/live"]);
    }
}
