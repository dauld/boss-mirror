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

impl From<Session> for SessionResponse {
    /// The endpoint's whole answer, as a value.
    ///
    /// It relays the signed session and invents nothing — which is
    /// what makes a break-glass session answerable here at all: that
    /// session carries the narrow `break-glass` role and NO employee
    /// id by design (Q4, docs/design/break-glass-is-a-key-you-hold.md),
    /// because resolving one would make the emergency door depend on
    /// boss-people being up. A reader that treats "no employee" as a
    /// broken login is reading this answer wrong; the answer itself is
    /// complete. Pulled out of the handler so that contract is a
    /// tested value rather than a shape assembled inline.
    fn from(s: Session) -> Self {
        Self {
            username: s.username,
            expires_at: s.expiry,
            employee_id: s.employee_id,
            role: s.role,
        }
    }
}

pub async fn session(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    match extract_session(&headers, &state.session_key) {
        Some(s) => Json(SessionResponse::from(s)).into_response(),
        None => (StatusCode::UNAUTHORIZED, "not signed in").into_response(),
    }
}

fn extract_session(headers: &HeaderMap, key: &[u8]) -> Option<Session> {
    let cookie_header = headers.get(header::COOKIE).and_then(|v| v.to_str().ok())?;
    let raw = find_cookie(cookie_header, session::COOKIE_NAME)?;
    Session::decode(raw, key).ok()
}

// The file's shape lives in boss-core (`tenant_manifest`) since
// fcc1d57b, so `boss tenant check` reads a manifest exactly the way
// this handler does — one definition, two doors (CLAUDE.md §9a).
// Branding is tenant data; core does not know it, which is why
// `display_name` is optional and the SPA falls back to "BOSS".
use boss_core::tenant_manifest::TenantToml;

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
/// Absent → empty manifest, and the SPA reads an empty `modules` as
/// every module OFF — a module is on only when listed true (ce68f137;
/// this comment said "all enabled" until backlog fa77e3d7). The boot
/// says how many are on (`modules_boot_line`), so an empty
/// declaration is visible. A file that exists but does not parse refused the BOOT
/// (`load_tenant_toml`); reaching this handler with one means the
/// file changed under the running gateway, which is logged and
/// answered empty rather than blanking the UI mid-flight.
pub async fn tenant_manifest() -> Response {
    Json(tenant_manifest_now()).into_response()
}

