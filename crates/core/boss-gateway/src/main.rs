use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::State;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[cfg(test)]
mod a_path_cannot_climb_out_of_its_route;
#[cfg(test)]
mod a_read_only_session_cannot_write;
mod api;
mod dot_segments;
mod inquiries;
mod plugin_files;
mod proxy;
mod public_reads;
mod role_headers;
mod site;
mod sponsors;
mod static_files;
mod visits;

// perf + timing live in the library so a test can drive the real
// request timer and read what it logs (backlog d9f64c4a).
use boss_gateway::{perf, timing};
use perf::PerfCollector;

use boss_gateway::local_auth::{self, CredentialStore, GuestAccess, LocalAuthState};

/// Auth provider — picks which middleware mints the
/// `boss_session` cookie.
///
/// - `local-auth` (default): file-backed email/password.
///   Login routes are mounted under `/api/auth/*`.
/// - `none`: bypass — no auth provider mounted. Test only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthProvider {
    LocalAuth,
    None,
}

impl AuthProvider {
    fn from_env() -> Self {
        match std::env::var("BOSS_AUTH_PROVIDER").as_deref() {
            Ok("none") => Self::None,
            Ok("local-auth") | Ok("") | Err(_) => Self::LocalAuth,
            Ok(other) => {
                tracing::warn!(provider = %other, "unknown BOSS_AUTH_PROVIDER; defaulting to local-auth");
                Self::LocalAuth
            }
        }
    }
}

pub(crate) struct AppState {
    pub session_key: Vec<u8>,
    pub proxy_client: reqwest::Client,
    pub perf: Arc<PerfCollector>,
    /// The estate machine token the gateway stamps on what it forwards —
    /// the mounted `current` slot every service's gate accepts from,
    /// re-read in the background (design 6805c764, car 2).
    pub machine_token: Arc<boss_core::machine_token::Source>,
}

