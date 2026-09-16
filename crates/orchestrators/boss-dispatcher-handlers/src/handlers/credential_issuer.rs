//! The credential broker's ports + external adapters (7ee101aa).
//!
//! Split from `credential_rotate_forgejo.rs` deliberately: every call
//! in THIS file is aimed at a non-BOSS endpoint — the forge's admin
//! API (authenticated by the broker's root token), the Cloudflare v4
//! API (the broker's Cloudflare root token, packet 04e5f833), the
//! public edge, and the cluster's own API server (authenticated by
//! the pod's ServiceAccount bearer) — so there is no BOSS actor or
//! sim-origin to stamp, and the file sits on
//! `dispatcher-actor-stamp.sh`'s allow-list for the same recorded
//! reason as `webhook_notify.rs`. The handlers' calls to the jobs API
//! live in the handler files and ARE stamped.
//!
//! NO VALUE IN ANY ERROR: every adapter returns plain-string errors
//! built from URLs and statuses. Forgejo response bodies are dropped
//! on failure because they are not guaranteed value-free; Cloudflare
//! and k8s error bodies are kept (a v4 error envelope and a k8s
//! `Status` carry codes and messages, never a secret) because a
//! failure that cannot be read costs the next diagnosis its evidence
//! (CLAUDE.md §Diagnosis, 2026-09-09).

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{Value as JsonValue, json};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Issuer port — the Forgejo token API behind a trait
// ---------------------------------------------------------------------------

/// One token as the issuer lists it. `token_last_eight` is the
/// identifier Forgejo exposes for cross-checking an installed value
/// against the ledger without ever holding the value itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    pub id: i64,
    pub name: String,
    pub token_last_eight: String,
}

/// The one moment the secret value exists outside its consumption
/// point: the mint response. It goes into the SecretStore and
/// nowhere else — not logs, not events, not errors.
#[derive(Debug, Clone)]
pub struct MintedToken {
    pub id: i64,
    pub sha1: String,
}

/// The Forgejo token API, verified against the live forge
/// (16.0.2+gitea-1.22.0 at the time of writing):
///   GET    /api/v1/admin/users/{u}/tokens          — list
///   POST   /api/v1/admin/users/{u}/tokens          — mint (201 → {id, sha1})
///   DELETE /api/v1/admin/users/{u}/tokens/{ref}    — revoke by id, or name
/// All three accept admin token auth (`Authorization: token …`),
/// unlike the non-admin `/users/{u}/tokens` route, which wants
/// BasicAuth. Errors are plain strings so fakes stay trivial.
#[async_trait]
pub trait ForgeTokenIssuer: Send + Sync {
    async fn list_tokens(&self, user: &str) -> Result<Vec<TokenInfo>, String>;
    async fn create_token(
        &self,
        user: &str,
        name: &str,
        scopes: &[String],
    ) -> Result<MintedToken, String>;
    /// Delete by id-or-name. `Ok(false)` = already absent, which a
    /// re-run treats as success (the point of revoking is absence).
    async fn delete_token(&self, user: &str, token_ref: &str) -> Result<bool, String>;
    /// Verify by effect: authenticate a repo read with `token`.
    async fn repo_readable_with(&self, token: &str, repo: &str) -> Result<bool, String>;
}

// ---------------------------------------------------------------------------
// Secret port — where consumers pick the value up
// ---------------------------------------------------------------------------

/// A named k8s Secret key. The broker only ever touches secrets it
/// is name-granted (RBAC `resourceNames`); `write_key` requires the
/// Secret to pre-exist because `create` cannot be name-scoped.
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn read_key(
        &self,
        namespace: &str,
        name: &str,
        key: &str,
    ) -> Result<Option<String>, String>;
    async fn write_key(
        &self,
        namespace: &str,
        name: &str,
        key: &str,
        value: &str,
    ) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Forgejo adapter
// ---------------------------------------------------------------------------

pub struct ForgejoAdmin {
    client: reqwest::Client,
    base: String,
    root_token: String,
}

impl ForgejoAdmin {
    /// `root_token` is the broker's root credential (k8s Secret
    /// `boss-credential-broker-root`, key `forgejo-token`), handed in
    /// by the binary from env. It is held to sign requests and is
    /// never logged, serialized, or included in an error.
    pub fn new(base: impl Into<String>, root_token: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            base: base.into(),
            root_token: root_token.into(),
        })
    }

    fn auth(&self) -> String {
        format!("token {}", self.root_token)
    }
}