/// The manifest as a value — what `/api/tenant/manifest` answers, and
/// what the static server inlines into `index.html` so the first
/// paint already knows the tenant (5578e42d). One reader of
/// tenant.toml, two doors.
pub fn tenant_manifest_now() -> TenantManifest {
    match tenant_toml_or_empty() {
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

/// The manifest as parsed: `Ok(None)` when there is no file (an
/// unnamed tenant — every reader treats that as empty), `Ok(Some)`
/// when it parses, and `Err` naming the file and toml's own line
/// when it EXISTS but cannot be read. The boot-time reader of
/// `[gateway] public_reads` (main.rs) is the third door on the one
/// parser, and the `Err` is its refusal.
///
/// WHY A REFUSAL AND NOT A LOG LINE (backlog 4f1ba1f9, 2026-09-18).
/// Until this car an unparseable manifest was `None` — the gateway
/// booted with no public reads, no modules and no labels, one log
/// line saying so, and the instance answered as a tenant that had
/// declared nothing. Fail-closed, but silent: a wrong target that
/// answers instead of erroring (CLAUDE.md §Doors). This is NOT the
/// case the boot-check rule protects (infra/lint/a-boot-check-cannot-
/// fail-the-boot.sh — a check reads LIVE registry rows, written at
/// runtime through the API that a refusal would take down, so it
/// logs and starts). The manifest is the configuration the router is
/// built from, delivered by the deploy (the image or the boss-tenant
/// ConfigMap) with `boss tenant check` as its pre-flight, and the act
/// that fixes it is the act that broke it: an edit to the delivered
/// file, needing nothing the refusal keeps dark. It joins the two
/// refusals already in this boot path with the same blast radius —
/// the launcher's "not a tenant directory; not starting" and
/// `PublicReads::resolve` refusing a path outside the table — rather
/// than being the one door on the parser that shrugs. Measured
/// 2026-09-18: the launcher's prepare stage catches the same typo
/// (its publish runs the check and DEGRADES the pod, sim held) only
/// on a fresh database; on a stamped instance the publish is skipped
/// and the gateway was the only reader left, and it said nothing.
pub(crate) fn load_tenant_toml() -> Result<Option<TenantToml>, String> {
    let Some(path) = tenant_toml_path() else {
        return Ok(None);
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{path}: {e}")),
    };
    // toml's error carries the line and column; it is not rephrased.
    TenantToml::parse(&text)
        .map(Some)
        .map_err(|e| format!("{path}: {e}"))
}

/// The boot's one line about `[modules]`: how many are on, and the
/// names on each side. Returns the on-count beside the line so the
/// caller can raise zero to a warning.
///
/// WHY (design 1054c099, question startup-line; backlog fa77e3d7,
/// 2026-09-22): prod's manifest answered `"modules":{}` — every
/// module-gated surface off — and nothing said so. It became visible
/// only when two SPA readers that had disagreed about it were made one
/// fact. Zero on is a legitimate tenant (one that runs only jobs,
/// people and messages), so this states rather than refuses.
///
/// The count is of what the manifest DECLARES, not "of N known": the
/// module names live in the SPA's nav catalog (and the list in
/// docs/tenant-contract.md), and a third copy here would be a fact
/// living twice with no pin (CLAUDE.md §9a).
pub(crate) fn modules_boot_line(manifest: Option<&TenantToml>) -> (usize, String) {
    const OFF: &str = "every module-gated surface is off";
    let Some(t) = manifest else {
        return (
            0,
            format!("tenant modules on: 0; no tenant manifest, so {OFF} (a missing key is off)"),
        );
    };
    let names = |want: bool| {
        t.modules
            .iter()
            .filter(|(_, on)| **on == want)
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
    };
    let (on, off) = (names(true), names(false));
    let head = format!(
        "tenant modules on: {} of {} declared",
        on.len(),
        t.modules.len()
    );
    let line = match (on.is_empty(), off.is_empty()) {
        (true, true) => {
            format!("{head}; the manifest lists no [modules], so {OFF} (a missing key is off)")
        }
        (true, false) => format!("{head}; declared off: {}; {OFF}", off.join(", ")),
        (false, true) => format!("{head}; on: {}", on.join(", ")),
        (false, false) => format!(
            "{head}; on: {}; declared off: {}",
            on.join(", "),
            off.join(", ")
        ),
    };
    (on.len(), line)
}

/// The request-time reader: the boot already refused a file that
/// does not parse, so an `Err` here is a file that changed under the
/// running gateway (a ConfigMap update reaches the mount live). Say
/// so, loudly, and answer empty — the process stays up.
fn tenant_toml_or_empty() -> Option<TenantToml> {
    match load_tenant_toml() {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(
                error = %e,
                "tenant manifest unreadable at request time (it changed under the running gateway); answering an empty manifest"
            );
            None
        }
    }
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
    let rows = match tenant_toml_or_empty() {
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

    /// The document carries the tenant (ce68f137): what the static
    /// server inlines into index.html is this value serialized, so the
    /// SPA can title the tab from `display_name` before its first
    /// fetch, and a `[modules]` block reaches it as written — the SPA
    /// reads a missing key as off, so the block must survive intact.
    #[tokio::test]
    async fn the_inlined_manifest_carries_the_tenants_name_and_modules() {
        let _serial = TENANT_TOML_LOCK.lock().await;
        let _tmp = write_tenant_toml(
            r#"
[meta]
tenant_id = "algedonic-llc"
display_name = "Algedonic, LLC"

[modules]
finance = true
sim = false
"#,
        );
        let json = serde_json::to_string(&tenant_manifest_now()).unwrap();
        let html = crate::static_files::inline_tenant_manifest("<head></head>", &json);
        assert!(
            html.contains(r#""display_name":"Algedonic, LLC""#),
            "the inlined manifest names the tenant: {html}"
        );
        assert!(
            html.contains(r#""modules":{"finance":true,"sim":false}"#),
            "the modules block rides as written: {html}"
        );
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

    /// A manifest that EXISTS but does not parse is a refusal that
    /// names the file and toml's own line, not an empty manifest
    /// (backlog 4f1ba1f9, 2026-09-18). Until this pin the boot read
    /// a typo'd tenant.toml as "no [gateway] public_reads, no
    /// [modules], no [labels]" with one log line — fail-closed but
    /// silent, and a wrong target that answers instead of erroring.
    /// The fixture is the demo tenant's shape with ONE bad line: the
    /// public_reads list is opened and never closed.
    #[tokio::test]
    async fn a_manifest_that_exists_but_does_not_parse_is_refused_naming_the_file_and_line() {
        let _serial = TENANT_TOML_LOCK.lock().await;
        let tmp = write_tenant_toml(
            "[meta]\ntenant_id = \"demo\"\n\n[gateway]\npublic_reads = [\"/api/workflows\"\n",
        );
        let err = match load_tenant_toml() {
            Err(e) => e,
            Ok(t) => panic!("a manifest with a bad line read as {t:?} instead of refusing"),
        };
        let path = tmp.path().display().to_string();
        assert!(err.contains(&path), "the refusal names the file: {err}");
        assert!(
            err.contains("line 5"),
            "the refusal carries toml's line for the bad one: {err}"
        );
    }

    /// The boot states how many modules are on (design 1054c099,
    /// question startup-line; backlog fa77e3d7). Prod answered
    /// `"modules":{}` for weeks and nothing said so, because an empty
    /// declaration is a confident answer, not an error. Zero on is a
    /// legitimate tenant, so it is a line, not a refusal — but the
    /// line carries the count, the names on each side, and what zero
    /// means for the SPA.
    #[test]
    fn the_boot_line_names_how_many_modules_are_on() {
        let empty = TenantToml::parse("[meta]\ntenant_id = \"algedonic\"\n").unwrap();
        let (on, line) = modules_boot_line(Some(&empty));
        assert_eq!(on, 0);
        assert_eq!(
            line,
            "tenant modules on: 0 of 0 declared; the manifest lists no [modules], \
             so every module-gated surface is off (a missing key is off)"
        );

        let (on, line) = modules_boot_line(None);
        assert_eq!(on, 0);
        assert_eq!(
            line,
            "tenant modules on: 0; no tenant manifest, \
             so every module-gated surface is off (a missing key is off)"
        );

        let some = TenantToml::parse(
            "[modules]\nfinance = true\nexec = true\nshop = false\nsupport = true\nqa = false\n",
        )
        .unwrap();
        let (on, line) = modules_boot_line(Some(&some));
        assert_eq!(on, 3);
        assert_eq!(
            line,
            "tenant modules on: 3 of 5 declared; on: exec, finance, support; \
             declared off: qa, shop"
        );

        let all_off = TenantToml::parse("[modules]\nsim = false\n").unwrap();
        let (on, line) = modules_boot_line(Some(&all_off));
        assert_eq!(on, 0);
        assert_eq!(
            line,
            "tenant modules on: 0 of 1 declared; declared off: sim; \
             every module-gated surface is off"
        );
    }

    /// Absent is still empty: an unnamed tenant, not an error.
    #[tokio::test]
    async fn an_absent_manifest_is_an_empty_one() {
        let _serial = TENANT_TOML_LOCK.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        unsafe {
            std::env::set_var(
                "BOSS_TENANT_MANIFEST_TOML",
                dir.path().join("no-such-tenant.toml"),
            );
        }
        assert!(matches!(load_tenant_toml(), Ok(None)));
    }

    /// The identity read path, from the cookie the break-glass
    /// ceremony mints to the JSON the SPA classifies.
    ///
    /// The emergency session is not an employee and never will be
    /// (Q4). This pins the endpoint's half of packet 2ef7726b: the
    /// answer names the break-glass actor and the narrow role, and
    /// omits `employee_id` rather than inventing one. The defect that
    /// packet reports was on the reading side — the SPA filed this
    /// answer under "unrecognized" — so this test is the statement
    /// that the answer being read was right.
    #[test]
    fn a_break_glass_session_answers_as_the_break_glass_actor_with_no_employee() {
        let (_set_cookie, sess) = boss_gateway::break_glass::mint_session(KEY);
        let headers = headers_with_cookie(&sess.encode(KEY));
        let decoded = extract_session(&headers, KEY).expect("the minted cookie decodes");

        let r = SessionResponse::from(decoded);
        assert_eq!(r.username, boss_gateway::break_glass::BREAK_GLASS_ACTOR);
        assert_eq!(r.role.as_deref(), Some(boss_core::roles::BREAK_GLASS_ROLE));
        assert_eq!(
            r.employee_id, None,
            "resolving an employee would make the emergency door depend on boss-people"
        );

        // The wire shape the SPA reads: no `employee_id` key at all,
        // which is what `classifyProbe` sees.
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("employee_id"),
            "a break-glass session must not carry an employee id: {json}"
        );
        assert!(json.contains(r#""role":"break-glass""#), "{json}");
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
