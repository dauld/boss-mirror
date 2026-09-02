//! boss-dispatcher — core service that auto-assigns ready Steps to
//! role-matched Employees. See lib.rs for the architectural rationale.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use boss_calendar_client::ReqwestCalendarClient;
use boss_dispatcher::config::DispatcherConfig;
use boss_dispatcher::dispatcher::{DispatcherCtx, run_loop};
use boss_dispatcher::http::{HttpState, router};
use boss_dispatcher::liveness::DispatcherLiveness;
use boss_dispatcher::rules::handler::HandlerRegistry;
use boss_dispatcher::rules::helpers_inventory::InventoryHelpers;
use boss_dispatcher::rules::jobs_spawn::JobsSpawn;
use boss_dispatcher::rules::registry::{
    Registry as RuleRegistry, load_active_rules, rules_changed, rules_fingerprint, wait_for_rules,
};
use boss_dispatcher::rules::runner::RulesRunner;
use boss_dispatcher::rules::schedule_runner::{DEFAULT_CATCHUP_CAP, ScheduleRunner};
use boss_dispatcher_handlers::handlers::{
    bill_payment_batch::BillPaymentBatch, commerce_invoice_issue::CommerceInvoiceIssue,
    docs_design_sweep::DocsDesignSweep, docs_flush_queue::DocsFlushQueue,
    estate_alarm::EstateAlarm, estate_compare::EstateCompare, gate_resolve::GateResolve,
    inventory_bill_approve::InventoryBillApprove,
    inventory_overhead_absorb::InventoryOverheadAbsorb,
    inventory_parts_consume::InventoryPartsConsume, inventory_parts_produce::InventoryPartsProduce,
    inventory_po_place::InventoryPoPlace, inventory_receive::InventoryReceive,
    jobs_auto_park::JobsAutoPark, jobs_clear_waiting::JobsClearWaiting,
    jobs_complete_linked_step::JobsCompleteLinkedStep, jobs_complete_step::JobsCompleteStep,
    jobs_subjob_resolve::JobsSubjobResolve, ledger_bill_approve::LedgerBillApprove,
    ledger_payroll_run_submit::LedgerPayrollRunSubmit, ledger_tax_accrue::LedgerTaxAccrue,
    ledger_tax_remit::LedgerTaxRemit, messages_expire_for_job::MessagesExpireForJob,
    messages_notify::MessagesNotify, messages_notify_job_terminal::MessagesNotifyJobTerminal,
    network_census::NetworkCensus, packaging_allocate::PackagingAllocate, people_hire::PeopleHire,
    people_terminate::PeopleTerminate, products_consume::ProductsConsume,
    products_consume_from_invoice::ProductsConsumeFromInvoice, products_produce::ProductsProduce,
    shipping_create::ShippingCreate, webhook_notify::WebhookNotify,
};
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let cfg = DispatcherConfig::default();
    info!(
        nats_url = %cfg.nats_url,
        jobs_api_url = %cfg.jobs_api_url,
        people_api_url = %cfg.people_api_url,
        inventory_api_url = %cfg.inventory_api_url,
        assignment_strategy = ?cfg.assignment_strategy,
        "boss-dispatcher starting"
    );

    let nats_client = async_nats::connect(&cfg.nats_url)
        .await
        .with_context(|| format!("connecting to NATS at {}", cfg.nats_url))?;
    let jetstream = async_nats::jetstream::new(nats_client);

    // `--reset-stream`: dev/regen one-shot. Drop the durable buffer (and its
    // consumers) and recreate it empty, then exit — so a fresh regen doesn't
    // replay the previous run's events against the just-reset database.
    if std::env::args().any(|a| a == "--reset-stream") {
        boss_nats::durable::reset_stream(&jetstream)
            .await
            .context("resetting BOSS_EVENTS stream")?;
        info!("BOSS_EVENTS stream reset; exiting");
        return Ok(());
    }
    // Durable dispatch requires the stream; fatal if JetStream is
    // unavailable — the dispatcher cannot guarantee delivery without it.
    boss_nats::durable::ensure_stream(&jetstream)
        .await
        .context("ensuring BOSS_EVENTS stream (JetStream required for durable dispatch)")?;

    let ctx = Arc::new(DispatcherCtx::new(
        cfg.jobs_api_url.clone(),
        cfg.people_api_url.clone(),
        cfg.assignment_strategy,
    ));
    // Shared consumer liveness — both loops mark it; /api/dispatcher/readyz
    // reads it. Lets readiness probes see the consumers actually bound, not
    // just the process answering /health.
    let live = Arc::new(DispatcherLiveness::default());
    let js_for_loop = jetstream.clone();
    let ctx_for_loop = ctx.clone();
    let live_for_loop = live.clone();
    tokio::spawn(async move {
        if let Err(e) = run_loop(ctx_for_loop, js_for_loop, live_for_loop).await {
            tracing::error!(error = %e, "dispatcher loop exited with error");
        }
    });

    // Postgres pool — the dispatcher loads its rule registry from the
    // append-only versioned `dispatcher_rules` table (replacing the legacy
    // rules.toml file) and serves it at /api/dispatcher/rules.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres for the dispatcher rule registry")?;

    // Load the rule registry from `dispatcher_rules` and start the rules
    // runner alongside the legacy role-assignment loop. They share the NATS
    // connection but subscribe to disjoint topics — the legacy loop owns
    // jobs.step.>, the runner owns whatever the registry declares.
    // Wait out an empty-at-boot rules table instead of accepting it
    // as final (823fcb22 mechanism 2: the one-shot load raced the
    // seed and the runner dead-aired forever). 60s covers any honest
    // init; past it we proceed empty, loudly.
    match wait_for_rules(
        &pool,
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(60),
    )
    .await
    .and_then(RuleRegistry::from_raw)
    {
        Ok(registry) => {
            info!(
                rule_count = registry.rules().len(),
                "rules registry loaded from dispatcher_rules"
            );
            let mut handlers = HandlerRegistry::new();
            handlers.register(JobsSpawn::new(cfg.jobs_api_url.clone()));
            // Auto-park: on a gate-run's green `gate-verdict` step, file
            // the car the `--park-*` intent describes, so a gate-green
            // branch never strands unparked. Needs the clock for a
            // precise gate-step stamp (dock-queue-time). Inert until a
            // rule on `step.done.gate-verdict` is published.
            // The raiser the estate series was recorded for: a HARD
            // finding persisting N consecutive comparisons becomes an
            // urgent packet (a5adfb99). Inert until a rule on
            // jobs.estate.compared is published.
            handlers.register(EstateAlarm::new(cfg.jobs_api_url.clone()));
            handlers.register(JobsAutoPark::new(
                cfg.jobs_api_url.clone(),
                cfg.clock_api_url.clone(),
            ));
            // D7 delegate-subjob write-back: on a child Job's
            // close, resolve the parent delegate-subjob step.
            handlers.register(JobsSubjobResolve::new(cfg.jobs_api_url.clone()));
            // A closed Job wakes its waiters: clears metadata.waiting_on
            // (the '*' job edge) so blocked steps re-evaluate (e9291570).
            handlers.register(JobsClearWaiting::new(cfg.jobs_api_url.clone()));
            // A closing Job completes the open step it was authorized
            // by, on the Job its declared edge names — the merged car
            // → feedback-packet obligation (2c4ae549). Generic: which
            // edge and which steps ride the rule row.
            handlers.register(JobsCompleteLinkedStep::new(cfg.jobs_api_url.clone()));
            // System-completes zero-duration, no-role markers
            // (trigger / outcome / milestone) the moment they go
            // Ready, so a Job flows past its structural checkpoints
            // without an executor. Shares the dispatcher's StepType
            // registry to classify markers (vs. real no-role work
            // like `task`).
            handlers.register(JobsCompleteStep::new(
                cfg.jobs_api_url.clone(),
                ctx.registry.clone(),
            ));
            // Agent gate executor: on step.ready for a gate kind
            // (demand-gate / availability-gate), read real
            // finished-goods stock, decide the outcome, and complete
            // the gate with it — computer-speed, no workforce slot.
            // Shares the StepType registry to classify gates.
            handlers.register(GateResolve::new(
                cfg.jobs_api_url.clone(),
                cfg.products_api_url.clone(),
                ctx.registry.clone(),
            ));
            // Packaging allocation — splits a brewed batch across formats by
            // demand and writes the packaged quantities, so the whole batch
            // always packages (WIP → FG, never dumped).
            handlers.register(PackagingAllocate::new(
                cfg.jobs_api_url.clone(),
                cfg.products_api_url.clone(),
            ));
            // Step-completion handlers — F15 migration. Each
            // is a pure HTTP client to the relevant public API.
            handlers.register(InventoryPoPlace::new(cfg.inventory_api_url.clone()));
            handlers.register(InventoryReceive::new(
                cfg.inventory_api_url.clone(),
                cfg.ledger_api_url.clone(),
            ));
            handlers.register(InventoryBillApprove::new(cfg.inventory_api_url.clone()));
            handlers.register(BillPaymentBatch::new(
                "inventory.bill.payment_batch",
                cfg.inventory_api_url.clone(),
                "/api/inventory/vendor-invoices/batch-pay",
            ));
            handlers.register(InventoryPartsConsume::new(cfg.inventory_api_url.clone()));
            // Production-overhead absorption (DR 1310 / CR <driver
            // expense>) sized rate_cents_per_bbl × batch bbl at runtime —
            // the rate rides the rule args, the batch size the job's own
            // data, so the seed stamps no amounts. Needs the jobs API for
            // the batch-bbl read.
            handlers.register(InventoryOverheadAbsorb::new(
                cfg.jobs_api_url.clone(),
                cfg.inventory_api_url.clone(),
            ));
            handlers.register(InventoryPartsProduce::new(cfg.inventory_api_url.clone()));
            // FG cost basis is derived from the brew's real consumed-input
            // cost, not a plug. The drain-actual-wip basis drains exactly
            // what consume capitalized (the ledger's DR-1310 facts), so the
            // handler needs the jobs + inventory + ledger APIs.
            handlers.register(ProductsProduce::new(
                cfg.products_api_url.clone(),
                cfg.jobs_api_url.clone(),
                cfg.inventory_api_url.clone(),
                cfg.ledger_api_url.clone(),
            ));
            handlers.register(ProductsConsume::new(cfg.products_api_url.clone()));
            // Q2 (inventory-value-conservation): the consume owns COGS.
            // Every issued invoice's FG lines drain stock + recognize
            // COGS through the products surface, replacing commerce's
            // in-tx cross-module UPDATE + the invoice JE's COGS leg.
            handlers.register(ProductsConsumeFromInvoice::new(
                cfg.products_api_url.clone(),
            ));
            handlers.register(CommerceInvoiceIssue::new(cfg.commerce_api_url.clone()));
            handlers.register(ShippingCreate::new(cfg.shipping_api_url.clone()));
            // Outbound integration edge: forward matched events to a
            // configured external webhook (e.g. a regen's simulator
            // playing external counterparties). No-op when
            // BOSS_EVENT_WEBHOOK_URL is unset; the system stays
            // unaware of who, if anyone, is on the other end.
            handlers.register(WebhookNotify::new(cfg.webhook_url.clone()));
            handlers.register(LedgerTaxRemit::new(cfg.ledger_api_url.clone()));
            // Per-production excise-tax accrual (DR 6550 / CR 2320),
            // fired on `step.done.production-produce` — the brewery's
            // federal beer excise liability accrues at packaging time,
            // drained quarterly by the excise-tax-filing Workflow.
            handlers.register(LedgerTaxAccrue::new(cfg.ledger_api_url.clone()));
            handlers.register(LedgerPayrollRunSubmit::new(cfg.ledger_api_url.clone()));
            // General AP bills (rent/utilities/…) → ledger subledger.
            handlers.register(LedgerBillApprove::new(cfg.ledger_api_url.clone()));
            handlers.register(BillPaymentBatch::new(
                "ledger.bill.payment_batch",
                cfg.ledger_api_url.clone(),
                "/api/ledger/bills/pay-run",
            ));
            handlers.register(PeopleHire::new(cfg.people_api_url.clone()));
            handlers.register(PeopleTerminate::new(cfg.people_api_url.clone()));
            // Push notifier: step.ready.* -> message the role's
            // on-call member (the pull-side assignments query is
            // the actual work driver; this is awareness).
            // A recorded design decision queues its doc's flush
            // (cea82de0 link 1; the worker stays operator-run until
            // its tree/remote question is decided).
            handlers.register(DocsFlushQueue::new(cfg.docs_api_url.clone()));
            // Reads the docs corpus AND the jobs board: the level
            // question spans both, which is why it is a sweep rather
            // than anything either service could answer alone.
            handlers.register(DocsDesignSweep::new(
                cfg.docs_api_url.clone(),
                cfg.jobs_api_url.clone(),
            ));
            // The packet-loss census (packet-loss.md, 9fb9904f): count
            // the network's conservation invariant on a clock and land
            // the counts as one jobs.network.census event per firing.
            // Report first — no raiser, no threshold; the series this
            // accumulates is what calibrates one later.
            handlers.register(NetworkCensus::new(cfg.jobs_api_url.clone()));
            // The estate comparison (59ef456a): declared vs observed,
            // fired by each jobs.estate.observed event, recorded as one
            // jobs.estate.compared event per observation. Report first
            // — the raiser comes later, calibrated on this series.
            handlers.register(EstateCompare::new(cfg.jobs_api_url.clone()));
            handlers.register(MessagesNotify::new(
                cfg.people_api_url.clone(),
                cfg.messages_api_url.clone(),
            ));
            // The filer hears how their packet ended, on ANY terminal
            // (David, 2026-08-11: the system may close feedback
            // without the filer approving, but must always tell them
            // the terminal state). Same messages surface, so the
            // deterministic id dedupes on redelivery.
            handlers.register(MessagesNotifyJobTerminal::new(
                cfg.jobs_api_url.clone(),
                cfg.messages_api_url.clone(),
            ));
            // Retire the notifications about a job once it closes
            // (David, 2026-08-14). Unread signals only — the port
            // carries why a direct never expires with the job.
            handlers.register(MessagesExpireForJob::new(cfg.messages_api_url.clone()));
            let helpers = Arc::new(InventoryHelpers::new(
                cfg.inventory_api_url.clone(),
                cfg.jobs_api_url.clone(),
            ));
            // Both runners rebuild per reload iteration below. The
            // schedule runner shares the SAME handlers (jobs.spawn et
            // al.) + the SAME parsed registry as the event runner — only
            // the trigger differs (clock day vs NATS event). Both are
            // Clone (handlers are Arc'd; the registry is parsed Exprs).
            //
            // Live reload (backlog `1e576baf`): the registry used to be
            // frozen at boot, so an authored rule silently did nothing
            // until the next restart. This supervision loop polls a
            // content fingerprint of `dispatcher_rules` and, when it
            // moves, aborts both runners and rebuilds them from a fresh
            // load — rebinding the durable consumer in case the topic
            // set grew. Aborting mid-event is safe by the existing
            // delivery contracts: an unACK'd JetStream message
            // redelivers after ack_wait, an unadvanced log-tail cursor
            // re-presents its row, and handlers are idempotent.
            //
            // Log-as-the-bus, stage 1 (transactional-audit-log Q6):
            // `BOSS_RULES_SOURCE=log` tails audit_log by id cursor
            // instead of the JetStream durable consumer. Default stays
            // jetstream until the cutover is observed on the
            // playground; the flip is one systemd drop-in, the
            // rollback is deleting it.
            let rules_source =
                std::env::var("BOSS_RULES_SOURCE").unwrap_or_else(|_| "jetstream".into());
            let fp = rules_fingerprint(&pool).await.unwrap_or_else(|e| {
                warn!(error = %e, "initial rules fingerprint failed; first poll will reload");
                String::new()
            });
            let js_for_rules = jetstream.clone();
            let clock_url = cfg.clock_api_url.clone();
            let calendar = Arc::new(ReqwestCalendarClient::new(cfg.calendar_api_url.clone()));
            let live_rules = live.clone();
            let pool_rules = pool.clone();
            tokio::spawn(async move {
                let mut registry = registry;
                let mut fp = fp;
                loop {
                    let runner = Arc::new(RulesRunner {
                        registry: registry.clone(),
                        handlers: handlers.clone(),
                        helpers: helpers.clone(),
                    });
                    let ev = {
                        let live = live_rules.clone();
                        if rules_source == "log" {
                            let pool = pool_rules.clone();
                            tokio::spawn(async move {
                                if let Err(e) = runner.run_log_tail(pool, live).await {
                                    tracing::error!(error = %e, "rules log tail exited with error");
                                }
                            })
                        } else {
                            let js = js_for_rules.clone();
                            tokio::spawn(async move {
                                if let Err(e) = runner.run(js, live).await {
                                    tracing::error!(error = %e, "rules runner exited with error");
                                }
                            })
                        }
                    };
                    // Clock-driven schedule runner: fires schedule-triggered
                    // rules on sim-day boundaries off the clock SSE feed.
                    // No-op (returns immediately) when the registry has no
                    // schedule rules.
                    let schedule_runner = Arc::new(ScheduleRunner {
                        registry: registry.clone(),
                        handlers: handlers.clone(),
                        helpers: helpers.clone(),
                        clock_url: clock_url.clone(),
                        pool: pool_rules.clone(),
                        calendar: calendar.clone(),
                        catchup_cap: DEFAULT_CATCHUP_CAP,
                    });
                    let sched = {
                        let live = live_rules.clone();
                        tokio::spawn(async move {
                            if let Err(e) = schedule_runner.run(live).await {
                                tracing::error!(error = %e, "schedule runner exited with error");
                            }
                        })
                    };
                    fp = rules_changed(&pool_rules, &fp, std::time::Duration::from_secs(30)).await;
                    info!(
                        "dispatcher_rules changed — reloading the registry and rebinding runners"
                    );
                    ev.abort();
                    sched.abort();
                    match load_active_rules(&pool_rules)
                        .await
                        .and_then(RuleRegistry::from_raw)
                    {
                        Ok(next) => {
                            info!(rule_count = next.rules().len(), "rules registry reloaded");
                            registry = next;
                        }
                        Err(e) => {
                            // Keep the running registry; the fingerprint has
                            // advanced, so a corrective write re-triggers.
                            tracing::error!(
                                error = %e,
                                "reloaded dispatcher_rules failed to parse; keeping the running registry"
                            );
                        }
                    }
                }
            });
        }
        Err(e) => {
            warn!(error = %e, "failed to load dispatcher_rules registry; runner not started");
        }
    }

    let app = router(HttpState { live, pool });
    let bind: SocketAddr = cfg
        .http_bind
        .parse()
        .with_context(|| format!("invalid http_bind `{}`", cfg.http_bind))?;
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding HTTP listener on {bind}"))?;
    info!(addr = %bind, "boss-dispatcher HTTP listening (health-only surface)");
    axum::serve(listener, app).await?;
    Ok(())
}
