//! Escalation router — auto-messages tenant-defined executives
//! when a critical ticket lands on a top-tier account.
//!
//! Subscribes to `jobs.job.created` events; for each new Job that
//! matches the rule (priority ∈ {emergency, urgent} AND the job's
//! subject is a platinum/gold account), resolves the executive
//! recipients (every Employee whose `role` Class is tagged
//! `metadata.is_executive = true` per the Class registry) via
//! `boss-people` and posts a `signal`-kind message to each via
//! `boss-messages`. Fire-and-forget: any failure is logged, never
//! blocks other events.
//!
//! v1 rule is hard-coded here rather than living in a database
//! table. Promoting to a configurable `escalation_rules` table is
//! the next pass if more than one rule shows up.
//!
//! ## Subject handling
//!
//! - `Subject::Account { id }` → the account_id is right there.
//! - `Subject` of kind `asset` → v1 skips this case; the
//!   asset→account lookup adds an extra HTTP hop and isn't
//!   required to ship the first useful escalation. A follow-up can
//!   add `GET /api/assets/{asset_id}` to cover asset-subject
//!   service tickets.
//! - Other subjects don't map to a account and are skipped.

use std::sync::Arc;
use std::time::Duration;

use boss_core::event::Event;
use boss_core::job::{Job, Priority, Subject};
use boss_core::port::EventBus;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::events::JOB_CREATED;

/// Knobs the escalation router needs at startup. Every field has a
/// reasonable prod default so a local dev loop can spawn it with
/// `EscalationConfig::default()`.
#[derive(Debug, Clone)]
pub struct EscalationConfig {
    pub people_url: String,
    /// Accounts are served by the accounts-api (split out of boss-people),
    /// so the account lookup uses this base; `people_url` is the employee
    /// lookup.
    pub accounts_url: String,
    pub messages_url: String,
    pub request_timeout: Duration,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            people_url: std::env::var("BOSS_PEOPLE_URL")
                .unwrap_or_else(|_| boss_ports::url("people")),
            accounts_url: std::env::var("BOSS_ACCOUNTS_URL")
                .unwrap_or_else(|_| boss_ports::url("accounts")),
            messages_url: std::env::var("BOSS_MESSAGES_URL")
                .unwrap_or_else(|_| boss_ports::url("messages")),
            request_timeout: Duration::from_secs(5),
        }
    }
}