#[async_trait]
impl ForgeTokenIssuer for ForgejoAdmin {
    async fn list_tokens(&self, user: &str) -> Result<Vec<TokenInfo>, String> {
        let url = format!(
            "{}/api/v1/admin/users/{user}/tokens?limit=50",
            self.base.trim_end_matches('/')
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET {url} returned {status}"));
        }
        let rows: Vec<JsonValue> = resp.json().await.map_err(|e| format!("{url}: {e}"))?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                Some(TokenInfo {
                    id: r.get("id")?.as_i64()?,
                    name: r.get("name")?.as_str()?.to_string(),
                    token_last_eight: r
                        .get("token_last_eight")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect())
    }

    async fn create_token(
        &self,
        user: &str,
        name: &str,
        scopes: &[String],
    ) -> Result<MintedToken, String> {
        let url = format!(
            "{}/api/v1/admin/users/{user}/tokens",
            self.base.trim_end_matches('/')
        );
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth())
            .json(&json!({ "name": name, "scopes": scopes }))
            .send()
            .await
            .map_err(|e| format!("POST {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            // Body deliberately dropped: issuer error bodies are not
            // guaranteed value-free, and a status is enough to act on.
            return Err(format!("POST {url} returned {status}"));
        }
        let body: JsonValue = resp.json().await.map_err(|e| format!("{url}: {e}"))?;
        let id = body
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| format!("POST {url}: response missing id"))?;
        let sha1 = body
            .get("sha1")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("POST {url}: response missing sha1"))?
            .to_string();
        Ok(MintedToken { id, sha1 })
    }

    async fn delete_token(&self, user: &str, token_ref: &str) -> Result<bool, String> {
        let url = format!(
            "{}/api/v1/admin/users/{user}/tokens/{token_ref}",
            self.base.trim_end_matches('/')
        );
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| format!("DELETE {url}: {e}"))?;
        match resp.status() {
            s if s.is_success() => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s => Err(format!("DELETE {url} returned {s}")),
        }
    }

    async fn repo_readable_with(&self, token: &str, repo: &str) -> Result<bool, String> {
        let url = format!("{}/api/v1/repos/{repo}", self.base.trim_end_matches('/'));
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("token {token}"))
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        Ok(resp.status().is_success())
    }
}

// ---------------------------------------------------------------------------
// Kubernetes adapter — raw REST against the in-cluster API
// ---------------------------------------------------------------------------

pub struct KubeSecretStore {
    client: reqwest::Client,
    base: String,
    bearer: String,
}

impl KubeSecretStore {
    /// Build from an explicit endpoint + credential (tests, or an
    /// out-of-cluster operator context). `ca_pem` is the cluster CA;
    /// `None` means the endpoint's cert chains to a system root
    /// (plain-http test stubs also land here).
    pub fn new(
        base: impl Into<String>,
        bearer: impl Into<String>,
        ca_pem: Option<&[u8]>,
    ) -> Result<Arc<Self>, String> {
        let mut b = reqwest::Client::builder();
        if let Some(pem) = ca_pem {
            let cert =
                reqwest::Certificate::from_pem(pem).map_err(|e| format!("cluster CA: {e}"))?;
            b = b.add_root_certificate(cert);
        }
        Ok(Arc::new(Self {
            client: b.build().map_err(|e| format!("http client: {e}"))?,
            base: base.into(),
            bearer: bearer.into(),
        }))
    }

    /// The standard in-cluster contract: KUBERNETES_SERVICE_HOST/PORT
    /// + the mounted ServiceAccount token and CA.
    pub fn in_cluster() -> Result<Arc<Self>, String> {
        const SA: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .map_err(|_| "KUBERNETES_SERVICE_HOST unset (not in a cluster)".to_string())?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let token = std::fs::read_to_string(format!("{SA}/token"))
            .map_err(|e| format!("read SA token: {e}"))?;
        let ca = std::fs::read(format!("{SA}/ca.crt")).map_err(|e| format!("read SA ca: {e}"))?;
        Self::new(
            format!("https://{host}:{port}"),
            token.trim().to_string(),
            Some(&ca),
        )
    }
}

