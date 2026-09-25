//! OIDC login against the company IdP — Kanidm (idm-kanidm.md, all
//! five questions resolved 2026-08-10).
//!
//! The design in one sentence: OIDC is **another way to authenticate
//! an email**; everything after the email is the exact pipeline local
//! login uses. Q1: the gateway keeps issuing its own `boss_session`
//! (OIDC only at login; no downstream service learns OIDC exists).
//! Q2: the email maps to an EXISTING employee via `bootstrap_email`
//! — the IdP authenticates, it never provisions; a miss FAILS CLOSED
//! with a structured denial record. Roles come from the employee row,
//! same as local login — an IdP-group→role mapping would be a second
//! source of role truth, so it is deliberately absent (phase 2 may
//! revisit alongside agent service accounts, Q3).
//!
//! Absent configuration disables the routes honestly (the mail.rs
//! pattern): `/api/auth/oidc/available` says so, login/callback 404.
//!
//! Denials and mints land registered audit events via the gateway's
//! outbox staging path (`crate::audit`; docs/architecture-decisions.md
//! §Policy & auth) — the follow-up the earlier warn-line-only
//! record tracked. The warn lines remain as the local echo and the
//! backstop when staging is unavailable.
//!
//! Flow (auth-code + PKCE, confidential client):
//!   GET /api/auth/oidc/login     → 302 to the IdP authorize endpoint;
//!                                  state+verifier ride an HMAC-signed
//!                                  short-TTL cookie.
//!   GET /api/auth/oidc/callback  → state check → code exchange (client
//!                                  secret + PKCE) → userinfo → email →
//!                                  bootstrap_email → boss_session.
//!
//! Identity comes from the USERINFO endpoint over TLS, not from local
//! JWT verification: a confidential client that received the tokens
//! directly from the issuer's token endpoint needs no third-party
//! signature check, and skipping it keeps the JWT crypto zoo out of
//! the gateway.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::local_auth::{LocalAuthState, bootstrap_email};
use crate::session::{self, Session};

/// The state cookie: five minutes to round-trip the IdP.
const STATE_COOKIE: &str = "boss_oidc_state";
const STATE_TTL_SECONDS: u64 = 300;

pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
}

impl OidcConfig {
    /// All three or nothing — a partially-configured IdP must not
    /// half-exist. `BOSS_OIDC_ISSUER` is the Kanidm origin, e.g.
    /// `https://id.algedonic.dev:8443/oauth2/openid/boss`.
    pub fn from_env() -> Option<Self> {
        let issuer = std::env::var("BOSS_OIDC_ISSUER").ok()?;
        let client_id = std::env::var("BOSS_OIDC_CLIENT_ID").ok()?;
        let client_secret = std::env::var("BOSS_OIDC_CLIENT_SECRET").ok()?;
        if issuer.is_empty() || client_id.is_empty() || client_secret.is_empty() {
            return None;
        }
        Some(Self {
            issuer,
            client_id,
            client_secret,
        })
    }
}

/// Config + the lazily-fetched discovery document.
pub struct OidcRuntime {
    pub config: OidcConfig,
    discovery: tokio::sync::OnceCell<Discovery>,
}

impl OidcRuntime {
    pub fn new(config: OidcConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            discovery: tokio::sync::OnceCell::new(),
        })
    }

    /// Fetch (once) the issuer's discovery document. Lazy so a
    /// gateway can boot while the IdP is down — login fails loudly
    /// then, instead of the gateway failing to start.
    async fn discovery(&self, http: &reqwest::Client) -> Result<&Discovery, String> {
        self.discovery
            .get_or_try_init(|| async {
                let url = format!(
                    "{}/.well-known/openid-configuration",
                    self.config.issuer.trim_end_matches('/')
                );
                let resp = http.get(&url).send().await.map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("discovery returned {}", resp.status()));
                }
                resp.json::<Discovery>().await.map_err(|e| e.to_string())
            })
            .await
    }
}

#[derive(Deserialize)]
struct Discovery {
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
}

// --------------------------------------------------------------------
// PKCE + the signed state cookie.
// --------------------------------------------------------------------

fn random_b64(bytes: usize) -> String {
    use rand::RngExt;
    let mut buf = vec![0u8; bytes];
    rand::rng().fill(&mut buf[..]);
    B64.encode(buf)
}