/// Slim view of `/api/people/accounts/{id}` — we only need tier +
/// name to decide whether the escalation fires. The accounts API
/// returns `AccountWithContacts` with `#[serde(flatten)]` on the
/// Account fields, so on the wire `tier` and `name` sit at the
/// top level alongside `contacts`; this struct must decode that
/// flat shape, not a nested `{ "account": { tier, name } }`.
#[derive(Debug, Deserialize)]
struct AccountSummary {
    tier: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct Employee {
    id: String,
    role: String,
    name: String,
}

/// Spawn the escalation subscriber in the background. Returns a
/// `JoinHandle` so the caller can await it on shutdown; normally you
/// just drop the handle and let the task run for the lifetime of the
/// service.
///
/// Delivery: a JetStream durable consumer (`jobs-escalation`) on the
/// BOSS_EVENTS stream — `jobs.job.created` is already in the stream's
/// subjects, so a missed escalation redelivers instead of vanishing
/// (this ran on plain core NATS until 2026-08-08: at-most-once, and a
/// drop silently never notified anyone — feedback `50ff6193`).
/// Redelivery is safe because the signal's deterministic message id
/// collapses duplicates (`signal_payload`). If the durable consumer
/// cannot be opened (no JetStream on this deployment), the router
/// degrades loudly to the old at-most-once subscription rather than
/// disabling itself.
/// Who the router is on the wire. Its signals are sent AS this id
/// (`signal_payload`'s `sender_id`), and every call it makes — the
/// account and roster reads, the sends — carries it as `x-boss-user`.
const ACTOR_ID: &str = "automation:escalation-router";

/// The router's `x-boss-user`: its own automation at operator tier,
/// the shape the dispatcher's rule actor and the gateway's own writes
/// sign with. It sent NO header until 2026-09-25, and the messages door
/// admitted it only because it trusted a headerless caller; that
/// allowance is gone (backlog e84de48e: "a request without the
/// identity header is not trusted"), so an unsigned router would have
/// been refused on every signal — silently, being fire-and-forget.
fn actor_header() -> String {
    serde_json::json!({
        "id": ACTOR_ID,
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// The router's HTTP client: its identity rides as a DEFAULT header, so
/// no call it makes can go out unsigned (the assignment dispatcher's
/// shape, `boss_dispatcher::Dispatcher::new`).
fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    let value = reqwest::header::HeaderValue::from_str(&actor_header())
        .map_err(|e| format!("x-boss-user header value: {e}"))?;
    headers.insert("x-boss-user", value);
    boss_core::machine_token::attach(&mut headers);
    reqwest::Client::builder()
        .timeout(timeout)
        .default_headers(headers)
        .build()
        .map_err(|e| e.to_string())
}

pub fn spawn_router(
    bus: Arc<boss_nats::NatsEventBus>,
    config: EscalationConfig,
) -> tokio::task::JoinHandle<()> {
    let client = match client(config.request_timeout) {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "escalation router: failed to build http client; disabled");
            return tokio::spawn(async {});
        }
    };

    tokio::spawn(async move {
        use futures::StreamExt;
        let js = bus.jetstream();
        match boss_nats::durable::open_durable(
            &js,
            boss_nats::durable::STREAM_NAME,
            "jobs-escalation",
            vec![JOB_CREATED.to_string()],
        )
        .await
        {
            Ok(messages) => {
                info!(
                    people_url = %config.people_url,
                    messages_url = %config.messages_url,
                    "escalation router: durable consumer 'jobs-escalation' on {}",
                    JOB_CREATED,
                );
                messages
                    .for_each(|msg| {
                        let client = client.clone();
                        let config = config.clone();
                        async move {
                            let msg = match msg {
                                Ok(m) => m,
                                Err(e) => {
                                    warn!(error = %e, "escalation router: message stream error");
                                    return;
                                }
                            };
                            // The publisher's envelope IS the serialized
                            // Event — decode it whole; `handle_event`
                            // reads the Job out of `payload` itself.
                            let event: Event = match serde_json::from_slice(&msg.payload) {
                                Ok(ev) => ev,
                                Err(e) => {
                                    debug!(error = %e, "escalation router: non-Event payload (ACK)");
                                    let _ = msg.ack().await;
                                    return;
                                }
                            };
                            // Inherit the triggering event's partition so a
                            // simulated Job's escalation writes simulated
                            // messages (same task-local the dispatcher sets).
                            // A shadow packet's escalation rides the same
                            // not-real chain: it fails closed exactly as a
                            // simulated one does (packet 508cc38c).
                            let partition =
                                boss_core::partition::Partition::from_event_payload(&event.payload);
                            let outcome = boss_core::sim_origin::with_sim_chain(
                                partition.fails_closed(),
                                handle_event(&client, &config, &event),
                            )
                            .await;
                            boss_nats::durable::settle(&msg, outcome).await;
                        }
                    })
                    .await;
                info!("escalation router durable stream closed");
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "escalation router: durable consumer unavailable — falling back \
                     to at-most-once core subscription (a dropped event is a lost \
                     escalation on this deployment)"
                );
                let mut stream = match bus.subscribe(JOB_CREATED).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "escalation router: subscribe failed; disabled");
                        return;
                    }
                };
                while let Some(event) = stream.next().await {
                    if let Err(e) = handle_event(&client, &config, &event).await {
                        warn!(error = %e, "escalation router: handler error");
                    }
                }
                info!("escalation router stream closed");
            }
        }
    })
}

async fn handle_event(
    client: &reqwest::Client,
    config: &EscalationConfig,
    event: &Event,
) -> Result<(), String> {
    let job: Job = match serde_json::from_value(event.payload.clone()) {
        Ok(j) => j,
        Err(_) => {
            // Replay payloads can diverge from the current Job shape.
            // Silently ignore rather than spam logs.
            return Ok(());
        }
    };

    if !priority_triggers(job.priority) {
        return Ok(());
    }
    let Some(account_id) = account_id_for_subject(&job.subject) else {
        return Ok(());
    };

    let account = fetch_account(client, &config.accounts_url, &account_id).await?;
    if !tier_triggers(&account.tier) {
        debug!(
            tier = %account.tier,
            account = %account_id,
            "escalation: account tier below threshold"
        );
        return Ok(());
    }

    let employees = fetch_employees(client, &config.people_url).await?;
    let recipients: Vec<&Employee> = employees
        .iter()
        .filter(|e| boss_core::roles::is_executive(&e.role))
        .collect();
    if recipients.is_empty() {
        warn!("escalation: no executive-role employees found; skipping");
        return Ok(());
    }

    let subject = format!(
        "[Escalation] {priority} ticket on {tier} account {name}",
        priority = priority_label(job.priority),
        tier = account.tier,
        name = account.name,
    );
    let body = format!(
        "A {priority}-priority {kind} job landed on {name} ({tier} tier).\n\n\
        Job: {title}\nJob ID: {job_id}\n\n\
        Open the Job detail to review and assign follow-up.",
        priority = priority_label(job.priority),
        kind = job.kind,
        name = account.name,
        tier = account.tier,
        title = job.title,
        job_id = job.id,
    );

    for recipient in recipients {
        send_signal(
            client,
            &config.messages_url,
            &recipient.id,
            &subject,
            &body,
            &job.id.to_string(),
        )
        .await
        .map_err(|e| {
            format!(
                "sending signal to {} ({}): {e}",
                recipient.name, recipient.id
            )
        })?;
    }

    info!(
        account = %account_id,
        tier = %account.tier,
        priority = %priority_label(job.priority),
        job = %job.id,
        "escalation: signals sent"
    );
    Ok(())
}