#[async_trait]
impl SecretStore for KubeSecretStore {
    async fn read_key(
        &self,
        namespace: &str,
        name: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        let url = format!(
            "{}/api/v1/namespaces/{namespace}/secrets/{name}",
            self.base.trim_end_matches('/')
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.bearer)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        match resp.status() {
            reqwest::StatusCode::NOT_FOUND => return Ok(None),
            s if !s.is_success() => return Err(format!("GET {url} returned {s}")),
            _ => {}
        }
        let body: JsonValue = resp.json().await.map_err(|e| format!("{url}: {e}"))?;
        let Some(b64) = body
            .get("data")
            .and_then(|d| d.get(key))
            .and_then(|v| v.as_str())
        else {
            return Ok(None);
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("secret {namespace}/{name} key {key}: base64: {e}"))?;
        String::from_utf8(bytes)
            .map(|s| Some(s.trim().to_string()))
            .map_err(|_| format!("secret {namespace}/{name} key {key}: not utf-8"))
    }

    async fn write_key(
        &self,
        namespace: &str,
        name: &str,
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        let url = format!(
            "{}/api/v1/namespaces/{namespace}/secrets/{name}",
            self.base.trim_end_matches('/')
        );
        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        let resp = self
            .client
            .patch(&url)
            .bearer_auth(&self.bearer)
            .header("Content-Type", "application/merge-patch+json")
            .json(&json!({ "data": { key: b64 } }))
            .send()
            .await
            .map_err(|e| format!("PATCH {url}: {e}"))?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Err(format!(
                "secret {namespace}/{name} does not exist — the broker is \
                 deliberately not granted `create` (it cannot be name-scoped); \
                 pre-create it: kubectl create secret generic {name} -n {namespace}"
            )),
            s => Err(format!("PATCH {url} returned {s}")),
        }
    }
}

/// Registered when the binary lacks broker configuration, so a rule
/// naming the rotation handler dead-letters loudly with the missing
/// knob's name instead of tripping `UnknownHandler` and aborting
/// dispatch for every co-fired rule.
pub struct Unconfigured(pub String);

#[async_trait]
impl ForgeTokenIssuer for Unconfigured {
    async fn list_tokens(&self, _u: &str) -> Result<Vec<TokenInfo>, String> {
        Err(self.0.clone())
    }
    async fn create_token(&self, _u: &str, _n: &str, _s: &[String]) -> Result<MintedToken, String> {
        Err(self.0.clone())
    }
    async fn delete_token(&self, _u: &str, _t: &str) -> Result<bool, String> {
        Err(self.0.clone())
    }
    async fn repo_readable_with(&self, _t: &str, _r: &str) -> Result<bool, String> {
        Err(self.0.clone())
    }
}

#[async_trait]
impl SecretStore for Unconfigured {
    async fn read_key(&self, _n: &str, _s: &str, _k: &str) -> Result<Option<String>, String> {
        Err(self.0.clone())
    }
    async fn write_key(&self, _n: &str, _s: &str, _k: &str, _v: &str) -> Result<(), String> {
        Err(self.0.clone())
    }
}

// ---------------------------------------------------------------------------
// Cloudflare Tunnel port — the second issuer (packet 04e5f833)
// ---------------------------------------------------------------------------

/// One tunnel as the account lists it. `connections` is the number of
/// live connector connections the API reports for it — the fact the
/// revoke phase refuses to delete under (a tunnel with a connector
/// still attached is serving somebody).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelInfo {
    pub id: String,
    pub name: String,
    pub connections: usize,
}

/// The account + zone the root token reaches. Both ids come from ONE
/// `GET /zones?name=<zone>` — the zone object carries its `account.id`,
/// and Zone:Read is in the root token's grant, so no second permission
/// (Account Settings:Read for `GET /accounts`) is needed to learn the
/// account tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneInfo {
    pub zone_id: String,
    pub account_id: String,
}

/// One DNS record as the zone lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsRecord {
    pub id: String,
    pub name: String,
    pub record_type: String,
    pub content: String,
    pub proxied: bool,
}

