//! `boss-docs-api` service: the design corpus index, backed by
//! Postgres. A read-layer over docs/design/*.md.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
#[cfg(not(feature = "postgres"))]
use boss_docs::InMemoryDocsRepo;
use boss_docs::config::DocsApiConfig;
use boss_docs::http::{DocsApiState, router};
use boss_docs::port::DocsRepository;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-docs-api",
    about = "Boss design decision tracker API",
    version
)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-docs-api.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let cli = Cli::parse();
    let cfg = DocsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        postgres_url = %boss_core::startup::mask_password(&cfg.postgres_url),
        http_bind = %cfg.http_bind,
        repo_root = %cfg.repo_root.display(),
        "boss-docs-api starting"
    );

    #[cfg(feature = "postgres")]
    let repo: Arc<dyn DocsRepository> = {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect(&cfg.postgres_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        Arc::new(boss_docs::PgDocsRepo::new(pool))
    };

    #[cfg(not(feature = "postgres"))]
    let repo: Arc<dyn DocsRepository> = {
        boss_core::startup::require_postgres_or_explicit_inmemory("boss-docs-api")?;
        Arc::new(InMemoryDocsRepo::new())
    };

    let state = DocsApiState {
        repo,
        repo_root: cfg.repo_root.clone(),
    };

    // Auto-reindex on startup. The reindex walks docs/design/*.md
    // and UPSERTs each into `design_docs` — idempotent, fast on
    // warm starts (~400ms across 41 docs). Eliminates the
    // "fresh DB → /design page empty until someone POSTs
    // /api/design/reindex by hand" foot-gun that surfaced after
    // every regen of the brewery seed bundle. Errors are logged
    // but don't fail startup; an empty index is recoverable from
    // the SPA's reindex button.
    match boss_docs::reindex::reindex(state.repo.as_ref(), &state.repo_root).await {
        Ok(stats) => info!(
            docs_indexed = stats.docs_indexed,
            docs_deleted = stats.docs_deleted,
            duration_ms = stats.duration_ms,
            "auto-reindex complete"
        ),
        Err(e) => {
            tracing::warn!(error = %e, "auto-reindex failed; /design will be empty until manual reindex")
        }
    }

    let app = router(state);

    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "boss-docs-api listening");

    axum::serve(listener, app).await?;

    Ok(())
}
