//! JSON APIs exposed by the gateway itself (not proxied).
//!
//! Today:
//! - `GET /api/session` — returns the authenticated username
//!   from the `boss_session` cookie so the frontend can identify the user.
//!   Returns 401 if the cookie is missing, malformed, tampered, or expired.
//! - `GET /api/tenant/manifest` — returns the active tenant's `[modules]`
//!   block (plus any `[labels]` overrides) so the SPA can gate sidebar
//!   entries and surface tenant-specific terminology.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::AppState;
use boss_gateway::session::{self, Session, find_cookie};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SessionResponse {
    pub username: String,
    /// Seconds since epoch.
    pub expires_at: u64,
    /// Boss employee id resolved at session-mint time (CF Access email
    /// lookup or login). `None` if the authenticated identity
    /// has no matching employee row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub employee_id: Option<String>,
    /// Role code (Class registry, subject_kind=employee). `None` for
    /// unknown users — the SPA renders those as "unrecognized".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

pub async fn session(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    match extract_session(&headers, &state.session_key) {
        Some(s) => Json(SessionResponse {
            username: s.username,
            expires_at: s.expiry,
            employee_id: s.employee_id,
            role: s.role,
        })
        .into_response(),
        None => (StatusCode::UNAUTHORIZED, "not signed in").into_response(),
    }
}

fn extract_session(headers: &HeaderMap, key: &[u8]) -> Option<Session> {
    let cookie_header = headers.get(header::COOKIE).and_then(|v| v.to_str().ok())?;
    let raw = find_cookie(cookie_header, session::COOKIE_NAME)?;
    Session::decode(raw, key).ok()
}

#[derive(Debug, Deserialize, Default)]
struct TenantMeta {
    /// The tenant's own name for itself. The SPA chrome had
    /// "Algedonic" / "Ales" hardcoded at three render sites, so the
    /// used-device-shop tenant rendered a brewery's name in its top
    /// bar. Branding is tenant data; core should not know it.
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    tenant_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TenantToml {
    #[serde(default)]
    meta: TenantMeta,
    #[serde(default)]
    modules: std::collections::BTreeMap<String, bool>,
    #[serde(default)]
    labels: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct TenantManifest {
    /// Tenant display name, e.g. "Algedonic Ales". Absent when the
    /// tenant.toml has no `[meta]` — the SPA falls back to "BOSS",
    /// which is right for a deployment that has not named itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub modules: std::collections::BTreeMap<String, bool>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub labels: std::collections::BTreeMap<String, String>,
}

/// `GET /api/tenant/manifest` — read the active tenant's tenant.toml
/// and return its `[modules]` + `[labels]` blocks.
///
/// Path: `BOSS_TENANT_MANIFEST_TOML` env var; default
/// `/etc/boss-gateway/tenant.toml` if present, else
/// `/opt/boss/examples/brewery/seeds/tenant.toml` (the brewery demo).
///
/// Failure modes are all silent → empty manifest. The SPA defaults
/// to "all modules enabled" when the manifest payload is empty, so a
/// missing or unparseable file falls back to that all-enabled default
/// rather than blanking the UI.
pub async fn tenant_manifest() -> Response {
    Json(tenant_manifest_now()).into_response()
}

/// The manifest as a value — what `/api/tenant/manifest` answers, and
/// what the static server inlines into `index.html` so the first
/// paint already knows the tenant (5578e42d). One reader of
/// tenant.toml, two doors.
pub fn tenant_manifest_now() -> TenantManifest {
    match load_tenant_toml() {
        Some(parsed) => TenantManifest {
            display_name: parsed.meta.display_name,
            tenant_id: parsed.meta.tenant_id,
            modules: parsed.modules,
            labels: parsed.labels,
        },
        None => TenantManifest {
            display_name: None,
            tenant_id: None,
            modules: Default::default(),
            labels: Default::default(),
        },
    }
}

fn tenant_toml_path() -> Option<String> {
    // 1. Explicit env override always wins.
    if let Ok(p) = std::env::var("BOSS_TENANT_MANIFEST_TOML") {
        return Some(p);
    }
    // 2. Walk the candidate list, return the first that exists. The
    //    /etc/boss-gateway/tenant.toml path is where production
    //    installs drop the tenant manifest; the in-tree examples
    //    paths are the OSS-quickstart fallbacks so a fresh clone
    //    boots with the brewery manifest active by default — no
    //    symlink dance required.
    for candidate in [
        "/etc/boss-gateway/tenant.toml",
        "/opt/boss/examples/brewery/seeds/tenant.toml",
        "examples/brewery/seeds/tenant.toml",
    ] {
        if std::path::Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }
    None
}

fn load_tenant_toml() -> Option<TenantToml> {
    let path = tenant_toml_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&text).ok()
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevenueCategory {
    pub code: String,
    pub label: String,
}

/// `GET /api/finance/revenue-categories` — return the tenant's named
/// revenue categories so the SPA's invoice-line dropdown stops
/// shipping a hardcoded `CATEGORY_KEYS` superset.
///
/// Source: every `[labels]` entry under tenant.toml whose key starts
/// with `finance.revenue_category.` becomes one `{code, label}` row.
/// Stable order (sorted by code) so the dropdown doesn't rearrange
/// between page loads. Empty list on a tenant that hasn't named any
/// categories — the SPA falls back to free-text entry, which is the
/// honest default for an unconfigured tenant.
pub async fn revenue_categories() -> Response {
    const PREFIX: &str = "finance.revenue_category.";
    let rows = match load_tenant_toml() {
        Some(parsed) => parsed
            .labels
            .into_iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(PREFIX).map(|code| RevenueCategory {
                    code: code.to_string(),
                    label: v,
                })
            })
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    Json(rows).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const KEY: &[u8; 32] = b"test-key-0123456789abcdef0123456";

    fn headers_with_cookie(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{}={}", session::COOKIE_NAME, value)).unwrap(),
        );
        h
    }

    #[test]
    fn extract_returns_session_for_valid_cookie() {
        let sess = Session::new("alice", 3600);
        let cookie = sess.encode(KEY);
        let headers = headers_with_cookie(&cookie);
        let got = extract_session(&headers, KEY).expect("should decode");
        assert_eq!(got.username, "alice");
    }

    #[test]
    fn extract_returns_none_without_cookie_header() {
        let headers = HeaderMap::new();
        assert!(extract_session(&headers, KEY).is_none());
    }

    #[test]
    fn extract_returns_none_when_cookie_missing_from_jar() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("other=xyz"));
        assert!(extract_session(&headers, KEY).is_none());
    }

    #[test]
    fn extract_returns_none_for_tampered_cookie() {
        let headers = headers_with_cookie("bogus.signature");
        assert!(extract_session(&headers, KEY).is_none());
    }

    #[test]
    fn extract_returns_none_for_wrong_key() {
        let sess = Session::new("alice", 3600);
        let cookie = sess.encode(KEY);
        let headers = headers_with_cookie(&cookie);
        assert!(extract_session(&headers, b"different-key-0123456789abcdef01").is_none());
    }

    #[test]
    fn session_response_serializes_with_expected_fields() {
        let r = SessionResponse {
            username: "alice".into(),
            expires_at: 1234567890,
            employee_id: None,
            role: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"username":"alice","expires_at":1234567890}"#);
    }

    /// Tenant manifest + revenue categories both read from the
    /// real tenant.toml on disk. The unit tests here cover the
    /// parsing/filtering logic against a synthetic TOML pinned to
    /// a tmp path via BOSS_TENANT_MANIFEST_TOML — keeps the
    /// brewery-seed file authoritative without coupling tests to it.
    fn write_tenant_toml(contents: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("create tempfile");
        f.write_all(contents.as_bytes()).expect("write tempfile");
        // SAFETY: tests are single-threaded per default and we set + unset
        // around each case below. Cargo test runs with --test-threads=1 for
        // this crate via the existing #[ignore] gates? No — they're not
        // ignored. The env-var pattern matches what other gateway tests do
        // (search for set_var). Worst case multiple tests stomp; both write
        // BEFORE reading inside the same scope so the parse sees the right
        // file. Sequential test runs in the same module avoid races.
        unsafe {
            std::env::set_var("BOSS_TENANT_MANIFEST_TOML", f.path());
        }
        f
    }

    // Serializes the two tests below: both clobber the process-global
    // BOSS_TENANT_MANIFEST_TOML env var that `revenue_categories` reads, so
    // running them in parallel (cargo test's default) raced — held across the
    // env write + the read.
    static TENANT_TOML_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn revenue_categories_filters_labels_by_prefix() {
        let _serial = TENANT_TOML_LOCK.lock().await;
        let _tmp = write_tenant_toml(
            r#"
[labels]
"finance.revenue_category.wholesale" = "Wholesale beer"
"finance.revenue_category.retail" = "Retail (DTC)"
"finance.revenue_category.taproom" = "Taproom pours"
"unrelated.label.key" = "should not appear"
"#,
        );
        let resp = revenue_categories().await;
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: Vec<RevenueCategory> = serde_json::from_slice(&body).unwrap();
        let codes: Vec<&str> = rows.iter().map(|r| r.code.as_str()).collect();
        assert_eq!(
            codes,
            vec!["retail", "taproom", "wholesale"],
            "sorted by code"
        );
        assert!(!rows.iter().any(|r| r.code == "unrelated.label.key"));
        let wholesale = rows.iter().find(|r| r.code == "wholesale").unwrap();
        assert_eq!(wholesale.label, "Wholesale beer");
    }

    #[tokio::test]
    async fn revenue_categories_empty_when_tenant_has_no_named_categories() {
        let _serial = TENANT_TOML_LOCK.lock().await;
        let _tmp = write_tenant_toml(
            r#"
[modules]
shop = true

[labels]
"other.label" = "Something"
"#,
        );
        let resp = revenue_categories().await;
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: Vec<RevenueCategory> = serde_json::from_slice(&body).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn session_response_includes_employee_id_and_role_when_set() {
        let r = SessionResponse {
            username: "emp-cto@example.com".into(),
            expires_at: 1234567890,
            employee_id: Some("emp-cto".into()),
            role: Some("cto".into()),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(
            json,
            r#"{"username":"emp-cto@example.com","expires_at":1234567890,"employee_id":"emp-cto","role":"cto"}"#
        );
    }
}