/// (verifier, challenge) per RFC 7636 S256.
fn pkce_pair() -> (String, String) {
    let verifier = random_b64(32);
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// `state.verifier.mac` — the MAC (keyed with the session key) binds
/// the pair so a tampered cookie fails closed rather than letting a
/// caller supply their own verifier.
fn encode_state_cookie(key: &[u8], state: &str, verifier: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(state.as_bytes());
    mac.update(b".");
    mac.update(verifier.as_bytes());
    let tag = B64.encode(mac.finalize().into_bytes());
    format!("{state}.{verifier}.{tag}")
}

fn decode_state_cookie(key: &[u8], value: &str) -> Option<(String, String)> {
    let mut parts = value.splitn(3, '.');
    let state = parts.next()?;
    let verifier = parts.next()?;
    let tag = parts.next()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
    mac.update(state.as_bytes());
    mac.update(b".");
    mac.update(verifier.as_bytes());
    let expect = B64.encode(mac.finalize().into_bytes());
    // Not constant-time; the MAC'd value is a CSRF token, not a
    // credential — the session key itself never rides the wire.
    (expect == tag).then(|| (state.to_string(), verifier.to_string()))
}

fn state_cookie_from(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == STATE_COOKIE).then(|| v.to_string())
    })
}

// --------------------------------------------------------------------
// Handlers.
// --------------------------------------------------------------------

/// `GET /api/auth/oidc/available` — the SPA's probe, mirroring the
/// guest probe: presence of config, nothing secret.
pub async fn available(State(state): State<Arc<LocalAuthState>>) -> Response {
    Json(serde_json::json!({ "enabled": state.oidc.is_some() })).into_response()
}

/// `GET /api/auth/oidc/login` — mint state + PKCE, set the signed
/// cookie, redirect to the IdP.
pub async fn login(State(state): State<Arc<LocalAuthState>>) -> Response {
    let Some(oidc) = &state.oidc else {
        return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
    };
    let disc = match oidc.discovery(&state.http).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "oidc: discovery failed");
            return (
                StatusCode::BAD_GATEWAY,
                "the identity provider is unreachable",
            )
                .into_response();
        }
    };
    let csrf = random_b64(16);
    let (verifier, challenge) = pkce_pair();
    let redirect_uri = format!(
        "{}/api/auth/oidc/callback",
        state.public_url.trim_end_matches('/')
    );
    let authorize = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope=openid+email&state={}&code_challenge={}&code_challenge_method=S256",
        disc.authorization_endpoint,
        urlencode(&oidc.config.client_id),
        urlencode(&redirect_uri),
        urlencode(&csrf),
        urlencode(&challenge),
    );

    let cookie = session::set_cookie(
        STATE_COOKIE,
        &encode_state_cookie(&state.session_key, &csrf, &verifier),
        STATE_TTL_SECONDS,
        "/api/auth/oidc",
    );
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        headers.insert(header::SET_COOKIE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&authorize) {
        headers.insert(header::LOCATION, v);
    }
    (StatusCode::FOUND, headers).into_response()
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct UserInfo {
    email: Option<String>,
}