/// Auth-event staging (docs/architecture-decisions.md §Policy &
/// auth). The URL is its
/// own variable — not BOSS_POSTGRES_URL — because it is expected to
/// carry the INSERT-only `boss_gateway_audit` role
/// (111-gateway-audit-events.sql), not the service superuser. Absent
/// → the disabled emitter, whose record is the structured warn line.
/// `connect_lazy` on purpose: the edge must come up whether or not
/// the database is reachable, and a failed INSERT already degrades
/// to the warn backstop.
fn build_auth_audit() -> boss_gateway::audit::AuthAudit {
    match std::env::var("BOSS_GATEWAY_AUDIT_DB_URL") {
        Err(_) => {
            tracing::info!("BOSS_GATEWAY_AUDIT_DB_URL unset — auth events degrade to warn lines");
            boss_gateway::audit::AuthAudit::disabled()
        }
        Ok(url) => match sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_lazy(&url)
        {
            Ok(pool) => {
                // The drain task stamps each event with wall time —
                // sim time is retired from the record (David,
                // 2026-08-22, packet a7a4cae5); an auth decision is
                // real-world activity in any clock mode.
                boss_gateway::audit::AuthAudit::spawn(Arc::new(
                    boss_events::outbox::PgOutboxRecorder::new(pool),
                ))
            }
            Err(e) => {
                tracing::warn!(error = %e, "audit DB URL unusable — auth events degrade to warn lines");
                boss_gateway::audit::AuthAudit::disabled()
            }
        },
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    // The port is boss-ports' `gateway` row, the same fact the LAN
    // machine door and the probe reader's table carry (backlog
    // 240e03f3); loopback stays the default, the manifest widens it.
    let listen = std::env::var("BOSS_LISTEN")
        .unwrap_or_else(|_| format!("127.0.0.1:{}", boss_ports::prod("gateway")));
    // Resolved by boss-core because the jobs API reads the same file to
    // verify presence tickets (backlog 72fe3640): one path rule, one
    // format, for the minter and the verifier.
    let session_key_path = boss_core::presence::session_key_path();
    let session_key = load_or_create_session_key(&session_key_path)
        .with_context(|| format!("loading session key from {}", session_key_path.display()))?;

    // Seed the executive-role cache from the Class registry so the
    // gateway's admin-ish gates recognise tenant-defined executives
    // via `has_global_read`. URL is the public proxy lookup since
    // the gateway is in front of itself. Skip on missing config or
    // transport failure — platform-admin + audit-readonly still
    // grant global read.
    let classes_url =
        std::env::var("BOSS_CLASSES_URL").unwrap_or_else(|_| boss_ports::url("classes"));
    let classes_client = boss_classes_client::ReqwestClassesClient::new(classes_url.clone());
    match boss_classes_client::seed_executive_role_cache(&classes_client).await {
        Ok(n) => {
            tracing::info!(count = n, classes_url = %classes_url, "executive role cache seeded")
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to seed executive roles from classes; gateway admin gates will skip executive checks")
        }
    }

    let proxy_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("building reverse-proxy http client")?;

    // Auth provider — local-auth (file-backed credentials)
    // is the v1 OSS default.
    let auth_provider = AuthProvider::from_env();
    tracing::info!(provider = ?auth_provider, "auth provider selected");

    let local_auth_state = if auth_provider == AuthProvider::LocalAuth {
        let auth_file = std::env::var("BOSS_AUTH_FILE")
            .unwrap_or_else(|_| "/var/lib/boss/auth/credentials.toml".into());
        let store = CredentialStore::load(&auth_file)
            .with_context(|| format!("loading credentials from {auth_file}"))?;
        tracing::info!(
            path = %auth_file,
            users = store.list_emails().len(),
            "local-auth credential store loaded"
        );
        Some(Arc::new(LocalAuthState {
            store,
            session_key: session_key.clone(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .context("local-auth http client")?,
            audit: build_auth_audit(),
            // One flag, one question: does this deployment hand out
            // a read-only session to anyone who asks?
            //
            // This rode on BOSS_DEMO_MODE, on the reasoning that a
            // demo is exactly where guest browsing belongs. That was
            // wrong in the way shared flags usually are — the same
            // variable also decided whether the simulator ran, so
            // turning off synthetic activity silently withdrew guest
            // access, and neither effect was visible from the name.
            //
            // Since design 2830b6b7 (2026-09-25) the one question has
            // three answers — no guest, a basic `visitor`, or the
            // system-audit read — and "1" means basic.
            guest_access: GuestAccess::from_env_or_off(
                std::env::var("BOSS_GUEST_ACCESS").ok().as_deref(),
            ),
            mail: boss_gateway::mail::from_env(),
            // The IdP front door (idm-kanidm.md). Absent config →
            // None → the oidc routes answer that they are off, the
            // same honesty pattern as the mail transport above.
            oidc: boss_gateway::oidc::OidcConfig::from_env()
                .map(boss_gateway::oidc::OidcRuntime::new),
            // Origin the reset link points at. Falls back to the
            // loopback listener, which is right for a laptop and
            // obviously wrong in a deploy — a link nobody outside the
            // box can open is easier to notice than a plausible one
            // pointing at the wrong host.
            public_url: std::env::var("BOSS_PUBLIC_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:4443".to_string()),
            forgot_seen: Default::default(),
        }))
    } else {
        None
    };

    let state = Arc::new(AppState {
        session_key,
        proxy_client,
        perf: Arc::new(PerfCollector::new()),
        machine_token: boss_core::machine_token::Source::watch(
            boss_core::machine_token::token_dir(),
        ),
    });

    // The sessionless read set — the tenant's declaration, resolved
    // once, refused by name if it names a door the gateway does not
    // offer (public_reads.rs). Absent → none. A manifest that exists
    // but does not parse is refused here too, naming the file and
    // toml's line (api.rs `load_tenant_toml`, backlog 4f1ba1f9): the
    // configuration the router is built from, not a boot check.
    let manifest = api::load_tenant_toml()
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("reading the tenant manifest")?;
    // How many modules are on, said once at boot (design 1054c099;
    // backlog fa77e3d7): prod ran with `modules = {}` — every
    // module-gated surface off — and no line anywhere said so. Zero is
    // legitimate, so it warns rather than refuses.
    match api::modules_boot_line(manifest.as_ref()) {
        (0, line) => tracing::warn!("{line}"),
        (_, line) => tracing::info!("{line}"),
    }
    let declared = manifest.map(|t| t.gateway.public_reads).unwrap_or_default();
    let public_reads = public_reads::PublicReads::resolve(&declared)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("resolving [gateway] public_reads from the tenant manifest")?;
    tracing::info!(public_reads = ?public_reads.paths(), "sessionless reads declared by the tenant");

    let app = build_router(local_auth_state.clone(), &public_reads);

    let app = app
        .layer(axum::middleware::from_fn_with_state(
            state.perf.clone(),
            timing::request_timer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            role_headers::inject_role_headers,
        ))
        .with_state(state);

    // The tenant site (site.rs), OUTERMOST: a request whose Host names
    // the instance's site hostname is answered from the site directory
    // before the session middleware above ever sees it. No site
    // configured → the router unchanged.
    // The roll (sponsors.rs) rides every site: its one computed
    // document, read from the jobs upstream as the gateway itself.
    // So does the page-view recorder (visits.rs): one www-visits
    // reading per HTML page served, drained to the jobs upstream in
    // batches by its own task — on with the site, off without one.
    let visits_sensor = visits::Recorder::sensor_from_env();
    let site = site::Site::from_env().map(|s| {
        s.with_sponsors(sponsors::SponsorRoll::new(Arc::new(
            sponsors::JobsApi::from_env(),
        )))
        .with_visits(visits::Recorder::spawn(
            Arc::new(visits::JobsApi::from_env()),
            visits_sensor.clone(),
        ))
    });
    match &site {
        Some(s) => tracing::info!(
            site_host = %s.host(),
            visits_sensor = %visits_sensor,
            "tenant site mounted; page views recorded"
        ),
        None => tracing::info!("no tenant site (BOSS_SITE_HOST / BOSS_SITE_DIR unset)"),
    }
    let site_host = site.as_ref().map(|s| s.host().to_string());
    let app = site::mount(app, site);

    // The site's one write (inquiries.rs), outside the read-only site
    // layer: `POST /site/inquiries` on the site host opens a
    // receive-an-inquiry packet through the accounts and jobs doors.
    // No site → no door.
    let door = site_host
        .as_deref()
        .map(|host| inquiries::Door::new(host, Arc::new(inquiries::Services::from_env())));
    let app = inquiries::mount(app, door);

    // The door every request enters, OUTERMOST (dot_segments.rs,
    // backlog 1d9b7db7): a path with a `.` or `..` segment, raw or
    // percent-encoded, is answered 400 before the site, the inquiry
    // door or any route sees it — reqwest would otherwise resolve it
    // after routing, and the upstream would receive a path no route
    // matched. Last, so nothing added above can run ahead of it.
    let app = dot_segments::mount(app);

    tracing::info!(listen = %listen, static_dir = %static_files::static_dir(), "boss-gateway starting");
    let listener = TcpListener::bind(&listen).await?;
    // With the peer address on every request: the inquiry door's
    // rate limit falls back to it when the tunnel sends no
    // `CF-Connecting-IP`.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Build the gateway route table.
///
/// Extracted from `main` so the routing itself can be tested. It
/// could not be before: the table was 400 lines inline in an async
/// `main` that also reads env, builds a session key, and binds a
/// socket, so the only way to ask "what does this path resolve to"
/// was to run the binary and curl it. Route precedence is exactly
/// the kind of claim that needs asserting rather than eyeballing —
/// a catch-all that quietly shadowed a real service would look
/// fine in review and 404 a working endpoint in production.
///
/// Middleware layers and `.with_state` stay in `main`: they need the
/// live `AppState`, and tests supply their own.
fn build_router(
    local_auth_state: Option<Arc<LocalAuthState>>,
    public_reads: &public_reads::PublicReads,
) -> axum::Router<Arc<AppState>> {
    // `/health` is NOT here: the gateway's own liveness answer is a
    // sessionless read like any other, so it is a row in
    // `public_reads::PUBLIC_BY_DESIGN` with its reason, registered by
    // `public_reads::mount` below (backlog bf1f5ad2, 2026-09-19). It
    // sat here as a route returning a constant, which made the set of
    // sessionless answers "that module, plus this line".
    let app = axum::Router::new()
        .route("/api/session", axum::routing::get(api::session))
        .route(
            "/api/tenant/manifest",
            axum::routing::get(api::tenant_manifest),
        )
        .route(
            "/api/finance/revenue-categories",
            axum::routing::get(api::revenue_categories),
        )
        .route("/api/gateway/perf", axum::routing::get(handle_perf))
        .route(
            "/api/gateway/perf/reset",
            axum::routing::post(handle_perf_reset),
        )
        // Dashboard: auth-gated SPA served from static files.
        .route("/dashboard", axum::routing::get(static_files::handle))
        .route("/dashboard/", axum::routing::get(static_files::handle))
        .route(
            "/dashboard/{*rest}",
            axum::routing::get(static_files::handle),
        )
        // Domain-service reverse proxies, all cookie-gated. Each entry
        // pairs a path prefix with a ProxyConfig in proxy.rs (which holds
        // the default upstream URL + any BOSS_<NAME>_UPSTREAM override).
        // Bare `/api/assets` (the SPA lists devices via
        // `/api/assets?account_id=…`, no sub-path) AND `/api/assets/{*rest}`
        // — same dual registration as /api/jobs + /api/people/accounts.
        // Without the bare route, `/api/assets?…` misses the proxy and
        // falls through to the SPA static handler (HTML, not JSON).
        // The estate registry + observation series (8a622ab7: the
        // /it/estate page 404'd on BOTH its sections — these endpoints
        // live on the jobs upstream and the gateway never grew the
        // prefix, so the fetches fell through to the SPA static handler).
        // Bare + sub-path, per the /api/assets rationale below.
        .route(
            "/api/estate",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/estate/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/assets",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ASSETS)),
        )
        .route(
            "/api/assets/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ASSETS)),
        )
        // Global search. BOTH the bare path and the sub-path, for the
        // reason spelled out on /api/assets above: the endpoint is
        // `/api/search?q=…` with no sub-path, and without the bare
        // route it misses the proxy and falls through to the SPA
        // static handler — which answers HTML, so the dropdown would
        // fail parsing JSON rather than 404.
        .route(
            "/api/search",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SEARCH)),
        )
        .route(
            "/api/search/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SEARCH)),
        )
        // Views. Both paths, same reason as /api/search above: the
        // list + create endpoint is the bare `/api/views`, so without
        // the bare route it falls through to the SPA static handler
        // and answers HTML to a fetch expecting JSON.
        .route(
            "/api/views",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::VIEWS)),
        )
        .route(
            "/api/views/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::VIEWS)),
        )
        // Dispatcher rule-registry surface (read-only) — backs the
        // /system/dispatcher cascade visualization.
        .route(
            "/api/dispatcher/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::DISPATCHER)),
        )
        // `/api/workflows[/*]` and `/api/jobs/live`
        // are registered by `public_reads::mount` below — the reads
        // an instance MAY answer without a session, each routed by
        // the tenant's `[gateway] public_reads` declaration (design
        // 11e60367 Q1, backlog b4afd7b9). They were pinned to
        // `handle_public` here as demo landing-page reads, which
        // made every instance's — including the company's — public.
        // Strict matchers, so they win over `/api/jobs/{*rest}`
        // either way.
        .route(
            "/api/jobs",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/jobs/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // Stations — the network's nodes (stations.md). Registry rows
        // and their evaluated queues live on the jobs upstream beside
        // workflows, so they proxy there. Auth-gated like `/api/jobs`:
        // the handlers apply the same job-read policy scope inside, so
        // a guest session reads the yard's dock through here. Without
        // these two lines the endpoints exist on the service and 404 at
        // the human door — which is how they shipped in train #10, with
        // the yard silently falling back to its derived dock.
        .route(
            "/api/stations",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/stations/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The departments registry — the `departments` table and each
        // department's readiness, served on the jobs upstream beside
        // stations, so they proxy there. The chrome bar reads the bare
        // list on every page to build its tabs (backlog dc5788ba): it
        // used to derive them from `/api/classes`, so the endpoint
        // existed on the service and had never needed a door. Both
        // matchers for the stations reason — `{*rest}` needs a
        // segment, so without the bare route the list falls through to
        // the SPA fallback and the client parses index.html as JSON.
        .route(
            "/api/departments",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/departments/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The yard's read-model — gate slots + capacity, the garage,
        // the boarding summary — computed on the jobs upstream, so it
        // proxies there beside stations. The Approach renders its
        // slots and garage ONLY from this response; a 404 here leaves
        // both `{#if}` sections unrendered while the client-derived
        // approach rows still paint, so the page looks shipped and
        // silently isn't. Exactly the stations failure above, one
        // endpoint later — which is why both live in the route test.
        .route(
            "/api/yard/status",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The yard's REGIONS — the region-card system map at /it (design
        // 0524fc95 car 2, train #475). Shipped on the jobs upstream and
        // fetched by the landing page, unrouted here: the fourth
        // instance of the stations shape, and the one David found on
        // his own screen (the /it landing rendered `HTTP 404`,
        // 2026-09-19). Since then `every_api_path_the_web_fetches_is_routed`
        // reads the bundle's fetches and refuses a fifth at the gate.
        .route(
            "/api/yard/regions",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The map's RAILS (design d2154293 car 2): what crosses each
        // border, what waits at it and which machine moves it. Same
        // upstream, same page, and routed here at the same time as the
        // fetch that reads it — the shape above is what happens when
        // those two are not one change.
        .route(
            "/api/yard/borders",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The ROUTES the Department Map draws (design e765b3fc, car R3):
        // the served sections, exits and entries. Car R3 landed its fetch
        // in train #718 without this route, so /it's routes read would
        // have 404'd at the human door — `every_api_path_the_web_fetches
        // _is_routed` red on main — and car M2, which rides these routes,
        // routes it with its own reads.
        .route(
            "/api/yard/routes",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The MOVES record (design e765b3fc): each packet that moved on
        // the map, and the stream the map's live layer holds open (car
        // M2, flight `it-map-live`). Served by the jobs upstream since
        // car M1, and routed in the car that first fetches it — the
        // stations shape again otherwise. The proxy streams the body,
        // as it does `/api/events/stream`.
        .route(
            "/api/yard/moves",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/yard/moves/stream",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // Every dispatcher rule's newest firing and dead-letters, read by
        // the rules list at /it/registry/rules (backlog 43c4451a). Same
        // upstream as the borders, whose record it re-reads, and routed
        // in the car that adds the fetch.
        .route(
            "/api/yard/rule-firings",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The flights read (design c4c2a607, backlog 73c31776): the
        // codes on for THIS session, which the jobs upstream resolves
        // from the signed `x-boss-user` the role-header layer sets. The
        // SPA reads it inlined on index.html (static_files.rs); this
        // route is the same answer for a fetch, routed in the car that
        // adds the read so it never ships unreachable at the door.
        .route(
            "/api/flights/mine",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The agent-run record — which actor built what, and what it
        // cost. `GET /api/agent-runs[?actor_id=&branch=&since=]` lists
        // the rows and `/cost` rolls them up; both live on the jobs
        // upstream beside the yard. GET only, and deliberately: the
        // POST that files a run names its own actor and its callers
        // (coding agents, the CLI) reach boss-jobs-api directly, so a
        // write door for no browser consumer is surface for nothing. A
        // POST through here gets a 405 from this MethodRouter, not the
        // catch-all's 404.
        //
        // Same failure as stations (#10) and /api/yard/status (#192),
        // third time: the surface shipped in train #294 and 404'd at
        // the human door, so the Crew Board — whose whole purpose is
        // showing actors working — shipped with no cost-per-actor read
        // at all (backlog 48bb0200). `/api/agent-rate-card` is the one
        // route on this service deliberately left unrouted: it answers
        // what a model costs per MTok, not what an actor spent, and
        // nothing reads it from a browser yet.
        .route(
            "/api/agent-runs",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/agent-runs/{*rest}",
            axum::routing::get(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // Surface opens (backlog 628f182b): the SPA posts each route
        // open here and the Codebase page reads the roll-up. The write
        // is credited to the SESSION — this proxy strips any
        // `x-boss-user` the client sent and signs the cookie's, which is
        // the whole reason the record is trustworthy. Session-gated
        // like /api/jobs; the sweep under `{*rest}` is refused upstream
        // for a user-tier session. Bare + sub-path, per /api/assets.
        .route(
            "/api/surface-opens",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/surface-opens/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // Scheduling routes live alongside jobs on the same upstream.
        // Auth-gated like the rest of /api/*.
        .route(
            "/api/scheduling/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // The calendar feed, /ics/{token}.ics, is registered by
        // `public_reads::mount` below as a PUBLIC_BY_DESIGN row, with
        // its reason beside it (backlog 240e03f3): every sessionless
        // read is a row in that module, none is pinned here.
        .route(
            "/api/catalog/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CATALOG)),
        )
        // Six /api/people/* route families served by accounts-api, not
        // people-api. These more-specific routes MUST come before the
        // /api/people/{*rest} catch-all so axum's longest-prefix
        // match routes them to the right upstream.
        .route(
            "/api/people/accounts",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/accounts/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/account-notes/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/account-account-team/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/support-cases",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/support-cases/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people/my-day/actions",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ACCOUNTS)),
        )
        .route(
            "/api/people",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::PEOPLE)),
        )
        .route(
            "/api/people/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::PEOPLE)),
        )
        // `/api/events/public-tail` — the curated companion to
        // /api/events/tail — is registered by `public_reads::mount`
        // below, sessionless only where the tenant declares it.
        .route(
            "/api/events/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::EVENTS)),
        )
        .route(
            "/api/commerce/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::COMMERCE)),
        )
        .route(
            "/api/content/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CONTENT)),
        )
        // File references (docs/architecture-decisions.md §Content,
        // files, knowledge). Lives on
        // boss-content-api alongside bulletins/manual; gateway routes
        // /api/files/* through the same upstream so the SPA's
        // <FileAttachments /> component just hits /api/files without
        // knowing where it terminates.
        .route(
            "/api/files",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CONTENT)),
        )
        .route(
            "/api/files/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CONTENT)),
        )
        .route(
            "/api/inventory/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::INVENTORY)),
        )
        .route(
            "/api/messages/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::MESSAGES)),
        )
        .route(
            "/api/shipping/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SHIPPING)),
        )
        // No /api/sim route: the sim runs in-process in the
        // boss-brewery-sim daemon, not behind an HTTP surface.
        .route(
            "/api/ml/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::ML)),
        )
        .route(
            "/api/ledger/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::LEDGER)),
        )
        // IT-panel provider status — lives in the ledger binary
        // because 3 of its 4 last-sync data sources are ledger-owned
        // tables. Same upstream, different path prefix.
        .route(
            "/api/it/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::LEDGER)),
        )
        .route(
            "/api/policy/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::POLICY)),
        )
        // Read-only registry services — Class taxonomies (per
        // subject_kind), Location entities, and SubjectKind rows.
        // The auth surface around them is unchanged: same
        // boss_session cookie gate as the rest of /api/*.
        //
        // Each needs BOTH a bare matcher (the list endpoint:
        // `GET /api/classes?subject_kind=…`, `GET /api/subject-kinds`,
        // `GET /api/locations`) AND a wildcard for per-row detail.
        // axum's `{*rest}` requires at least one segment, so without
        // the bare route the list call falls through to the SPA
        // fallback and the client parses index.html as JSON. Same
        // shape as /api/products + /api/people + /api/jobs.
        .route(
            "/api/classes",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CLASSES)),
        )
        .route(
            "/api/classes/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CLASSES)),
        )
        .route(
            "/api/locations",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::LOCATIONS)),
        )
        .route(
            "/api/locations/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::LOCATIONS)),
        )
        .route(
            "/api/subject-kinds",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SUBJECT_KINDS)),
        )
        .route(
            "/api/subject-kinds/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::SUBJECT_KINDS)),
        )
        // Two matchers: bare for the list endpoint (GET /api/products)
        // + wildcard for per-sku detail and on-hand/by-location. axum's
        // {*rest} requires at least one segment, so the bare path
        // needs its own route or the list endpoint 404s. Same shape
        // as /api/people + /api/jobs.
        .route(
            "/api/products",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::PRODUCTS)),
        )
        .route(
            "/api/products/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::PRODUCTS)),
        )
        .route(
            "/api/campaigns",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CAMPAIGNS)),
        )
        .route(
            "/api/campaigns/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CAMPAIGNS)),
        )
        .route(
            "/api/customers",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CUSTOMERS)),
        )
        .route(
            "/api/customers/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CUSTOMERS)),
        )
        .route(
            "/api/calendar/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::CALENDAR)),
        )
        // /api/snapshot, /api/snapshot/* and /api/observability/health
        // proxied to boss-observability until 2026-09-23, when it
        // retired as superseded-by (backlog 467175e7, car B); each is
        // now a catch-all miss, pinned by
        // `the_retired_observability_routes_are_misses` below.
        // Simulator UX — boss-simulator hosts both the /simulator SPA
        // bundle and its /simulator/api/* control+status surface. The
        // whole prefix is proxied (not stripped); the service nests its
        // sub-app under /simulator. Cookie-gated like the dashboard so the
        // demo session + persona flow apply (the service's own operator
        // gate refuses control writes for audit-readonly). These specific
        // routes win over the /{*rest} SPA fallback below.
        //
        // All THREE spellings are needed. `{*rest}` needs at least one
        // segment, so `/simulator/` — what a browser sends for a
        // trailing-slash link, a bookmark, or the SPA's own base URL —
        // matched neither of the other two and fell through to the root
        // SPA route, which answered a /simulator URL with the MAIN
        // app's index.html. Same missing-bare-matcher shape as the
        // `/api` catch-all below, and the reason this one is pinned by
        // a test (`the_simulator_prefix_never_resolves_to_the_main_spa`).
        .route(
            "/simulator",
            axum::routing::any(|s, r| proxy::handle_app(s, r, &proxy::SIMULATOR)),
        )
        .route(
            "/simulator/",
            axum::routing::any(|s, r| proxy::handle_app(s, r, &proxy::SIMULATOR)),
        )
        .route(
            "/simulator/{*rest}",
            axum::routing::any(|s, r| proxy::handle_app(s, r, &proxy::SIMULATOR)),
        )
        // Step UX plugin bundles — served from the plugins dir on
        // disk. See docs/architecture-decisions.md §Step UX & frontend.
        .route("/plugins/{*rest}", axum::routing::get(plugin_files::handle))
        // Catch-all for /api misses. Must sit above the SPA fallback
        // conceptually; matchit resolves by specificity rather than
        // registration order, so every service route declared above
        // still wins and only genuine misses arrive here.
        .route("/api", axum::routing::any(api_not_found))
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        // Root-level: SPA static files. Auth-gated like /dashboard.
        .route("/", axum::routing::get(static_files::handle))
        .route("/{*rest}", axum::routing::get(static_files::handle));

    // The declarable sessionless reads, by this instance's manifest.
    let app = public_reads::mount(app, public_reads);

    // Local-auth routes — only mounted when BOSS_AUTH_PROVIDER=
    // local-auth. These handlers carry their own state (the
    // CredentialStore + the session_key + an http client for
    // bootstrap_email lookups against boss-people-api).
    if let Some(la) = local_auth_state {
        // Passkey ceremony (docs/design/presence.md, packet 7218c3f1):
        // best-effort mount — a malformed BOSS_PUBLIC_URL must degrade
        // to "no passkey routes", never crash the front door. The route
        // list is `passkey_router`'s, the one the tests drive; it was
        // spelled out here as well until backlog 3bddce66 (2026-09-23).
        let app = match boss_gateway::passkey::PasskeyState::from_env(la.session_key.clone()) {
            Ok(pk) => app.merge(boss_gateway::passkey::passkey_router(std::sync::Arc::new(
                pk,
            ))),
            Err(e) => {
                tracing::warn!(error = %e, "passkey ceremony not mounted");
                app
            }
        };
        // Break-glass ceremony (docs/design/break-glass-is-a-key-you-
        // hold.md): the gateway as its own WebAuthn verifier. Best-
        // effort mount for the same reason as the presence passkey —
        // a malformed BOSS_PUBLIC_URL must degrade to "no break-glass
        // routes", never crash the front door. The credentials.toml
        // path above stays untouched during the soak (Q6).
        let app = match boss_gateway::break_glass::BreakGlassState::from_env(
            la.session_key.clone(),
            la.audit.clone(),
        ) {
            Ok(bg) => {
                let bg = std::sync::Arc::new(bg);
                app.route(
                    "/break-glass",
                    axum::routing::get(boss_gateway::break_glass::ceremony_page),
                )
                .route(
                    "/api/auth/break-glass/enroll/begin",
                    axum::routing::post(boss_gateway::break_glass::enroll_begin)
                        .with_state(bg.clone()),
                )
                .route(
                    "/api/auth/break-glass/enroll/finish",
                    axum::routing::post(boss_gateway::break_glass::enroll_finish)
                        .with_state(bg.clone()),
                )
                .route(
                    "/api/auth/break-glass/assert/begin",
                    axum::routing::post(boss_gateway::break_glass::assert_begin)
                        .with_state(bg.clone()),
                )
                .route(
                    "/api/auth/break-glass/assert/finish",
                    axum::routing::post(boss_gateway::break_glass::assert_finish).with_state(bg),
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, "break-glass ceremony not mounted");
                app
            }
        };
        app.route(
            "/api/auth/login",
            axum::routing::post(local_auth::login).with_state(la.clone()),
        )
        .route("/api/auth/logout", axum::routing::post(local_auth::logout))
        .route(
            "/api/auth/me",
            axum::routing::get(local_auth::me).with_state(la.clone()),
        )
        .route(
            "/api/auth/guest",
            axum::routing::get(local_auth::guest_available)
                .post(local_auth::guest)
                .with_state(la.clone()),
        )
        .route(
            "/api/auth/onboard",
            axum::routing::post(local_auth::onboard).with_state(la.clone()),
        )
        .route(
            "/api/auth/issue-reset",
            axum::routing::post(local_auth::issue_reset).with_state(la.clone()),
        )
        .route(
            "/api/auth/forgot",
            axum::routing::post(local_auth::forgot).with_state(la.clone()),
        )
        .route(
            "/api/auth/reset",
            axum::routing::post(local_auth::reset).with_state(la.clone()),
        )
        // The IdP front door (idm-kanidm.md): probe, redirect,
        // callback. Same state as local auth on purpose — OIDC is
        // another way to authenticate an email, and everything after
        // the email is the local-login pipeline.
        .route(
            "/api/auth/oidc/available",
            axum::routing::get(boss_gateway::oidc::available).with_state(la.clone()),
        )
        .route(
            "/api/auth/oidc/login",
            axum::routing::get(boss_gateway::oidc::login).with_state(la.clone()),
        )
        .route(
            "/api/auth/oidc/callback",
            axum::routing::get(boss_gateway::oidc::callback).with_state(la),
        )
    } else {
        app
    }
}

