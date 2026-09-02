//! `credential.rotate.forgejo` — the credential broker's first
//! rotation handler (packet 7ee101aa, first leg).
//!
//! Rotates a Forgejo access token end-to-end as MACHINE steps of a
//! `rotate-a-credential` packet: mint a replacement via the issuer's
//! admin API, install it into the named k8s Secret its consumers
//! mount, verify by effect (the new token can read the repo it
//! exists for), revoke the old token, and record each phase as the
//! completion of the packet's own `issue` / `install` / `verify` /
//! `revoke` steps — so the audit trail IS the packet, with the rule
//! as actor. THE SECRET VALUE NEVER ENTERS A PACKET: every evidence
//! field records an identifier (token name/id, secret path, value
//! length) or an observed effect, never the value.
//!
//! Fired by a rule on `step.done.credential-rotation` — a dedicated
//! StepType (the `gate-verdict` precedent) so the rule targets
//! exactly the scope step of a rotation packet and never fires on
//! an ordinary `task`. The rule row carries the credential's
//! consumer declaration as args (which Secret, which user, which
//! scopes, which repo proves it) — per-credential registry data,
//! not code.
//!
//! ## Idempotence (per rotation packet)
//!
//! JetStream is at-least-once, so the whole flow re-runs safely.
//! The minted token's name is derived from the packet id
//! (`rotation_token_name`), which makes the issuer the idempotence
//! ledger:
//!   - name absent            → mint fresh.
//!   - name present AND the installed Secret's last-eight matches
//!     that token → the rotation already happened; converge the
//!     remaining phases (verify / revoke / step evidence) only.
//!   - name present but the Secret does NOT match → a previous
//!     attempt minted and then died before installing; the value is
//!     unrecoverable (Forgejo returns the sha1 exactly once), so
//!     delete the orphan by name and mint again.
//!
//! Order is the protocol: issue, install, verify, THEN revoke — the
//! destructive call runs last and only after the new credential is
//! proven working, exactly as the workflow's own description demands.

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde_json::{Value as JsonValue, json};
use std::sync::Arc;

use super::common::{StepEvent, dispatcher_actor_header, dispatcher_reader_header};
use super::credential_issuer::{ForgeTokenIssuer, SecretStore, TokenInfo};

// ---------------------------------------------------------------------------
// Pure planning — the decision under test
// ---------------------------------------------------------------------------

/// The minted token's name: `{secret_name}-{first 8 of the packet id}`.
/// The packet id is the rotation's identity, so the name is the
/// idempotence key; the secret-name prefix keeps the issuer's token
/// list legible ("what is this token for" answers itself).
pub fn rotation_token_name(secret_name: &str, job_id: &str) -> String {
    let short: String = job_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    format!("{secret_name}-{short}")
}