/// The Cloudflare v4 API surface the tunnel rotation needs, measured
/// from cloudflared's own client (`cfapi/tunnel.go`, master 2026-09-16)
/// and the zone/DNS calls `boss-tls.yaml` already makes:
///   GET    /zones?name=<zone>                                  — zone id + account id
///   GET    /accounts/{a}/cfd_tunnel?name=<n>&is_deleted=false  — ledger by name
///   POST   /accounts/{a}/cfd_tunnel {name, tunnel_secret, config_src}
///   GET    /accounts/{a}/cfd_tunnel/{id}/connections           — [{id, conns: [...]}]
///   DELETE /accounts/{a}/cfd_tunnel/{id}                       — refuses under live conns
///   GET    /zones/{z}/dns_records?type=CNAME&name=<fqdn>
///   POST   /zones/{z}/dns_records / PATCH /zones/{z}/dns_records/{id}
/// The tunnel secret is generated CLIENT-SIDE (32 random bytes,
/// base64) exactly as `cloudflared tunnel create` does, so the
/// credentials file can be built from what the handler already holds
/// and never depends on the create response echoing it back.
#[async_trait]
pub trait CloudflareTunnels: Send + Sync {
    async fn zone(&self, zone_name: &str) -> Result<ZoneInfo, String>;
    async fn find_tunnel(&self, account_id: &str, name: &str)
    -> Result<Option<TunnelInfo>, String>;
    /// `tunnel_secret_b64` is the base64 of >= 32 random bytes. Returns
    /// the new tunnel's id.
    async fn create_tunnel(
        &self,
        account_id: &str,
        name: &str,
        tunnel_secret_b64: &str,
    ) -> Result<String, String>;
    /// Live connections across every connector attached to the tunnel.
    async fn tunnel_connections(&self, account_id: &str, tunnel_id: &str) -> Result<usize, String>;
    /// `Ok(false)` = already absent. Deliberately NO `cascade=true`: a
    /// tunnel with live connections is refused by the API, and that
    /// refusal is the safety the revoke phase relies on.
    async fn delete_tunnel(&self, account_id: &str, tunnel_id: &str) -> Result<bool, String>;
    async fn find_dns_record(
        &self,
        zone_id: &str,
        record_type: &str,
        name: &str,
    ) -> Result<Option<DnsRecord>, String>;
    async fn create_dns_record(
        &self,
        zone_id: &str,
        record_type: &str,
        name: &str,
        content: &str,
        proxied: bool,
        comment: &str,
    ) -> Result<(), String>;
    async fn update_dns_record(
        &self,
        zone_id: &str,
        record_id: &str,
        content: &str,
        proxied: bool,
        comment: &str,
    ) -> Result<(), String>;
    /// Verify by effect through the edge: the HTTP status the public
    /// hostname answers at `path` (an `Err` is a transport failure,
    /// not a status).
    async fn edge_status(&self, hostname: &str, path: &str) -> Result<u16, String>;
}

/// Restart a Deployment so a connector that reads its credentials
/// file at start picks the new file up. `Ok(false)` = no such
/// Deployment (the connector car has not landed here yet) — recorded,
/// not fatal, because the install is still complete.
#[async_trait]
pub trait WorkloadRestarter: Send + Sync {
    /// Is the Deployment there at all? Asked BEFORE a rotation mints:
    /// pointing the public hostnames at a tunnel no connector serves
    /// is an outage, not a rotation.
    async fn deployment_exists(&self, namespace: &str, name: &str) -> Result<bool, String>;
    /// `reason` is what the rollout is FOR (the rotation packet + the
    /// new tunnel id): it becomes the pod-template annotation whose
    /// change triggers the rollout, so the same rotation asked twice
    /// is one restart, and the reason is readable on the pod.
    async fn restart_deployment(
        &self,
        namespace: &str,
        name: &str,
        reason: &str,
    ) -> Result<bool, String>;
}

/// The one shape the connector accepts: `cloudflared`'s
/// `connection.Credentials` struct with Go's default field names
/// (`connection/connection.go`, read 2026-09-16 — `[]byte` marshals
/// as standard base64, `uuid.UUID` as its string form; `Endpoint` is
/// optional on read). `cloudflared tunnel run <TunnelID>
/// --credentials-file <this>` is what consumes it.
pub fn tunnel_credentials_json(
    account_id: &str,
    tunnel_id: &str,
    tunnel_secret_b64: &str,
) -> String {
    json!({
        "AccountTag": account_id,
        "TunnelSecret": tunnel_secret_b64,
        "TunnelID": tunnel_id,
    })
    .to_string()
}