/// Anything under `/api` that matched no service above is a routing
/// miss, and says so in JSON.
///
/// Without this it fell through to the `/{*rest}` SPA route and came
/// back as `200 text/html` — the whole index.html. A client then fails
/// deserializing at column 1 with a parser error that names JSON and
/// never mentions the route, which is how `/api/workflows` (the
/// plausible-looking spelling of `/api/workflows`) cost two detours
/// before anyone suspected the URL.
///
/// The repo already knew about this class: several services carry a
/// bare `/api/<name>` matcher purely because axum's `{*rest}` needs
/// at least one segment, each added after the SPA swallowed a list
/// endpoint. That is per-service whack-a-mole for a routing-shaped
/// problem. This closes it once — a static or longer-prefix route
/// still wins under matchit, so every real service keeps its match
/// and only genuine misses land here.
async fn api_not_found(uri: axum::http::Uri) -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "error": "no such API route",
            "path": uri.path(),
        })),
    )
}

/// Returns a JSON snapshot of per-endpoint latency percentiles
/// recorded since gateway startup (or last reset).
async fn handle_perf(State(state): State<Arc<AppState>>) -> axum::Json<perf::PerfSnapshot> {
    axum::Json(state.perf.snapshot())
}

/// Clears all recorded histograms. Useful before/after a specific
/// benchmark or fix so percentiles aren't diluted by old data.
///
/// The gateway's one own write that is not an auth ceremony, and until
/// backlog 07e797b4 it answered anyone — no session asked for. It now
/// passes the same edge gate as every proxied write: a session (401),
/// and not a read-only one (the named 403).
async fn handle_perf_reset(
    State(state): State<Arc<AppState>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let Some(refusal) = proxy::writer_gate(&headers, &method, uri.path(), &state) {
        return refusal;
    }
    state.perf.reset();
    "ok".into_response()
}

