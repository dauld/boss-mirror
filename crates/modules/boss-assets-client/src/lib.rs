//! HTTP client port for reaching the boss-assets service.
//!
//! Several domain services need to ask assets questions as part of
//! their own write guards — "does this account have any open
//! service tickets before we delete it?", "are any active assets
//! still using this system model before we delete it?", etc. We
//! express those questions as trait methods so production code
//! can call the real assets HTTP API while tests can substitute a
//! fake without spinning up a real assets process.
//!
//! The trait and the reqwest-backed adapter live in this crate
//! so both `boss-people` (for the account delete guard) and
//! `boss-catalog` (for the system model delete guard) can depend
//! on one shared definition instead of duplicating it or creating
//! an awkward `catalog -> people` dependency.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};

/// Service-name marker for the shared [`HttpClientError`] —
/// `Display` text reads `"assets service unreachable: …"`.
#[derive(Debug)]
pub struct Assets;
impl ServiceLabel for Assets {
    const NAME: &'static str = "assets";
}

/// Transport error for the Assets client. Alias of the shared
/// [`HttpClientError`] so existing `AssetsClientError::Unreachable`
/// constructors and matches keep compiling.
pub type AssetsClientError = HttpClientError<Assets>;

/// Questions domain services ask assets before running destructive
/// operations on their own data.
#[async_trait]
pub trait AssetsClient: Send + Sync {
    /// Count of currently-open service tickets associated with the
    /// given account. Returns `0` for unknown accounts. Used by the
    /// boss-people account delete guard.
    async fn open_ticket_count_for_account(
        &self,
        account_id: &str,
    ) -> Result<u64, AssetsClientError>;

    /// Count of assets in any active lifecycle phase (i.e. not
    /// decommissioned) that reference the given catalog SKU.
    /// Returns `0` for unknown SKUs. Used by the boss-catalog
    /// system-model delete guard.
    async fn active_asset_count_for_sku(&self, sku: &str) -> Result<u64, AssetsClientError>;
}

/// Production `AssetsClient` that calls the assets HTTP API over
/// reqwest. 5-second timeout per call so an unresponsive assets
/// service can't wedge a delete operation indefinitely.
pub struct ReqwestAssetsClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestAssetsClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }

    async fn fetch_count(&self, url: &str) -> Result<u64, AssetsClientError> {
        let body: serde_json::Value = http_client::get_json(&self.http, url).await?;
        body.get("count")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| AssetsClientError::MalformedBody(format!("missing count in {body}")))
    }
}

#[async_trait]
impl AssetsClient for ReqwestAssetsClient {
    async fn open_ticket_count_for_account(
        &self,
        account_id: &str,
    ) -> Result<u64, AssetsClientError> {
        let url = format!(
            "{}/api/assets/accounts/{}/open-tickets/count",
            self.base_url, account_id
        );
        self.fetch_count(&url).await
    }

    async fn active_asset_count_for_sku(&self, sku: &str) -> Result<u64, AssetsClientError> {
        let url = format!("{}/api/assets/kb/{}/active-count", self.base_url, sku);
        self.fetch_count(&url).await
    }
}

/// Canned answer shape for [`FakeAssetsClient`].
///
/// - `Count(n)` — return `n` for every key.
/// - `PerKey(map)` — return the keyed count (account id or SKU),
///   defaulting to `0` for keys not in the map.
/// - `Unreachable(msg)` — fail every call with
///   [`AssetsClientError::Unreachable`], to exercise fail-closed paths.
pub enum FakeAssetsResponse {
    Count(u64),
    PerKey(std::collections::HashMap<String, u64>),
    Unreachable(String),
}