/// The `TunnelID` an installed credentials file names, if the value
/// parses as one. Anything else — empty, not JSON, a different shape
/// — is `None`, which the planner reads as "nothing of ours installed".
pub fn installed_tunnel_id(credentials_json: &str) -> Option<String> {
    serde_json::from_str::<JsonValue>(credentials_json)
        .ok()?
        .get("TunnelID")?
        .as_str()
        .map(str::to_string)
}

/// 32 random bytes, base64 — the tunnel secret cloudflared's own
/// `create` generates (`generateTunnelSecret`, 32 bytes) and the API
/// minimum ("at least 32 bytes and encoded as a base64 string").
pub fn fresh_tunnel_secret_b64() -> String {
    use rand::RngExt;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes[..]);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Cloudflare adapter — the v4 API with the broker's root token
// ---------------------------------------------------------------------------

pub struct CloudflareApi {
    client: reqwest::Client,
    base: String,
    root_token: String,
}

impl CloudflareApi {
    /// `root_token` is the broker's Cloudflare root credential (k8s
    /// Secret `boss-credential-broker-root`, key `cloudflare-token`;
    /// Cloudflare Tunnel:Edit + Zone:DNS:Edit + Zone:Read), handed in
    /// by the binary from env. Held to sign requests; never logged,
    /// serialized, or included in an error.
    pub fn new(base: impl Into<String>, root_token: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            base: base.into(),
            root_token: root_token.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base.trim_end_matches('/'))
    }

    /// Send, then unwrap the v4 envelope `{success, errors, result}`.
    /// EVERY failure names method + URL + status + body: Cloudflare
    /// error bodies carry codes and messages, never a secret (the only
    /// value in this exchange is the tunnel secret in OUR request), so
    /// there is no reason to drop the one thing a diagnosis needs.
    async fn call(&self, req: reqwest::RequestBuilder, what: &str) -> Result<JsonValue, String> {
        let resp = req
            .bearer_auth(&self.root_token)
            .send()
            .await
            .map_err(|e| format!("{what}: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("{what} returned {status}: {}", clip(&text)));
        }
        let body: JsonValue = serde_json::from_str(&text)
            .map_err(|e| format!("{what}: not JSON ({e}): {}", clip(&text)))?;
        if body.get("success").and_then(|v| v.as_bool()) != Some(true) {
            return Err(format!("{what}: success=false: {}", clip(&text)));
        }
        Ok(body.get("result").cloned().unwrap_or(JsonValue::Null))
    }
}

/// Error bodies in full up to a screenful; a runaway HTML page is
/// still named by its head.
fn clip(s: &str) -> String {
    const MAX: usize = 600;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let head: String = s.chars().take(MAX).collect();
        format!("{head}…")
    }
}

fn tunnel_from(v: &JsonValue) -> Option<TunnelInfo> {
    Some(TunnelInfo {
        id: v.get("id")?.as_str()?.to_string(),
        name: v.get("name")?.as_str()?.to_string(),
        connections: v
            .get("connections")
            .and_then(|c| c.as_array())
            .map(Vec::len)
            .unwrap_or(0),
    })
}

