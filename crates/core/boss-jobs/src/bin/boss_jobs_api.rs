//! `boss-jobs-api` service: jobs domain + NATS event bus + HTTP API.
//!
//! Wires the jobs repository (Postgres) to NATS for
//! event distribution and exposes an axum HTTP API.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::jobs_config::JobsApiConfig;
use boss_jobs::port::JobsRepository;
use boss_nats::NatsEventBus;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-jobs-api", about = "Boss Jobs API service", version)]
struct Cli {
    /// Path to the service config (TOML)
    #[arg(short, long, default_value = "/etc/boss-jobs-api.toml")]
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
    let cfg = JobsApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    info!(
        nats_url = %cfg.nats_url,
        http_bind = %cfg.http_bind,
        postgres = cfg.postgres_url.is_some(),
        "boss-jobs-api starting"
    );

    // Connect to NATS.
    let bus = Arc::new(
        NatsEventBus::connect(&cfg.nats_url)
            .await
            .with_context(|| format!("connecting to NATS at {}", cfg.nats_url))?,
    );

    // Seed the executive-role cache from the Class registry — the
    // escalation router below filters recipients by `is_executive`.
    // Skip on missing config or transport failure; the router still
    // runs but won't page anyone in that state.
    if let Some(url) = &cfg.classes_api_url {
        let client = boss_classes_client::ReqwestClassesClient::new(url.clone());
        match boss_classes_client::seed_executive_role_cache(&client).await {
            Ok(n) => info!(count = n, classes_api_url = %url, "executive role cache seeded"),
            Err(e) => {
                tracing::warn!(error = %e, "failed to seed executive roles from classes; escalation router will skip executive paging")
            }
        }
    } else {
        info!("classes_api_url unset; executive role cache disabled");
    }

    // Spawn the escalation router as a background subscriber. It
    // listens for jobs.job.created events and pages executives when
    // a critical ticket lands on a platinum/gold account. Running
    // inside the jobs-api service keeps the subscriber close to the
    // publisher without a new deploy unit.
    // Concrete bus, not the port: the router opens a JetStream durable
    // consumer (at-least-once) and only falls back to the port-shaped
    // core subscription when JetStream is absent.
    let _escalation_handle = boss_jobs::escalation::spawn_router(
        bus.clone(),
        boss_jobs::escalation::EscalationConfig::default(),
    );

    let (cancel_tx, cancel_rx) = watch::channel(false);

    // Build the publisher: bus + (optional) Postgres audit writer.
    #[allow(unused_mut)]
    let mut publisher = boss_core::publisher::DomainPublisher::new(
        bus.clone() as std::sync::Arc<dyn boss_core::port::EventBus>,
        "jobs",
    );

    // Optional cross-service calendar client. None → step
    // reservation hook is a no-op; rollout-friendly across the
    // calendar service deploy.
    let calendar: Option<Arc<dyn boss_calendar_client::CalendarClient>> =
        cfg.calendar_api_url.as_deref().map(|url| {
            info!(calendar_api_url = %url, "calendar client wired up — step reservation hook live");
            Arc::new(boss_calendar_client::ReqwestCalendarClient::new(url))
                as Arc<dyn boss_calendar_client::CalendarClient>
        });
    if calendar.is_none() {
        info!("calendar_api_url unset — step reservation hook disabled");
    }

    // Optional SubjectKind registry client. None → Subject writes
    // accept any kind string. Same opt-in shape as `calendar`.
    let subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>> =
        cfg.subject_kinds_api_url.as_deref().map(|url| {
            info!(subject_kinds_api_url = %url, "subject-kinds client wired up — Custom subject validation live");
            Arc::new(boss_subject_kinds_client::ReqwestSubjectKindsClient::new(url))
                as Arc<dyn boss_subject_kinds_client::SubjectKindsClient>
        });
    if subject_kinds.is_none() {
        info!(
            "subject_kinds_api_url unset — Custom subject validation disabled (Phase A behaviour)"
        );
    }

    // Authoritative clock — every audit_log row + every "now" the
    // jobs API stamps comes from clock-api. Default URL pulled
    // from boss-ports; override via BOSS_CLOCK_URL. Constructed
    // here (not in run_server) because the boot-time registry
    // reconcile below already needs a clock-routed "now" for the
    // events its writes record.
    let clock_url = std::env::var("BOSS_CLOCK_URL").unwrap_or_else(|_| boss_ports::url("clock"));
    info!(%clock_url, "clock client wired");
    let clock: Arc<dyn boss_clock_client::ClockClient> =
        Arc::new(boss_clock_client::ReqwestClockClient::new(clock_url));

