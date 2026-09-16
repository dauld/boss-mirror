//! `credential.rotate.cloudflare-tunnel` — the credential broker's
//! second rotation handler (packet 04e5f833; David 2026-09-16: "avoid
//! doing anything by hand unless absolutely required").
//!
//! Rotates the Cloudflare Tunnel credential end-to-end as MACHINE
//! steps of a `rotate-a-credential` packet, the exact shape of
//! `credential_rotate_forgejo.rs` with a different issuer:
//!
//! - **issue** — create a NEW tunnel via the account API, named from
//!   the packet id (the idempotence ledger), with a client-generated
//!   32-byte secret, as `cloudflared tunnel create` itself does.
//! - **install** — write the connector's `credentials.json` into the
//!   declared k8s Secret, point every declared hostname at
//!   `<tunnel id>.cfargotunnel.com` (proxied CNAME), and restart the
//!   declared connector Deployment — cloudflared reads its credentials
//!   file ONCE, at start (`readTunnelCredentials` in
//!   `cmd/cloudflared/tunnel/subcommand_context.go`), and the tunnel id
//!   inside it changes with every rotation, so a running connector
//!   never follows the Secret on its own.
//! - **verify** — a connector reports connected on the new tunnel AND
//!   the verify hostname answers 2xx at `/health` through the edge. A
//!   bounded in-invocation wait, then "not yet" (a NAK): JetStream's
//!   ack window is 30 s (`boss_nats::durable::ACK_WAIT`), so a longer
//!   wait here would be redelivered mid-run and mint twice; the
//!   redelivery schedule IS the honest wait (~4 min across the budget),
//!   each retry converging on the already-installed tunnel.
//! - **revoke** — delete the OLD tunnel the scope step named in
//!   `old_token` (a tunnel NAME, never a value), which invalidates every
//!   connector token minted for it — but ONLY when it shows zero live
//!   connections. The boss-gcp connector stays attached to the old
//!   tunnel until 0b7804f3 retires it, and deleting a tunnel under a
//!   live connector is deleting somebody's traffic; so the handler
//!   records "still has N connections — revoke deferred" on the revoke
//!   step and leaves it open, rather than guessing.
//!
//! Before any of that: a DECLARED connector Deployment that does not
//! exist is a permanent refusal (nothing minted) — the install would
//! point the public hostnames at a tunnel nobody serves.
//!
//! Each phase lands a `credential.*` event through the registry's
//! rotation door and completes the packet's own step; THE SECRET
//! VALUE NEVER ENTERS A PACKET OR AN EVENT — evidence is identifiers
//! (tunnel name/id, Secret path, byte length, DNS targets) and observed
//! effects. The rule row carries the credential's consumer declaration
//! as args (zone, Secret, hostnames, verify hostname, connector
//! Deployment) — per-credential registry data, not code. Until the
//! zone-as-data car (5e58922c) lands, the `hostnames` arg IS the
//! declaration of which records point at the tunnel.
//!
//! ## Idempotence (per rotation packet)
//!
//! The tunnel's name is derived from the packet id, and the installed
//! Secret names its tunnel id, so the account is the ledger:
//!   - name absent                                  → mint fresh.
//!   - name present AND the Secret's TunnelID = its id → converge only.
//!   - name present but the Secret does NOT name it   → a prior attempt
//!     minted and died before installing; the secret is unrecoverable
//!     (it was generated here), so delete the orphan and mint again.
//!
//! Steps are completed as each phase becomes true (issue+install right
//! after the install, not at the end as the forgejo handler does),
//! because verify here legitimately spans several redeliveries and
//! the packet should show the mint and install as soon as they are
//! facts.

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use boss_jobs::credentials::RotationPhase;
use serde_json::{Value as JsonValue, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::common::{StepEvent, dispatcher_actor_header, dispatcher_reader_header};
use super::credential_issuer::{
    CloudflareTunnels, SecretStore, TunnelInfo, WorkloadRestarter, fresh_tunnel_secret_b64,
    installed_tunnel_id, tunnel_credentials_json,
};

/// The public path verified through the edge: the gateway's
/// unauthenticated health route (`boss-gateway` `is_public_path`).
pub const VERIFY_PATH: &str = "/health";

// ---------------------------------------------------------------------------
// Pure planning — the decision under test
// ---------------------------------------------------------------------------

/// The new tunnel's name: `{prefix}-{first 8 of the packet id}`. The
/// packet id is the rotation's identity, so the name is the
/// idempotence key; the prefix keeps the account's tunnel list
/// legible.
pub fn rotation_tunnel_name(prefix: &str, job_id: &str) -> String {
    let short: String = job_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    format!("{prefix}-{short}")
}

/// The CNAME target a hostname points at to reach a tunnel.
pub fn tunnel_cname_target(tunnel_id: &str) -> String {
    format!("{tunnel_id}.cfargotunnel.com")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationPlan {
    /// This packet's tunnel exists and the Secret names it: mint and
    /// install already happened. Converge the rest only.
    AlreadyInstalled { tunnel_id: String },
    /// This packet's tunnel exists but the Secret does not name it: a
    /// prior attempt lost the secret. Delete the orphan, mint again.
    ReplaceStale { orphan_id: String },
    /// Nothing from this packet on the account yet.
    MintFresh,
}

pub fn plan_rotation(existing: Option<&TunnelInfo>, installed_id: Option<&str>) -> RotationPlan {
    match existing {
        None => RotationPlan::MintFresh,
        Some(t) => match installed_id {
            Some(id) if id == t.id => RotationPlan::AlreadyInstalled {
                tunnel_id: t.id.clone(),
            },
            _ => RotationPlan::ReplaceStale {
                orphan_id: t.id.clone(),
            },
        },
    }
}

/// What one DNS upsert did — evidence, not a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsOutcome {
    Created,
    Updated,
    Unchanged,
}

impl DnsOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            DnsOutcome::Created => "created",
            DnsOutcome::Updated => "updated",
            DnsOutcome::Unchanged => "unchanged",
        }
    }
}

/// How long one invocation waits for the connector before answering
/// "not yet". Bounded well inside the 30 s ack window — the
/// redelivery schedule carries the rest of the wait.
#[derive(Debug, Clone, Copy)]
pub struct VerifyPoll {
    pub attempts: u32,
    pub interval: Duration,
}

impl Default for VerifyPoll {
    fn default() -> Self {
        Self {
            attempts: 3,
            interval: Duration::from_secs(3),
        }
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

pub struct CredentialRotateCloudflareTunnel {
    client: reqwest::Client,
    jobs_base: String,
    cloudflare: Arc<dyn CloudflareTunnels>,
    secrets: Arc<dyn SecretStore>,
    workloads: Arc<dyn WorkloadRestarter>,
    poll: VerifyPoll,
}

/// One step of the rotation packet as the jobs-api lists it.
struct StepView {
    id: String,
    status: String,
    metadata: serde_json::Map<String, JsonValue>,
}

/// The rule row's consumer declaration, parsed once.
struct Declaration<'a> {
    zone: &'a str,
    account_id: Option<&'a str>,
    secret_namespace: &'a str,
    secret_name: &'a str,
    secret_key: &'a str,
    tunnel_name_prefix: &'a str,
    hostnames: Vec<&'a str>,
    verify_hostname: &'a str,
    credential_id: &'a str,
    restart_deployment: Option<&'a str>,
}

fn optional_arg<'a>(args: &'a [(String, Value)], name: &str) -> Option<&'a str> {
    match arg(args, name) {
        Some(Value::String(s)) => Some(s.trim()).filter(|s| !s.is_empty()),
        _ => None,
    }
}