#[async_trait]
impl CloudflareTunnels for CloudflareApi {
    async fn zone(&self, zone_name: &str) -> Result<ZoneInfo, String> {
        let url = self.url(&format!("/zones?name={zone_name}"));
        let result = self
            .call(self.client.get(&url), &format!("GET {url}"))
            .await?;
        let zone = result
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| format!("GET {url}: zone {zone_name} not visible to the root token"))?;
        let zone_id = zone
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("GET {url}: zone has no id"))?;
        let account_id = zone
            .get("account")
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("GET {url}: zone {zone_name} carries no account.id"))?;
        Ok(ZoneInfo {
            zone_id: zone_id.to_string(),
            account_id: account_id.to_string(),
        })
    }

    async fn find_tunnel(
        &self,
        account_id: &str,
        name: &str,
    ) -> Result<Option<TunnelInfo>, String> {
        let url = self.url(&format!(
            "/accounts/{account_id}/cfd_tunnel?name={name}&is_deleted=false"
        ));
        let result = self
            .call(self.client.get(&url), &format!("GET {url}"))
            .await?;
        Ok(result
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(tunnel_from)
            .find(|t| t.name == name))
    }

    async fn create_tunnel(
        &self,
        account_id: &str,
        name: &str,
        tunnel_secret_b64: &str,
    ) -> Result<String, String> {
        let url = self.url(&format!("/accounts/{account_id}/cfd_tunnel"));
        let body = json!({
            "name": name,
            "tunnel_secret": tunnel_secret_b64,
            "config_src": "local",
        });
        let result = self
            .call(self.client.post(&url).json(&body), &format!("POST {url}"))
            .await?;
        result
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("POST {url}: response result carries no id"))
    }

    async fn tunnel_connections(&self, account_id: &str, tunnel_id: &str) -> Result<usize, String> {
        let url = self.url(&format!(
            "/accounts/{account_id}/cfd_tunnel/{tunnel_id}/connections"
        ));
        let result = self
            .call(self.client.get(&url), &format!("GET {url}"))
            .await?;
        Ok(result
            .as_array()
            .into_iter()
            .flatten()
            .map(|client| {
                client
                    .get("conns")
                    .and_then(|c| c.as_array())
                    .map(Vec::len)
                    .unwrap_or(0)
            })
            .sum())
    }

    async fn delete_tunnel(&self, account_id: &str, tunnel_id: &str) -> Result<bool, String> {
        let url = self.url(&format!("/accounts/{account_id}/cfd_tunnel/{tunnel_id}"));
        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&self.root_token)
            .send()
            .await
            .map_err(|e| format!("DELETE {url}: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        match status {
            s if s.is_success() => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s => Err(format!("DELETE {url} returned {s}: {}", clip(&text))),
        }
    }

    async fn find_dns_record(
        &self,
        zone_id: &str,
        record_type: &str,
        name: &str,
    ) -> Result<Option<DnsRecord>, String> {
        let url = self.url(&format!(
            "/zones/{zone_id}/dns_records?type={record_type}&name={name}"
        ));
        let result = self
            .call(self.client.get(&url), &format!("GET {url}"))
            .await?;
        Ok(result
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some(DnsRecord {
                    id: r.get("id")?.as_str()?.to_string(),
                    name: r.get("name")?.as_str()?.to_string(),
                    record_type: r.get("type")?.as_str()?.to_string(),
                    content: r.get("content")?.as_str()?.to_string(),
                    proxied: r.get("proxied").and_then(|v| v.as_bool()).unwrap_or(false),
                })
            })
            .find(|r| r.name == name && r.record_type == record_type))
    }

    async fn create_dns_record(
        &self,
        zone_id: &str,
        record_type: &str,
        name: &str,
        content: &str,
        proxied: bool,
        comment: &str,
    ) -> Result<(), String> {
        let url = self.url(&format!("/zones/{zone_id}/dns_records"));
        let body = json!({
            "type": record_type,
            "name": name,
            "content": content,
            "proxied": proxied,
            "ttl": 1,
            "comment": comment,
        });
        self.call(self.client.post(&url).json(&body), &format!("POST {url}"))
            .await
            .map(|_| ())
    }

    async fn update_dns_record(
        &self,
        zone_id: &str,
        record_id: &str,
        content: &str,
        proxied: bool,
        comment: &str,
    ) -> Result<(), String> {
        let url = self.url(&format!("/zones/{zone_id}/dns_records/{record_id}"));
        let body = json!({ "content": content, "proxied": proxied, "comment": comment });
        self.call(self.client.patch(&url).json(&body), &format!("PATCH {url}"))
            .await
            .map(|_| ())
    }

    async fn edge_status(&self, hostname: &str, path: &str) -> Result<u16, String> {
        let url = format!("https://{hostname}{path}");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        Ok(resp.status().as_u16())
    }
}

/// `kubectl rollout restart` is a PATCH of a pod-template annotation
/// (kubectl stamps `restartedAt` with the wallclock; this stamps
/// `boss.dev/restarted-for` with the REASON, which changes exactly
/// when a new rotation asks and never reads a clock) — name-scoped
/// RBAC (`patch` on the one Deployment, boss-credential-broker.yaml),
/// the same posture as the Secret write.
#[async_trait]
impl WorkloadRestarter for KubeSecretStore {
    async fn deployment_exists(&self, namespace: &str, name: &str) -> Result<bool, String> {
        let url = format!(
            "{}/apis/apps/v1/namespaces/{namespace}/deployments/{name}",
            self.base.trim_end_matches('/')
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.bearer)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        match status {
            s if s.is_success() => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s => Err(format!("GET {url} returned {s}: {}", clip(&text))),
        }
    }

