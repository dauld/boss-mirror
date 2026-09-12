//! `boss-people-api` service: employee roster backed by Postgres.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_classes_client::{ClassesClient, ReqwestClassesClient};
use boss_locations_client::{LocationsClient, ReqwestLocationsClient};
use boss_people::http::{PeopleApiState, router};
use boss_people::people_config::PeopleApiConfig;
use clap::Parser;
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-people-api", about = "Boss People API service", version)]
struct Cli {
    #[arg(short, long, default_value = "/etc/boss-people-api.toml")]
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
    let cfg = PeopleApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(http_bind = %cfg.http_bind, "boss-people-api starting");
    // people-api keeps `assets_api_url` in config (the open-ticket
    // lookups that used it live in boss-accounts-api now) but does
    // not dial the assets client itself.

    // Mandatory Class registry client. Employee writes validate
    // `role` via `class_exists("employee", code)` before commit —
    // the only gate keeping an unregistered role code out.
    let classes_client: Arc<dyn ClassesClient> = {
        info!(classes_api_url = %cfg.classes_api_url, "Class registry validation enabled");
        Arc::new(ReqwestClassesClient::new(cfg.classes_api_url.clone()))
    };

    // Seed the executive-role cache from the Class registry so
    // `has_global_read` recognises tenant-defined executives. Skip
    // on transport failure — platform-admin + audit-readonly still
    // grant global read.
    match boss_classes_client::seed_executive_role_cache(classes_client.as_ref()).await {
        Ok(n) => info!(count = n, "executive role cache seeded"),
        Err(e) => {
            tracing::warn!(error = %e, "failed to seed executive roles from classes; falling back to platform-admin/audit-readonly only")
        }
    }

    // Mandatory Locations registry client. Employee writes validate
    // `location` via `location_exists(id)` before commit.
    let locations_client: Arc<dyn LocationsClient> = {
        info!(locations_api_url = %cfg.locations_api_url, "Locations registry validation enabled");
        Arc::new(ReqwestLocationsClient::new(cfg.locations_api_url.clone()))
    };

    // One pool per service. PgPool is internally Arc'd, so cloning is
    // cheap and every sub-router shares the same connection slots
    // instead of fragmenting them across many small pools.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let people = Arc::new(boss_people::PgPeople::with_registries(
        pool.clone(),
        classes_client.clone(),
        locations_client.clone(),
    ));

    // Connect to NATS for domain event publishing (optional).
    let publisher = match &cfg.nats_url {
        Some(url) => {
            let bus = boss_nats::NatsEventBus::connect(url)
                .await
                .with_context(|| format!("connecting to NATS at {url}"))?;
            #[allow(unused_mut)]
            let mut pub_ = boss_core::publisher::DomainPublisher::new(Arc::new(bus), "people");
            {
                pub_ = pub_.with_audit(std::sync::Arc::new(boss_events::PgAuditWriter::new(
                    pool.clone(),
                )));
            }
            info!(nats_url = %url, "domain event publishing + audit trail enabled");
            Some(pub_)
        }
        None => {
            info!("no nats_url configured — domain events will not be published");
            None
        }
    };

    // Clock client first — sub-routers below need it stamped
    // through their states so audit_log rows land sim-dated under
    // clock-api sim mode.
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    let clock: Arc<dyn boss_clock_client::ClockClient> = Arc::new(
        boss_clock_client::ReqwestClockClient::new(clock_url.clone()),
    );
    info!(%clock_url, "clock client wired");

    // Wire the sim-mode probe into the publisher so every
    // every stamp automatically injects `_simulated: bool` into
    // the audit_log payload without per-handler changes.
    let publisher = publisher.map(|p| {
        p.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
            clock.clone(),
        )))
    });

    // Mount workflow and search routers first (more-specific routes),
    // then merge the people CRUD router (has catch-all /{id}).
    let mut app = boss_people::workflows::workflow_router(
        pool.clone(),
        std::sync::Arc::new(boss_people::PgPeople::new(pool.clone())),
        publisher.clone(),
        clock.clone(),
    )
    .merge(boss_people::requisitions::requisitions_router(
        pool.clone(),
        publisher.clone(),
        clock.clone(),
    ))
    .merge(boss_people::employee_changes::employee_changes_router(
        pool.clone(),
        publisher.clone(),
        clock.clone(),
    ))
    .merge(boss_people::scope::scope_router(pool.clone()))
    .merge(boss_people::webauthn::webauthn_router(
        pool.clone(),
        clock.clone(),
    ));
    // people-api owns only the employee-side routers. The
    // accounts-side routers (accounts, account_team_members,
    // account_notes, account_next_actions, account_risk_scores,
    // support_cases) and the audit_log read surface
    // (tail/stream/export/public-tail) are their own services, one
    // binary per domain:
    //   - boss-accounts-api  (port 7550) — see
    //     crates/modules/boss-accounts/src/bin/boss_accounts_api.rs
    //   - boss-events-api    (port 7150) — see
    //     crates/core/boss-events/src/bin/boss_events_api.rs

    // Wire the calendar client if configured. The PTO endpoint
    // returns 503 when calendar isn't set up; everything else
    // works either way.
    let calendar: Option<std::sync::Arc<dyn boss_calendar_client::CalendarClient>> =
        cfg.calendar_api_url.as_deref().map(|url| {
            info!(calendar_api_url = %url, "calendar client wired up — PTO endpoint live");
            std::sync::Arc::new(boss_calendar_client::ReqwestCalendarClient::new(url))
                as std::sync::Arc<dyn boss_calendar_client::CalendarClient>
        });
    if calendar.is_none() {
        tracing::info!("calendar_api_url unset — POST /api/people/pto will return 503");
    }
    app = app.merge(boss_people::pto::pto_router(
        boss_people::pto::PtoApiState { calendar },
    ));

    // Optional SubjectKind registry — opt-in validator for
    // tenant-extensible Subject discriminators (see
    // http.rs::check_custom_subject). Wires off the same
    // subject_kinds_api_url already used by boss-jobs.
    let subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>> =
        cfg.subject_kinds_api_url.as_deref().map(|url| {
            info!(subject_kinds_api_url = %url, "SubjectKind registry validation enabled");
            Arc::new(boss_subject_kinds_client::ReqwestSubjectKindsClient::new(
                url,
            )) as Arc<dyn boss_subject_kinds_client::SubjectKindsClient>
        });

    let state = PeopleApiState {
        people,
        publisher,
        policy: None,
        subject_kinds,
        clock,
    };
    app = app.merge(router(state));
    // Sim-origin middleware: extract x-sim-origin header and set the
    // per-request task-local so the publisher inherits the sim
    // marker. Closes the gap where a sim chain could trigger a
    // non-sim event on a service running with a wall clock.
    let app = app.layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ));

    let http_addr: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "people HTTP API listening");

    axum::serve(listener, app).await?;
    Ok(())
}