impl<'a> Declaration<'a> {
    fn parse(args: &'a [(String, Value)]) -> Result<Self, HandlerError> {
        Ok(Self {
            zone: arg_string(args, "zone")?,
            account_id: optional_arg(args, "account_id"),
            secret_namespace: arg_string(args, "secret_namespace")?,
            secret_name: arg_string(args, "secret_name")?,
            secret_key: arg_string(args, "secret_key")?,
            tunnel_name_prefix: arg_string(args, "tunnel_name_prefix")?,
            hostnames: arg_string(args, "hostnames")?
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect(),
            verify_hostname: arg_string(args, "verify_hostname")?,
            credential_id: arg_string(args, "credential_id")?,
            restart_deployment: optional_arg(args, "restart_deployment"),
        })
    }
}

impl CredentialRotateCloudflareTunnel {
    pub fn new(
        jobs_base: impl Into<String>,
        cloudflare: Arc<dyn CloudflareTunnels>,
        secrets: Arc<dyn SecretStore>,
        workloads: Arc<dyn WorkloadRestarter>,
    ) -> Arc<Self> {
        Self::with_poll(
            jobs_base,
            cloudflare,
            secrets,
            workloads,
            VerifyPoll::default(),
        )
    }

    pub fn with_poll(
        jobs_base: impl Into<String>,
        cloudflare: Arc<dyn CloudflareTunnels>,
        secrets: Arc<dyn SecretStore>,
        workloads: Arc<dyn WorkloadRestarter>,
        poll: VerifyPoll,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: super::common::api_client(),
            jobs_base: jobs_base.into(),
            cloudflare,
            secrets,
            workloads,
            poll,
        })
    }

    fn jobs(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// The packet's steps keyed by spec slug. One read serves every
    /// completion below.
    async fn fetch_steps(&self, job_id: &str) -> Result<HashMap<String, StepView>, HandlerError> {
        let url = format!("{}/api/jobs/{job_id}", self.jobs());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", super::common::sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {text}"
            )));
        }
        let body: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{url}: {e}")))?;
        let mut out = HashMap::new();
        for s in body
            .get("steps")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let (Some(slug), Some(id), Some(status)) = (
                s.get("spec_slug").and_then(|v| v.as_str()),
                s.get("id").and_then(|v| v.as_str()),
                s.get("status").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            out.insert(
                slug.to_string(),
                StepView {
                    id: id.to_string(),
                    status: status.to_string(),
                    metadata: s
                        .get("metadata")
                        .and_then(|v| v.as_object())
                        .cloned()
                        .unwrap_or_default(),
                },
            );
        }
        Ok(out)
    }

    /// PUT one step: merged metadata, and `status: completed` when
    /// `complete` (PATCH-on-PUT replaces `metadata` wholesale, so the
    /// existing keys ride along). Already-completed steps are left
    /// alone — the redelivery path. A slug the packet lacks is
    /// skipped: the packet's workflow version decides which phases it
    /// records.
    async fn put_step(
        &self,
        rule_name: &str,
        job_id: &str,
        steps: &HashMap<String, StepView>,
        slug: &str,
        evidence: &[(&str, String)],
        complete: bool,
    ) -> Result<(), HandlerError> {
        let Some(step) = steps.get(slug) else {
            tracing::warn!(job_id, slug, "rotation packet has no such step; skipping");
            return Ok(());
        };
        if step.status == "completed" {
            return Ok(());
        }
        let mut metadata = step.metadata.clone();
        for (k, v) in evidence {
            metadata.insert((*k).to_string(), json!(v));
        }
        if !complete && metadata == step.metadata {
            // An annotation that says what the step already says is
            // a write with no information in it.
            return Ok(());
        }
        let mut body = json!({ "metadata": metadata });
        if complete {
            body["status"] = json!("completed");
        }
        let url = format!("{}/api/jobs/{job_id}/steps/{}", self.jobs(), step.id);
        let resp = self
            .client
            .put(&url)
            .header("Content-Type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(rule_name))
            .header("x-sim-origin", super::common::sim_origin_value())
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {url} returned {status}: {text}"
            )));
        }
        Ok(())
    }

    async fn complete_step(
        &self,
        rule_name: &str,
        job_id: &str,
        steps: &HashMap<String, StepView>,
        slug: &str,
        evidence: &[(&str, String)],
    ) -> Result<(), HandlerError> {
        self.put_step(rule_name, job_id, steps, slug, evidence, true)
            .await
    }

    /// Land one rotation phase on the log through the registry's
    /// rotation door (`POST /api/credentials/{id}/rotation/{phase}`).
    async fn record_phase(
        &self,
        rule_name: &str,
        credential_id: &str,
        phase: RotationPhase,
        evidence: JsonValue,
    ) -> Result<(), HandlerError> {
        let url = format!(
            "{}/api/credentials/{credential_id}/rotation/{}",
            self.jobs(),
            phase.as_str()
        );
        super::common::post_json(&self.client, &url, &evidence, rule_name).await
    }

    /// Point one hostname at the tunnel: a proxied CNAME, created if
    /// absent, corrected if it says anything else, left alone if it
    /// already says this. Idempotent by content.
    async fn upsert_cname(
        &self,
        zone_id: &str,
        hostname: &str,
        target: &str,
        comment: &str,
    ) -> Result<DnsOutcome, String> {
        match self
            .cloudflare
            .find_dns_record(zone_id, "CNAME", hostname)
            .await?
        {
            None => {
                self.cloudflare
                    .create_dns_record(zone_id, "CNAME", hostname, target, true, comment)
                    .await?;
                Ok(DnsOutcome::Created)
            }
            Some(rec) if rec.content == target && rec.proxied => Ok(DnsOutcome::Unchanged),
            Some(rec) => {
                self.cloudflare
                    .update_dns_record(zone_id, &rec.id, target, true, comment)
                    .await?;
                Ok(DnsOutcome::Updated)
            }
        }
    }
}

#[async_trait]
impl Handler for CredentialRotateCloudflareTunnel {
    fn name(&self) -> &'static str {
        "credential.rotate.cloudflare-tunnel"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        let decl = Declaration::parse(args)?;
        let rule = ctx.rule_name.as_str();

