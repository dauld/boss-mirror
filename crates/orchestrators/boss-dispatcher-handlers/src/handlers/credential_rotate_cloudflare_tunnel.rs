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
//!   never follows the Secret on its own. The tunnel each rewritten
//!   CNAME used to point at is recorded on the install step as
//!   `previous_tunnel_id`: the revoke phase reads it back.
//! - **verify** — a connector reports connected on the new tunnel AND
//!   the verify hostname answers 2xx at `/health` through the edge. A
//!   bounded in-invocation wait, then "not yet" (a NAK): JetStream's
//!   ack window is 30 s (`boss_nats::durable::ACK_WAIT`), so a longer
//!   wait here would be redelivered mid-run and mint twice; the
//!   redelivery schedule IS the honest wait (~4 min across the budget),
//!   each retry converging on the already-installed tunnel.
//! - **revoke** — DISCOVER the old tunnels and delete each one that has
//!   no connector left on it. The candidates are every live tunnel in
//!   the account named `<tunnel_name_prefix>-*` whose id is not the one
//!   the installed Secret names, plus whatever the rewritten CNAMEs
//!   pointed at before the install, plus the scope step's `old_token`
//!   (a tunnel NAME, never a value) when the scoper knew it — one more
//!   candidate, never the only one. The CURRENT tunnel is never a
//!   candidate. Deleting a tunnel invalidates every connector token
//!   minted for it; deleting one under a live connector is deleting
//!   somebody's traffic, so a candidate with connections is recorded
//!   as "revoke deferred" and the step is left open. A candidate that
//!   is already gone (deleted by hand) is "already revoked", not an
//!   error. Until 5e8efcf5 the phase acted only on `old_token` and,
//!   absent, left the step to a person — a hand act the no-hand-work
//!   rule forbids, while the leaked token (9c760dd7) could still attach
//!   a connector to the undeleted tunnel.
//!
//! ## The re-fire door (5e8efcf5)
//!
//! A deferred revoke completes WITHOUT a new packet: the daily rule
//! `broker-revokes-the-cloudflare-tunnel-daily` invokes this handler
//! with `phase = "revoke"` and no packet in its payload (a clock
//! firing carries only `_day`). That mode lists every open
//! `rotate-a-credential` packet about this credential whose `revoke`
//! step is ready, and runs ONLY the revoke phase on each, idempotently
//! — nothing is minted, no DNS is touched, no connector restarted — and
//! completes the step when nothing is left to revoke, which closes the
//! packet `rotated`. The retire verb's own effect (an ops-request
//! `retire-cloudflared` answering exit 0) was measured as a trigger and
//! is NOT visible to the rule layer: `jobs.job.closed` carries no job
//! metadata and the runner's `step.done.task` carries
//! `disposition`/`exit_code` but not the verb, so the daily cadence is
//! the door.
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

/// The protocol the sweep reads. Named here because the clock firing
/// carries no packet: the handler has to go and find them.
pub const ROTATION_KIND: &str = "rotate-a-credential";

/// The one value of the `phase` arg the clock rule may pass. A clock
/// firing of the FULL rotation would have nothing to mint for.
pub const REVOKE_PHASE: &str = "revoke";

/// The rule that re-fires the revoke phase; named on the deferred
/// step so the reader knows what will act next, and when.
pub const REFIRE_RULE: &str = "broker-revokes-the-cloudflare-tunnel-daily";

// ---------------------------------------------------------------------------
// Pure planning — the decisions under test
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

/// The inverse: the tunnel id a CNAME target names, if it is one.
pub fn tunnel_id_of_cname_target(content: &str) -> Option<&str> {
    content
        .trim()
        .trim_end_matches('.')
        .strip_suffix(".cfargotunnel.com")
        .filter(|id| !id.is_empty())
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

/// How the revoke phase found a candidate — recorded beside each one
/// so the step says how the machine knew, not only what it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundBy {
    /// Named `<prefix>-*`: a sibling this rule's earlier rotation minted.
    Prefix,
    /// The CNAME target the install phase rewrote.
    PreviousTarget,
    /// Named on the scope step as `old_token`.
    Scoped,
}

impl FoundBy {
    fn as_str(self) -> &'static str {
        match self {
            FoundBy::Prefix => "named with the rotation prefix",
            FoundBy::PreviousTarget => "the previous CNAME target",
            FoundBy::Scoped => "named old_token on the scope step",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeCandidate {
    pub tunnel: TunnelInfo,
    pub found_by: FoundBy,
}

/// The revoke phase's reading of the account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RevokePlan {
    /// Live tunnels to retire, in the account's listing order. NEVER
    /// the current one.
    pub candidates: Vec<RevokeCandidate>,
    /// Things the packet named that the account no longer lists —
    /// already revoked (by an earlier pass, or by hand), not an error.
    pub already_absent: Vec<String>,
}

/// PURE: which of the account's live tunnels the revoke phase acts on.
///
/// `current_id` is what the installed Secret names — the one tunnel a
/// connector serves — and is excluded before any other rule is read,
/// so no combination of prefix, previous target and `old_token` can
/// put it on the list. `previous_ids` are the CNAME targets the install
/// rewrote; `old_name` is the scope step's `old_token`.
pub fn plan_revoke(
    account: &[TunnelInfo],
    prefix: &str,
    current_id: &str,
    previous_ids: &[String],
    old_name: Option<&str>,
) -> RevokePlan {
    let sibling_prefix = format!("{prefix}-");
    let candidates: Vec<RevokeCandidate> = account
        .iter()
        .filter(|t| t.id != current_id)
        .filter_map(|t| {
            let found_by = if t.name.starts_with(&sibling_prefix) {
                FoundBy::Prefix
            } else if previous_ids.iter().any(|p| p == &t.id) {
                FoundBy::PreviousTarget
            } else if old_name.is_some_and(|n| n == t.name) {
                FoundBy::Scoped
            } else {
                return None;
            };
            Some(RevokeCandidate {
                tunnel: t.clone(),
                found_by,
            })
        })
        .collect();
    let mut already_absent: Vec<String> = previous_ids
        .iter()
        .filter(|p| p.as_str() != current_id && !account.iter().any(|t| &t.id == *p))
        .map(|p| format!("tunnel id {p} (the previous CNAME target)"))
        .collect();
    if let Some(name) = old_name
        && !account.iter().any(|t| t.name == name)
    {
        already_absent.push(format!("tunnel {name} (named old_token on the scope step)"));
    }
    RevokePlan {
        candidates,
        already_absent,
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

/// The rule row's consumer declaration, parsed once. `hostnames` and
/// `verify_hostname` are what the install and verify phases read; the
/// revoke-only clock rule declares neither, and the full rotation
/// refuses without them at the point of use.
struct Declaration<'a> {
    zone: &'a str,
    account_id: Option<&'a str>,
    secret_namespace: &'a str,
    secret_name: &'a str,
    secret_key: &'a str,
    tunnel_name_prefix: &'a str,
    hostnames: Vec<&'a str>,
    verify_hostname: Option<&'a str>,
    credential_id: &'a str,
    restart_deployment: Option<&'a str>,
    phase: Option<&'a str>,
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
            hostnames: optional_arg(args, "hostnames")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect(),
            verify_hostname: optional_arg(args, "verify_hostname"),
            credential_id: arg_string(args, "credential_id")?,
            restart_deployment: optional_arg(args, "restart_deployment"),
            phase: optional_arg(args, "phase"),
        })
    }
}