/// Load the HMAC session key from disk, or generate one on first run.
/// File is 32 random bytes stored as hex; perms 0600.
fn load_or_create_session_key(path: &Path) -> Result<Vec<u8>> {
    use std::io::Write;
    if path.exists() {
        let hex = std::fs::read_to_string(path)?;
        return Ok(boss_core::presence::parse_session_key(&hex)?);
    }

    tracing::info!(path = %path.display(), "generating new session key");
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let hex = hex_encode(&bytes);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = f.metadata()?.permissions();
        perms.set_mode(0o600);
        f.set_permissions(perms)?;
    }
    f.write_all(hex.as_bytes())?;
    Ok(bytes.to_vec())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What this gateway writes, the shared reader reads back — the jobs
    /// API verifies presence tickets with the key through that reader
    /// (backlog 72fe3640). Its reject cases live beside it in boss-core.
    #[test]
    fn a_written_key_reads_back_through_the_shared_parser() {
        let bytes: Vec<u8> = (0u8..32).map(|b| b.wrapping_mul(37)).collect();
        let hex = hex_encode(&bytes);
        assert_eq!(&hex[..6], "00254a");
        assert_eq!(boss_core::presence::parse_session_key(&hex), Ok(bytes));
    }

    #[test]
    fn load_or_create_generates_new_key_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.key");
        let key = load_or_create_session_key(&path).unwrap();
        assert_eq!(key.len(), 32);
        assert!(path.exists());
    }

    #[test]
    fn load_or_create_reuses_existing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.key");
        let first = load_or_create_session_key(&path).unwrap();
        let second = load_or_create_session_key(&path).unwrap();
        assert_eq!(first, second);
    }
}