    async fn restart_deployment(
        &self,
        namespace: &str,
        name: &str,
        reason: &str,
    ) -> Result<bool, String> {
        let url = format!(
            "{}/apis/apps/v1/namespaces/{namespace}/deployments/{name}",
            self.base.trim_end_matches('/')
        );
        let resp = self
            .client
            .patch(&url)
            .bearer_auth(&self.bearer)
            .header("Content-Type", "application/strategic-merge-patch+json")
            .json(&json!({
                "spec": { "template": { "metadata": { "annotations": {
                    "boss.dev/restarted-for": reason
                } } } }
            }))
            .send()
            .await
            .map_err(|e| format!("PATCH {url}: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        match status {
            s if s.is_success() => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            s => Err(format!("PATCH {url} returned {s}: {}", clip(&text))),
        }
    }
}

#[async_trait]
impl CloudflareTunnels for Unconfigured {
    async fn zone(&self, _z: &str) -> Result<ZoneInfo, String> {
        Err(self.0.clone())
    }
    async fn find_tunnel(&self, _a: &str, _n: &str) -> Result<Option<TunnelInfo>, String> {
        Err(self.0.clone())
    }
    async fn create_tunnel(&self, _a: &str, _n: &str, _s: &str) -> Result<String, String> {
        Err(self.0.clone())
    }
    async fn tunnel_connections(&self, _a: &str, _t: &str) -> Result<usize, String> {
        Err(self.0.clone())
    }
    async fn delete_tunnel(&self, _a: &str, _t: &str) -> Result<bool, String> {
        Err(self.0.clone())
    }
    async fn find_dns_record(
        &self,
        _z: &str,
        _t: &str,
        _n: &str,
    ) -> Result<Option<DnsRecord>, String> {
        Err(self.0.clone())
    }
    async fn create_dns_record(
        &self,
        _z: &str,
        _t: &str,
        _n: &str,
        _c: &str,
        _p: bool,
        _m: &str,
    ) -> Result<(), String> {
        Err(self.0.clone())
    }
    async fn update_dns_record(
        &self,
        _z: &str,
        _r: &str,
        _c: &str,
        _p: bool,
        _m: &str,
    ) -> Result<(), String> {
        Err(self.0.clone())
    }
    async fn edge_status(&self, _h: &str, _p: &str) -> Result<u16, String> {
        Err(self.0.clone())
    }
}

#[async_trait]
impl WorkloadRestarter for Unconfigured {
    async fn deployment_exists(&self, _n: &str, _d: &str) -> Result<bool, String> {
        Err(self.0.clone())
    }
    async fn restart_deployment(&self, _n: &str, _d: &str, _r: &str) -> Result<bool, String> {
        Err(self.0.clone())
    }
}

#[cfg(test)]
mod cloudflare_tests {
    use super::*;

    /// The credentials.json shape is pinned to cloudflared's
    /// `connection.Credentials` (Go default field names): exactly the
    /// three keys its reader needs, secret as standard base64 of 32
    /// bytes.
    #[test]
    fn credentials_json_is_the_shape_cloudflared_reads() {
        let secret = fresh_tunnel_secret_b64();
        let s = tunnel_credentials_json("acct-1", "9e0f7a9c-0000-4000-8000-000000000001", &secret);
        let v: JsonValue = serde_json::from_str(&s).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["AccountTag", "TunnelID", "TunnelSecret"]);
        assert_eq!(v["AccountTag"], "acct-1");
        assert_eq!(v["TunnelID"], "9e0f7a9c-0000-4000-8000-000000000001");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(v["TunnelSecret"].as_str().unwrap())
            .expect("TunnelSecret is standard base64");
        assert_eq!(raw.len(), 32, "cloudflared requires >= 32 decoded bytes");
        assert_eq!(
            installed_tunnel_id(&s).as_deref(),
            Some("9e0f7a9c-0000-4000-8000-000000000001")
        );
    }

    #[test]
    fn two_fresh_secrets_differ() {
        assert_ne!(fresh_tunnel_secret_b64(), fresh_tunnel_secret_b64());
    }

    #[test]
    fn a_foreign_or_empty_secret_value_names_no_tunnel() {
        assert_eq!(installed_tunnel_id(""), None);
        assert_eq!(installed_tunnel_id("not json"), None);
        assert_eq!(installed_tunnel_id(r#"{"token":"x"}"#), None);
    }
}