/// Per-packet facts the revoke phase reads off the packet itself —
/// the same three on the first pass and on every re-fire.
struct RevokeInputs<'a> {
    job_id: &'a str,
    /// What the installed Secret names: the one tunnel never revoked.
    current_id: &'a str,
    /// The scope step's `old_token`, if the scoper knew it.
    old_name: Option<&'a str>,
    /// The CNAME targets the install phase rewrote.
    previous_ids: Vec<String>,
}

/// `previous_tunnel_id` on the install step, as the install phase
/// writes it: ids joined with `, `.
fn previous_ids_recorded(steps: &HashMap<String, StepView>) -> Vec<String> {
    steps
        .get("install")
        .and_then(|s| s.metadata.get("previous_tunnel_id"))
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn old_token_of(metadata: &serde_json::Map<String, JsonValue>) -> Option<&str> {
    metadata
        .get("old_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The packet's steps keyed by spec slug, from a job body.
fn steps_of(body: &JsonValue) -> HashMap<String, StepView> {
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
    out
}

/// PURE: is this open packet about the credential the rule declares?
/// The same two spellings the rotation rule's `when` matches: opened ON
/// the credential (the subject), or naming it on the scope step.
fn is_about_credential(
    job: &JsonValue,
    steps: &HashMap<String, StepView>,
    credential_id: &str,
) -> bool {
    let subject = job
        .get("subject")
        .and_then(|s| s.get("id"))
        .and_then(|v| v.as_str());
    subject == Some(credential_id)
        || steps
            .get("scope")
            .and_then(|s| s.metadata.get("credential"))
            .and_then(|v| v.as_str())
            == Some(credential_id)
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

    async fn get_json(&self, url: &str) -> Result<JsonValue, HandlerError> {
        let resp = self
            .client
            .get(url)
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
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{url}: {e}")))
    }

    /// The packet's steps keyed by spec slug. One read serves every
    /// completion below.
    async fn fetch_steps(&self, job_id: &str) -> Result<HashMap<String, StepView>, HandlerError> {
        let url = format!("{}/api/jobs/{job_id}", self.jobs());
        Ok(steps_of(&self.get_json(&url).await?))
    }

    /// Every open rotation packet, paged on `total` so one sorted past
    /// a page is still found (the same paging `jobs.run-car-probes`
    /// does, for the same reason).
    async fn open_rotations(&self) -> Result<Vec<JsonValue>, HandlerError> {
        const PAGE: usize = 200;
        let mut rows: Vec<JsonValue> = Vec::new();
        loop {
            let body = self
                .get_json(&format!(
                    "{}/api/jobs?kind={ROTATION_KIND}&status=open&limit={PAGE}&offset={}",
                    self.jobs(),
                    rows.len()
                ))
                .await?;
            let total = body.get("total").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
            // A page with no `data` array is NO ANSWER. Read as zero
            // rows it broke the loop on `got == 0` and the sweep ACKed
            // having looked at nothing (backlog 37fc5837).
            let page: Vec<JsonValue> =
                super::common::rows_or_refuse(&body, "the open-rotation read (GET /api/jobs)")
                    .map_err(HandlerError::Downstream)?;
            let got = page.len();
            rows.extend(page);
            if got == 0 || rows.len() >= total {
                break;
            }
        }
        Ok(rows)
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
    /// already says this. Idempotent by content. Also answers which
    /// tunnel the record pointed at BEFORE, if it was one — the revoke
    /// phase's second source of candidates.
    async fn upsert_cname(
        &self,
        zone_id: &str,
        hostname: &str,
        target: &str,
        comment: &str,
    ) -> Result<(DnsOutcome, Option<String>), String> {
        match self
            .cloudflare
            .find_dns_record(zone_id, "CNAME", hostname)
            .await?
        {
            None => {
                self.cloudflare
                    .create_dns_record(zone_id, "CNAME", hostname, target, true, comment)
                    .await?;
                Ok((DnsOutcome::Created, None))
            }
            Some(rec) if rec.content == target && rec.proxied => Ok((DnsOutcome::Unchanged, None)),
            Some(rec) => {
                let previous = tunnel_id_of_cname_target(&rec.content).map(String::from);
                self.cloudflare
                    .update_dns_record(zone_id, &rec.id, target, true, comment)
                    .await?;
                Ok((DnsOutcome::Updated, previous))
            }
        }
    }

    /// The account id the rule declares, or the one the zone carries.
    /// Zone:Read is in the root grant; `GET /accounts` would need
    /// Account Settings:Read, which the ceremony did not mint.
    async fn account_id(&self, decl: &Declaration<'_>) -> Result<(String, String), HandlerError> {
        let zone = self
            .cloudflare
            .zone(decl.zone)
            .await
            .map_err(HandlerError::Downstream)?;
        let account = decl.account_id.map(String::from).unwrap_or(zone.account_id);
        Ok((zone.zone_id, account))
    }

    /// The revoke phase — ONE definition, run at the end of a first
    /// pass and again by every clock re-fire. Reads the account, plans
    /// against the current tunnel, deletes what has no connector,
    /// records what it deferred, and completes the step when nothing
    /// is left. A completed step is left alone. Idempotent: what an
    /// earlier pass deleted is simply not listed any more, and its
    /// record survives on the step (`revoked` is appended to, never
    /// replaced).
    async fn revoke_phase(
        &self,
        rule: &str,
        decl: &Declaration<'_>,
        account_id: &str,
        steps: &HashMap<String, StepView>,
        inputs: &RevokeInputs<'_>,
    ) -> Result<(), HandlerError> {
        let job_id = inputs.job_id;
        if steps.get("revoke").is_some_and(|s| s.status == "completed") {
            return Ok(());
        }
        let account = self
            .cloudflare
            .list_tunnels(account_id)
            .await
            .map_err(HandlerError::Downstream)?;
        let plan = plan_revoke(
            &account,
            decl.tunnel_name_prefix,
            inputs.current_id,
            &inputs.previous_ids,
            inputs.old_name,
        );
        // The pure plan excludes the current tunnel before any other
        // rule is read; this is the runtime lock on that, because the
        // cost of being wrong here is the public hostnames going dark.
        if let Some(c) = plan
            .candidates
            .iter()
            .find(|c| c.tunnel.id == inputs.current_id)
        {
            return Err(HandlerError::Permanent(format!(
                "revoke plan listed the CURRENT tunnel {} ({}) as a candidate; refusing",
                c.tunnel.name, c.tunnel.id
            )));
        }

        let mut deleted: Vec<JsonValue> = Vec::new();
        let mut deleted_lines: Vec<String> = Vec::new();
        let mut deferred: Vec<String> = Vec::new();
        for c in &plan.candidates {
            let (name, id, how) = (&c.tunnel.name, &c.tunnel.id, c.found_by.as_str());
            let live = self
                .cloudflare
                .tunnel_connections(account_id, id)
                .await
                .map_err(HandlerError::Downstream)?;
            if live > 0 {
                deferred.push(format!(
                    "{name} (id {id}) still has {live} live connection(s), {how}"
                ));
                continue;
            }
            let deleted_now = self
                .cloudflare
                .delete_tunnel(account_id, id)
                .await
                .map_err(HandlerError::Downstream)?;
            deleted.push(json!({
                "name": name,
                "id": id,
                "found_by": how,
                "deleted_now": deleted_now,
            }));
            deleted_lines.push(if deleted_now {
                format!(
                    "{name} (id {id}) — 0 connections, deleted via DELETE \
                     /accounts/{account_id}/cfd_tunnel/{id} ({how}); every connector token \
                     minted for it is now invalid"
                )
            } else {
                format!("{name} (id {id}) — already absent when deleted ({how})")
            });
        }
        // The merge observed, never assumed: the account no longer
        // lists what was deleted.
        if !deleted.is_empty() {
            let after = self
                .cloudflare
                .list_tunnels(account_id)
                .await
                .map_err(HandlerError::Downstream)?;
            let still: Vec<&str> = after
                .iter()
                .filter(|t| deleted.iter().any(|d| d["id"] == t.id))
                .map(|t| t.name.as_str())
                .collect();
            if !still.is_empty() {
                return Err(HandlerError::Downstream(format!(
                    "tunnel(s) still listed after delete: {}",
                    still.join(", ")
                )));
            }
        }
        let absent_lines: Vec<String> = plan
            .already_absent
            .iter()
            .map(|a| format!("{a} — already absent from the account (already revoked)"))
            .collect();

        // The step's `revoked` is cumulative across passes: what a
        // first pass deleted must still be on the record when the
        // re-fire completes the step days later.
        let mut lines: Vec<String> = steps
            .get("revoke")
            .and_then(|s| s.metadata.get("revoked"))
            .and_then(|v| v.as_str())
            .map(|s| s.split("; ").map(String::from).collect())
            .unwrap_or_default();
        for l in deleted_lines.iter().chain(absent_lines.iter()) {
            if !lines.contains(l) {
                lines.push(l.clone());
            }
        }
        let current = inputs.current_id;
        if lines.is_empty() && deferred.is_empty() {
            lines.push(format!(
                "nothing to revoke: account {account_id} lists no {}-* tunnel other than the \
                 current {current}, the install rewrote no CNAME away from another tunnel, and \
                 the scope step named no old_token",
                decl.tunnel_name_prefix
            ));
        }
        let confirmed_dead = if deleted.is_empty() {
            format!(
                "account {account_id} tunnel list (is_deleted=false) holds nothing to revoke \
                 besides the current {current}"
            )
        } else {
            format!(
                "account {account_id} tunnel list (is_deleted=false) no longer contains {}",
                deleted
                    .iter()
                    .filter_map(|d| d["name"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        if !deferred.is_empty() {
            // Deleting under a live connector is deleting somebody's
            // traffic. Say so on the step, leave it open, and stop —
            // not an error: the daily re-fire returns until they are
            // gone.
            let note = format!(
                "{} — revoke deferred; the current tunnel {current} is installed and verified. \
                 The daily rule {REFIRE_RULE} re-runs this phase and completes it once no \
                 candidate has a connector left, so this step needs no hand.",
                deferred.join("; ")
            );
            tracing::warn!(job_id, %note);
            let mut evidence = vec![("revoke_deferred", note)];
            if !lines.is_empty() {
                evidence.push(("revoked", lines.join("; ")));
            }
            self.put_step(rule, job_id, steps, "revoke", &evidence, false)
                .await?;
            if !deleted.is_empty() {
                self.record_phase(
                    rule,
                    decl.credential_id,
                    RotationPhase::Revoked,
                    json!({
                        "job_id": job_id,
                        "current_tunnel_id": current,
                        "deleted": deleted,
                        "deferred": deferred,
                        "already_absent": plan.already_absent,
                        "complete": false,
                    }),
                )
                .await?;
            }
            return Ok(());
        }

        self.record_phase(
            rule,
            decl.credential_id,
            RotationPhase::Revoked,
            json!({
                "job_id": job_id,
                "current_tunnel_id": current,
                "deleted": deleted,
                "deferred": deferred,
                "already_absent": plan.already_absent,
                "confirmed_dead": confirmed_dead,
                "complete": true,
            }),
        )
        .await?;
        let mut evidence = vec![
            ("revoked", lines.join("; ")),
            ("confirmed_dead", confirmed_dead),
        ];
        if steps
            .get("revoke")
            .is_some_and(|s| s.metadata.contains_key("revoke_deferred"))
        {
            evidence.push((
                "revoke_deferred",
                "cleared: no candidate has a connector left".to_string(),
            ));
        }
        self.complete_step(rule, job_id, steps, "revoke", &evidence)
            .await
    }

    /// The clock door: only the revoke phase, over every open rotation
    /// packet about this credential whose revoke step is ready.
    async fn revoke_sweep(&self, rule: &str, decl: &Declaration<'_>) -> Result<(), HandlerError> {
        let (_zone_id, account_id) = self.account_id(decl).await?;
        let installed = self
            .secrets
            .read_key(decl.secret_namespace, decl.secret_name, decl.secret_key)
            .await
            .map_err(HandlerError::Downstream)?;
        let Some(current_id) = installed.as_deref().and_then(installed_tunnel_id) else {
            // Without the current tunnel there is no "not the current
            // one", and a sweep that guessed would be the outage the
            // exclusion exists to prevent.
            return Err(HandlerError::Downstream(format!(
                "Secret {}/{} key {} names no tunnel; cannot tell the current tunnel from the \
                 old ones, so nothing is revoked",
                decl.secret_namespace, decl.secret_name, decl.secret_key
            )));
        };
        let mut swept = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for job in self.open_rotations().await? {
            let steps = steps_of(&job);
            if !is_about_credential(&job, &steps, decl.credential_id) {
                continue;
            }
            let Some(job_id) = job.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if !steps
                .get("revoke")
                .is_some_and(|s| s.status == "ready" || s.status == "active")
            {
                continue;
            }
            swept += 1;
            let inputs = RevokeInputs {
                job_id,
                current_id: &current_id,
                old_name: steps.get("scope").and_then(|s| old_token_of(&s.metadata)),
                previous_ids: previous_ids_recorded(&steps),
            };
            if let Err(e) = self
                .revoke_phase(rule, decl, &account_id, &steps, &inputs)
                .await
            {
                // One packet's failure does not hold the others; it is
                // named in the error the runner records.
                failures.push(format!("{job_id}: {e}"));
            }
        }
        tracing::info!(
            swept,
            failed = failures.len(),
            "revoke re-fire over open {ROTATION_KIND} packets"
        );
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HandlerError::Downstream(format!(
                "revoke re-fire: {} of {swept} packet(s) failed: {}",
                failures.len(),
                failures.join(" | ")
            )))
        }
    }

    /// The step door: the whole rotation for the packet whose scope
    /// step just completed.
    async fn rotate_packet(
        &self,
        rule: &str,
        decl: &Declaration<'_>,
        ev: &StepEvent<'_>,
    ) -> Result<(), HandlerError> {
        let verify_hostname = decl
            .verify_hostname
            .ok_or_else(|| HandlerError::MissingArg("verify_hostname".into()))?;
        // Per-rotation facts off the scope step: the old tunnel's
        // NAME (never a value), if the scoper knows it — one more
        // revoke candidate beside what the phase discovers.
        let old_tunnel = old_token_of(ev.metadata);

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

        let (zone_id, account_id) = self.account_id(decl).await?;
        let account_id = account_id.as_str();

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
        // What the records pointed at before: the revoke phase's
        // second source of candidates, recorded on the install step so
        // a re-fire days later reads the same fact. A converge pass
        // finds the CNAMEs already right and learns nothing new, so
        // the recorded value is kept.
        let mut previous_ids: Vec<String> = previous_ids_recorded(&steps);
        for host in &decl.hostnames {
            let (outcome, previous) = self
                .upsert_cname(&zone_id, host, &cname_target, &comment)
                .await
                .map_err(HandlerError::Downstream)?;
            if let Some(p) = previous
                && p != tunnel_id
                && !previous_ids.contains(&p)
            {
                previous_ids.push(p);
            }
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
                    "previous_tunnel_ids": previous_ids,
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
                ("previous_tunnel_id", previous_ids.join(", ")),
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
                .edge_status(verify_hostname, VERIFY_PATH)
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
                 and https://{verify_hostname}{VERIFY_PATH} answered {edge_text}; old tunnel NOT revoked"
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
                    "verify_hostname": verify_hostname,
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
                         {connections} live connection(s); https://{verify_hostname}{VERIFY_PATH} \
                         answered {edge_text} through the edge"
                    ),
                ),
                ("method", "api".to_string()),
            ],
        )
        .await?;

        // Revoke — last, discovered, and only what has no connector
        // left. The tunnel just installed is the current one.
        let inputs = RevokeInputs {
            job_id: ev.job_id,
            current_id: &tunnel_id,
            old_name: old_tunnel,
            previous_ids,
        };
        self.revoke_phase(rule, decl, account_id, &steps, &inputs)
            .await
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
        let decl = Declaration::parse(args)?;
        let rule = ctx.rule_name.as_str();
        // Two doors, told apart by what fired: a step event carries the
        // packet; a clock firing carries only `_day` and must say
        // `phase = "revoke"` — there is nothing to mint for on a day.
        let is_step_event = ctx
            .event_payload
            .get("job_id")
            .and_then(|v| v.as_str())
            .is_some();
        match (is_step_event, decl.phase) {
            (true, None) => {
                let ev = StepEvent::from_payload(&ctx.event_payload)?;
                self.rotate_packet(rule, &decl, &ev).await
            }
            (false, Some(REVOKE_PHASE)) => self.revoke_sweep(rule, &decl).await,
            (false, other) => Err(HandlerError::Permanent(format!(
                "a clock firing of {} must declare phase = {REVOKE_PHASE:?} (got {other:?}): \
                 there is no packet to rotate on a day, only revokes to finish",
                self.name()
            ))),
            (true, Some(phase)) => Err(HandlerError::Permanent(format!(
                "phase = {phase:?} is the clock rule's declaration; a step event runs the whole \
                 rotation and declares no phase"
            ))),
        }
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

    // ----- the revoke plan (5e8efcf5) -----

    #[test]
    fn plan_revoke_finds_prefix_siblings_previous_targets_and_the_scoped_name_never_the_current() {
        let account = vec![
            tun("t-new", "boss-cluster-2ff7a586", 2),
            tun("t-old", "boss-cluster-e202c7c3", 0),
            tun("t-prev", "legacy-by-hand", 0),
            tun("t-named", "boss-gcp-tunnel", 1),
            tun("t-other", "something-else-entirely", 0),
        ];
        let plan = plan_revoke(
            &account,
            "boss-cluster",
            "t-new",
            &["t-prev".to_string()],
            Some("boss-gcp-tunnel"),
        );
        let found: Vec<(&str, FoundBy)> = plan
            .candidates
            .iter()
            .map(|c| (c.tunnel.id.as_str(), c.found_by))
            .collect();
        assert_eq!(
            found,
            vec![
                ("t-old", FoundBy::Prefix),
                ("t-prev", FoundBy::PreviousTarget),
                ("t-named", FoundBy::Scoped),
            ]
        );
        assert!(plan.already_absent.is_empty());
    }

    #[test]
    fn plan_revoke_never_lists_the_current_tunnel_whatever_names_it() {
        // The current tunnel matches EVERY rule at once — the prefix,
        // the previous target, and old_token — and has no connector.
        // It is still not a candidate: exclusion happens before any
        // rule is read.
        let account = vec![tun("t-new", "boss-cluster-2ff7a586", 0)];
        let plan = plan_revoke(
            &account,
            "boss-cluster",
            "t-new",
            &["t-new".to_string()],
            Some("boss-cluster-2ff7a586"),
        );
        assert!(plan.candidates.is_empty(), "{plan:?}");
        assert!(
            plan.already_absent.is_empty(),
            "the current is not 'absent' either: {plan:?}"
        );
    }

    #[test]
    fn plan_revoke_names_what_the_packet_named_that_the_account_no_longer_lists() {
        // Deleted by hand in the dashboard, as e202c7c3's was on
        // 2026-09-16: already revoked, not an error.
        let account = vec![tun("t-new", "boss-cluster-2ff7a586", 2)];
        let plan = plan_revoke(
            &account,
            "boss-cluster",
            "t-new",
            &["t-prev".to_string()],
            Some("boss-gcp-tunnel"),
        );
        assert!(plan.candidates.is_empty());
        assert_eq!(
            plan.already_absent,
            vec![
                "tunnel id t-prev (the previous CNAME target)".to_string(),
                "tunnel boss-gcp-tunnel (named old_token on the scope step)".to_string(),
            ]
        );
    }

    #[test]
    fn a_cname_target_names_its_tunnel_and_anything_else_names_none() {
        assert_eq!(
            tunnel_id_of_cname_target("t-old.cfargotunnel.com"),
            Some("t-old")
        );
        assert_eq!(
            tunnel_id_of_cname_target("t-old.cfargotunnel.com."),
            Some("t-old")
        );
        assert_eq!(tunnel_id_of_cname_target("10.20.0.33"), None);
        assert_eq!(tunnel_id_of_cname_target(".cfargotunnel.com"), None);
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
        async fn list_tunnels(&self, account_id: &str) -> Result<Vec<TunnelInfo>, String> {
            assert_eq!(account_id, "acct-from-zone");
            let conns = self.connections.lock().unwrap();
            Ok(self
                .tunnels
                .lock()
                .unwrap()
                .iter()
                .map(|t| TunnelInfo {
                    connections: conns.get(&t.id).copied().unwrap_or(0),
                    ..t.clone()
                })
                .collect())
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
        // The CNAME the install rewrote pointed at t-old: recorded on
        // the install step for the revoke phase (and its re-fires).
        assert_eq!(install["previous_tunnel_id"], "t-old");
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
        let revoked = revoke["revoked"].as_str().unwrap();
        assert!(
            revoked.contains("boss-gcp-tunnel (id t-old) — 0 connections, deleted via DELETE"),
            "{revoked}"
        );
        assert!(
            revoke["confirmed_dead"]
                .as_str()
                .unwrap()
                .contains("no longer contains boss-gcp-tunnel")
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
        assert_eq!(installed_ev["previous_tunnel_ids"], json!(["t-old"]));
        let verified_ev = &events[2].1;
        assert_eq!(verified_ev["connections"], 2);
        assert_eq!(verified_ev["verify_hostname"], "playground.algedonic.dev");
        let revoked_ev = &events[3].1;
        assert_eq!(revoked_ev["current_tunnel_id"], new_id);
        assert_eq!(revoked_ev["deleted"].as_array().unwrap().len(), 1);
        assert_eq!(revoked_ev["deleted"][0]["name"], "boss-gcp-tunnel");
        assert_eq!(revoked_ev["deleted"][0]["id"], "t-old");
        assert_eq!(revoked_ev["deleted"][0]["deleted_now"], true);
        assert_eq!(revoked_ev["complete"], true);
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
        assert!(
            note.contains("boss-gcp-tunnel (id t-old) still has 1 live connection"),
            "{note}"
        );
        assert!(note.contains("revoke deferred"), "{note}");
        assert!(
            note.contains(REFIRE_RULE),
            "the deferred step names what acts next: {note}"
        );
        assert_eq!(revoke["metadata"]["kept"], "yes");
        assert!(
            revoke["metadata"].get("revoked").is_none(),
            "nothing was revoked, so nothing claims to be: {revoke}"
        );

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
        // orphan. No old_token was named and the account holds no
        // other prefix sibling, so the revoke phase finds nothing —
        // and says so, completing the step rather than leaving it to
        // a person (5e8efcf5).
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
        assert_eq!(events[0].1["replaced_orphan"], true);
        assert_eq!(events[3].1["deleted"], json!([]));
        assert_eq!(events[3].1["complete"], true);
        let puts = _captured.lock().unwrap().clone();
        let revoke = puts
            .iter()
            .find(|(sid, _)| sid == "step-revoke")
            .map(|(_, b)| b.clone())
            .expect("revoke completed");
        assert_eq!(revoke["status"], "completed");
        assert!(
            revoke["metadata"]["revoked"]
                .as_str()
                .unwrap()
                .starts_with("nothing to revoke:"),
            "{revoke}"
        );
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

    // ----- the re-fire door (5e8efcf5) -----

    /// A jobs-api stub for the CLOCK door: serves the open-rotation
    /// list (`GET /api/jobs?kind=rotate-a-credential&status=open`) from
    /// the packets given, plus the same step PUT and rotation-door
    /// capture as `stub_jobs_api`.
    async fn stub_sweep_api(packets: Vec<JsonValue>) -> (String, Captured, Captured) {
        use axum::extract::{Path, Query};
        use axum::{Json, Router, routing::get, routing::post, routing::put};

        let captured: Captured = Default::default();
        let cap = captured.clone();
        let rotations: Captured = Default::default();
        let rot = rotations.clone();
        let list = Arc::new(packets);
        let jobs = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let list = list.clone();
                    async move {
                        assert_eq!(q.get("kind").map(String::as_str), Some(ROTATION_KIND));
                        assert_eq!(q.get("status").map(String::as_str), Some("open"));
                        Json(json!({ "data": *list, "total": list.len() }))
                    }
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

    /// The clock rule's declaration: only what the revoke phase reads.
    fn sweep_args() -> Vec<(String, Value)> {
        [
            ("phase", "revoke"),
            ("zone", "algedonic.dev"),
            ("secret_namespace", "boss"),
            ("secret_name", "cloudflare-tunnel-credentials"),
            ("secret_key", "credentials.json"),
            ("tunnel_name_prefix", "boss-cluster"),
            ("credential_id", "cloudflare-tunnel-credentials"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect()
    }

    /// What a schedule firing carries: the day, and no packet.
    fn clock_ctx() -> InvocationContext {
        InvocationContext {
            rule_name: REFIRE_RULE.into(),
            triggering_event_id: "clock-2026-09-17".into(),
            triggering_topic: "schedule".into(),
            event_payload: json!({ "_day": "2026-09-17" }),
        }
    }

    /// An open rotation packet parked at `revoke`, as the list serves
    /// it: issue/install/verify done by an earlier pass, the install
    /// step carrying what the CNAMEs pointed at before.
    fn packet_at_revoke(
        id: &str,
        subject: &str,
        scope: JsonValue,
        previous_tunnel_id: &str,
        revoke_status: &str,
    ) -> JsonValue {
        let step = |slug: &str, status: &str, metadata: JsonValue| {
            json!({
                "id": format!("step-{slug}"),
                "spec_slug": slug,
                "status": status,
                "metadata": metadata,
            })
        };
        json!({
            "id": id,
            "kind": ROTATION_KIND,
            "status": "open",
            "subject": { "subject_kind": "custom", "id": subject },
            "steps": [
                step("scope", "completed", scope),
                step("issue", "completed", json!({ "issued": "…" })),
                step(
                    "install",
                    "completed",
                    json!({ "installed": "…", "previous_tunnel_id": previous_tunnel_id })
                ),
                step("verify", "completed", json!({ "verified": "…" })),
                step(
                    "revoke",
                    revoke_status,
                    json!({ "kept": "yes", "revoke_deferred": "an earlier pass deferred" })
                ),
            ],
        })
    }

    const JOB: &str = "2ff7a586-56da-4479-a76f-c486a2da0d90";

    fn about_the_tunnel() -> JsonValue {
        json!({ "credential": "cloudflare-tunnel-credentials" })
    }

    fn installed_secret(tunnel_id: &str) -> Arc<FakeSecrets> {
        FakeSecrets::seeded(
            "boss",
            "cloudflare-tunnel-credentials",
            "credentials.json",
            &tunnel_credentials_json("acct-from-zone", tunnel_id, "c2VjcmV0"),
        )
    }

    #[tokio::test]
    async fn a_first_pass_without_old_token_discovers_the_siblings_and_defers_the_live_one() {
        // No old_token (e202c7c3's case: the old name was not knowable
        // from the pod). The account holds a prefix sibling with no
        // connector, a prefix sibling still serving, and an unrelated
        // tunnel with a different prefix.
        let cf = FakeCloudflare::with_tunnels(vec![
            tun("t-old", "boss-cluster-e202c7c3", 0),
            tun("t-live", "boss-cluster-11111111", 2),
            tun("t-other", "something-else-entirely", 0),
        ]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let (jobs_url, captured, rotations) = stub_jobs_api(PENDING_PHASES).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter);
        h.invoke(&rotation_args(), &scope_done_ctx(None))
            .await
            .expect("a partly deferred revoke is not an error");

        // Deleted exactly the idle sibling; the live sibling, the
        // unrelated tunnel and the new current survive.
        assert_eq!(
            cf.deleted.lock().unwrap().clone(),
            vec!["t-old".to_string()]
        );
        assert!(cf.has_tunnel("boss-cluster-11111111"));
        assert!(cf.has_tunnel("something-else-entirely"));
        assert!(cf.has_tunnel(NEW_NAME));

        // The revoke step is annotated with BOTH facts and left open.
        let puts = captured.lock().unwrap().clone();
        let (sid, revoke) = puts.last().unwrap();
        assert_eq!(sid, "step-revoke");
        assert!(revoke.get("status").is_none(), "left open: {revoke}");
        let revoked = revoke["metadata"]["revoked"].as_str().unwrap();
        assert!(
            revoked.contains("boss-cluster-e202c7c3 (id t-old) — 0 connections"),
            "{revoked}"
        );
        assert!(
            revoked.contains("named with the rotation prefix"),
            "{revoked}"
        );
        let note = revoke["metadata"]["revoke_deferred"].as_str().unwrap();
        assert!(
            note.contains("boss-cluster-11111111 (id t-live) still has 2 live connection(s)"),
            "{note}"
        );
        assert!(note.contains(REFIRE_RULE), "{note}");

        // The deletion is a fact and is evented as one, marked as an
        // incomplete pass.
        let events = rotations.lock().unwrap().clone();
        let (path, revoked_ev) = events.last().unwrap();
        assert_eq!(path, "cloudflare-tunnel-credentials/revoked");
        assert_eq!(revoked_ev["complete"], false);
        assert_eq!(revoked_ev["deleted"][0]["id"], "t-old");
        assert_eq!(revoked_ev["deferred"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_clock_re_fire_revokes_what_is_left_and_completes_the_phase() {
        // The connector retired since the first pass: every candidate
        // is idle now. The sweep runs ONLY the revoke phase.
        let cf = FakeCloudflare::with_tunnels(vec![
            tun("t-new", NEW_NAME, 2),
            tun("t-old", "boss-cluster-e202c7c3", 0),
            tun("t-prev", "boss-gcp-tunnel", 0),
            tun("t-other", "something-else-entirely", 0),
        ]);
        let secrets = installed_secret("t-new");
        let restarter = FakeRestarter::live(cf.clone());
        let packet = packet_at_revoke(JOB, "bosspipeline", about_the_tunnel(), "t-prev", "ready");
        let (jobs_url, captured, rotations) = stub_sweep_api(vec![packet]).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter.clone());
        h.invoke(&sweep_args(), &clock_ctx())
            .await
            .expect("the re-fire succeeds");

        assert_eq!(
            cf.deleted.lock().unwrap().clone(),
            vec!["t-old".to_string(), "t-prev".to_string()]
        );
        assert!(
            cf.has_tunnel(NEW_NAME),
            "the current tunnel is never a candidate"
        );
        assert!(cf.has_tunnel("something-else-entirely"));
        // Revoke-only: nothing minted, no DNS, no restart.
        assert!(cf.created.lock().unwrap().is_empty());
        assert!(cf.dns_writes.lock().unwrap().is_empty());
        assert!(restarter.restarts.lock().unwrap().is_empty());

        // ONE write: the revoke step completed, cumulative record,
        // the deferral cleared, existing keys kept.
        let puts = captured.lock().unwrap().clone();
        assert_eq!(puts.len(), 1, "{puts:?}");
        let (sid, revoke) = &puts[0];
        assert_eq!(sid, "step-revoke");
        assert_eq!(revoke["status"], "completed");
        let revoked = revoke["metadata"]["revoked"].as_str().unwrap();
        assert!(
            revoked.contains("boss-cluster-e202c7c3 (id t-old) — 0 connections"),
            "{revoked}"
        );
        assert!(
            revoked.contains("boss-gcp-tunnel (id t-prev) — 0 connections"),
            "{revoked}"
        );
        assert!(revoked.contains("the previous CNAME target"), "{revoked}");
        assert!(
            revoke["metadata"]["revoke_deferred"]
                .as_str()
                .unwrap()
                .starts_with("cleared"),
            "{revoke}"
        );
        assert_eq!(revoke["metadata"]["kept"], "yes");

        let events = rotations.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "cloudflare-tunnel-credentials/revoked");
        assert_eq!(events[0].1["job_id"], JOB);
        assert_eq!(events[0].1["complete"], true);
        assert_eq!(events[0].1["deleted"].as_array().unwrap().len(), 2);
    }

    /// An open-rotation read with no `data` array is no answer (backlog
    /// 37fc5837). The paged loop stopped on `got == 0` and returned NO
    /// ROTATIONS, so the clock door ACKed a sweep that looked at nothing
    /// and a deferred revoke stayed deferred with nobody told — the
    /// 833e2d0a defect in this handler's own copy of the walk.
    #[tokio::test]
    async fn an_open_rotation_read_with_no_data_array_refuses_by_name() {
        use crate::handlers::listing_stub::{assert_refused_by_name, no_data_array, serve};
        let cf = FakeCloudflare::with_tunnels(vec![
            tun("t-new", NEW_NAME, 2),
            tun("t-old", "boss-cluster-e202c7c3", 0),
        ]);
        let secrets = installed_secret("t-new");
        let restarter = FakeRestarter::live(cf.clone());
        let stub = serve(vec![("/api/jobs", no_data_array())]).await;

        let h = handler(stub.base.clone(), cf.clone(), secrets, restarter);
        let res = h.invoke(&sweep_args(), &clock_ctx()).await;
        assert!(
            cf.deleted.lock().unwrap().is_empty(),
            "nothing revoked blind"
        );
        assert_eq!(stub.writes(), Vec::<String>::new());
        assert_refused_by_name(res, "the open-rotation read");
    }

    #[tokio::test]
    async fn a_clock_re_fire_records_a_hand_deleted_tunnel_as_already_revoked_and_completes() {
        // David deleted the old tunnel in the dashboard (2026-09-16
        // ~14:35Z): the packet still names it (old_token, and the
        // install's previous target) and the account no longer lists
        // it. Already revoked — the phase completes, nothing errors.
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-new", NEW_NAME, 2)]);
        let secrets = installed_secret("t-new");
        let restarter = FakeRestarter::live(cf.clone());
        let scope = json!({
            "credential": "cloudflare-tunnel-credentials",
            "old_token": "boss-gcp-tunnel",
        });
        let packet = packet_at_revoke(JOB, "bosspipeline", scope, "t-prev", "ready");
        let (jobs_url, captured, rotations) = stub_sweep_api(vec![packet]).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter);
        h.invoke(&sweep_args(), &clock_ctx())
            .await
            .expect("already revoked is not an error");

        assert!(cf.deleted.lock().unwrap().is_empty());
        let puts = captured.lock().unwrap().clone();
        assert_eq!(puts.len(), 1);
        let revoke = &puts[0].1;
        assert_eq!(revoke["status"], "completed");
        let revoked = revoke["metadata"]["revoked"].as_str().unwrap();
        assert!(
            revoked.contains(
                "tunnel boss-gcp-tunnel (named old_token on the scope step) — already absent"
            ),
            "{revoked}"
        );
        assert!(
            revoked.contains("tunnel id t-prev (the previous CNAME target) — already absent"),
            "{revoked}"
        );
        let events = rotations.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1["complete"], true);
        assert_eq!(events[0].1["already_absent"].as_array().unwrap().len(), 2);
        assert_eq!(events[0].1["deleted"], json!([]));
    }

    #[tokio::test]
    async fn a_clock_re_fire_defers_again_while_a_sibling_still_has_a_connector() {
        let cf = FakeCloudflare::with_tunnels(vec![
            tun("t-new", NEW_NAME, 2),
            tun("t-live", "boss-cluster-e202c7c3", 1),
        ]);
        let secrets = installed_secret("t-new");
        let restarter = FakeRestarter::live(cf.clone());
        let packet = packet_at_revoke(JOB, "bosspipeline", about_the_tunnel(), "", "ready");
        let (jobs_url, captured, rotations) = stub_sweep_api(vec![packet]).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter);
        h.invoke(&sweep_args(), &clock_ctx())
            .await
            .expect("deferred again is not an error");

        assert!(cf.deleted.lock().unwrap().is_empty());
        let puts = captured.lock().unwrap().clone();
        assert_eq!(puts.len(), 1);
        assert!(
            puts[0].1.get("status").is_none(),
            "still open: {}",
            puts[0].1
        );
        let note = puts[0].1["metadata"]["revoke_deferred"].as_str().unwrap();
        assert!(
            note.contains("(id t-live) still has 1 live connection"),
            "{note}"
        );
        assert!(
            rotations.lock().unwrap().is_empty(),
            "nothing deleted, nothing evented"
        );
    }

    #[tokio::test]
    async fn the_current_tunnel_is_never_revoked_even_with_no_connector_on_it() {
        // The connector is down (0 connections on the current tunnel).
        // The sweep still deletes only the sibling.
        let cf = FakeCloudflare::with_tunnels(vec![
            tun("t-new", NEW_NAME, 0),
            tun("t-old", "boss-cluster-e202c7c3", 0),
        ]);
        let secrets = installed_secret("t-new");
        let restarter = FakeRestarter::live(cf.clone());
        let packet = packet_at_revoke(JOB, "bosspipeline", about_the_tunnel(), "t-new", "ready");
        let (jobs_url, _captured, _rotations) = stub_sweep_api(vec![packet]).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter);
        h.invoke(&sweep_args(), &clock_ctx()).await.expect("ok");

        assert_eq!(
            cf.deleted.lock().unwrap().clone(),
            vec!["t-old".to_string()]
        );
        assert!(cf.has_tunnel(NEW_NAME));
    }

    #[tokio::test]
    async fn a_clock_re_fire_acts_only_through_a_packet_at_revoke_about_this_credential() {
        // An idle sibling exists, but the only open packets are about
        // another credential, or not yet at revoke: nothing is touched.
        let cf = FakeCloudflare::with_tunnels(vec![
            tun("t-new", NEW_NAME, 2),
            tun("t-old", "boss-cluster-e202c7c3", 0),
        ]);
        let secrets = installed_secret("t-new");
        let restarter = FakeRestarter::live(cf.clone());
        let forge = packet_at_revoke(
            "aaaaaaaa-0000-4000-8000-000000000001",
            "boss-dev-forge-token",
            json!({}),
            "",
            "ready",
        );
        let not_yet = packet_at_revoke(
            "bbbbbbbb-0000-4000-8000-000000000002",
            "cloudflare-tunnel-credentials",
            json!({}),
            "",
            "pending",
        );
        let (jobs_url, captured, rotations) = stub_sweep_api(vec![forge, not_yet]).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter);
        h.invoke(&sweep_args(), &clock_ctx()).await.expect("ok");

        assert!(cf.deleted.lock().unwrap().is_empty());
        assert!(captured.lock().unwrap().is_empty());
        assert!(rotations.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_clock_re_fire_with_no_installed_secret_revokes_nothing() {
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-old", "boss-cluster-e202c7c3", 0)]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let packet = packet_at_revoke(JOB, "bosspipeline", about_the_tunnel(), "", "ready");
        let (jobs_url, captured, _rotations) = stub_sweep_api(vec![packet]).await;

        let h = handler(jobs_url, cf.clone(), secrets, restarter);
        let err = h
            .invoke(&sweep_args(), &clock_ctx())
            .await
            .expect_err("no current tunnel, no revoke");
        assert!(format!("{err}").contains("names no tunnel"), "{err}");
        assert!(cf.deleted.lock().unwrap().is_empty());
        assert!(captured.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_clock_firing_without_phase_revoke_is_refused_before_any_call() {
        let cf = FakeCloudflare::with_tunnels(vec![tun("t-old", "boss-cluster-e202c7c3", 0)]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let h = handler("http://127.0.0.1:1".into(), cf.clone(), secrets, restarter);
        let mut args = sweep_args();
        args.retain(|(k, _)| k != "phase");
        let err = h
            .invoke(&args, &clock_ctx())
            .await
            .expect_err("a day has nothing to mint for");
        assert!(err.is_permanent(), "{err:?}");
        assert!(cf.deleted.lock().unwrap().is_empty());
        assert!(cf.has_tunnel("boss-cluster-e202c7c3"));
    }

    #[tokio::test]
    async fn a_step_event_declaring_a_phase_is_refused_before_any_call() {
        let cf = FakeCloudflare::with_tunnels(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let restarter = FakeRestarter::live(cf.clone());
        let h = handler("http://127.0.0.1:1".into(), cf.clone(), secrets, restarter);
        let mut args = rotation_args();
        args.push(("phase".into(), Value::String("revoke".into())));
        let err = h
            .invoke(&args, &scope_done_ctx(None))
            .await
            .expect_err("a step event runs the whole rotation");
        assert!(err.is_permanent(), "{err:?}");
        assert!(cf.created.lock().unwrap().is_empty());
    }
}
