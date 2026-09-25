//! boss-policy-api — row-level authorization service.
//!
//! Serves check / my-scope / admin endpoints over the PolicyRepository.
//! On startup, seeds DEFAULT_RULES (per D8) — idempotent, operator
//! edits survive restarts.
//!
//! Wires the Postgres-backed `PgPolicy` adapter so rules persist
//! across restarts. The `postgres` feature is required at build
//! time (Cargo `required-features`); the in-memory path remains
//! available only for tests via the library API.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use tracing::{info, warn};

use boss_policy::http::{PolicyApiState, router};
use boss_policy::port::PolicyRepository;
use boss_policy::{PgPolicy, PolicyEngine, default_rules};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "boss_policy=info,info".into()),
        )
        .init();

    let postgres_url = std::env::var("BOSS_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://boss:boss@127.0.0.1/boss".to_string());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&postgres_url)
        .await
        .with_context(|| format!("connecting to Postgres at {postgres_url}"))?;

    let repo: Arc<PgPolicy> = Arc::new(PgPolicy::new(pool));

    // Reconcile the default rules (per D8). Insert rules that don't
    // exist; refresh bootstrap-owned rows whose scope or active flag
    // drifted from the current code default; preserve any row whose
    // `updated_by != 'bootstrap'` (operator-tuned). This is the
    // explicit self-heal path: widened scopes, corrected typos, and
    // renamed-resource ids in the code defaults propagate to the live
    // DB instead of silently diverging.
    let defaults = default_rules();
    let stats = repo.bootstrap_reconcile(&defaults).await?;
    info!(
        inserted = stats.inserted,
        refreshed = stats.refreshed,
        preserved = stats.preserved,
        unchanged = stats.unchanged,
        total = defaults.len(),
        "reconciled default policy rules"
    );

    let engine = Arc::new(PolicyEngine::new(repo.clone()));
    let state = PolicyApiState { repo, engine };
    let app: Router = router(state);

    // Default port pulled from boss_ports — single source of truth
    // shared with the config generator + every BOSS_POLICY_URL
    // default. The 7060/7250 collision once lived right here; the
    // table now makes drift impossible.
    let port = std::env::var("BOSS_POLICY_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or_else(|| boss_ports::prod("policy"));
    // Loopback: the gateway is the sole trust boundary and is
    // co-located in every deployment (SECURITY.md §Deployment
    // trust model). Set BOSS_POLICY_BIND_HOST to widen deliberately.
    let host = std::env::var("BOSS_POLICY_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let bind = format!("{host}:{port}");

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            warn!(?e, "failed to bind {bind}; exiting");
            return Err(e.into());
        }
    };
    info!(%bind, "boss-policy-api listening (postgres-backed)");
    let app = boss_core::machine_gate::mount(app, "policy", &["/api/policy/health"]);
    axum::serve(listener, app).await?;
    Ok(())
}