#[cfg(test)]
mod routing_tests {
    //! Routing tests. They live in the binary because the route table
    //! does: `api`, `proxy`, and `static_files` are bin-local modules,
    //! so an integration test under `tests/` cannot reach them.

    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// The catch-all's signature. Asserting on the body rather than
    /// the status is deliberate — a proxied route whose upstream is
    /// down also answers 404-ish, and the question here is *which
    /// handler ran*, not what it thought of the request.
    const MISS: &str = "no such API route";

    fn app_with(local_auth: Option<Arc<LocalAuthState>>) -> axum::Router {
        app_declaring(local_auth, &public_reads::PublicReads::none())
    }

    fn app_declaring(
        local_auth: Option<Arc<LocalAuthState>>,
        reads: &public_reads::PublicReads,
    ) -> axum::Router {
        let state = Arc::new(AppState {
            session_key: vec![0u8; 32],
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(PerfCollector::new()),
            machine_token: Default::default(),
        });
        build_router(local_auth, reads).with_state(state)
    }

    /// The default: a manifest that declares nothing.
    fn app() -> axum::Router {
        app_with(None)
    }

    /// The reads the demo tenant's landing page makes without a
    /// session — what its seeds/tenant.toml declares, plus the sub-path
    /// the `/api/workflows` family covers.
    const DEMO_PUBLIC: &[&str] = &[
        "/api/workflows",
        "/api/workflows/some-kind",
        "/api/jobs/live",
        "/api/events/public-tail",
    ];

    fn demo() -> public_reads::PublicReads {
        public_reads::PublicReads::resolve(&[
            "/api/workflows".to_string(),
            "/api/jobs/live".to_string(),
            "/api/events/public-tail".to_string(),
        ])
        .expect("the demo tenant's three are declarable")
    }