fn priority_triggers(p: Priority) -> bool {
    matches!(p, Priority::Emergency | Priority::Urgent)
}

fn tier_triggers(tier: &str) -> bool {
    matches!(tier, "platinum" | "gold")
}

fn account_id_for_subject(subject: &Subject) -> Option<String> {
    use boss_core::primitives::Subject as _;
    // System / PurchaseOrder / Campaign / Employee / Custom don't
    // map to a account here. System-subject tickets are the most
    // common gap; a follow-up can add an assets lookup.
    if subject.kind() == "account" {
        Some(subject.id().to_string())
    } else {
        None
    }
}

fn priority_label(p: Priority) -> &'static str {
    match p {
        Priority::Emergency => "emergency",
        Priority::Urgent => "urgent",
        Priority::Standard => "standard",
        Priority::Scheduled => "scheduled",
    }
}

async fn fetch_account(
    client: &reqwest::Client,
    accounts_url: &str,
    account_id: &str,
) -> Result<AccountSummary, String> {
    let url = format!("{accounts_url}/api/people/accounts/{account_id}");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: status {}", resp.status()));
    }
    resp.json::<AccountSummary>()
        .await
        .map_err(|e| format!("decode {url}: {e}"))
}

async fn fetch_employees(
    client: &reqwest::Client,
    people_url: &str,
) -> Result<Vec<Employee>, String> {
    let url = format!("{people_url}/api/people");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: status {}", resp.status()));
    }
    resp.json().await.map_err(|e| format!("decode {url}: {e}"))
}

/// The wire payload for one escalation signal. Pure so the dedup
/// contract is unit-testable: the deterministic `id` collapses
/// redelivered events on the messages `ON CONFLICT (id) DO NOTHING`
/// insert (same shape as the dispatcher's `notify:{step}:{recipient}`)
/// — under at-least-once delivery a fresh id per attempt would
/// double-message every executive.
fn signal_payload(
    recipient_id: &str,
    subject: &str,
    body: &str,
    job_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": format!("escalation:{job_id}:{recipient_id}"),
        "sender_id": ACTOR_ID,
        "recipient_id": recipient_id,
        "subject": subject,
        "body": body,
        "kind": "signal",
        "entity_ref": {
            "entity_type": "job",
            "entity_id": job_id,
            "entity_path": format!("/jobs/{job_id}"),
        },
    })
}