    // Postgres, or refuse: the in-memory serving branch is deleted (below).
    if let Some(ref pg_url) = cfg.postgres_url {
        info!("using Postgres jobs storage");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(20)
            .connect(pg_url)
            .await
            .with_context(|| "connecting to Postgres")?;
        // Subject existence validator: one indexed lookup against the
        // `subjects` identity table, uniform for every kind — platform,
        // tenant-defined, all of them (subject-model design R1, approved
        // 2026-07-15). Replaces the four-upstream HTTP prober and its
        // fall-through kinds; the create handler fails CLOSED when the
        // check is unavailable (Q2: abort by default).
        let subject_existence: Option<
            Arc<dyn boss_jobs::subject_existence::SubjectExistenceCheck>,
        > = {
            info!("subject existence gate wired to the subjects identity table (all kinds)");
            Some(
                Arc::new(boss_jobs::subject_existence::PgSubjectExistence::new(
                    pool.clone(),
                )) as Arc<dyn boss_jobs::subject_existence::SubjectExistenceCheck>,
            )
        };
        publisher = publisher.with_audit(std::sync::Arc::new(boss_events::PgAuditWriter::new(
            pool.clone(),
        )));
        info!("audit_log persistence enabled");
        // Pass the URLs alongside the pool so the demo-loop
        // restart-epoch endpoint can spawn boss-rebuild-all against the
        // same DB, and purge the JetStream delivery buffer on the same
        // broker, without re-parsing config. The NATS URL comes from
        // config rather than the environment — this service has never
        // set `BOSS_NATS_URL`, so reading one would skip the purge.
        let jobs = Arc::new(boss_jobs::PgJobs::with_urls(
            pool.clone(),
            pg_url.to_string(),
            cfg.nats_url.clone(),
        ));
        let kind_registry: Arc<dyn boss_jobs::WorkflowRegistry> =
            Arc::new(boss_jobs::PgWorkflows::new(pool.clone()));
        reconcile_platform_workflows(kind_registry.as_ref(), jobs.as_ref(), &clock).await;
        let plugin_registry: Arc<dyn boss_jobs::StepPluginRegistry> =
            Arc::new(boss_jobs::PgStepPlugins::new(pool.clone()));
        let scheduling: Arc<dyn boss_jobs::scheduling::SchedulingRepository> =
            Arc::new(boss_jobs::scheduling::PgScheduling::new(pool.clone()));
        let cadence: Arc<dyn boss_jobs::cadence::CadenceRepository> =
            Arc::new(boss_jobs::cadence::PgCadence::new(pool.clone()));
        // The delivery pipeline's policy content, served to the train
        // conductor through the same door as everything else it reads
        // (docs/design/delivery-as-protocol.md).
        let delivery: Arc<dyn boss_jobs::delivery::DeliveryPolicyRepository> =
            Arc::new(boss_jobs::delivery::PgDeliveryPolicy::new(pool.clone()));
        // The credentials registry: knowledge about credentials —
        // scopes, storage locations, consumers — never values
        // (packet 7ee101aa, second leg).
        let credentials: Arc<dyn boss_jobs::credentials::CredentialsRegistry> =
            Arc::new(boss_jobs::credentials::PgCredentials::new(pool.clone()));
        // The agent-run record (backlog 83344e16): what an actor's run
        // cost, on the one door `boss-api` already reaches.
        let agent_runs: Arc<dyn boss_jobs::agent_runs::AgentRunLog> =
            Arc::new(boss_jobs::agent_runs::PgAgentRuns::new(pool.clone()));
        // Q7: human job-owner resolution over the people roster.
        let people_url =
            std::env::var("BOSS_PEOPLE_URL").unwrap_or_else(|_| boss_ports::url("people"));
        let roster: Option<Arc<dyn boss_jobs::owner_resolution::RosterLookup>> = Some(Arc::new(
            boss_jobs::owner_resolution::ReqwestRosterLookup::new(people_url.clone()),
        ));
        info!(%people_url, "human job-owner resolution wired (Q7)");
        let stations: Arc<dyn boss_jobs::StationRegistry> =
            Arc::new(boss_jobs::PgStations::new(pool.clone()));
        verify_station_viability(stations.as_ref()).await;
        return run_server(
            Some(
                std::sync::Arc::new(boss_jobs::job_edges::PgJobEdges::new(pool.clone()))
                    as std::sync::Arc<dyn boss_jobs::job_edges::JobEdgesRegistry>,
            ),
            Some(stations),
            jobs,
            bus,
            publisher,
            Some(kind_registry),
            Some(plugin_registry),
            Some(scheduling),
            Some(cadence),
            Some(delivery),
            Some(credentials),
            Some(agent_runs),
            calendar,
            subject_kinds,
            subject_existence,
            roster,
            clock.clone(),
            cancel_tx,
            cancel_rx,
            &cfg.http_bind,
        )
        .await;
    }