/// Test fake for [`AssetsClient`] with canned answers.
///
/// Domain services consult assets inside their own write guards
/// ("does this account have open tickets before we delete it?", "are
/// any active assets still using this SKU?"). Tests inject this fake
/// via `Arc<dyn AssetsClient>` and pre-load the answer the real assets
/// service would give, without spinning up an assets process.
///
/// Every method records the queried key, so `.calls()` reflects what
/// the guard asked regardless of which method it exercised — callers
/// can assert the guard actually fired before a destructive write.
///
/// Constructors:
/// - [`with_count`](Self::with_count) — same count for any key.
/// - [`with_per_account`](Self::with_per_account) /
///   [`with_per_sku`](Self::with_per_sku) — per-key counts (the two are
///   aliases; the distinct names read at the call site).
/// - [`unreachable`](Self::unreachable) — fail every call.
pub struct FakeAssetsClient {
    response: FakeAssetsResponse,
    calls: std::sync::Mutex<Vec<String>>,
}

impl FakeAssetsClient {
    /// Return `n` for every account/SKU query.
    pub fn with_count(n: u64) -> Self {
        Self {
            response: FakeAssetsResponse::Count(n),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Return per-account open-ticket counts, defaulting to `0` for
    /// accounts not in the map.
    pub fn with_per_account(map: std::collections::HashMap<String, u64>) -> Self {
        Self {
            response: FakeAssetsResponse::PerKey(map),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Return per-SKU active-system counts, defaulting to `0` for SKUs
    /// not in the map. Alias of [`with_per_account`](Self::with_per_account)
    /// — both back the same keyed lookup; the name reads at the call site.
    pub fn with_per_sku(map: std::collections::HashMap<String, u64>) -> Self {
        Self::with_per_account(map)
    }

    /// Fail every call with [`AssetsClientError::Unreachable`].
    pub fn unreachable(msg: impl Into<String>) -> Self {
        Self {
            response: FakeAssetsResponse::Unreachable(msg.into()),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Keys (account ids or SKUs) the guard has queried, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("poisoned fake-assets mutex")
            .clone()
    }

    fn answer(&self, key: &str) -> Result<u64, AssetsClientError> {
        self.calls
            .lock()
            .expect("poisoned fake-assets mutex")
            .push(key.to_string());
        match &self.response {
            FakeAssetsResponse::Count(n) => Ok(*n),
            FakeAssetsResponse::PerKey(map) => Ok(map.get(key).copied().unwrap_or(0)),
            FakeAssetsResponse::Unreachable(msg) => {
                Err(AssetsClientError::Unreachable(msg.clone()))
            }
        }
    }
}

#[async_trait]
impl AssetsClient for FakeAssetsClient {
    async fn open_ticket_count_for_account(
        &self,
        account_id: &str,
    ) -> Result<u64, AssetsClientError> {
        self.answer(account_id)
    }

    async fn active_asset_count_for_sku(&self, sku: &str) -> Result<u64, AssetsClientError> {
        self.answer(sku)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn with_count_returns_constant_and_records_calls() {
        let fake = FakeAssetsClient::with_count(3);
        assert_eq!(
            fake.open_ticket_count_for_account("account-1")
                .await
                .unwrap(),
            3
        );
        assert_eq!(fake.active_asset_count_for_sku("sku-a").await.unwrap(), 3);
        assert_eq!(fake.calls(), vec!["account-1", "sku-a"]);
    }

    #[tokio::test]
    async fn per_key_defaults_to_zero_for_unknown_keys() {
        let mut map = std::collections::HashMap::new();
        map.insert("account-1".to_string(), 5u64);
        let fake = FakeAssetsClient::with_per_account(map);
        assert_eq!(
            fake.open_ticket_count_for_account("account-1")
                .await
                .unwrap(),
            5
        );
        assert_eq!(
            fake.open_ticket_count_for_account("account-2")
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn unreachable_fails_every_call() {
        let fake = FakeAssetsClient::unreachable("connection refused");
        let err = fake.active_asset_count_for_sku("sku-a").await.unwrap_err();
        assert!(matches!(err, AssetsClientError::Unreachable(_)));
    }
}