/// Last eight characters, the granularity Forgejo's ledger exposes.
pub fn last_eight(s: &str) -> &str {
    let n = s.chars().count();
    if n <= 8 {
        s
    } else {
        let start = s
            .char_indices()
            .nth(n - 8)
            .map(|(i, _)| i)
            .unwrap_or_default();
        &s[start..]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationPlan {
    /// This packet's token exists and the Secret holds it: the
    /// mint+install already happened. Converge the rest only.
    AlreadyInstalled { token_id: i64 },
    /// This packet's token exists but the Secret does not hold it: a
    /// prior attempt lost the value. Delete the orphan, mint again.
    ReplaceStale,
    /// Nothing from this packet on the issuer yet.
    MintFresh,
}

pub fn plan_rotation(
    existing: &[TokenInfo],
    installed_last8: Option<&str>,
    token_name: &str,
) -> RotationPlan {
    match existing.iter().find(|t| t.name == token_name) {
        None => RotationPlan::MintFresh,
        Some(t) => match installed_last8 {
            Some(l8) if l8 == t.token_last_eight => {
                RotationPlan::AlreadyInstalled { token_id: t.id }
            }
            _ => RotationPlan::ReplaceStale,
        },
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

pub struct CredentialRotateForgejo {
    client: reqwest::Client,
    jobs_base: String,
    issuer: Arc<dyn ForgeTokenIssuer>,
    secrets: Arc<dyn SecretStore>,
}

/// One step of the rotation packet as the jobs-api lists it.
struct StepView {
    id: String,
    status: String,
    metadata: serde_json::Map<String, JsonValue>,
}

impl CredentialRotateForgejo {
    pub fn new(
        jobs_base: impl Into<String>,
        issuer: Arc<dyn ForgeTokenIssuer>,
        secrets: Arc<dyn SecretStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: super::common::api_client(),
            jobs_base: jobs_base.into(),
            issuer,
            secrets,
        })
    }

    fn jobs(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// The packet's steps keyed by spec slug. One read serves every
    /// completion below.
    async fn fetch_steps(
        &self,
        job_id: &str,
    ) -> Result<std::collections::HashMap<String, StepView>, HandlerError> {
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
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}"
            )));
        }
        let body: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{url}: {e}")))?;
        let mut out = std::collections::HashMap::new();
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

    /// Complete one packet step with evidence fields, merging the
    /// step's existing metadata (PATCH-on-PUT replaces `metadata`
    /// wholesale). Already-completed steps are left alone — that is
    /// the redelivery path. A slug the packet lacks is skipped: the
    /// packet's workflow version decides which phases it records.
    async fn complete_step(
        &self,
        rule_name: &str,
        job_id: &str,
        steps: &std::collections::HashMap<String, StepView>,
        slug: &str,
        evidence: &[(&str, String)],
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
        let url = format!("{}/api/jobs/{job_id}/steps/{}", self.jobs(), step.id);
        let resp = self
            .client
            .put(&url)
            .header("Content-Type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(rule_name))
            .header("x-sim-origin", super::common::sim_origin_value())
            .json(&json!({ "status": "completed", "metadata": metadata }))
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
}

#[async_trait]
impl Handler for CredentialRotateForgejo {
    fn name(&self) -> &'static str {
        "credential.rotate.forgejo"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let ev = StepEvent::from_payload(&ctx.event_payload)?;

        // The credential's consumer declaration — rule-row data.
        let forge_user = arg_string(args, "forge_user")?;
        let secret_namespace = arg_string(args, "secret_namespace")?;
        let secret_name = arg_string(args, "secret_name")?;
        let secret_key = arg_string(args, "secret_key")?;
        let scopes: Vec<String> = arg_string(args, "scopes")?
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let verify_repo = arg_string(args, "verify_repo")?;

        // Per-rotation facts off the scope step: the old token's
        // NAME OR ID (never its value), if the scoper knows it.
        let old_token = ev
            .metadata
            .get("old_token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let token_name = rotation_token_name(secret_name, ev.job_id);

        // Plan against the issuer's ledger + the installed value.
        let existing = self
            .issuer
            .list_tokens(forge_user)
            .await
            .map_err(HandlerError::Downstream)?;
        let installed = self
            .secrets
            .read_key(secret_namespace, secret_name, secret_key)
            .await
            .map_err(HandlerError::Downstream)?;
        let plan = plan_rotation(&existing, installed.as_deref().map(last_eight), &token_name);

        // issue + install (or converge if a prior run already did).
        let (token_id, token_value) = match plan {
            RotationPlan::AlreadyInstalled { token_id } => {
                // The Secret provably holds this packet's token; it
                // is the only remaining copy of the value.
                (token_id, installed.unwrap_or_default())
            }
            RotationPlan::ReplaceStale | RotationPlan::MintFresh => {
                if plan == RotationPlan::ReplaceStale {
                    // Orphan from a died attempt; its value is gone
                    // for good, so retire it before re-minting.
                    self.issuer
                        .delete_token(forge_user, &token_name)
                        .await
                        .map_err(HandlerError::Downstream)?;
                }
                let minted = self
                    .issuer
                    .create_token(forge_user, &token_name, &scopes)
                    .await
                    .map_err(HandlerError::Downstream)?;
                self.secrets
                    .write_key(secret_namespace, secret_name, secret_key, &minted.sha1)
                    .await
                    .map_err(HandlerError::Downstream)?;
                (minted.id, minted.sha1)
            }
        };

        // Verify by effect BEFORE anything destructive: the new
        // token must actually work at the thing it exists for.
        let readable = self
            .issuer
            .repo_readable_with(&token_value, verify_repo)
            .await
            .map_err(HandlerError::Downstream)?;
        if !readable {
            return Err(HandlerError::Downstream(format!(
                "verify-by-effect failed: token {token_name} cannot read {verify_repo}; \
                 old token NOT revoked"
            )));
        }

        // Revoke the old token — last, and only what the scoper
        // named. Refuses to eat the token this rotation just minted.
        let mut revoke_evidence: Option<(String, String)> = None;
        if let Some(old) = old_token {
            if old == token_name || old == token_id.to_string() {
                return Err(HandlerError::Permanent(format!(
                    "old_token {old:?} names this rotation's own replacement"
                )));
            }
            let deleted = self
                .issuer
                .delete_token(forge_user, old)
                .await
                .map_err(HandlerError::Downstream)?;
            let after = self
                .issuer
                .list_tokens(forge_user)
                .await
                .map_err(HandlerError::Downstream)?;
            let still_there = after
                .iter()
                .any(|t| t.name == old || t.id.to_string() == old);
            if still_there {
                return Err(HandlerError::Downstream(format!(
                    "old token {old} still present after delete"
                )));
            }
            revoke_evidence = Some((
                if deleted {
                    format!("forgejo token {old} deleted via admin API")
                } else {
                    format!("forgejo token {old} was already absent")
                },
                format!("issuer token list for {forge_user} no longer contains {old}"),
            ));
        }

        // Record each phase as the packet's own steps. The step PUTs
        // are the audit events: actor = this rule, evidence =
        // identifiers and observed effects, never a value.
        let steps = self.fetch_steps(ev.job_id).await?;
        self.complete_step(
            &ctx.rule_name,
            ev.job_id,
            &steps,
            "issue",
            &[
                (
                    "issued",
                    format!(
                        "forgejo token {token_name} (id {token_id}) minted via \
                         POST /api/v1/admin/users/{forge_user}/tokens"
                    ),
                ),
                (
                    "issuer",
                    format!(
                        "credential-broker ({}), root credential boss-credential-broker-root",
                        ctx.rule_name
                    ),
                ),
            ],
        )
        .await?;
        self.complete_step(
            &ctx.rule_name,
            ev.job_id,
            &steps,
            "install",
            &[
                (
                    "installed",
                    format!(
                        "k8s Secret {secret_namespace}/{secret_name} key {secret_key} \
                         updated ({} bytes); consumers pick it up from their mounts",
                        token_value.len()
                    ),
                ),
                ("permissions", scopes.join(",")),
            ],
        )
        .await?;
        self.complete_step(
            &ctx.rule_name,
            ev.job_id,
            &steps,
            "verify",
            &[
                (
                    "verified",
                    format!("GET /api/v1/repos/{verify_repo} authenticated with the new token"),
                ),
                ("method", "api".to_string()),
            ],
        )
        .await?;
        if let Some((revoked, confirmed_dead)) = revoke_evidence {
            self.complete_step(
                &ctx.rule_name,
                ev.job_id,
                &steps,
                "revoke",
                &[("revoked", revoked), ("confirmed_dead", confirmed_dead)],
            )
            .await?;
        } else {
            tracing::info!(
                job_id = ev.job_id,
                "no old_token named on the scope step; revoke left to its assignee"
            );
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::credential_issuer::MintedToken;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ----- pure planning -----

    #[test]
    fn token_name_derives_from_secret_and_packet_id() {
        assert_eq!(
            rotation_token_name("boss-dev-forge-token", "7ee101aa-3267-4745"),
            "boss-dev-forge-token-7ee101aa"
        );
    }

    #[test]
    fn last_eight_handles_short_and_long() {
        assert_eq!(last_eight("abc"), "abc");
        assert_eq!(last_eight("0123456789abcdef"), "89abcdef");
    }

    fn tok(id: i64, name: &str, last8: &str) -> TokenInfo {
        TokenInfo {
            id,
            name: name.into(),
            token_last_eight: last8.into(),
        }
    }

    #[test]
    fn plan_mints_fresh_when_issuer_has_no_rotation_token() {
        let plan = plan_rotation(&[tok(1, "other", "aaaa1111")], None, "sec-7ee101aa");
        assert_eq!(plan, RotationPlan::MintFresh);
    }

    #[test]
    fn plan_recognizes_a_completed_install() {
        let plan = plan_rotation(
            &[tok(9, "sec-7ee101aa", "89abcdef")],
            Some("89abcdef"),
            "sec-7ee101aa",
        );
        assert_eq!(plan, RotationPlan::AlreadyInstalled { token_id: 9 });
    }

    #[test]
    fn plan_replaces_an_orphan_whose_value_was_lost() {
        // Token minted, process died before the Secret write: the
        // installed value (or its absence) does not match.
        let existing = [tok(9, "sec-7ee101aa", "89abcdef")];
        assert_eq!(
            plan_rotation(&existing, Some("00000000"), "sec-7ee101aa"),
            RotationPlan::ReplaceStale
        );
        assert_eq!(
            plan_rotation(&existing, None, "sec-7ee101aa"),
            RotationPlan::ReplaceStale
        );
    }

    // ----- in-memory fakes -----

    #[derive(Default)]
    struct FakeIssuer {
        tokens: Mutex<Vec<TokenInfo>>,
        /// sha1 by token name, so verification can check "the minted
        /// value authenticates".
        values: Mutex<HashMap<String, String>>,
        minted: Mutex<Vec<(String, Vec<String>)>>,
        next_id: Mutex<i64>,
    }

    impl FakeIssuer {
        fn with_tokens(tokens: Vec<TokenInfo>) -> Arc<Self> {
            let f = Self::default();
            *f.tokens.lock().unwrap() = tokens;
            *f.next_id.lock().unwrap() = 100;
            Arc::new(f)
        }
    }

    #[async_trait]
    impl ForgeTokenIssuer for FakeIssuer {
        async fn list_tokens(&self, _user: &str) -> Result<Vec<TokenInfo>, String> {
            Ok(self.tokens.lock().unwrap().clone())
        }
        async fn create_token(
            &self,
            _user: &str,
            name: &str,
            scopes: &[String],
        ) -> Result<MintedToken, String> {
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            let sha1 = format!("sha1-of-{name}-{}", *id);
            self.tokens.lock().unwrap().push(TokenInfo {
                id: *id,
                name: name.to_string(),
                token_last_eight: last_eight(&sha1).to_string(),
            });
            self.values
                .lock()
                .unwrap()
                .insert(name.to_string(), sha1.clone());
            self.minted
                .lock()
                .unwrap()
                .push((name.to_string(), scopes.to_vec()));
            Ok(MintedToken { id: *id, sha1 })
        }
        async fn delete_token(&self, _user: &str, token_ref: &str) -> Result<bool, String> {
            let mut toks = self.tokens.lock().unwrap();
            let before = toks.len();
            toks.retain(|t| t.name != token_ref && t.id.to_string() != token_ref);
            Ok(toks.len() < before)
        }
        async fn repo_readable_with(&self, token: &str, _repo: &str) -> Result<bool, String> {
            Ok(self.values.lock().unwrap().values().any(|v| v == token))
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
    /// stub's base URL + captured step PUTs as (step_id, body).
    async fn stub_jobs_api(
        step_statuses: &'static [(&'static str, &'static str)],
    ) -> (String, Captured) {
        use axum::extract::Path;
        use axum::{Json, Router, routing::get, routing::put};

        let captured: Captured = Default::default();
        let cap = captured.clone();
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
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, jobs).await.unwrap() });
        (format!("http://{addr}"), captured)
    }

    fn rotation_args() -> Vec<(String, Value)> {
        [
            ("forge_user", "david"),
            ("secret_namespace", "boss-dev"),
            ("secret_name", "boss-dev-forge-token"),
            ("secret_key", "token"),
            ("scopes", "write:repository"),
            ("verify_repo", "david/boss"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect()
    }

    fn scope_done_ctx(old_token: Option<&str>) -> InvocationContext {
        let mut metadata = json!({
            "credential": "boss-dev-forge-token",
            "reason": "transcript exposure",
            "locations": "k8s secret boss-dev/boss-dev-forge-token",
            "consumers": "/etc/boss-train/forge.token, git credential helper",
        });
        if let Some(old) = old_token {
            metadata["old_token"] = json!(old);
        }
        InvocationContext {
            rule_name: "broker-rotates-the-boss-dev-forge-token".into(),
            triggering_event_id: "evt-rot-1".into(),
            triggering_topic: "step.done.credential-rotation".into(),
            event_payload: json!({
                "job_id": "7ee101aa-3267-4745-8096-06d07df7e144",
                "step_id": "step-scope",
                "kind": "credential-rotation",
                "subject_kind": "custom",
                "subject_id": "boss-dev-forge-token",
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

    #[tokio::test]
    async fn full_rotation_mints_installs_verifies_revokes_and_records() {
        let issuer = FakeIssuer::with_tokens(vec![tok(7, "the-old-write-token", "deadbeef")]);
        // Old token's value known to the issuer so verification-by-
        // value distinguishes old from new.
        issuer
            .values
            .lock()
            .unwrap()
            .insert("the-old-write-token".into(), "old-value".into());
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, captured) = stub_jobs_api(PENDING_PHASES).await;

        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets.clone());
        h.invoke(
            &rotation_args(),
            &scope_done_ctx(Some("the-old-write-token")),
        )
        .await
        .expect("rotation succeeds");

        // Minted once, with the declared scopes, under the packet-derived name.
        let minted = issuer.minted.lock().unwrap().clone();
        assert_eq!(minted.len(), 1);
        assert_eq!(minted[0].0, "boss-dev-forge-token-7ee101aa");
        assert_eq!(minted[0].1, vec!["write:repository".to_string()]);

        // Installed: the Secret holds the minted value.
        let installed = secrets.get("boss-dev", "boss-dev-forge-token", "token");
        assert_eq!(
            installed.as_deref(),
            issuer
                .values
                .lock()
                .unwrap()
                .get("boss-dev-forge-token-7ee101aa")
                .map(String::as_str)
        );

        // Revoked: the old token is gone from the issuer.
        assert!(
            !issuer
                .tokens
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.name == "the-old-write-token")
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
            let value = issuer.values.lock().unwrap()["boss-dev-forge-token-7ee101aa"].clone();
            assert!(
                !flat.contains(&value),
                "secret value leaked into a step body: {flat}"
            );
        }
        let issue_body = &puts[0].1;
        assert!(
            issue_body["metadata"]["issued"]
                .as_str()
                .unwrap()
                .contains("boss-dev-forge-token-7ee101aa")
        );
        assert!(issue_body["metadata"]["issuer"].as_str().is_some());
        let revoke_body = &puts[3].1;
        assert!(
            revoke_body["metadata"]["confirmed_dead"]
                .as_str()
                .unwrap()
                .contains("no longer contains")
        );
    }

    #[tokio::test]
    async fn redelivery_after_a_finished_rotation_mints_nothing() {
        // The issuer already holds this packet's token and the Secret
        // holds its value; every phase step is already completed.
        let sha = "sha1-of-boss-dev-forge-token-7ee101aa-101";
        let issuer = FakeIssuer::with_tokens(vec![tok(
            101,
            "boss-dev-forge-token-7ee101aa",
            last_eight(sha),
        )]);
        issuer
            .values
            .lock()
            .unwrap()
            .insert("boss-dev-forge-token-7ee101aa".into(), sha.into());
        let secrets = FakeSecrets::seeded("boss-dev", "boss-dev-forge-token", "token", sha);
        const ALL_DONE: &[(&str, &str)] = &[
            ("scope", "completed"),
            ("issue", "completed"),
            ("install", "completed"),
            ("verify", "completed"),
            ("revoke", "completed"),
        ];
        let (jobs_url, captured) = stub_jobs_api(ALL_DONE).await;

        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets);
        h.invoke(&rotation_args(), &scope_done_ctx(None))
            .await
            .expect("idempotent re-run succeeds");

        assert!(issuer.minted.lock().unwrap().is_empty(), "no second mint");
        assert!(captured.lock().unwrap().is_empty(), "no step rewrites");
    }

    #[tokio::test]
    async fn a_lost_value_replay_retires_the_orphan_and_mints_again() {
        // Prior attempt minted (id 55) then died before the Secret
        // write — the Secret is empty, the value unrecoverable.
        let issuer =
            FakeIssuer::with_tokens(vec![tok(55, "boss-dev-forge-token-7ee101aa", "51gone55")]);
        *issuer.next_id.lock().unwrap() = 100;
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, _captured) = stub_jobs_api(PENDING_PHASES).await;

        let h = CredentialRotateForgejo::new(jobs_url, issuer.clone(), secrets.clone());
        h.invoke(&rotation_args(), &scope_done_ctx(None))
            .await
            .expect("replay succeeds");

        let toks = issuer.tokens.lock().unwrap().clone();
        let mine: Vec<_> = toks
            .iter()
            .filter(|t| t.name == "boss-dev-forge-token-7ee101aa")
            .collect();
        assert_eq!(mine.len(), 1, "exactly one rotation token survives");
        assert_ne!(mine[0].id, 55, "the orphan was retired");
        assert!(
            secrets
                .get("boss-dev", "boss-dev-forge-token", "token")
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_failed_verification_stops_before_anything_destructive() {
        struct UnverifiableIssuer(Arc<FakeIssuer>);
        #[async_trait]
        impl ForgeTokenIssuer for UnverifiableIssuer {
            async fn list_tokens(&self, u: &str) -> Result<Vec<TokenInfo>, String> {
                self.0.list_tokens(u).await
            }
            async fn create_token(
                &self,
                u: &str,
                n: &str,
                s: &[String],
            ) -> Result<MintedToken, String> {
                self.0.create_token(u, n, s).await
            }
            async fn delete_token(&self, u: &str, t: &str) -> Result<bool, String> {
                self.0.delete_token(u, t).await
            }
            async fn repo_readable_with(&self, _t: &str, _r: &str) -> Result<bool, String> {
                Ok(false)
            }
        }
        let inner = FakeIssuer::with_tokens(vec![tok(7, "the-old-write-token", "deadbeef")]);
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, captured) = stub_jobs_api(PENDING_PHASES).await;

        let h = CredentialRotateForgejo::new(
            jobs_url,
            Arc::new(UnverifiableIssuer(inner.clone())),
            secrets,
        );
        let err = h
            .invoke(
                &rotation_args(),
                &scope_done_ctx(Some("the-old-write-token")),
            )
            .await
            .expect_err("verification failure is an error");
        assert!(matches!(err, HandlerError::Downstream(_)));

        // The old token survives and no packet step was touched.
        assert!(
            inner
                .tokens
                .lock()
                .unwrap()
                .iter()
                .any(|t| t.name == "the-old-write-token")
        );
        assert!(captured.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn naming_the_new_token_as_old_is_refused_permanently() {
        let issuer = FakeIssuer::with_tokens(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let (jobs_url, _c) = stub_jobs_api(PENDING_PHASES).await;
        let h = CredentialRotateForgejo::new(jobs_url, issuer, secrets);
        let err = h
            .invoke(
                &rotation_args(),
                &scope_done_ctx(Some("boss-dev-forge-token-7ee101aa")),
            )
            .await
            .expect_err("self-revocation is refused");
        assert!(err.is_permanent(), "got {err:?}");
    }

    #[tokio::test]
    async fn missing_declaration_arg_is_reported() {
        let issuer = FakeIssuer::with_tokens(vec![]);
        let secrets = Arc::new(FakeSecrets::default());
        let h = CredentialRotateForgejo::new("http://127.0.0.1:1", issuer, secrets);
        let mut args = rotation_args();
        args.retain(|(k, _)| k != "verify_repo");
        let err = h
            .invoke(&args, &scope_done_ctx(None))
            .await
            .expect_err("missing arg");
        assert!(matches!(err, HandlerError::MissingArg(_)));
    }
}