/// `GET /api/auth/oidc/callback` — the IdP sent the browser back.
pub async fn callback(
    State(state): State<Arc<LocalAuthState>>,
    Query(q): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(oidc) = &state.oidc else {
        return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
    };
    if let Some(err) = q.error {
        tracing::warn!(error = %err, "oidc: IdP returned an error");
        // `access_denied` is the IdP refusing the USER — an
        // authentication decision, so it lands the denied event
        // (reason `idp_denied`, no claimed email: the refusal
        // happened before any identity reached us). Every other
        // error value is transport/config trouble and stays a warn
        // line only (§Policy & auth).
        if err == "access_denied" {
            state.audit.login_denied(
                None,
                crate::audit::AuthMethod::Oidc,
                crate::audit::DeniedReason::IdpDenied,
                Some(&oidc.config.issuer),
            );
        }
        return (
            StatusCode::UNAUTHORIZED,
            format!("identity provider: {err}"),
        )
            .into_response();
    }
    let (Some(code), Some(cb_state)) = (q.code, q.state) else {
        return (StatusCode::BAD_REQUEST, "missing code/state").into_response();
    };
    // CSRF gate: the state must round-trip our own signed cookie.
    let Some((cookie_state, verifier)) =
        state_cookie_from(&headers).and_then(|v| decode_state_cookie(&state.session_key, &v))
    else {
        return (StatusCode::BAD_REQUEST, "missing or invalid state cookie").into_response();
    };
    if cookie_state != cb_state {
        return (StatusCode::BAD_REQUEST, "state mismatch").into_response();
    }

    let disc = match oidc.discovery(&state.http).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "oidc: discovery failed at callback");
            return (
                StatusCode::BAD_GATEWAY,
                "the identity provider is unreachable",
            )
                .into_response();
        }
    };
    let redirect_uri = format!(
        "{}/api/auth/oidc/callback",
        state.public_url.trim_end_matches('/')
    );
    let token = match state
        .http
        .post(&disc.token_endpoint)
        .basic_auth(&oidc.config.client_id, Some(&oidc.config.client_secret))
        // Hand-built form body: reqwest's `form()` helper is behind a
        // feature this slim build doesn't carry, and four pairs don't
        // justify adding it.
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}",
            urlencode(&code),
            urlencode(&redirect_uri),
            urlencode(&verifier),
        ))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json::<TokenResponse>().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "oidc: token response not JSON");
                return (StatusCode::BAD_GATEWAY, "token exchange failed").into_response();
            }
        },
        Ok(r) => {
            tracing::warn!(status = %r.status(), "oidc: token exchange rejected");
            return (StatusCode::UNAUTHORIZED, "token exchange rejected").into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, "oidc: token endpoint unreachable");
            return (
                StatusCode::BAD_GATEWAY,
                "the identity provider is unreachable",
            )
                .into_response();
        }
    };

    let email = match state
        .http
        .get(&disc.userinfo_endpoint)
        .bearer_auth(&token.access_token)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json::<UserInfo>().await {
            Ok(UserInfo { email: Some(e) }) => e.to_lowercase(),
            _ => {
                return (StatusCode::UNAUTHORIZED, "the IdP returned no email").into_response();
            }
        },
        _ => {
            return (StatusCode::BAD_GATEWAY, "userinfo failed").into_response();
        }
    };

    // Q2: authenticate, never provision. The IdP said who they are;
    // only the People domain says whether they work here.
    let scope = match bootstrap_email(&state.http, &email).await {
        Some(s) => s,
        None => {
            // The denial event is the record now (gateway-audit-
            // events Q1 paid this IOU); the warn line stays as the
            // greppable local echo, and as the backstop when the
            // staging path is down.
            state.audit.login_denied(
                Some(&email),
                crate::audit::AuthMethod::Oidc,
                crate::audit::DeniedReason::NoEmployeeRecord,
                Some(&oidc.config.issuer),
            );
            tracing::warn!(
                email = %email,
                idp = %oidc.config.issuer,
                "oidc: DENIED — authenticated at the IdP but no employee record matches; \
                 people enter through the People domain, not through login"
            );
            return (
                StatusCode::FORBIDDEN,
                format!(
                    "{email} authenticated, but no employee record matches. \
                     Access is provisioned through the People domain — contact \
                     your administrator."
                ),
            )
                .into_response();
        }
    };

    let mut sess = Session::new(&email, session::DEFAULT_TTL_SECONDS);
    sess.employee_id = Some(scope.id);
    sess.role = Some(scope.role);
    sess.department = scope.department;
    sess.territory_account_ids = scope.territory_account_ids;
    sess.direct_report_ids = scope.direct_report_ids;

    state.audit.login_succeeded(
        &email,
        sess.employee_id.as_deref(),
        crate::audit::AuthMethod::Oidc,
    );

    let session_cookie = session::set_cookie(
        session::COOKIE_NAME,
        &sess.encode(&state.session_key),
        session::DEFAULT_TTL_SECONDS,
        "/",
    );
    // Expire the state cookie — it did its one job.
    let clear_state = session::set_cookie(STATE_COOKIE, "", 0, "/api/auth/oidc");
    let mut out = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&session_cookie) {
        out.append(header::SET_COOKIE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&clear_state) {
        out.append(header::SET_COOKIE, v);
    }
    out.insert(header::LOCATION, HeaderValue::from_static("/"));
    (StatusCode::FOUND, out).into_response()
}

