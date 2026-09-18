//! The dispatcher rule registry, read for the readiness read.
//!
//! The rules a deployment enforces live in the dispatcher — loaded from
//! `dispatcher_rules` at its boot and served at
//! `GET /api/dispatcher/rules`, with each rule's `source` (`product` or
//! `tenant:<id>`). The readiness read asks that surface rather than the
//! table: the surface is what is FIRING, and reading a sibling service's
//! table from here would be a second reader of its schema. Port + a
//! reqwest adapter + an in-memory fake, the `owner_resolution` shape.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};
use serde_json::Value;

/// The service name in the transport error's text.
#[derive(Debug)]
pub struct Dispatcher;

impl ServiceLabel for Dispatcher {
    const NAME: &'static str = "dispatcher";
}

#[async_trait]
pub trait DispatcherRules: Send + Sync {
    /// Every enforced rule, as the dispatcher's read surface serves it
    /// (`name`, `version`, `source`, `do`, …). `Err` is the surface not
    /// answering — a different fact from "no rules", and answered as one.
    async fn enforced_rules(&self) -> Result<Vec<Value>, String>;
}

/// `GET {base}/api/dispatcher/rules` → its `rules` array.
pub struct ReqwestDispatcherRules {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestDispatcherRules {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl DispatcherRules for ReqwestDispatcherRules {
    async fn enforced_rules(&self) -> Result<Vec<Value>, String> {
        let url = format!("{}/api/dispatcher/rules", self.base_url);
        let body: Value = http_client::get_json::<Dispatcher, Value>(&self.http, &url)
            .await
            .map_err(|e: HttpClientError<Dispatcher>| format!("GET {url}: {e}"))?;
        // The surface answers 200 with `error` set when its own load
        // failed; that is not an empty registry.
        if let Some(err) = body.get("error").and_then(Value::as_str) {
            return Err(format!("GET {url}: the dispatcher reported: {err}"));
        }
        Ok(body
            .get("rules")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
}

/// A fixed rule list, for tests and the in-memory path.
pub struct FakeDispatcherRules(pub Vec<Value>);

#[async_trait]
impl DispatcherRules for FakeDispatcherRules {
    async fn enforced_rules(&self) -> Result<Vec<Value>, String> {
        Ok(self.0.clone())
    }
}
