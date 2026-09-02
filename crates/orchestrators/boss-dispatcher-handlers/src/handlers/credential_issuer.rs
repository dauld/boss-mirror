//! The credential broker's ports + external adapters (7ee101aa).
//!
//! Split from `credential_rotate_forgejo.rs` deliberately: every call
//! in THIS file is aimed at a non-BOSS endpoint — the forge's admin
//! API (authenticated by the broker's root token) and the cluster's
//! own API server (authenticated by the pod's ServiceAccount bearer)
//! — so there is no BOSS actor or sim-origin to stamp, and the file
//! sits on `dispatcher-actor-stamp.sh`'s allow-list for the same
//! recorded reason as `webhook_notify.rs`. The handler's calls to
//! the jobs API live in the handler file and ARE stamped.
//!
//! NO VALUE IN ANY ERROR: both adapters return plain-string errors
//! built from URLs and statuses only. Issuer response bodies are
//! dropped on failure because they are not guaranteed value-free.

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
