use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::State;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

mod api;
mod perf;
mod plugin_files;
mod proxy;
mod role_headers;
mod static_files;
mod timing;

use perf::PerfCollector;

use boss_gateway::local_auth::{self, CredentialStore, LocalAuthState};

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

    let listen = std::env::var("BOSS_LISTEN").unwrap_or_else(|_| "127.0.0.1:4443".into());
    let session_key_path: std::path::PathBuf = std::env::var("BOSS_SESSION_KEY")
        .unwrap_or_else(|_| "/var/lib/boss-gateway/session.key".into())
        .into();
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
            guest_access: std::env::var("BOSS_GUEST_ACCESS").as_deref() == Ok("1"),
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
    });

    let app = build_router(local_auth_state.clone());

    let app = app
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            timing::request_timer,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            role_headers::inject_role_headers,
        ))
        .with_state(state);

    tracing::info!(listen = %listen, static_dir = %static_files::static_dir(), "boss-gateway starting");
    let listener = TcpListener::bind(&listen).await?;
    axum::serve(listener, app).await?;

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
fn build_router(local_auth_state: Option<Arc<LocalAuthState>>) -> axum::Router<Arc<AppState>> {
    let app = axum::Router::new()
        .route("/health", axum::routing::get(handle_health))
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
        // Public read surface for the unauth landing page (`/`) —
        // live fetch from /api/workflows/{kind}, no session
        // required. Strict path matchers win over `/api/jobs/{*rest}`
        // in axum's router.
        // Writes / step metadata / detail routes stay auth-gated.
        // The expanded list — `/api/jobs/summary` and the bare
        // `/api/jobs` GET — turns the landing page from a static
        // workflow-diagram preview into a live window into the
        // brewery's running operating company.
        //
        // For `/api/workflows` and `/api/workflows/{*rest}` we
        // pin GET to the public handler so the landing page can
        // read without auth, AND chain the other methods through
        // the auth-gated handler on the same MethodRouter — without
        // the chain, POST/PUT/DELETE would return 405 because axum
        // picks the most-specific matching path first and these
        // strict matchers shadow the wildcard `/api/jobs/{*rest}`.
        .route(
            "/api/workflows",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS))
                .post(|s, r| proxy::handle(s, r, &proxy::JOBS))
                .put(|s, r| proxy::handle(s, r, &proxy::JOBS))
                .delete(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/workflows/{*rest}",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS))
                .post(|s, r| proxy::handle(s, r, &proxy::JOBS))
                .put(|s, r| proxy::handle(s, r, &proxy::JOBS))
                .delete(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/jobs/summary",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS)),
        )
        .route(
            "/api/jobs/live",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS)),
        )
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
        // Scheduling routes live alongside jobs on the same upstream.
        // Auth-gated like the rest of /api/*.
        .route(
            "/api/scheduling/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::JOBS)),
        )
        // Public calendar-feed endpoint: /ics/{token}.ics. The token in
        // the URL is the authentication — calendar clients can't carry
        // auth cookies, so we proxy this one path without the cookie
        // gate. Upstream (boss-jobs-api) validates the token.
        .route(
            "/ics/{*rest}",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::JOBS)),
        )
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
        // Public companion to /api/events/tail — unauth, restricted
        // to a curated demo-friendly topic set. Powers the public
        // landing page's right-rail event tail. Upstream (boss-events)
        // returns the curated allow-list shape; the gateway just
        // proxies unauth so visitors see it.
        .route(
            "/api/events/public-tail",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::EVENTS)),
        )
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
        .route(
            "/api/design/{*rest}",
            axum::routing::any(|s, r| proxy::handle(s, r, &proxy::DESIGN)),
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
        // Policy proxy carries a graceful-fallback for my-scope POST so
        // pages don't log 502s if the policy upstream is down.
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
        // Observability aggregator — the SPA's Operations page reads
        // /api/snapshot for the cybernetics rollup. Two strict matchers
        // (no trailing path) plus a wildcard for any future sub-paths.
        // Public read so the unauth landing-page mode keeps working;
        // there's no per-tenant data here yet, just synthetic agent
        // activity from the demo_agents config (or real cross-VM
        // rollups when [[vms]] is populated).
        .route(
            "/api/snapshot",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::OBSERVABILITY)),
        )
        .route(
            "/api/snapshot/{*rest}",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::OBSERVABILITY)),
        )
        // The IT Monitoring page probes /api/<port-name>/health for
        // every PORTS entry. boss-observability and boss-docs both
        // expose their routes under different prefixes (/api/events,
        // /api/snapshot, /api/agents for observability; /api/design/*
        // for docs), so without these aliases the monitoring page
        // shows them as 'down' even when running.
        .route(
            "/api/observability/health",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::OBSERVABILITY)),
        )
        .route(
            "/api/docs/health",
            axum::routing::get(|s, r| proxy::handle_public(s, r, &proxy::DESIGN)),
        )
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

    // Local-auth routes — only mounted when BOSS_AUTH_PROVIDER=
    // local-auth. These handlers carry their own state (the
    // CredentialStore + the session_key + an http client for
    // bootstrap_email lookups against boss-people-api).
    if let Some(la) = local_auth_state {
        // Passkey ceremony (docs/design/presence.md, packet 7218c3f1):
        // best-effort mount — a malformed BOSS_PUBLIC_URL must degrade
        // to "no passkey routes", never crash the front door.
        let app = match boss_gateway::passkey::PasskeyState::from_env(la.session_key.clone()) {
            Ok(pk) => {
                let pk = std::sync::Arc::new(pk);
                app.route(
                    "/api/auth/passkey/register/begin",
                    axum::routing::post(boss_gateway::passkey::register_begin)
                        .with_state(pk.clone()),
                )
                .route(
                    "/api/auth/passkey/register/finish",
                    axum::routing::post(boss_gateway::passkey::register_finish)
                        .with_state(pk.clone()),
                )
                .route(
                    "/api/auth/passkey/assert/begin",
                    axum::routing::post(boss_gateway::passkey::assert_begin).with_state(pk.clone()),
                )
                .route(
                    "/api/auth/passkey/assert/finish",
                    axum::routing::post(boss_gateway::passkey::assert_finish)
                        .with_state(pk.clone()),
                )
                .route(
                    "/api/auth/passkey/credentials",
                    axum::routing::get(boss_gateway::passkey::credentials_list).with_state(pk),
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, "passkey ceremony not mounted");
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

async fn handle_health() -> &'static str {
    "ok"
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
async fn handle_perf_reset(State(state): State<Arc<AppState>>) -> &'static str {
    state.perf.reset();
    "ok"
}

/// Load the HMAC session key from disk, or generate one on first run.
/// File is 32 random bytes stored as hex; perms 0600.
fn load_or_create_session_key(path: &Path) -> Result<Vec<u8>> {
    use std::io::Write;
    if path.exists() {
        let hex = std::fs::read_to_string(path)?;
        let bytes = hex_decode(hex.trim())
            .ok_or_else(|| anyhow::anyhow!("session key file is not valid hex"))?;
        if bytes.len() < 32 {
            anyhow::bail!("session key must be at least 32 bytes");
        }
        return Ok(bytes);
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

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let bytes = [0x00, 0x01, 0xaf, 0xff, 0x7e];
        let hex = hex_encode(&bytes);
        assert_eq!(hex, "0001afff7e");
        assert_eq!(hex_decode(&hex), Some(bytes.to_vec()));
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert_eq!(hex_decode("abc"), None);
    }

    #[test]
    fn hex_decode_rejects_non_hex() {
        assert_eq!(hex_decode("zz"), None);
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
        let state = Arc::new(AppState {
            session_key: vec![0u8; 32],
            proxy_client: reqwest::Client::new(),
            perf: Arc::new(PerfCollector::new()),
        });
        build_router(local_auth).with_state(state)
    }

    fn app() -> axum::Router {
        app_with(None)
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
            "/api/design/docs",
            "/api/it/health",
            // Shipped on the service in train #10 and unreachable at
            // the door until train #12 — the reason this list exists.
            "/api/stations",
            "/api/stations/loading-dock/queue",
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
            guest_access: true,
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