    anyhow::bail!(
        "boss-jobs-api: postgres_url is required. The in-memory serving branch was deleted \
         (be793304): nothing ever set the opt-in it guarded, every image builds --features postgres, \
         and a system of record whose writes vanish on restart is not a degraded mode but a lie."
    )
}

#[allow(clippy::too_many_arguments)]
async fn run_server<R: JobsRepository + 'static>(
    job_edges: Option<std::sync::Arc<dyn boss_jobs::job_edges::JobEdgesRegistry>>,
    stations: Option<Arc<dyn boss_jobs::StationRegistry>>,
    jobs: Arc<R>,
    bus: Arc<NatsEventBus>,
    publisher: boss_core::publisher::DomainPublisher,
    kind_registry: Option<Arc<dyn boss_jobs::WorkflowRegistry>>,
    plugin_registry: Option<Arc<dyn boss_jobs::StepPluginRegistry>>,
    scheduling: Option<Arc<dyn boss_jobs::scheduling::SchedulingRepository>>,
    cadence: Option<Arc<dyn boss_jobs::cadence::CadenceRepository>>,
    delivery: Option<Arc<dyn boss_jobs::delivery::DeliveryPolicyRepository>>,
    credentials: Option<Arc<dyn boss_jobs::credentials::CredentialsRegistry>>,
    agent_runs: Option<Arc<dyn boss_jobs::agent_runs::AgentRunLog>>,
    calendar: Option<Arc<dyn boss_calendar_client::CalendarClient>>,
    subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    subject_existence: Option<Arc<dyn boss_jobs::subject_existence::SubjectExistenceCheck>>,
    roster: Option<Arc<dyn boss_jobs::owner_resolution::RosterLookup>>,
    clock: Arc<dyn boss_clock_client::ClockClient>,
    cancel_tx: watch::Sender<bool>,
    cancel_rx: watch::Receiver<bool>,
    http_bind: &str,
) -> Result<()> {
    // Start axum HTTP server.
    let step_registry = Arc::new(boss_jobs::step_registry::StepRegistry::v1());
    info!(
        step_types = step_registry.all().len(),
        "step type registry loaded"
    );

    // Policy client: wires boss-policy-api for row-level authorization.
    // Default URL pulled from the boss-ports table — single source
    // of truth shared with `infra/deploy-services.sh`. Override via
    // BOSS_POLICY_URL. The 7060/7250 collision (`bb60c58` +
    // `8bf0f0a`) that motivated boss-ports lived right here.
    let policy_url = std::env::var("BOSS_POLICY_URL").unwrap_or_else(|_| boss_ports::url("policy"));
    // Banner so a port-collision misconfiguration surfaces in
    // journalctl immediately — not 30 minutes later when the
    // landing page won't load — a guard against the historical
    // 7060/7250 port collision.
    tracing::info!(policy_url = %policy_url, "policy client configured");
    // Wrap the prod client in the sim-origin bypass: simulator traffic
    // (x-sim-origin, already stamped _simulated) is authorized at the
    // boundary on the trusted box; real traffic is enforced per-role by
    // the inner ReqwestPolicyClient.
    let policy: Arc<dyn boss_policy_client::PolicyClient> =
        Arc::new(boss_policy_client::SimBypassPolicyClient::new(Arc::new(
            boss_policy_client::ReqwestPolicyClient::new(policy_url),
        )));

    // Wire the sim-mode probe into the publisher so every stamp
    // injects `_simulated: bool` into the audit_log payload without
    // per-handler changes. `publisher` here is `DomainPublisher`
    // (not Option) — wire directly.
    let publisher = publisher.with_sim_probe(Arc::new(boss_clock_client::ClockSimProbe::new(
        clock.clone(),
    )));
    let scheduling_publisher = publisher.clone();

    let state = JobsApiState {
        job_edges,
        stations,
        jobs,
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry,
        plugin_registry,
        calendar,
        subject_kinds,
        subject_existence,
        roster,
        clock: clock.clone(),
        // The yard-status read-model reads these registries directly (the
        // /api/cadence and /api/delivery doors are operator-only, so the
        // browser cannot). Clone the Arc — the same repos back both the
        // operator doors below and this read-model.
        cadence: cadence.clone(),
        delivery: delivery.clone(),
    };
    let mut app = router(state);
    if let Some(repo) = scheduling {
        info!("scheduling routes mounted at /api/scheduling/*");
        app = app.merge(boss_jobs::scheduling::http::router(
            boss_jobs::scheduling::http::SchedulingApiState {
                repo,
                publisher: Some(scheduling_publisher),
                clock: clock.clone(),
            },
        ));
    }
    if let Some(repo) = cadence {
        info!("cadence routes mounted at /api/cadence/*");
        app = app.merge(boss_jobs::cadence::http::router(
            boss_jobs::cadence::http::CadenceApiState { repo },
        ));
    }
    if let Some(repo) = delivery {
        info!("delivery policy routes mounted at /api/delivery/policy/*");
        app = app.merge(boss_jobs::delivery::http::router(
            boss_jobs::delivery::http::DeliveryPolicyApiState { repo },
        ));
    }
    if let Some(registry) = credentials {
        info!("credentials registry routes mounted at /api/credentials (locations, never values)");
        app = app.merge(boss_jobs::credentials::http::router(
            boss_jobs::credentials::http::CredentialsApiState { registry },
        ));
    }
    if let Some(log) = agent_runs {
        info!(
            "agent-run record mounted at /api/agent-runs (+ /cost) and /api/agent-rate-card              (read-only card)"
        );
        app = app.merge(boss_jobs::agent_runs::http::router(
            boss_jobs::agent_runs::http::AgentRunsApiState { log },
        ));
    }
    // Sim-origin middleware: extract x-sim-origin header and set the
    // per-request task-local so the publisher inherits the sim
    // marker. Closes the gap where a sim chain could trigger a
    // non-sim event on a service running with a wall clock.
    let app = app.layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ));
    // The machine door's write gate (7fcd78fa phase 1): when
    // BOSS_MACHINE_TOKEN is set, state-changing requests must carry
    // it. Layered in the binary — this process is the one that knows
    // the door is on a network — and wrapping the merged app so the
    // scheduling/cadence routers are behind the same gate.
    let machine_token = boss_core::machine_token::from_env();
    if machine_token.is_some() {
        info!(
            "machine token configured: writes require {}",
            boss_core::machine_token::HEADER
        );
    } else {
        warn!(
            "no BOSS_MACHINE_TOKEN configured: the machine door accepts unauthenticated writes \
             (7fcd78fa phase 1 is dormant)"
        );
    }
    let app = app.layer(axum::middleware::from_fn(move |req, next| {
        boss_jobs::http::machine_gate::machine_gate(machine_token.clone(), req, next)
    }));
    let http_addr: SocketAddr = http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{http_bind}`"))?;
    let listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("binding HTTP listener on {http_addr}"))?;
    info!(addr = %http_addr, "jobs HTTP API listening");

    let mut http_rx = cancel_rx.clone();
    let http_task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = http_rx.changed().await;
        };
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
        {
            error!(error = %e, "HTTP server exited with error");
        }
    });

    // Wait for Ctrl+C.
    tokio::signal::ctrl_c().await.ok();
    info!("shutdown signal received");
    let _ = cancel_tx.send(true);

    let _ = http_task.await;
    info!("boss-jobs-api shut down cleanly");
    Ok(())
}

/// Reconcile the code-resident platform Workflows against the live
/// registry: insert if missing, refresh bootstrap-owned drift, preserve
/// operator edits — same shape as
/// `boss_policy_client::PolicyRepository::bootstrap_reconcile`.
///
/// `platform_workflows()` IS EMPTY since 2026-09-11, so this is a no-op
/// on every boot and logs `total=0`. That is the destination, not a
/// regression: every platform protocol is a file under
/// infra/platform/workflows/ seeded by `boss-platform-workflow-seed`
/// (insert-if-missing), and the four rows that were reconciled until
/// then are simply no longer reconciled — they keep the version they
/// reached, and an operator's edit to one now survives a pod roll.
///
/// The call stays because the reconcile contract is still a port method
/// with two adapters and its own tests, and because the stats line is
/// where a platform kind reappearing as a Rust literal would announce
/// itself. Retiring reconcile outright is a separate change.
///
/// Logs the stats line on every boot so operators can see the
/// reconcile decision land in real time.
///
/// Each inserted/republished row records `jobs.kind.published`
/// with the row (registry-events invariant), attributed to the
/// named `bootstrap-reconciler` automation at a clock-routed
/// "now" — a boot-time reconcile in sim mode stamps sim time.
async fn reconcile_platform_workflows<R: JobsRepository>(
    registry: &dyn boss_jobs::WorkflowRegistry,
    jobs: &R,
    clock: &Arc<dyn boss_clock_client::ClockClient>,
) {
    use boss_jobs::registry::platform_workflows;
    let defaults = platform_workflows();
    let actor = boss_core::actor::ActorId::Automation("bootstrap-reconciler".into());
    let now = boss_clock_client::now_from(clock).await;
    match registry.bootstrap_reconcile(&defaults, &actor, now).await {
        Ok(stats) => {
            info!(
                inserted = stats.inserted,
                republished = stats.republished,
                preserved = stats.preserved,
                unchanged = stats.unchanged,
                rejected = stats.rejected,
                total = defaults.len(),
                "reconciled platform Workflows"
            );
            if stats.rejected > 0 {
                // A shipped default that fails the viability lint is
                // a code bug, not an operational one — the reconcile
                // already refused to seed it and named it at ERROR.
                tracing::error!(
                    rejected = stats.rejected,
                    "platform Workflow default(s) failed the viability lint and were NOT seeded"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "platform Workflow reconcile failed");
        }
    }
    verify_registry_viability(registry, jobs).await;
}

/// Boot-time viability check over the station registry — the sibling
/// of [`verify_registry_viability`], for the queues rather than the
/// protocols, under the same contract.
///
/// Never exits and never writes. Until 2026-09-08 this retired each
/// unviable row at boot — a persisted write from a boot path, the
/// same defect class that took the system of record down twice on
/// 2026-09-07 through the Workflow check (packet 7752e636). A boot
/// check reports; it does not act. A failure of the CHECK itself is
/// logged and start continues: the station registry is a read surface
/// over packets, and losing the check is not a reason to take the
/// jobs API down. See `boss_jobs::station_quarantine`.
async fn verify_station_viability(stations: &dyn boss_jobs::StationRegistry) {
    match boss_jobs::station_quarantine::check_active_stations_viable(stations).await {
        Ok(report) if !report.unviable.is_empty() => {
            tracing::error!(
                unviable = report.unviable.len(),
                active = report.checked,
                "started with unviable active station(s) — each is named above; nothing was \
                 retired, service is up"
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(error = %e, "boot station viability check could not complete; starting anyway");
        }
    }
}

/// Boot-time viability re-verification: every active Workflow in the
/// registry must still pass the viability lint. A previously-valid
/// spec can become invalid if an upstream StepType's enum domain
/// changes.
///
/// Never exits and never writes. This used to `exit(1)` on the first
/// bad row — one registry row, whole-service outage (2026-08-13) —
/// and then to auto-retire unpinned rows and refuse to start over
/// pinned ones, which on 2026-09-07 crash-looped the system of record
/// over one pinned `incident-post-mortem` Job and silently retired a
/// live `publish-to-github`. A boot check reports; it does not act.
/// See `boss_jobs::workflow_quarantine`.
async fn verify_registry_viability<R: JobsRepository>(
    registry: &dyn boss_jobs::WorkflowRegistry,
    jobs: &R,
) {
    match boss_jobs::workflow_quarantine::check_active_workflows_viable(registry, jobs).await {
        Ok(report) if !report.unviable.is_empty() => {
            tracing::error!(
                unviable = report.unviable.len(),
                active = report.checked,
                "started with unviable active Workflow(s) — each is named above; nothing was \
                 retired, service is up"
            );
        }
        Ok(_) => {}
        Err(e) => {
            // The check itself could not run. Losing the check is not
            // a reason to take the system of record down.
            tracing::error!(error = %e, "boot viability check could not complete; starting anyway");
        }
    }
}