async fn send_signal(
    client: &reqwest::Client,
    messages_url: &str,
    recipient_id: &str,
    subject: &str,
    body: &str,
    job_id: &str,
) -> Result<(), String> {
    let url = format!("{messages_url}/api/messages/send");
    let body_json = signal_payload(recipient_id, subject, body, job_id);
    // Say the sim-ness out loud rather than relying on absence — the
    // dispatcher handlers stamp every downstream call the same way. A
    // simulated Job's escalation must write a simulated message, or
    // rebuilds diverge on the marker.
    let sim_origin = if boss_core::sim_origin::is_in_sim_chain() {
        "true"
    } else {
        "false"
    };
    let resp = client
        .post(&url)
        .header("x-sim-origin", sim_origin)
        .json(&body_json)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("POST {url}: status {}", resp.status()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_emergency_and_urgent_trigger() {
        assert!(priority_triggers(Priority::Emergency));
        assert!(priority_triggers(Priority::Urgent));
        assert!(!priority_triggers(Priority::Standard));
        assert!(!priority_triggers(Priority::Scheduled));
    }

    #[test]
    fn tier_only_top_two_trigger() {
        assert!(tier_triggers("platinum"));
        assert!(tier_triggers("gold"));
        assert!(!tier_triggers("silver"));
        assert!(!tier_triggers("bronze"));
    }

    #[test]
    fn account_subject_extracts_id() {
        let s = Subject::new("account", "account-42");
        assert_eq!(account_id_for_subject(&s), Some("account-42".into()));
    }

    #[test]
    fn asset_subject_skipped_in_v1() {
        let s = Subject::new("asset", "SYS-123");
        assert_eq!(account_id_for_subject(&s), None);
    }

    /// Pin the wire-shape contract for `/api/people/accounts/{id}`.
    /// boss-accounts returns `AccountWithContacts` with
    /// `#[serde(flatten)]` on the `Account` field — so `tier` and
    /// `name` sit at the top level alongside `contacts`. Pre-fix
    /// the escalation router decoded into `AccountBundle { account:
    /// AccountSummary }` expecting a wrapper key that never
    /// existed; every escalation event silently dropped during
    /// regen.
    ///
    /// If the accounts API ever changes its response shape (drops
    /// the flatten, renames `tier`, etc.) this test breaks loudly
    /// instead of letting the escalation router silently no-op.
    #[test]
    fn signal_payload_carries_the_deterministic_dedup_id() {
        let p = signal_payload("emp-ceo", "subj", "body", "job-123");
        assert_eq!(
            p["id"], "escalation:job-123:emp-ceo",
            "redelivered events must collapse on the messages ON CONFLICT (id) \
             insert — a fresh id per attempt double-messages executives"
        );
        assert_eq!(p["recipient_id"], "emp-ceo");
        assert_eq!(p["kind"], "signal");
        assert_eq!(p["entity_ref"]["entity_id"], "job-123");
    }

    /// EVERY CALL THE ROUTER MAKES IS SIGNED (backlog e84de48e). The
    /// messages door stopped trusting a request with no `x-boss-user`
    /// on 2026-09-25, and the router sent none. A stub messages service
    /// records what arrives on a real socket: the send carries the
    /// router's own automation at operator tier, and that identity is
    /// the sender the signal claims to be — so it passes the door's
    /// sender match even without the operator tier.
    #[tokio::test]
    async fn a_signal_is_sent_signed_as_the_router() {
        use axum::http::HeaderMap;
        use std::sync::Mutex;
        type Seen = Arc<Mutex<Vec<(Option<String>, serde_json::Value)>>>;
        let seen: Seen = Arc::default();
        let app = axum::Router::new().route(
            "/api/messages/send",
            axum::routing::post({
                let seen = seen.clone();
                move |headers: HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
                    let seen = seen.clone();
                    async move {
                        let user = headers
                            .get("x-boss-user")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        seen.lock().unwrap().push((user, body));
                        axum::http::StatusCode::CREATED
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = client(Duration::from_secs(5)).expect("the client builds");
        send_signal(&http, &base, "emp-ceo", "subj", "body", "job-1")
            .await
            .expect("the stub accepts the signal");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let (user, body) = &seen[0];
        let user: boss_policy_client::User =
            serde_json::from_str(user.as_deref().expect("the send carried no x-boss-user"))
                .expect("the header is a User");
        assert_eq!(user.id, ACTOR_ID);
        assert_eq!(user.access_tier, boss_policy_client::AccessTier::Operator);
        assert_eq!(body["sender_id"], user.id.as_str());
    }

    /// The dispatcher shipped a consumer whose filter named a subject
    /// the stream doesn't ingest — silent zero deliveries, pinned by
    /// its own seed test. Same guard here: the escalation filter must
    /// be covered by the durable stream's subjects, or the durable
    /// swap silently consumes nothing.
    #[test]
    fn escalation_subject_is_captured_by_the_durable_stream() {
        let covered = boss_nats::durable::stream_subjects().iter().any(|s| {
            s == JOB_CREATED || (s.ends_with('>') && JOB_CREATED.starts_with(&s[..s.len() - 1]))
        });
        assert!(
            covered,
            "{JOB_CREATED} is not covered by stream_subjects() — the durable \
             escalation consumer would open cleanly and deliver nothing"
        );
    }

    #[test]
    fn account_summary_decodes_flat_wire_shape() {
        let body = serde_json::json!({
            "id": "account-42",
            "name": "Hopswell Brewing",
            "director": "Pat Director",
            "city": "Austin",
            "state": "TX",
            "tier": "gold",
            "customer_since": "2025-06-01",
            "territory_rep_id": "emp-rep-001",
            "account_type": "wholesale-distributor",
            "contacts": [],
        });
        let summary: AccountSummary =
            serde_json::from_value(body).expect("AccountSummary decodes the flat wire shape");
        assert_eq!(summary.tier, "gold");
        assert_eq!(summary.name, "Hopswell Brewing");
    }
}