        // Per-rotation facts off the scope step: the old tunnel's
        // NAME (never a value), if the scoper knows it.
        let old_tunnel = ev
            .metadata
            .get("old_token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let tunnel_name = rotation_tunnel_name(decl.tunnel_name_prefix, ev.job_id);
        // Refused BEFORE any side effect: a scope step that names this
        // rotation's own replacement as the kill target is an
        // authoring fault, not a transient.
        if old_tunnel == Some(tunnel_name.as_str()) {
            return Err(HandlerError::Permanent(format!(
                "old_token {tunnel_name:?} names this rotation's own replacement"
            )));
        }

        // One read serves the event-emission guards AND the step
        // completions: a completed step means its event already
        // landed (events land before steps), so do not re-emit.
        let steps = self.fetch_steps(ev.job_id).await?;
        let step_done = |slug: &str| steps.get(slug).is_some_and(|s| s.status == "completed");

        // The zone answers both ids the rest needs. Zone:Read is in
        // the root grant; `GET /accounts` would need Account
        // Settings:Read, which the ceremony did not mint.
        let zone = self
            .cloudflare
            .zone(decl.zone)
            .await
            .map_err(HandlerError::Downstream)?;
        let account_id = decl.account_id.unwrap_or(zone.account_id.as_str());

        // A declared connector must exist before anything is minted:
        // the install repoints the public hostnames at the new tunnel,
        // and a tunnel nobody serves is an outage. Refused permanently
        // — the connector car landing later does not re-fire a scope
        // step already completed; a fresh rotation packet does.
        if let Some(name) = decl.restart_deployment
            && !self
                .workloads
                .deployment_exists(decl.secret_namespace, name)
                .await
                .map_err(HandlerError::Downstream)?
        {
            return Err(HandlerError::Permanent(format!(
                "connector deployment {}/{name} does not exist: rotating now would point {} at a                  tunnel no connector serves. Land the connector (5a2bb0ce), then open a fresh                  rotate-a-credential packet; nothing was minted.",
                decl.secret_namespace,
                decl.hostnames.join(", ")
            )));
        }

        // Plan against the account's ledger + the installed Secret.
        let existing = self
            .cloudflare
            .find_tunnel(account_id, &tunnel_name)
            .await
            .map_err(HandlerError::Downstream)?;
        let installed = self
            .secrets
            .read_key(decl.secret_namespace, decl.secret_name, decl.secret_key)
            .await
            .map_err(HandlerError::Downstream)?;
        let installed_id = installed.as_deref().and_then(installed_tunnel_id);
        let plan = plan_rotation(existing.as_ref(), installed_id.as_deref());

        // issue + install (or converge if a prior run already did).
        // `fresh_creds` is Some only when this invocation minted: the
        // credentials file exists in this binding and in the Secret,
        // nowhere else, and the converge arm never touches a value.
        let (tunnel_id, fresh_creds, replaced_orphan) = match plan {
            RotationPlan::AlreadyInstalled { tunnel_id } => (tunnel_id, None, false),
            RotationPlan::ReplaceStale { orphan_id } => {
                // Orphan from a died attempt; its secret is gone for
                // good (generated here, never stored), so retire it
                // before re-minting.
                self.cloudflare
                    .delete_tunnel(account_id, &orphan_id)
                    .await
                    .map_err(HandlerError::Downstream)?;
                let secret = fresh_tunnel_secret_b64();
                let id = self
                    .cloudflare
                    .create_tunnel(account_id, &tunnel_name, &secret)
                    .await
                    .map_err(HandlerError::Downstream)?;
                let creds = tunnel_credentials_json(account_id, &id, &secret);
                (id, Some(creds), true)
            }
            RotationPlan::MintFresh => {
                let secret = fresh_tunnel_secret_b64();
                let id = self
                    .cloudflare
                    .create_tunnel(account_id, &tunnel_name, &secret)
                    .await
                    .map_err(HandlerError::Downstream)?;
                let creds = tunnel_credentials_json(account_id, &id, &secret);
                (id, Some(creds), false)
            }
        };
        let minted_now = fresh_creds.is_some();
        let value_length = fresh_creds
            .as_deref()
            .or(installed.as_deref())
            .map(str::len)
            .unwrap_or(0);

        if minted_now {
            self.record_phase(
                rule,
                decl.credential_id,
                RotationPhase::Minted,
                json!({
                    "job_id": ev.job_id,
                    "tunnel_name": tunnel_name,
                    "tunnel_id": tunnel_id,
                    "account_id": account_id,
                    "config_src": "local",
                    "replaced_orphan": replaced_orphan,
                }),
            )
            .await?;
        }

        // Install: the Secret first (the connector's file), then DNS,
        // then the connector restart. DNS is re-upserted on the
        // converge path too — idempotent by content, and a prior run
        // may have died between the Secret write and the CNAMEs.
        if let Some(creds) = fresh_creds.as_deref() {
            self.secrets
                .write_key(
                    decl.secret_namespace,
                    decl.secret_name,
                    decl.secret_key,
                    creds,
                )
                .await
                .map_err(HandlerError::Downstream)?;
        }
        let cname_target = tunnel_cname_target(&tunnel_id);
        let comment = format!("BOSS tunnel {tunnel_name} (rotation packet {})", ev.job_id);
        let mut dns_evidence: Vec<String> = Vec::with_capacity(decl.hostnames.len());
        for host in &decl.hostnames {
            let outcome = self
                .upsert_cname(&zone.zone_id, host, &cname_target, &comment)
                .await
                .map_err(HandlerError::Downstream)?;
            dns_evidence.push(format!(
                "{host} CNAME {cname_target} proxied ({})",
                outcome.as_str()
            ));
        }
        // The connector reads its credentials file at start, so a
        // fresh install restarts it. Only when minted NOW: a converge
        // pass re-runs on every redelivery while verify waits, and
        // restarting the connector on each would keep it from ever
        // connecting.
        let restart_evidence = match (minted_now, decl.restart_deployment) {
            (true, Some(name)) => {
                let restarted = self
                    .workloads
                    .restart_deployment(
                        decl.secret_namespace,
                        name,
                        &format!("credential-rotation {} tunnel {tunnel_id}", ev.job_id),
                    )
                    .await
                    .map_err(HandlerError::Downstream)?;
                if restarted {
                    format!(
                        "deployment {}/{name} rollout-restarted to reread the file",
                        decl.secret_namespace
                    )
                } else {
                    format!(
                        "deployment {}/{name} not found — no connector declared here yet; nothing restarted",
                        decl.secret_namespace
                    )
                }
            }
            (true, None) => "no connector deployment declared; nothing restarted".to_string(),
            (false, _) => "converged; connector not restarted again".to_string(),
        };
        if minted_now || !step_done("install") {
            self.record_phase(
                rule,
                decl.credential_id,
                RotationPhase::Installed,
                json!({
                    "job_id": ev.job_id,
                    "tunnel_name": tunnel_name,
                    "tunnel_id": tunnel_id,
                    "secret_namespace": decl.secret_namespace,
                    "secret_name": decl.secret_name,
                    "secret_key": decl.secret_key,
                    "value_length": value_length,
                    "dns": dns_evidence,
                    "connector": restart_evidence,
                    "converged": !minted_now,
                }),
            )
            .await?;
        }

        // The mint and the install are facts now; the packet shows
        // them before verify starts its (possibly multi-delivery) wait.
        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "issue",
            &[
                (
                    "issued",
                    format!(
                        "cloudflare tunnel {tunnel_name} (id {tunnel_id}) created via \
                         POST /accounts/{account_id}/cfd_tunnel, config_src local"
                    ),
                ),
                (
                    "issuer",
                    format!(
                        "credential-broker ({rule}), root credential boss-credential-broker-root key cloudflare-token"
                    ),
                ),
            ],
        )
        .await?;
        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "install",
            &[
                (
                    "installed",
                    format!(
                        "k8s Secret {}/{} key {} updated ({value_length} bytes: credentials.json \
                         naming tunnel {tunnel_id}); DNS: {}; {restart_evidence}",
                        decl.secret_namespace,
                        decl.secret_name,
                        decl.secret_key,
                        dns_evidence.join("; "),
                    ),
                ),
                (
                    "permissions",
                    "the tunnel's own credentials: connect as this tunnel, nothing else"
                        .to_string(),
                ),
            ],
        )
        .await?;

        // Verify by effect BEFORE anything destructive: a connector is
        // on the new tunnel and the public hostname answers through
        // the edge. Bounded here; "not yet" hands the wait to the
        // redelivery schedule.
        let mut observed = (0usize, None::<Result<u16, String>>);
        let mut verified = false;
        for attempt in 0..self.poll.attempts.max(1) {
            if attempt > 0 && !self.poll.interval.is_zero() {
                tokio::time::sleep(self.poll.interval).await;
            }
            let conns = self
                .cloudflare
                .tunnel_connections(account_id, &tunnel_id)
                .await
                .map_err(HandlerError::Downstream)?;
            let edge = self
                .cloudflare
                .edge_status(decl.verify_hostname, VERIFY_PATH)
                .await;
            let edge_ok = matches!(edge, Ok(s) if (200..300).contains(&s));
            observed = (conns, Some(edge));
            if conns > 0 && edge_ok {
                verified = true;
                break;
            }
        }
        let (connections, edge) = observed;
        let edge_text = match &edge {
            Some(Ok(s)) => s.to_string(),
            Some(Err(e)) => format!("transport error: {e}"),
            None => "not probed".to_string(),
        };
        if !verified {
            return Err(HandlerError::Downstream(format!(
                "not yet: tunnel {tunnel_name} ({tunnel_id}) has {connections} live connections \
                 and https://{}{VERIFY_PATH} answered {edge_text}; old tunnel NOT revoked",
                decl.verify_hostname
            )));
        }
        if !step_done("verify") {
            self.record_phase(
                rule,
                decl.credential_id,
                RotationPhase::Verified,
                json!({
                    "job_id": ev.job_id,
                    "tunnel_name": tunnel_name,
                    "tunnel_id": tunnel_id,
                    "connections": connections,
                    "verify_hostname": decl.verify_hostname,
                    "edge_status": edge_text,
                    "method": "api",
                }),
            )
            .await?;
        }
        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "verify",
            &[
                (
                    "verified",
                    format!(
                        "GET /accounts/{account_id}/cfd_tunnel/{tunnel_id}/connections reports \
                         {connections} live connection(s); https://{}{VERIFY_PATH} answered \
                         {edge_text} through the edge",
                        decl.verify_hostname
                    ),
                ),
                ("method", "api".to_string()),
            ],
        )
        .await?;

        // Revoke the old tunnel — last, only what the scoper named,
        // and only with no connector left on it.
        let Some(old_name) = old_tunnel else {
            tracing::info!(
                job_id = ev.job_id,
                "no old_token named on the scope step; revoke left to its assignee"
            );
            return Ok(());
        };
        let old = self
            .cloudflare
            .find_tunnel(account_id, old_name)
            .await
            .map_err(HandlerError::Downstream)?;
        let (deleted_now, old_id) = match old {
            None => (false, None),
            Some(t) if t.id == tunnel_id => {
                return Err(HandlerError::Permanent(format!(
                    "old_token {old_name:?} resolves to this rotation's own replacement ({tunnel_id})"
                )));
            }
            Some(t) => {
                let live = self
                    .cloudflare
                    .tunnel_connections(account_id, &t.id)
                    .await
                    .map_err(HandlerError::Downstream)?;
                if live > 0 {
                    // Deleting under a live connector is deleting
                    // somebody's traffic. Say so on the step, leave it
                    // open, and stop — not an error: this is the
                    // expected state until the old connector retires.
                    let note = format!(
                        "old tunnel {old_name} ({}) still has {live} live connection(s) — revoke \
                         deferred; the new tunnel {tunnel_name} is installed and verified. Once the \
                         old connector is retired (0b7804f3), re-fire this rotation or delete the \
                         old tunnel by name and record it here.",
                        t.id
                    );
                    tracing::warn!(job_id = ev.job_id, %note);
                    self.put_step(
                        rule,
                        ev.job_id,
                        &steps,
                        "revoke",
                        &[("revoke_deferred", note)],
                        false,
                    )
                    .await?;
                    return Ok(());
                }
                let deleted = self
                    .cloudflare
                    .delete_tunnel(account_id, &t.id)
                    .await
                    .map_err(HandlerError::Downstream)?;
                (deleted, Some(t.id))
            }
        };
        let still_there = self
            .cloudflare
            .find_tunnel(account_id, old_name)
            .await
            .map_err(HandlerError::Downstream)?
            .is_some();
        if still_there {
            return Err(HandlerError::Downstream(format!(
                "old tunnel {old_name} still listed after delete"
            )));
        }
        let confirmed_dead = format!(
            "account {account_id} tunnel list (is_deleted=false) no longer contains {old_name}"
        );
        if !step_done("revoke") {
            self.record_phase(
                rule,
                decl.credential_id,
                RotationPhase::Revoked,
                json!({
                    "job_id": ev.job_id,
                    "old_tunnel": old_name,
                    "old_tunnel_id": old_id,
                    "deleted_now": deleted_now,
                    "confirmed_dead": confirmed_dead,
                }),
            )
            .await?;
        }
        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "revoke",
            &[
                (
                    "revoked",
                    match old_id {
                        Some(id) if deleted_now => format!(
                            "cloudflare tunnel {old_name} ({id}) deleted via DELETE \
                             /accounts/{account_id}/cfd_tunnel/{id}; every connector token minted for \
                             it is now invalid"
                        ),
                        _ => format!("cloudflare tunnel {old_name} was already absent"),
                    },
                ),
                ("confirmed_dead", confirmed_dead),
            ],
        )
        .await?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::credential_issuer::{DnsRecord, ZoneInfo};
    use std::sync::Mutex;

    // ----- pure planning -----

    #[test]
    fn tunnel_name_derives_from_prefix_and_packet_id() {
        assert_eq!(
            rotation_tunnel_name("boss-cluster", "2ff7a586-56da-4479"),
            "boss-cluster-2ff7a586"
        );
    }

    #[test]
    fn cname_target_is_the_tunnel_id_under_cfargotunnel() {
        assert_eq!(
            tunnel_cname_target("11111111-2222-4333-8444-555555555555"),
            "11111111-2222-4333-8444-555555555555.cfargotunnel.com"
        );
    }

    fn tun(id: &str, name: &str, connections: usize) -> TunnelInfo {
        TunnelInfo {
            id: id.into(),
            name: name.into(),
            connections,
        }
    }

    #[test]
    fn plan_mints_fresh_when_the_account_has_no_rotation_tunnel() {
        assert_eq!(plan_rotation(None, None), RotationPlan::MintFresh);
        assert_eq!(
            plan_rotation(None, Some("some-other-id")),
            RotationPlan::MintFresh
        );
    }

    #[test]
    fn plan_recognizes_a_completed_install() {
        let t = tun("t-new", "boss-cluster-2ff7a586", 0);
        assert_eq!(
            plan_rotation(Some(&t), Some("t-new")),
            RotationPlan::AlreadyInstalled {
                tunnel_id: "t-new".into()
            }
        );
    }

    #[test]
    fn plan_replaces_an_orphan_whose_secret_was_lost() {
        let t = tun("t-orphan", "boss-cluster-2ff7a586", 0);
        assert_eq!(
            plan_rotation(Some(&t), Some("t-old")),
            RotationPlan::ReplaceStale {
                orphan_id: "t-orphan".into()
            }
        );
        assert_eq!(
            plan_rotation(Some(&t), None),
            RotationPlan::ReplaceStale {
                orphan_id: "t-orphan".into()
            }
        );
    }

    // ----- in-memory fakes -----

    /// The account as the fake Cloudflare sees it. Connections are
    /// settable per tunnel id; the fake restarter "connects" the
    /// newest tunnel when the Deployment is restarted, which is what
    /// a real rollout does a few seconds later.
    #[derive(Default)]
    struct FakeCloudflare {
        tunnels: Mutex<Vec<TunnelInfo>>,
        connections: Mutex<HashMap<String, usize>>,
        dns: Mutex<Vec<DnsRecord>>,
        created: Mutex<Vec<(String, String)>>,
        deleted: Mutex<Vec<String>>,
        dns_writes: Mutex<Vec<String>>,
        edge: Mutex<u16>,
        next_id: Mutex<u32>,
    }

    impl FakeCloudflare {
        fn with_tunnels(tunnels: Vec<TunnelInfo>) -> Arc<Self> {
            let f = Self::default();
            for t in &tunnels {
                f.connections
                    .lock()
                    .unwrap()
                    .insert(t.id.clone(), t.connections);
            }
            *f.tunnels.lock().unwrap() = tunnels;
            *f.edge.lock().unwrap() = 200;
            Arc::new(f)
        }
        fn has_tunnel(&self, name: &str) -> bool {
            self.tunnels.lock().unwrap().iter().any(|t| t.name == name)
        }
        fn newest_id(&self) -> Option<String> {
            self.tunnels.lock().unwrap().last().map(|t| t.id.clone())
        }
        fn cname(&self, host: &str) -> Option<DnsRecord> {
            self.dns
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.name == host && r.record_type == "CNAME")
                .cloned()
        }
    }

    #[async_trait]
    impl CloudflareTunnels for FakeCloudflare {
        async fn zone(&self, zone_name: &str) -> Result<ZoneInfo, String> {
            assert_eq!(zone_name, "algedonic.dev");
            Ok(ZoneInfo {
                zone_id: "zone-1".into(),
                account_id: "acct-from-zone".into(),
            })
        }
        async fn find_tunnel(
            &self,
            account_id: &str,
            name: &str,
        ) -> Result<Option<TunnelInfo>, String> {
            assert_eq!(account_id, "acct-from-zone");
            let conns = self.connections.lock().unwrap();
            Ok(self
                .tunnels
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.name == name)
                .map(|t| TunnelInfo {
                    connections: conns.get(&t.id).copied().unwrap_or(0),
                    ..t.clone()
                }))
        }
        async fn create_tunnel(
            &self,
            _account_id: &str,
            name: &str,
            tunnel_secret_b64: &str,
        ) -> Result<String, String> {
            if self.has_tunnel(name) {
                return Err(format!(
                    "POST cfd_tunnel returned 409: tunnel {name} exists"
                ));
            }
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            let id = format!("tunnel-{}", *n);
            self.tunnels.lock().unwrap().push(tun(&id, name, 0));
            self.created
                .lock()
                .unwrap()
                .push((name.to_string(), tunnel_secret_b64.to_string()));
            Ok(id)
        }
        async fn tunnel_connections(&self, _a: &str, tunnel_id: &str) -> Result<usize, String> {
            Ok(self
                .connections
                .lock()
                .unwrap()
                .get(tunnel_id)
                .copied()
                .unwrap_or(0))
        }
        async fn delete_tunnel(&self, _a: &str, tunnel_id: &str) -> Result<bool, String> {
            let live = self
                .connections
                .lock()
                .unwrap()
                .get(tunnel_id)
                .copied()
                .unwrap_or(0);
            assert_eq!(
                live, 0,
                "the fake refuses to delete under live connections, as the API does"
            );
            let mut ts = self.tunnels.lock().unwrap();
            let before = ts.len();
            ts.retain(|t| t.id != tunnel_id);
            self.deleted.lock().unwrap().push(tunnel_id.to_string());
            Ok(ts.len() < before)
        }
        async fn find_dns_record(
            &self,
            _z: &str,
            record_type: &str,
            name: &str,
        ) -> Result<Option<DnsRecord>, String> {
            Ok(self
                .dns
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.name == name && r.record_type == record_type)
                .cloned())
        }
        async fn create_dns_record(
            &self,
            _z: &str,
            record_type: &str,
            name: &str,
            content: &str,
            proxied: bool,
            _comment: &str,
        ) -> Result<(), String> {
            let id = format!("rec-{}", self.dns.lock().unwrap().len() + 1);
            self.dns.lock().unwrap().push(DnsRecord {
                id,
                name: name.into(),
                record_type: record_type.into(),
                content: content.into(),
                proxied,
            });
            self.dns_writes
                .lock()
                .unwrap()
                .push(format!("create {name}"));
            Ok(())
        }
        async fn update_dns_record(
            &self,
            _z: &str,
            record_id: &str,
            content: &str,
            proxied: bool,
            _comment: &str,
        ) -> Result<(), String> {
            let mut dns = self.dns.lock().unwrap();
            let rec = dns
                .iter_mut()
                .find(|r| r.id == record_id)
                .ok_or_else(|| format!("PATCH dns_records/{record_id} returned 404"))?;
            rec.content = content.into();
            rec.proxied = proxied;
            self.dns_writes
                .lock()
                .unwrap()
                .push(format!("update {}", rec.name));
            Ok(())
        }
        async fn edge_status(&self, hostname: &str, path: &str) -> Result<u16, String> {
            assert_eq!(hostname, "playground.algedonic.dev");
            assert_eq!(path, "/health");
            Ok(*self.edge.lock().unwrap())
        }
    }

    /// Restarting the connector attaches it to the newest tunnel —
    /// unless built `dead`, which models a connector that never
    /// comes up (the car for it has not landed).
    struct FakeRestarter {
        cf: Arc<FakeCloudflare>,
        restarts: Mutex<Vec<String>>,
        connects: bool,
        exists: bool,
    }

    impl FakeRestarter {
        fn live(cf: Arc<FakeCloudflare>) -> Arc<Self> {
            Arc::new(Self {
                cf,
                restarts: Mutex::new(vec![]),
                connects: true,
                exists: true,
            })
        }
        fn dead(cf: Arc<FakeCloudflare>) -> Arc<Self> {
            Arc::new(Self {
                cf,
                restarts: Mutex::new(vec![]),
                connects: false,
                exists: true,
            })
        }
        fn absent(cf: Arc<FakeCloudflare>) -> Arc<Self> {
            Arc::new(Self {
                cf,
                restarts: Mutex::new(vec![]),
                connects: false,
                exists: false,
            })
        }
    }

    #[async_trait]
    impl WorkloadRestarter for FakeRestarter {
        async fn deployment_exists(&self, _ns: &str, _name: &str) -> Result<bool, String> {
            Ok(self.exists)
        }
        async fn restart_deployment(
            &self,
            namespace: &str,
            name: &str,
            reason: &str,
        ) -> Result<bool, String> {
            assert!(
                reason.starts_with("credential-rotation 2ff7a586-") && reason.contains(" tunnel "),
                "the restart says what it is for: {reason}"
            );
            self.restarts
                .lock()
                .unwrap()
                .push(format!("{namespace}/{name}"));
            if !self.exists {
                return Ok(false);
            }
            if self.connects
                && let Some(id) = self.cf.newest_id()
            {
                self.cf.connections.lock().unwrap().insert(id, 2);
            }
            Ok(true)
        }
    }

    #[derive(Default)]
    struct FakeSecrets {
        map: Mutex<HashMap<String, String>>,
    }

    impl FakeSecrets {
        fn seeded(ns: &str, name: &str, key: &str, value: &str) -> Arc<Self> {
            let f = Self::default();
            f.map
                .lock()
                .unwrap()
                .insert(format!("{ns}/{name}/{key}"), value.to_string());
            Arc::new(f)
        }
        fn get(&self, ns: &str, name: &str, key: &str) -> Option<String> {
            self.map
                .lock()
                .unwrap()
                .get(&format!("{ns}/{name}/{key}"))
                .cloned()
        }
    }

    #[async_trait]
    impl SecretStore for FakeSecrets {
        async fn read_key(
            &self,
            ns: &str,
            name: &str,
            key: &str,
        ) -> Result<Option<String>, String> {
            Ok(self.get(ns, name, key))
        }
        async fn write_key(
            &self,
            ns: &str,
            name: &str,
            key: &str,
            value: &str,
        ) -> Result<(), String> {
            self.map
                .lock()
                .unwrap()
                .insert(format!("{ns}/{name}/{key}"), value.to_string());
            Ok(())
        }
    }

    // ----- jobs-api stub (the house axum idiom) -----

    type Captured = std::sync::Arc<Mutex<Vec<(String, JsonValue)>>>;

    /// A rotation packet with the machine phases pending. Returns the
    /// stub's base URL + captured step PUTs as (step_id, body) +
    /// captured rotation-door POSTs as ("{credential_id}/{phase}", body).
    async fn stub_jobs_api(
        step_statuses: &'static [(&'static str, &'static str)],
    ) -> (String, Captured, Captured) {
        use axum::extract::Path;
        use axum::{Json, Router, routing::get, routing::post, routing::put};

        let captured: Captured = Default::default();
        let cap = captured.clone();
        let rotations: Captured = Default::default();
        let rot = rotations.clone();
        let jobs = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| async move {
                    let steps: Vec<JsonValue> = step_statuses
                        .iter()
                        .map(|(slug, status)| {
                            json!({
                                "id": format!("step-{slug}"),
                                "spec_slug": slug,
                                "status": status,
                                "metadata": { "kept": "yes" },
                            })
                        })
                        .collect();
                    Json(json!({ "id": id, "steps": steps }))
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                put(
                    move |Path((_id, sid)): Path<(String, String)>, Json(body): Json<JsonValue>| {
                        let cap = cap.clone();
                        async move {
                            cap.lock().unwrap().push((sid, body));
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route(
                "/api/credentials/{id}/rotation/{phase}",
                post(
                    move |Path((id, phase)): Path<(String, String)>,
                          Json(body): Json<JsonValue>| {
                        let rot = rot.clone();
                        async move {
                            rot.lock().unwrap().push((format!("{id}/{phase}"), body));
                            Json(json!({ "recorded": true }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, jobs).await.unwrap() });
        (format!("http://{addr}"), captured, rotations)
    }

    fn rotation_args() -> Vec<(String, Value)> {
        [
            ("zone", "algedonic.dev"),
            ("secret_namespace", "boss"),
            ("secret_name", "cloudflare-tunnel-credentials"),
            ("secret_key", "credentials.json"),
            ("tunnel_name_prefix", "boss-cluster"),
            ("hostnames", "boss.algedonic.dev, playground.algedonic.dev"),
            ("verify_hostname", "playground.algedonic.dev"),
            ("credential_id", "cloudflare-tunnel-credentials"),
            ("restart_deployment", "cloudflared"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect()
    }

    fn scope_done_ctx(old_token: Option<&str>) -> InvocationContext {
        let mut metadata = json!({
            "credential": "cloudflare-tunnel-credentials",
            "reason": "token leaked into ops-request 0117de08 by unit-cat",
            "locations": "boss-gcp cloudflared.service ExecStart --token",
            "consumers": "cloudflared on boss-gcp; the in-cluster connector (5a2bb0ce)",
        });
        if let Some(old) = old_token {
            metadata["old_token"] = json!(old);
        }
        InvocationContext {
            rule_name: "broker-rotates-the-cloudflare-tunnel".into(),
            triggering_event_id: "evt-rot-cf-1".into(),
            triggering_topic: "step.done.credential-rotation".into(),
            event_payload: json!({
                "job_id": "2ff7a586-56da-4479-a76f-c486a2da0d90",
                "step_id": "step-scope",
                "kind": "credential-rotation",
                "subject_kind": "custom",
                "subject_id": "bosspipeline",
                "metadata": metadata,
            }),
        }
    }

    const PENDING_PHASES: &[(&str, &str)] = &[
        ("scope", "completed"),
        ("issue", "ready"),
        ("install", "pending"),
        ("verify", "pending"),
        ("revoke", "pending"),
    ];

    const NO_WAIT: VerifyPoll = VerifyPoll {
        attempts: 2,
        interval: Duration::ZERO,
    };

    fn handler(
        jobs_url: String,
        cf: Arc<FakeCloudflare>,
        secrets: Arc<FakeSecrets>,
        restarter: Arc<FakeRestarter>,
    ) -> Arc<CredentialRotateCloudflareTunnel> {
        CredentialRotateCloudflareTunnel::with_poll(jobs_url, cf, secrets, restarter, NO_WAIT)
    }

    const NEW_NAME: &str = "boss-cluster-2ff7a586";

    #[tokio::test]
    async fn full_rotation_mints_installs_points_dns_verifies_and_revokes() {
        // The old tunnel has no connector left (boss-gcp's retired).
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-old", "boss-gcp-tunnel", 0)]);
        // playground already points at the old tunnel; boss.* has no CNAME.
        cf.dns.lock().unwrap().push(DnsRecord {
            id: "rec-old".into(),
            name: "playground.algedonic.dev".into(),
            record_type: "CNAME".into(),
            content: "t-old.cfargotunnel.com".into(),
            proxied: true,
        });
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let (jobs_url, captured, rotations) = stub_jobs_api(PENDING_PHASES).await;

        let h = handler(jobs_url, cf.clone(), secrets.clone(), restarter.clone());
        h.invoke(&rotation_args(), &scope_done_ctx(Some("boss-gcp-tunnel")))
            .await
            .expect("rotation succeeds");

        // Minted once, under the packet-derived name, with a 32-byte secret.
        let created = cf.created.lock().unwrap().clone();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, NEW_NAME);
        let secret_b64 = created[0].1.clone();
        let new_id = cf.newest_id().unwrap();

        // Installed: the Secret holds cloudflared's credentials file
        // naming the new tunnel, the account tag from the ZONE lookup
        // (no account_id arg), and the very secret the mint sent.
        let installed = secrets
            .get("boss", "cloudflare-tunnel-credentials", "credentials.json")
            .expect("secret written");
        let creds: JsonValue = serde_json::from_str(&installed).unwrap();
        assert_eq!(creds["TunnelID"], new_id);
        assert_eq!(creds["AccountTag"], "acct-from-zone");
        assert_eq!(creds["TunnelSecret"], secret_b64);

        // DNS: both hostnames are proxied CNAMEs to the new tunnel —
        // one created, one corrected.
        let target = tunnel_cname_target(&new_id);
        for host in ["boss.algedonic.dev", "playground.algedonic.dev"] {
            let rec = cf
                .cname(host)
                .unwrap_or_else(|| panic!("{host} has a CNAME"));
            assert_eq!(rec.content, target);
            assert!(rec.proxied);
        }
        assert_eq!(
            cf.dns_writes.lock().unwrap().clone(),
            vec![
                "create boss.algedonic.dev".to_string(),
                "update playground.algedonic.dev".to_string()
            ]
        );

        // The connector was restarted, once, in the Secret's namespace.
        assert_eq!(
            restarter.restarts.lock().unwrap().clone(),
            vec!["boss/cloudflared".to_string()]
        );

        // Revoked: the old tunnel is gone; the new one survives.
        assert!(!cf.has_tunnel("boss-gcp-tunnel"));
        assert!(cf.has_tunnel(NEW_NAME));
        assert_eq!(
            cf.deleted.lock().unwrap().clone(),
            vec!["t-old".to_string()]
        );

        // Recorded: issue, install, verify, revoke completed in order
        // with required-at-done evidence, existing metadata kept, and
        // no secret value anywhere in any body.
        let puts = captured.lock().unwrap().clone();
        let order: Vec<&str> = puts.iter().map(|(sid, _)| sid.as_str()).collect();
        assert_eq!(
            order,
            vec!["step-issue", "step-install", "step-verify", "step-revoke"]
        );
        for (_, body) in &puts {
            assert_eq!(body["status"], "completed");
            assert_eq!(body["metadata"]["kept"], "yes");
            let flat = body.to_string();
            assert!(
                !flat.contains(&secret_b64),
                "tunnel secret leaked into a step body: {flat}"
            );
        }
        let issue = &puts[0].1["metadata"];
        assert!(issue["issued"].as_str().unwrap().contains(NEW_NAME));
        assert!(issue["issued"].as_str().unwrap().contains(&new_id));
        assert!(
            issue["issuer"]
                .as_str()
                .unwrap()
                .contains("cloudflare-token")
        );
        let install = &puts[1].1["metadata"];
        let installed_text = install["installed"].as_str().unwrap();
        assert!(installed_text.contains("boss/cloudflare-tunnel-credentials"));
        assert!(installed_text.contains("boss.algedonic.dev CNAME"));
        assert!(installed_text.contains("(created)"));
        assert!(installed_text.contains("(updated)"));
        assert!(installed_text.contains("rollout-restarted"));
        let verify = &puts[2].1["metadata"];
        assert!(
            verify["verified"]
                .as_str()
                .unwrap()
                .contains("2 live connection")
        );
        assert!(
            verify["verified"]
                .as_str()
                .unwrap()
                .contains("answered 200")
        );
        assert_eq!(verify["method"], "api");
        let revoke = &puts[3].1["metadata"];
        assert!(
            revoke["revoked"]
                .as_str()
                .unwrap()
                .contains("boss-gcp-tunnel (t-old) deleted")
        );
        assert!(
            revoke["confirmed_dead"]
                .as_str()
                .unwrap()
                .contains("no longer contains")
        );

        // Evented: one credential.* event per phase, in protocol
        // order, addressed to the credential the RULE declares (the
        // packet's Subject is `bosspipeline`, not the credential) —
        // and no secret value in any of them.
        let events = rotations.lock().unwrap().clone();
        let order: Vec<&str> = events.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "cloudflare-tunnel-credentials/minted",
                "cloudflare-tunnel-credentials/installed",
                "cloudflare-tunnel-credentials/verified",
                "cloudflare-tunnel-credentials/revoked",
            ]
        );
        for (path, body) in &events {
            let flat = body.to_string();
            assert!(
                !flat.contains(&secret_b64),
                "tunnel secret leaked into a rotation event ({path}): {flat}"
            );
            assert_eq!(body["job_id"], "2ff7a586-56da-4479-a76f-c486a2da0d90");
        }
        let minted = &events[0].1;
        assert_eq!(minted["tunnel_name"], NEW_NAME);
        assert_eq!(minted["tunnel_id"], new_id);
        assert_eq!(minted["account_id"], "acct-from-zone");
        assert_eq!(minted["replaced_orphan"], false);
        let installed_ev = &events[1].1;
        assert_eq!(installed_ev["secret_name"], "cloudflare-tunnel-credentials");
        assert_eq!(installed_ev["secret_key"], "credentials.json");
        assert_eq!(
            installed_ev["value_length"].as_u64().unwrap(),
            installed.len() as u64,
            "the event carries the value's LENGTH, never the value"
        );
        assert_eq!(installed_ev["converged"], false);
        assert_eq!(installed_ev["dns"].as_array().unwrap().len(), 2);
        let verified_ev = &events[2].1;
        assert_eq!(verified_ev["connections"], 2);
        assert_eq!(verified_ev["verify_hostname"], "playground.algedonic.dev");
        let revoked_ev = &events[3].1;
        assert_eq!(revoked_ev["old_tunnel"], "boss-gcp-tunnel");
        assert_eq!(revoked_ev["old_tunnel_id"], "t-old");
        assert_eq!(revoked_ev["deleted_now"], true);
    }

    #[tokio::test]
    async fn a_live_old_connector_defers_the_revoke_and_says_so() {
        // boss-gcp's connector is still attached to the old tunnel.
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-old", "boss-gcp-tunnel", 1)]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let (jobs_url, captured, rotations) = stub_jobs_api(PENDING_PHASES).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter);
        h.invoke(&rotation_args(), &scope_done_ctx(Some("boss-gcp-tunnel")))
            .await
            .expect("a deferred revoke is not an error");

        // The old tunnel survives, untouched.
        assert!(cf.has_tunnel("boss-gcp-tunnel"));
        assert!(cf.deleted.lock().unwrap().is_empty());

        // issue / install / verify completed; revoke ANNOTATED, open.
        let puts = captured.lock().unwrap().clone();
        let order: Vec<&str> = puts.iter().map(|(sid, _)| sid.as_str()).collect();
        assert_eq!(
            order,
            vec!["step-issue", "step-install", "step-verify", "step-revoke"]
        );
        let revoke = &puts[3].1;
        assert!(
            revoke.get("status").is_none(),
            "revoke is NOT completed: {revoke}"
        );
        let note = revoke["metadata"]["revoke_deferred"].as_str().unwrap();
        assert!(note.contains("boss-gcp-tunnel (t-old) still has 1 live connection"));
        assert!(note.contains("revoke deferred"));
        assert_eq!(revoke["metadata"]["kept"], "yes");

        // Three events; no `revoked` — nothing was revoked.
        let order: Vec<String> = rotations
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(
            order,
            vec![
                "cloudflare-tunnel-credentials/minted",
                "cloudflare-tunnel-credentials/installed",
                "cloudflare-tunnel-credentials/verified",
            ]
        );
    }

    #[tokio::test]
    async fn a_connector_that_never_connects_is_not_yet_and_revokes_nothing() {
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-old", "boss-gcp-tunnel", 0)]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::dead(cf.clone());
        let (jobs_url, captured, rotations) = stub_jobs_api(PENDING_PHASES).await;

        let h = handler(jobs_url, cf.clone(), secrets.clone(), restarter);
        let err = h
            .invoke(&rotation_args(), &scope_done_ctx(Some("boss-gcp-tunnel")))
            .await
            .expect_err("unverified is not done");
        match &err {
            HandlerError::Downstream(msg) => {
                assert!(msg.starts_with("not yet:"), "{msg}");
                assert!(msg.contains("0 live connections"), "{msg}");
                assert!(msg.contains("NOT revoked"), "{msg}");
            }
            other => panic!("expected a retryable Downstream, got {other:?}"),
        }
        assert!(
            !err.is_permanent(),
            "a NAK, so the redelivery schedule carries the wait"
        );

        // The old tunnel survives. The mint and install ARE facts and
        // are on the record — steps and events — so the packet shows
        // where the rotation stands while the wait continues.
        assert!(cf.has_tunnel("boss-gcp-tunnel"));
        assert!(
            secrets
                .get("boss", "cloudflare-tunnel-credentials", "credentials.json")
                .is_some()
        );
        let order: Vec<String> = captured
            .lock()
            .unwrap()
            .iter()
            .map(|(sid, _)| sid.clone())
            .collect();
        assert_eq!(order, vec!["step-issue", "step-install"]);
        let order: Vec<String> = rotations
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(
            order,
            vec![
                "cloudflare-tunnel-credentials/minted",
                "cloudflare-tunnel-credentials/installed",
            ]
        );
    }

    #[tokio::test]
    async fn redelivery_after_a_finished_rotation_mints_nothing() {
        // The account already holds this packet's tunnel, the Secret
        // names it, DNS already points at it, and every step is done.
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-new", NEW_NAME, 2)]);
        let target = tunnel_cname_target("t-new");
        for (i, host) in ["boss.algedonic.dev", "playground.algedonic.dev"]
            .iter()
            .enumerate()
        {
            cf.dns.lock().unwrap().push(DnsRecord {
                id: format!("rec-{i}"),
                name: (*host).into(),
                record_type: "CNAME".into(),
                content: target.clone(),
                proxied: true,
            });
        }
        let secrets = FakeSecrets::seeded(
            "boss",
            "cloudflare-tunnel-credentials",
            "credentials.json",
            &tunnel_credentials_json("acct-from-zone", "t-new", "c2VjcmV0"),
        );
        let restarter = FakeRestarter::live(cf.clone());
        const ALL_DONE: &[(&str, &str)] = &[
            ("scope", "completed"),
            ("issue", "completed"),
            ("install", "completed"),
            ("verify", "completed"),
            ("revoke", "completed"),
        ];
        let (jobs_url, captured, rotations) = stub_jobs_api(ALL_DONE).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter.clone());
        h.invoke(&rotation_args(), &scope_done_ctx(Some("boss-gcp-tunnel")))
            .await
            .expect("idempotent re-run succeeds");

        assert!(cf.created.lock().unwrap().is_empty(), "no second mint");
        assert!(
            cf.dns_writes.lock().unwrap().is_empty(),
            "DNS already right"
        );
        assert!(
            restarter.restarts.lock().unwrap().is_empty(),
            "no restart on converge"
        );
        assert!(captured.lock().unwrap().is_empty(), "no step rewrites");
        assert!(
            rotations.lock().unwrap().is_empty(),
            "a finished rotation redelivered emits NOTHING"
        );
    }

    #[tokio::test]
    async fn a_lost_secret_replay_retires_the_orphan_and_mints_again() {
        // Prior attempt minted t-orphan then died before the Secret
        // write — the Secret is empty, the secret unrecoverable.
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-orphan", NEW_NAME, 0)]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let (jobs_url, _captured, rotations) = stub_jobs_api(PENDING_PHASES).await;

        let h = handler(jobs_url, cf.clone(), secrets.clone(), restarter);
        h.invoke(&rotation_args(), &scope_done_ctx(None))
            .await
            .expect("replay succeeds");

        let mine: Vec<TunnelInfo> = cf
            .tunnels
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.name == NEW_NAME)
            .cloned()
            .collect();
        assert_eq!(mine.len(), 1, "exactly one rotation tunnel survives");
        assert_ne!(mine[0].id, "t-orphan", "the orphan was retired");
        assert_eq!(
            cf.deleted.lock().unwrap().clone(),
            vec!["t-orphan".to_string()]
        );
        let installed = secrets
            .get("boss", "cloudflare-tunnel-credentials", "credentials.json")
            .unwrap();
        assert_eq!(
            installed_tunnel_id(&installed).as_deref(),
            Some(mine[0].id.as_str())
        );

        // The replacement mint is evented and says it retired an
        // orphan; no old_token was named, so nothing claims a revoke.
        let events = rotations.lock().unwrap().clone();
        let order: Vec<&str> = events.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "cloudflare-tunnel-credentials/minted",
                "cloudflare-tunnel-credentials/installed",
                "cloudflare-tunnel-credentials/verified",
            ]
        );
        assert_eq!(events[0].1["replaced_orphan"], true);
    }

    #[tokio::test]
    async fn a_declared_connector_that_does_not_exist_refuses_before_any_mint() {
        // The connector car (5a2bb0ce) has not landed: no Deployment.
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-old", "boss-gcp-tunnel", 1)]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::absent(cf.clone());
        let (jobs_url, captured, rotations) = stub_jobs_api(PENDING_PHASES).await;
        let h = handler(jobs_url, cf.clone(), secrets.clone(), restarter);
        let err = h
            .invoke(&rotation_args(), &scope_done_ctx(Some("boss-gcp-tunnel")))
            .await
            .expect_err("no connector, no rotation");
        assert!(err.is_permanent(), "got {err:?}");
        let msg = format!("{err:?}");
        assert!(msg.contains("boss/cloudflared does not exist"), "{msg}");
        assert!(msg.contains("nothing was minted"), "{msg}");
        // Nothing happened: no tunnel, no Secret, no DNS, no step, no event.
        assert!(cf.created.lock().unwrap().is_empty());
        assert!(cf.dns_writes.lock().unwrap().is_empty());
        assert!(
            secrets
                .get("boss", "cloudflare-tunnel-credentials", "credentials.json")
                .is_none()
        );
        assert!(captured.lock().unwrap().is_empty());
        assert!(rotations.lock().unwrap().is_empty());
        assert!(cf.has_tunnel("boss-gcp-tunnel"));
    }

    #[tokio::test]
    async fn naming_the_new_tunnel_as_old_is_refused_before_any_mint() {
        let cf = FakeCloudflare::with_tunnels(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let (jobs_url, _c, rotations) = stub_jobs_api(PENDING_PHASES).await;
        let h = handler(jobs_url, cf.clone(), secrets, restarter);
        let err = h
            .invoke(&rotation_args(), &scope_done_ctx(Some(NEW_NAME)))
            .await
            .expect_err("self-revocation is refused");
        assert!(err.is_permanent(), "got {err:?}");
        assert!(cf.created.lock().unwrap().is_empty());
        assert!(rotations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_declaration_arg_is_reported() {
        let cf = FakeCloudflare::with_tunnels(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let h = handler("http://127.0.0.1:1".into(), cf, secrets, restarter);
        let mut args = rotation_args();
        args.retain(|(k, _)| k != "verify_hostname");
        let err = h
            .invoke(&args, &scope_done_ctx(None))
            .await
            .expect_err("missing arg");
        assert!(matches!(err, HandlerError::MissingArg(_)));
    }
}