    async fn get(app: axum::Router, path: &str) -> (StatusCode, String) {
        let resp = app
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .expect("router responds");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn unmatched_api_path_is_a_json_404() {
        // A path that looks right and is not. SINGULAR `/api/workflow`
        // — a plausible typo for the real `/api/workflows`, and it
        // must miss loudly rather than be answered with the SPA.
        //
        // The probe has now been broken twice by renames, which is the
        // actual lesson here. It was `/api/job-kinds`, chosen because
        // the real route was `/api/jobs/kinds`; the Workflow rename
        // made that spelling correct. It was then re-chosen as
        // `/api/job-kinds` again, and the rebase sweep rewrote it to
        // `/api/workflows` — a live route — so the test asserted a 404
        // against something that resolves.
        //
        // A probe asserting "this must NOT resolve" cannot be a string
        // a vocabulary sweep will touch. The singular survives any
        // `job-kind`/`job_kind` substitution because it contains
        // neither, and no rename produces it.
        let (status, body) = get(app(), "/api/workflow").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        assert!(body.contains(MISS), "body: {body}");
        assert!(
            body.contains("/api/workflow"),
            "the 404 should name the path that missed: {body}"
        );
        assert!(
            !body.to_lowercase().contains("<!doctype html"),
            "an /api miss must never return the SPA — that is the bug: {body}"
        );
    }

    #[tokio::test]
    async fn a_bare_api_root_is_also_a_miss() {
        let (status, body) = get(app(), "/api").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
        assert!(body.contains(MISS), "body: {body}");
    }

    /// The whole risk of a catch-all: that it quietly swallows a real
    /// route. matchit resolves by specificity rather than registration
    /// order, which is the assumption this pins — including the two
    /// shapes that motivated the per-service bare matchers (a list
    /// endpoint with no trailing segment, and a nested path).
    #[tokio::test]
    async fn no_service_route_is_shadowed_by_the_catch_all() {
        const REAL: &[&str] = &[
            "/api/session",
            "/api/tenant/manifest",
            "/api/finance/revenue-categories",
            "/api/gateway/perf",
            "/api/jobs",
            "/api/workflows",
            "/api/jobs/summary",
            "/api/classes",
            "/api/classes/employee",
            "/api/locations",
            "/api/subject-kinds",
            "/api/people",
            "/api/people/accounts",
            "/api/views",
            // `/api/events/tail`, not bare `/api/events` — the events
            // service has no list endpoint there, so the bare path is
            // a genuine miss and SHOULD reach the catch-all. Writing
            // it into this list first was my error, and the failure
            // was the test doing its job: it cannot tell a route I
            // wrongly believe exists from one the catch-all stole.
            "/api/events/tail",
            "/api/it/health",
            // Shipped on the service in train #10 and unreachable at
            // the door until train #12 — the reason this list exists.
            "/api/stations",
            "/api/stations/loading-dock/queue",
            // The departments registry: the bare list is what the
            // chrome bar reads on every page (dc5788ba), and it is
            // exactly the shape — a list endpoint with no trailing
            // segment — the per-service bare matchers exist for.
            "/api/departments",
            "/api/departments/sales/readiness",
            // The yard's server-computed read-model: gate slots,
            // capacity and the garage. Same failure as the stations
            // pair one endpoint later — it shipped in train #192 and
            // 404'd at the door, so the Approach rendered its
            // client-derived line items and silently dropped the
            // slots and the garage entirely (the sections are
            // `{#if}`-gated on a status that never became ready).
            "/api/yard/status",
            // The regions map (train #475): fourth instance, found on
            // David's screen; `every_api_path_the_web_fetches_is_routed`
            // below now derives this list from the bundle instead.
            "/api/yard/regions",
            // The moves record (design e765b3fc, cars M1 and M2): served
            // by the jobs upstream since M1 and first fetched by the map's
            // live layer in M2, so routed in the car that reads it.
            "/api/yard/moves",
            "/api/yard/moves/stream",
            // The served routes the map draws (car R3), fetched since #718.
            "/api/yard/routes",
            // The flights read (73c31776): inlined on index.html, and
            // the same answer for a page the gateway did not serve.
            "/api/flights/mine",
            // The agent-run record — what each actor built and what it
            // cost. Shipped on the jobs upstream in train #294 and
            // unreachable at the human door ever since: the Crew Board
            // wanted exactly this read on 2026-09-11 and shipped
            // without it (backlog 48bb0200). Third instance of the
            // stations/yard shape above, which is why all three now sit
            // in this list rather than being rediscovered a fourth
            // time from an empty panel.
            "/api/agent-runs",
            "/api/agent-runs/cost",
            // Surface opens (628f182b): the SPA's write and the
            // Codebase page's roll-up read, both on the jobs upstream.
            // Listed on the day they shipped, so the fourth instance
            // of the stations shape is refused here rather than found
            // from a panel that never fills.
            "/api/surface-opens",
            "/api/surface-opens/rollup",
        ];
        for path in REAL {
            let (_, body) = get(app(), path).await;
            assert!(
                !body.contains(MISS),
                "`{path}` fell through to the /api catch-all — the catch-all is \
                 shadowing a real service route"
            );
        }
    }

    /// Every `/api/...` path the web bundle fetches, read from
    /// `apps/web/src` itself. Each string or template literal that opens
    /// `/api/` yields one probe path:
    /// - a template cut by `${…}` probes with a placeholder segment
    ///   (`/api/jobs/${id}` → `/api/jobs/probe`);
    /// - a literal assigned to a name (`const API_BASE = '/api/ledger'`)
    ///   is a base every fetch suffixes, so it probes `/api/ledger/probe`;
    /// - a literal ending in `/` is a `startsWith` prefix, not a fetch;
    /// - a mention in a comment is prose.
    /// Test files are fixtures, and `dev-server.ts` is the dev proxy's
    /// own route table, not the bundle — both skipped.
    fn api_paths_the_web_fetches() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/web/src")
            .canonicalize()
            .expect("apps/web/src exists beside the crates");
        let mut files = Vec::new();
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("readable dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, out);
                    continue;
                }
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let is_source = name.ends_with(".ts") || name.ends_with(".svelte");
                let is_fixture =
                    name.contains(".test.") || name.contains(".spec.") || name == "dev-server.ts";
                if is_source && !is_fixture {
                    out.push(path);
                }
            }
        }
        walk(&root, &mut files);
        files.sort();
        let mut found: Vec<(String, String)> = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).expect("readable source");
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(&file)
                .display()
                .to_string();
            for (i, _) in text.match_indices("/api/") {
                // The literal must OPEN here — the byte before is a quote
                // or a backtick — and not inside a comment.
                if i == 0 || !matches!(text.as_bytes()[i - 1], b'\'' | b'"' | b'`') {
                    continue;
                }
                let line_start = text[..i].rfind('\n').map_or(0, |n| n + 1);
                let before = &text[line_start..i - 1];
                if before.contains("//") || before.trim_start().starts_with('*') {
                    continue;
                }
                let rest = &text[i..];
                let end = rest
                    .find(|c: char| {
                        !(c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
                    })
                    .unwrap_or(rest.len());
                let mut path = rest[..end].to_string();
                let cut_by_template = rest[end..].starts_with("${");
                // `const API_BASE = '…'` — with the spaces; an HTML
                // `href="…"` has none and is a fetch of exactly that path.
                let assigned = before.ends_with(" = ");
                if cut_by_template {
                    if path.ends_with('/') {
                        path.push_str("probe");
                    }
                } else if assigned {
                    path.push_str("/probe");
                } else if path.ends_with('/') {
                    continue;
                }
                // The prefix of a fully dynamic path names no route.
                if path == "/api/probe" {
                    continue;
                }
                if !found.iter().any(|(p, _)| *p == path) {
                    found.push((path, rel.clone()));
                }
            }
        }
        found
    }

    /// A local-auth state with an empty credential store — enough to
    /// mount the `/api/auth/*` routes, which `app()` leaves off.
    fn empty_local_auth() -> Arc<LocalAuthState> {
        let store = CredentialStore::load("/nonexistent/boss-test-credentials.toml")
            .expect("empty credential store");
        Arc::new(LocalAuthState {
            store,
            session_key: vec![0u8; 32],
            http: reqwest::Client::new(),
            audit: boss_gateway::audit::AuthAudit::disabled(),
            guest_access: GuestAccess::Basic,
            oidc: None,
            mail: boss_gateway::mail::from_env(),
            public_url: "https://boss.test".into(),
            forgot_seen: Default::default(),
        })
    }

    /// The list above, derived — not remembered. `/api/stations` (#10),
    /// `/api/yard/status` (#192), `/api/agent-runs` (#294) and
    /// `/api/yard/regions` (#475) each shipped on the jobs upstream,
    /// were fetched by a page, and 404'd at the human door because the
    /// gateway's route table is the one place the path was not written
    /// — and the REAL list above needed the same author to remember it
    /// a second time. The fourth instance was found on David's screen
    /// (the Train Yard's landing rendered `HTTP 404`). So the check now
    /// reads the consumer: every `/api/...` literal in `apps/web/src`
    /// must resolve on this router to something other than the
    /// catch-all. A page cannot fetch a route the gateway does not have
    /// without redding the gate.
    #[tokio::test]
    async fn every_api_path_the_web_fetches_is_routed() {
        let paths = api_paths_the_web_fetches();
        assert!(
            paths.len() > 50,
            "the scan found only {} paths — the web source moved or the scan broke",
            paths.len()
        );
        let la = empty_local_auth();
        let mut unrouted = Vec::new();
        for (path, file) in &paths {
            let (_, body) = get(app_with(Some(la.clone())), path).await;
            if body.contains(MISS) {
                unrouted.push(format!("{path}  (fetched by {file})"));
            }
        }
        assert!(
            unrouted.is_empty(),
            "the web fetches {} path(s) the gateway does not route — each 404s at the \
             human door; add the route beside its service's block in main.rs:\n  {}",
            unrouted.len(),
            unrouted.join("\n  ")
        );
    }

    /// The declarable landing-page reads, on an instance whose manifest
    /// declares none of them (design 11e60367 Q1, backlog b4afd7b9):
    /// each answers 401 like any other /api route — the session gate
    /// refuses before anything is forwarded — and none has fallen
    /// through to the catch-all. This is the company's instance: its
    /// tenant.toml carries no `[gateway] public_reads`, and prod needs
    /// no edit for the default to be none.
    #[tokio::test]
    async fn an_undeclared_public_read_refuses_a_sessionless_caller() {
        for path in DEMO_PUBLIC {
            let (status, body) = get(app(), path).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "`{path}` must be session-gated when the tenant declares nothing: {body}"
            );
            assert!(
                !body.contains(MISS),
                "`{path}` reached the /api catch-all: {body}"
            );
        }
    }

    /// The same reads on an instance that declares them — the demo
    /// tenant, whose landing page at `/` is served without a session and reads
    /// these to render. The discriminator is the same as everywhere in
    /// this module: the gated proxy answers 401 before it forwards,
    /// the public one forwards (and, with no upstream in a unit test,
    /// answers whatever the forward answers — never 401).
    #[tokio::test]
    async fn a_declared_public_read_skips_the_session_gate() {
        for path in DEMO_PUBLIC {
            let (status, body) = get(app_declaring(None, &demo()), path).await;
            assert_ne!(
                status,
                StatusCode::UNAUTHORIZED,
                "`{path}` is declared public by the demo tenant and must not be session-gated: {body}"
            );
            assert!(
                !body.contains(MISS),
                "`{path}` reached the /api catch-all: {body}"
            );
        }
    }

    /// The packet summary is session-gated EVEN on the demo tenant
    /// (backlog 19f08bd6): it left the publishable table because it
    /// counts what the caller may read, so a sessionless GET meets the
    /// gate through `/api/jobs/{*rest}` like every other packet read —
    /// 401 before anything is forwarded — and never the catch-all.
    #[tokio::test]
    async fn the_jobs_summary_is_session_gated_on_the_demo_tenant() {
        for path in ["/api/jobs/summary", "/api/jobs/summary?status=closed"] {
            let (status, body) = get(app_declaring(None, &demo()), path).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "`{path}` must meet the session gate: {body}"
            );
            assert!(
                !body.contains(MISS),
                "`{path}` reached the /api catch-all: {body}"
            );
        }
    }

    /// A declaration opens a GET, never a write: on the demo tenant, a
    /// POST to `/api/workflows` still meets the session gate (and not
    /// a 405 — the strict matcher chains the other methods through
    /// the gated proxy).
    #[tokio::test]
    async fn a_declaration_never_makes_a_write_public() {
        for path in ["/api/workflows", "/api/workflows/some-kind"] {
            let resp = app_declaring(None, &demo())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .expect("router responds");
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "POST `{path}` must be session-gated on every instance"
            );
        }
    }

    /// The agent-run reads carry per-actor build cost, so they must sit
    /// behind the session-gated `proxy::handle` and never
    /// `handle_public`. The discriminator needs no upstream: the gated
    /// proxy refuses a cookie-less request with 401 before it forwards
    /// anything, while a public route would try to reach boss-jobs-api
    /// and answer 502. A read that leaks what each actor costs is worse
    /// than one that 404s, which is why this is pinned next to the
    /// route rather than left to review.
    #[tokio::test]
    async fn the_agent_run_reads_refuse_a_sessionless_caller() {
        for path in ["/api/agent-runs", "/api/agent-runs/cost"] {
            let (status, body) = get(app(), path).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "`{path}` must be gated by the session proxy: {body}"
            );
            assert!(
                !body.contains(MISS),
                "`{path}` reached the /api catch-all: {body}"
            );
        }
    }

    /// boss-observability retired as superseded-by (backlog 467175e7,
    /// car B, 2026-09-23), and its three routes left with it: the
    /// snapshot, its sub-paths, and the health alias the old IT
    /// Monitoring page probed. Each is now an honest 404 from the /api
    /// catch-all — a route left proxying to a service no pod starts
    /// would answer 502 forever, which reads as an outage rather than
    /// as a thing that does not exist. Nothing in apps/web fetches any
    /// of them (`every_api_path_the_web_fetches_is_routed` would say).
    #[tokio::test]
    async fn the_retired_observability_routes_are_misses() {
        for path in [
            "/api/snapshot",
            "/api/snapshot/capabilities",
            "/api/observability/health",
        ] {
            let (status, body) = get(app(), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "`{path}`: {body}");
            assert!(
                body.contains(MISS),
                "`{path}` must reach the /api catch-all, not a proxy: {body}"
            );
        }
    }

    /// The gateway's own liveness answer keeps answering a stranger,
    /// now that the table and not this file registers it (backlog
    /// bf1f5ad2, 2026-09-19). `boss doctor` reads it with no session
    /// and the tunnel rotation verifies a new connector by asking for
    /// it through the edge, so the move had to be invisible on the
    /// wire: 200, `ok`, no session.
    #[tokio::test]
    async fn the_liveness_answer_is_still_ok_to_a_sessionless_caller() {
        let (status, body) = get(app(), "/health").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "ok", "the constant is the answer, unchanged");
    }

    /// The calendar feed is public BY DESIGN on every instance, whatever
    /// the tenant declares (public_reads::PUBLIC_BY_DESIGN): the token
    /// in the URL is the credential, because a calendar client cannot
    /// hold a session cookie. The discriminator is the same as
    /// everywhere in this module — the gated proxy answers 401 before
    /// it forwards, the public one forwards (and, with no upstream in a
    /// unit test, answers whatever the forward answers, never 401).
    #[tokio::test]
    async fn the_calendar_feed_is_public_by_design_on_an_instance_that_declares_nothing() {
        let (status, body) = get(app(), "/ics/some-token.ics").await;
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "`/ics/{{token}}.ics` carries its own credential and must not meet the session gate: {body}"
        );
        assert!(
            !body.contains(MISS),
            "`/ics/some-token.ics` reached the /api catch-all: {body}"
        );
    }

    /// EVERY SESSIONLESS READ IS A ROW. The route table in this file
    /// registers no sessionless proxy of its own: the tenant-declared
    /// reads and the by-design ones are both rows in public_reads.rs,
    /// each with the reason it may be public, and that module is the
    /// one place the hardening inventory reads to audit the door
    /// (backlog 240e03f3). A `handle_public` written back into this
    /// file is a door no inventory names. The needle is spelled in two
    /// halves so this test's own text does not match it.
    #[test]
    fn the_route_table_registers_no_sessionless_proxy_of_its_own() {
        let needle = concat!("proxy::handle_", "public");
        let hits: Vec<usize> = include_str!("main.rs")
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains(needle) && !l.trim_start().starts_with("//"))
            .map(|(n, _)| n + 1)
            .collect();
        assert!(
            hits.is_empty(),
            "main.rs registers a sessionless proxy directly at line(s) {hits:?} — every \
             sessionless read is a row in public_reads.rs (PUBLISHABLE, declared by the \
             tenant, or PUBLIC_BY_DESIGN, with its reason)"
        );
    }

    /// Local-auth routes are registered AFTER the catch-all, on a
    /// conditionally-built router. Static paths still win, but that is
    /// worth asserting rather than assuming, since these are the only
    /// `/api` routes added outside the main chain.
    #[tokio::test]
    async fn local_auth_routes_survive_the_catch_all() {
        // `load` on a path that does not exist yields an empty store,
        // which is all the routing test needs.
        let store = CredentialStore::load("/nonexistent/boss-test-credentials.toml")
            .expect("empty credential store");
        let la = Arc::new(LocalAuthState {
            store,
            session_key: vec![0u8; 32],
            http: reqwest::Client::new(),
            audit: boss_gateway::audit::AuthAudit::disabled(),
            guest_access: GuestAccess::Basic,
            oidc: None,
            mail: boss_gateway::mail::from_env(),
            public_url: "https://boss.test".into(),
            forgot_seen: Default::default(),
        });

        for path in [
            "/api/auth/me",
            "/api/auth/login",
            "/api/auth/guest",
            "/api/auth/forgot",
        ] {
            let (_, body) = get(app_with(Some(la.clone())), path).await;
            assert!(
                !body.contains(MISS),
                "`{path}` fell through to the /api catch-all"
            );
        }
    }

    /// The break-glass ceremony routes are registered on the same
    /// conditionally-built router as local auth; they too must beat
    /// the /api catch-all, and the ceremony page must beat the SPA
    /// fallback — a /break-glass URL answered with the dashboard
    /// shell would be exactly the /simulator failure shape below.
    #[tokio::test]
    async fn break_glass_routes_survive_catch_all_and_spa_fallback() {
        let store = CredentialStore::load("/nonexistent/boss-test-credentials.toml")
            .expect("empty credential store");
        let la = Arc::new(LocalAuthState {
            store,
            session_key: vec![0u8; 32],
            http: reqwest::Client::new(),
            audit: boss_gateway::audit::AuthAudit::disabled(),
            guest_access: GuestAccess::Off,
            oidc: None,
            mail: boss_gateway::mail::from_env(),
            public_url: "https://boss.test".into(),
            forgot_seen: Default::default(),
        });

        for path in [
            "/api/auth/break-glass/enroll/begin",
            "/api/auth/break-glass/enroll/finish",
            "/api/auth/break-glass/assert/begin",
            "/api/auth/break-glass/assert/finish",
        ] {
            let (_, body) = get(app_with(Some(la.clone())), path).await;
            assert!(
                !body.contains(MISS),
                "`{path}` fell through to the /api catch-all"
            );
        }

        let (status, body) = get(app_with(Some(la)), "/break-glass").await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert!(
            body.contains("break-glass/assert/begin"),
            "/break-glass must serve the gateway's own ceremony page, not the \
             SPA fallback: {body}"
        );
    }

    /// Non-API paths must still reach the SPA — the fix narrows the
    /// fallback, it does not remove it.
    #[tokio::test]
    async fn non_api_paths_still_reach_the_spa_fallback() {
        let (_, body) = get(app(), "/ux/jobs").await;
        assert!(
            !body.contains(MISS),
            "a page route must not be treated as an API miss: {body}"
        );
    }

    /// Same request a browser makes when someone clicks a link: GET
    /// with an Accept that asks for HTML.
    async fn navigate(app: axum::Router, path: &str) -> (StatusCode, String, String) {
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(axum::http::header::ACCEPT, "text/html,*/*;q=0.8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("router responds");
        let status = resp.status();
        let location = resp
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            location,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    /// The simulator is a DIFFERENT app behind the same door, and
    /// `/simulator/` — the spelling a browser produces from a trailing
    /// slash, a redirect, or a relative link — matched neither
    /// `/simulator` nor `/simulator/{*rest}`: matchit's `{*rest}` needs
    /// at least one segment, exactly the shape that motivated the bare
    /// `/api` matcher above. So it fell through to the root `/{*rest}`
    /// SPA route and the visitor was served the MAIN app's index.html —
    /// "Algedonic Ales", the dashboard shell — under a /simulator URL.
    ///
    /// That is worse than an error. boss-simulator's own "bundle not
    /// installed" stub says what is wrong; another app's shell just
    /// looks broken, and got reported as "the simulator didn't load"
    /// (69a0421d) while the real fault was one missing route.
    ///
    /// The discriminator is which handler answers an unauthenticated
    /// document navigation: the simulator proxy (`proxy::handle_app`)
    /// sends a browser to `/login?next=…`, the root SPA handler
    /// (`static_files::handle`) returns a bare 401. No upstream, no
    /// session, no files on disk — so this pins routing and nothing
    /// else.
    #[tokio::test]
    async fn the_simulator_prefix_never_resolves_to_the_main_spa() {
        for path in [
            "/simulator",
            // The regression. Every other spelling already worked.
            "/simulator/",
            "/simulator/config",
            "/simulator/api/status",
        ] {
            let (status, location, body) = navigate(app(), path).await;
            assert_eq!(
                status,
                StatusCode::SEE_OTHER,
                "`{path}` was not routed to boss-simulator — it fell through to the \
                 root SPA handler, which serves the MAIN app's index.html to anyone \
                 with a session. body: {body}"
            );
            assert_eq!(
                location,
                format!("/login?next={path}"),
                "`{path}` should come back from the simulator proxy's login redirect"
            );
            assert!(
                !body.to_lowercase().contains("<!doctype html"),
                "a /simulator path must never be answered with another app's HTML \
                 shell: {body}"
            );
        }
    }
}