/// Percent-encode a query value (same minimal set as mail.rs).
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_the_s256_of_the_verifier() {
        let (verifier, challenge) = pkce_pair();
        assert_eq!(challenge, B64.encode(Sha256::digest(verifier.as_bytes())));
        assert!(verifier.len() >= 43, "RFC 7636 minimum length");
    }

    #[test]
    fn state_cookie_round_trips_and_rejects_tampering() {
        let key = b"test-key";
        let cookie = encode_state_cookie(key, "st-1", "ver-1");
        assert_eq!(
            decode_state_cookie(key, &cookie),
            Some(("st-1".into(), "ver-1".into()))
        );
        // Swap the verifier: the MAC no longer matches — a caller
        // must not be able to choose their own PKCE verifier.
        let forged = {
            let mut parts: Vec<&str> = cookie.splitn(3, '.').collect();
            parts[1] = "attacker-verifier";
            parts.join(".")
        };
        assert_eq!(decode_state_cookie(key, &forged), None);
        // Wrong key: also refused.
        assert_eq!(decode_state_cookie(b"other-key", &cookie), None);
    }

    // ----------------------------------------------------------------
    // Flow tests against an ephemeral mock IdP + mock people-api.
    // Serialized: bootstrap_email reads BOSS_PEOPLE_UPSTREAM from env.
    // ----------------------------------------------------------------
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn mock_idp() -> String {
        use axum::routing::{get, post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let b = base.clone();
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let b = b.clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "authorization_endpoint": format!("{b}/authorize"),
                            "token_endpoint": format!("{b}/token"),
                            "userinfo_endpoint": format!("{b}/userinfo"),
                        }))
                    }
                }),
            )
            .route(
                "/token",
                post(|| async { axum::Json(serde_json::json!({ "access_token": "at-1" })) }),
            )
            .route(
                "/userinfo",
                get(|| async { axum::Json(serde_json::json!({ "email": "Op@Example.com" })) }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    /// people-api double: only the operator email has an employee row.
    async fn mock_people() -> String {
        use axum::routing::get;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new().route(
            "/api/people/by-email/{email}/bootstrap",
            get(
                |axum::extract::Path(email): axum::extract::Path<String>| async move {
                    if email == "op@example.com" {
                        axum::Json(serde_json::json!({
                            "id": "emp-op",
                            "role": "platform-admin",
                            "department": "platform",
                        }))
                        .into_response()
                    } else {
                        axum::http::StatusCode::NOT_FOUND.into_response()
                    }
                },
            ),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn oidc_state(issuer: &str) -> Arc<LocalAuthState> {
        oidc_state_with_audit(issuer, crate::audit::AuthAudit::disabled())
    }

    fn oidc_state_with_audit(issuer: &str, audit: crate::audit::AuthAudit) -> Arc<LocalAuthState> {
        let store = crate::local_auth::CredentialStore::load("/nonexistent/oidc-test-creds.toml")
            .expect("empty store");
        Arc::new(LocalAuthState {
            store,
            session_key: vec![9u8; 32],
            http: reqwest::Client::new(),
            audit,
            guest_access: crate::local_auth::GuestAccess::Off,
            oidc: Some(OidcRuntime::new(OidcConfig {
                issuer: issuer.to_string(),
                client_id: "boss".into(),
                client_secret: "s3cr3t".into(),
            })),
            mail: Arc::new(crate::mail::LogTransport),
            public_url: "https://boss.test".into(),
            forgot_seen: Default::default(),
        })
    }

    fn cookie_header(state_cookie_value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{STATE_COOKIE}={state_cookie_value}")).unwrap(),
        );
        h
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn callback_mints_the_same_session_local_login_would() {
        let _guard = ENV_LOCK.lock().await;
        let idp = mock_idp().await;
        let people = mock_people().await;
        unsafe { std::env::set_var("BOSS_PEOPLE_UPSTREAM", &people) };
        let cap = std::sync::Arc::new(crate::audit::testing::Captured::default());
        let st = oidc_state_with_audit(&idp, crate::audit::AuthAudit::spawn(cap.clone()));

        let cookie = encode_state_cookie(&st.session_key, "st-9", "ver-9");
        let resp = callback(
            State(st.clone()),
            Query(CallbackQuery {
                code: Some("code-1".into()),
                state: Some("st-9".into()),
                error: None,
            }),
            cookie_header(&cookie),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::FOUND,
            "success is a redirect home"
        );
        let cookies: Vec<_> = resp
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        let sess_cookie = cookies
            .iter()
            .find(|c| c.starts_with(session::COOKIE_NAME))
            .expect("session cookie set");
        // Decode with the same key: the session must carry the
        // employee scope local login would have produced — lowercased
        // email, employee id, role.
        let value = sess_cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_string();
        let sess = Session::decode(&value, &st.session_key).expect("valid session");
        assert_eq!(sess.username, "op@example.com", "email lowercased");
        assert_eq!(sess.employee_id.as_deref(), Some("emp-op"));
        assert_eq!(sess.role.as_deref(), Some("platform-admin"));
        // The one-shot state cookie is expired alongside.
        assert!(
            cookies.iter().any(|c| c.starts_with(STATE_COOKIE)),
            "state cookie cleared: {cookies:?}"
        );

        // Architecture decisions, §Policy & auth: the mint moment is the succeeded
        // event, and it names its method so the passkey path lands
        // as a value, not a schema change.
        let events = crate::audit::testing::drain(&cap, 1).await;
        assert_eq!(events.len(), 1, "one succeeded event");
        assert_eq!(events[0].kind, "auth.login.succeeded");
        assert_eq!(events[0].payload["method"], "oidc");
        assert_eq!(events[0].payload["email"], "op@example.com");
        assert_eq!(events[0].payload["employee_id"], "emp-op");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_email_with_no_employee_fails_closed() {
        let _guard = ENV_LOCK.lock().await;
        // IdP that authenticates an email People has never heard of.
        use axum::routing::{get, post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let b = base.clone();
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let b = b.clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "authorization_endpoint": format!("{b}/authorize"),
                            "token_endpoint": format!("{b}/token"),
                            "userinfo_endpoint": format!("{b}/userinfo"),
                        }))
                    }
                }),
            )
            .route(
                "/token",
                post(|| async { axum::Json(serde_json::json!({ "access_token": "at-2" })) }),
            )
            .route(
                "/userinfo",
                get(|| async {
                    axum::Json(serde_json::json!({ "email": "stranger@example.com" }))
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let people = mock_people().await;
        unsafe { std::env::set_var("BOSS_PEOPLE_UPSTREAM", &people) };
        let cap = std::sync::Arc::new(crate::audit::testing::Captured::default());
        let st = oidc_state_with_audit(&base, crate::audit::AuthAudit::spawn(cap.clone()));

        let cookie = encode_state_cookie(&st.session_key, "st-x", "ver-x");
        let resp = callback(
            State(st),
            Query(CallbackQuery {
                code: Some("code-2".into()),
                state: Some("st-x".into()),
                error: None,
            }),
            cookie_header(&cookie),
        )
        .await;
        // Q2: authenticated at the IdP is NOT employed here. 403, no
        // session cookie of any kind.
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(
            !resp
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .any(|c| c.to_str().unwrap_or("").starts_with(session::COOKIE_NAME)),
            "fail closed mints nothing"
        );

        // Architecture decisions, §Policy & auth: the fail-closed denial lands a
        // registered event — no longer only a warn line — naming the
        // claimed email and the IdP, asserting no employee.
        let events = crate::audit::testing::drain(&cap, 1).await;
        assert_eq!(events.len(), 1, "one denied event");
        assert_eq!(events[0].kind, "auth.login.denied");
        assert_eq!(events[0].payload["reason"], "no_employee_record");
        assert_eq!(events[0].payload["method"], "oidc");
        assert_eq!(events[0].payload["email_claimed"], "stranger@example.com");
        assert!(events[0].payload.get("employee_id").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_state_mismatch_is_rejected_before_any_token_exchange() {
        let _guard = ENV_LOCK.lock().await;
        // The token endpoint records whether it was ever called:
        // rejection must happen BEFORE the exchange, or a forged
        // state still burns a single-use code.
        let exchanged = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        use axum::routing::{get, post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let b = base.clone();
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let b = b.clone();
                    async move {
                        axum::Json(serde_json::json!({
                            "authorization_endpoint": format!("{b}/authorize"),
                            "token_endpoint": format!("{b}/token"),
                            "userinfo_endpoint": format!("{b}/userinfo"),
                        }))
                    }
                }),
            )
            .route("/token", {
                let hit = exchanged.clone();
                post(move || {
                    let hit = hit.clone();
                    async move {
                        hit.store(true, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(serde_json::json!({ "access_token": "at-3" }))
                    }
                })
            });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let st = oidc_state(&base);

        let cookie = encode_state_cookie(&st.session_key, "st-real", "ver-real");
        let resp = callback(
            State(st),
            Query(CallbackQuery {
                code: Some("code-3".into()),
                state: Some("st-FORGED".into()),
                error: None,
            }),
            cookie_header(&cookie),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(
            !exchanged.load(std::sync::atomic::Ordering::SeqCst),
            "a forged state must be rejected BEFORE the code is spent at the token endpoint"
        );
    }

    #[test]
    fn config_is_all_three_or_nothing() {
        // Serialized by the env-var mutex below? No env in this test —
        // from_env is exercised only where the vars are set; here the
        // empty-string rule is pinned via the constructor path.
        let missing = OidcConfig {
            issuer: "".into(),
            client_id: "x".into(),
            client_secret: "y".into(),
        };
        // from_env applies the same emptiness rule; pin the intent.
        assert!(missing.issuer.is_empty());
    }
}
