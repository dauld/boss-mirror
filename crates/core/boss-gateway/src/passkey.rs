//! The passkey ceremony — BOSS-native WebAuthn for presence assurance.
//!
//! Design: docs/design/presence.md (Q1-Q3 resolved 2026-08-16) and the
//! BOSS-native call accepted on packet 7218c3f1: enrolment happens
//! behind an already-authenticated session, assertions verify against
//! a BOSS-issued challenge bound to the step's shape hash, credentials
//! live in a BOSS table (boss-people's `webauthn_credentials`), and
//! Kanidm stays login-only — delegating the ceremony to it would lose
//! per-step challenge binding.
//!
//! Q2's decision, honored literally: the challenge IS the shape hash,
//! with the replay caveat resolved exactly as recorded:
//! `challenge = sha256(shape_hash || ":" || nonce)`.
//!
//! The passkey signature is itself the binding: an assertion cannot be
//! replayed against a different step (different hash → different
//! challenge), nor against the same step after an edit (the hash
//! moved), nor twice (the nonce'd challenge row is single-use).
//!
//! Split of labor: this module runs the cryptographic ceremony against
//! the browser (webauthn-rs; the custom-challenge state is built
//! through the crate's own serde, so every verification step stays in
//! the library). boss-people stores credentials and the single-use
//! challenge ledger. boss-jobs consumes the OUTCOME: a verified
//! assertion becomes a short-lived HMAC ticket (`x-presence-ticket`),
//! which the role_headers middleware — after stripping every inbound
//! `x-boss-*` so a client cannot forge it — swaps for a trusted
//! `x-boss-presence` header that the sign-off endpoint reads as
//! `produced = Presence`. No fallback path exists anywhere in the
//! chain, per Q3: an assurance level with a bypass is a comment, not a
//! control.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
};

use crate::session::{self, Session};

type HmacSha256 = Hmac<Sha256>;

/// How long a verified assertion is redeemable as a ticket. Long
/// enough for the SPA to attach it to the very next sign-off POST,
/// short enough that a leaked ticket is nearly worthless — and the
/// nonce makes it single-step regardless.
const TICKET_TTL_SECONDS: u64 = 120;

pub struct PasskeyState {
    pub session_key: Vec<u8>,
    pub http: reqwest::Client,
    pub people_base: String,
    pub jobs_base: String,
    pub webauthn: Webauthn,
}

impl PasskeyState {
    /// rp_id / origin derive from BOSS_PUBLIC_URL — the one host
    /// browsers actually see (the OIDC callback constraint already
    /// pins this URL; see boss-project memory on playground origins).
    pub fn from_env(session_key: Vec<u8>) -> anyhow::Result<Self> {
        let public_url = std::env::var("BOSS_PUBLIC_URL")
            .unwrap_or_else(|_| "http://localhost:8000".to_string());
        let origin = Url::parse(&public_url)?;
        let rp_id = origin
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("BOSS_PUBLIC_URL has no host"))?
            .to_string();
        let webauthn = WebauthnBuilder::new(&rp_id, &origin)?
            .rp_name("BOSS")
            .build()?;
        Ok(Self {
            session_key,
            http: reqwest::Client::new(),
            people_base: std::env::var("BOSS_PEOPLE_UPSTREAM")
                .unwrap_or_else(|_| boss_ports::url("people")),
            jobs_base: std::env::var("BOSS_JOBS_UPSTREAM")
                .unwrap_or_else(|_| boss_ports::url("jobs")),
            webauthn,
        })
    }
}

pub fn passkey_router(state: Arc<PasskeyState>) -> Router {
    Router::new()
        .route("/api/auth/passkey/register/begin", post(register_begin))
        .route("/api/auth/passkey/register/finish", post(register_finish))
        .route("/api/auth/passkey/assert/begin", post(assert_begin))
        .route("/api/auth/passkey/assert/finish", post(assert_finish))
        .route(
            "/api/auth/passkey/credentials",
            axum::routing::get(credentials_list),
        )
        .with_state(state)
}

/// The browser-safe listing: the session's OWN passkeys, metadata
/// only (no public keys). The raw people surface is platform
/// machinery; this is the door the SPA uses.
pub async fn credentials_list(
    State(state): State<Arc<PasskeyState>>,
    headers: HeaderMap,
) -> Response {
    let (_sess, employee_id) = match employee_session(&headers, &state.session_key) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let rows = match state.stored_passkeys(&employee_id).await {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "credential_id": r["credential_id"],
                "label": r["label"],
                "registered_at": r["registered_at"],
                "last_used_at": r["last_used_at"],
            })
        })
        .collect();
    Json(out).into_response()
}

/// `DELETE /api/auth/passkey/credentials/{credential_id}` — remove one
/// of the session's own passkeys. The rule lives in boss-people (the
/// last one stays, 409 in the user's terms); this proxies for the
/// session's employee and passes that refusal through verbatim, so the
/// panel can show the reason rather than "failed".
pub async fn credentials_remove(
    State(state): State<Arc<PasskeyState>>,
    headers: HeaderMap,
    axum::extract::Path(credential_id): axum::extract::Path<String>,
) -> Response {
    let (_sess, employee_id) = match employee_session(&headers, &state.session_key) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let url = format!(
        "{}/api/people/{}/webauthn-credentials/{}",
        state.people_base, employee_id, credential_id
    );
    let resp = match state.request(reqwest::Method::DELETE, url).send().await {
        Ok(r) => r,
        Err(e) => {
            return err(StatusCode::BAD_GATEWAY, format!("people unreachable: {e}"))
                .into_response();
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    match status {
        StatusCode::NO_CONTENT => StatusCode::NO_CONTENT.into_response(),
        StatusCode::CONFLICT | StatusCode::NOT_FOUND => {
            (status, resp.text().await.unwrap_or_default()).into_response()
        }
        _ => err(StatusCode::BAD_GATEWAY, "credential removal failed").into_response(),
    }
}

/// The challenge recipe, in one place so the begin and any future
/// audit tooling cannot drift: sha256 over the utf8 of
/// `<shape_hash>:<nonce>`, both hex strings.
pub fn presence_challenge(shape_hash: &str, nonce: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(shape_hash.as_bytes());
    h.update(b":");
    h.update(nonce.as_bytes());
    h.finalize().to_vec()
}

// ---------------------------------------------------------------------------
// The presence ticket — a verified assertion, portable for two minutes
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct PresenceTicket {
    /// employee id the assertion verified for
    pub i: String,
    /// step id the challenge was minted for
    pub s: String,
    /// shape hash at mint time
    pub h: String,
    /// server nonce — recorded on the stamp for single-use audit
    pub n: String,
    /// absolute expiry, seconds since epoch
    pub e: u64,
}

impl PresenceTicket {
    pub fn encode(&self, key: &[u8]) -> String {
        let payload = serde_json::to_vec(self).expect("serialize PresenceTicket");
        let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(payload_b64.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{payload_b64}.{sig_b64}")
    }

    pub fn decode(value: &str, key: &[u8], now_epoch: u64) -> Option<Self> {
        let (payload_b64, sig_b64) = value.split_once('.')?;
        let sig = URL_SAFE_NO_PAD.decode(sig_b64).ok()?;
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(payload_b64.as_bytes());
        let expected = mac.finalize().into_bytes();
        if expected.ct_eq(&sig).unwrap_u8() != 1 {
            return None;
        }
        let ticket: PresenceTicket =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_b64).ok()?).ok()?;
        (ticket.e > now_epoch).then_some(ticket)
    }
}

/// The trusted header injected by role_headers after a valid ticket,
/// and read by boss-jobs' sign-off endpoint as `produced`.
pub const PRESENCE_HEADER: &str = "x-boss-presence";
/// The client-supplied ticket header. Deliberately NOT `x-boss-*`:
/// the edge strip removes that whole prefix from inbound traffic, and
/// the ticket must survive to be verified (forgery is caught by the
/// HMAC, not the strip).
pub const TICKET_HEADER: &str = "x-presence-ticket";

/// Verify a ticket header value and produce the trusted header's JSON.
/// Called from role_headers inside the session branch.
pub fn presence_header_from_ticket(value: &str, key: &[u8]) -> Option<String> {
    let t = PresenceTicket::decode(value, key, now_epoch())?;
    Some(
        json!({
            "employee_id": t.i,
            "step_id": t.s,
            "shape_hash": t.h,
            "nonce": t.n,
        })
        .to_string(),
    )
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn session_of(headers: &HeaderMap, key: &[u8]) -> Option<Session> {
    let cookie_header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    let value = session::find_cookie(cookie_header, session::COOKIE_NAME)?;
    Session::decode(value, key).ok()
}

/// Employee-bearing session or 401 — guests and unresolved logins
/// cannot hold credentials.
fn employee_session(headers: &HeaderMap, key: &[u8]) -> Result<(Session, String), ErrResp> {
    let sess = session_of(headers, key).ok_or_else(|| (StatusCode::UNAUTHORIZED, String::new()))?;
    let emp = sess.employee_id.clone().ok_or_else(|| {
        err(
            StatusCode::FORBIDDEN,
            "session resolves to no employee — passkeys bind to employees",
        )
    })?;
    Ok((sess, emp))
}

/// Small error value for helper Results — converted to a Response at
/// the handler boundary (clippy::result_large_err: Response is a
/// 128-byte-plus payload and these are cold refusal paths).
type ErrResp = (StatusCode, String);

fn err(status: StatusCode, msg: impl Into<String>) -> ErrResp {
    (status, msg.into())
}

/// The same refusal, already a Response — for direct returns inside
/// handlers.
fn err2(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, msg.into()).into_response()
}

impl PasskeyState {
    /// Machine-token-stamped server-side call, identifying as the
    /// gateway's own internal actor. boss-people's webauthn storage
    /// requires a `platform-admin` caller (its paths are also
    /// browser-reachable through the /api/people proxy, so it cannot
    /// trust callers by position) — this identity is how the ceremony
    /// passes that gate while ordinary proxied sessions are refused.
    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        let mut rb = self.http.request(method, url).header(
            "x-boss-user",
            json!({
                "id": "automation:gateway",
                "role": "platform-admin",
                "access_tier": "operator",
            })
            .to_string(),
        );
        if let Some(token) = boss_core::machine_token::from_env() {
            rb = rb.header(boss_core::machine_token::HEADER, token);
        }
        rb
    }

    async fn stored_passkeys(&self, employee_id: &str) -> Result<Vec<Value>, ErrResp> {
        let url = format!(
            "{}/api/people/{}/webauthn-credentials",
            self.people_base, employee_id
        );
        let resp = self
            .request(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("people unreachable: {e}")))?;
        if !resp.status().is_success() {
            return Err(err(StatusCode::BAD_GATEWAY, "credential lookup failed"));
        }
        resp.json::<Vec<Value>>().await.map_err(|e| {
            err(
                StatusCode::BAD_GATEWAY,
                format!("credential list malformed: {e}"),
            )
        })
    }

    /// Stored rows carry `public_key` = b64url(serde_json(Passkey)).
    /// Returns the decoded Passkey JSON values ({"cred": ...}).
    fn passkey_jsons(rows: &[Value]) -> Result<Vec<Value>, ErrResp> {
        rows.iter()
            .map(|r| {
                let b64 = r["public_key"].as_str().ok_or_else(|| {
                    err(StatusCode::BAD_GATEWAY, "credential row missing public_key")
                })?;
                let bytes = URL_SAFE_NO_PAD.decode(b64).map_err(|_| {
                    err(
                        StatusCode::BAD_GATEWAY,
                        "credential public_key not base64url",
                    )
                })?;
                serde_json::from_slice(&bytes).map_err(|_| {
                    err(
                        StatusCode::BAD_GATEWAY,
                        "credential public_key not a stored passkey",
                    )
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Enrolment
// ---------------------------------------------------------------------------

pub async fn register_begin(
    State(state): State<Arc<PasskeyState>>,
    headers: HeaderMap,
    _body: Option<Json<Value>>,
) -> Response {
    let (sess, employee_id) = match employee_session(&headers, &state.session_key) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    // Exclude already-registered credentials so an authenticator
    // cannot double-enrol.
    let rows = match state.stored_passkeys(&employee_id).await {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let exclude: Option<Vec<webauthn_rs::prelude::CredentialID>> = if rows.is_empty() {
        None
    } else {
        let ids = rows
            .iter()
            .filter_map(|r| r["credential_id"].as_str())
            .filter_map(|b| URL_SAFE_NO_PAD.decode(b).ok())
            .map(webauthn_rs::prelude::CredentialID::from)
            .collect();
        Some(ids)
    };
    // A stable per-employee UUID: v5 over the employee id in a fixed
    // namespace. WebAuthn wants a user handle; employees have string
    // ids.
    let user_uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, employee_id.as_bytes());
    let (ccr, reg_state): (CreationChallengeResponse, PasskeyRegistration) = match state
        .webauthn
        .start_passkey_registration(user_uuid, &sess.username, &sess.username, exclude)
    {
        Ok(v) => v,
        Err(e) => return err2(StatusCode::INTERNAL_SERVER_ERROR, format!("webauthn: {e}")),
    };
    // The registration state is the thing that must round-trip; the
    // challenge ledger carries it opaquely (flow=register).
    let challenge_id = Uuid::new_v4().to_string();
    let state_bytes = serde_json::to_vec(&reg_state).expect("serialize PasskeyRegistration");
    let mint = state
        .request(
            reqwest::Method::POST,
            format!("{}/api/people/presence-challenges", state.people_base),
        )
        .json(&json!({
            "id": challenge_id,
            "employee_id": employee_id,
            "challenge": URL_SAFE_NO_PAD.encode(&state_bytes),
            "flow": "register",
        }))
        .send()
        .await;
    match mint {
        Ok(r) if r.status().is_success() => Json(json!({
            "challenge_id": challenge_id,
            "options": ccr,
        }))
        .into_response(),
        Ok(r) => err2(
            StatusCode::BAD_GATEWAY,
            format!("challenge mint failed: {}", r.status()),
        ),
        Err(e) => err2(StatusCode::BAD_GATEWAY, format!("people unreachable: {e}")),
    }
}

#[derive(Deserialize)]
pub struct RegisterFinishBody {
    challenge_id: String,
    #[serde(default)]
    label: Option<String>,
    credential: RegisterPublicKeyCredential,
}

pub async fn register_finish(
    State(state): State<Arc<PasskeyState>>,
    headers: HeaderMap,
    Json(body): Json<RegisterFinishBody>,
) -> Response {
    let (_sess, employee_id) = match employee_session(&headers, &state.session_key) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let row = match consume_challenge(&state, &body.challenge_id).await {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    if row["flow"] != "register" || row["employee_id"] != employee_id.as_str() {
        return err2(
            StatusCode::FORBIDDEN,
            "challenge was minted for someone else",
        );
    }
    let state_bytes = match row["challenge"]
        .as_str()
        .and_then(|b| URL_SAFE_NO_PAD.decode(b).ok())
    {
        Some(v) => v,
        None => {
            return err2(
                StatusCode::BAD_GATEWAY,
                "stored registration state unreadable",
            );
        }
    };
    let reg_state: PasskeyRegistration = match serde_json::from_slice(&state_bytes) {
        Ok(v) => v,
        Err(_) => {
            return err2(
                StatusCode::BAD_GATEWAY,
                "stored registration state unreadable",
            );
        }
    };
    let passkey = match state
        .webauthn
        .finish_passkey_registration(&body.credential, &reg_state)
    {
        Ok(v) => v,
        Err(e) => {
            return err2(
                StatusCode::BAD_REQUEST,
                format!("attestation rejected: {e}"),
            );
        }
    };
    let cred_id_b64 = URL_SAFE_NO_PAD.encode(passkey.cred_id().as_ref());
    let passkey_b64 =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&passkey).expect("serialize Passkey"));
    let store = state
        .request(
            reqwest::Method::POST,
            format!(
                "{}/api/people/{}/webauthn-credentials",
                state.people_base, employee_id
            ),
        )
        .json(&json!({
            "credential_id": cred_id_b64,
            "public_key": passkey_b64,
            "label": body.label.unwrap_or_else(|| "passkey".to_string()),
        }))
        .send()
        .await;
    match store {
        Ok(r) if r.status().is_success() => (
            StatusCode::CREATED,
            Json(json!({ "credential_id": cred_id_b64 })),
        )
            .into_response(),
        Ok(r) if r.status() == reqwest::StatusCode::CONFLICT => {
            err2(StatusCode::CONFLICT, "credential already registered")
        }
        Ok(r) => err2(
            StatusCode::BAD_GATEWAY,
            format!("credential store failed: {}", r.status()),
        ),
        Err(e) => err2(StatusCode::BAD_GATEWAY, format!("people unreachable: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Assertion — the presence half
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AssertBeginBody {
    job_id: String,
    step_id: String,
}

pub async fn assert_begin(
    State(state): State<Arc<PasskeyState>>,
    headers: HeaderMap,
    Json(body): Json<AssertBeginBody>,
) -> Response {
    let (sess, employee_id) = match employee_session(&headers, &state.session_key) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    // The step's CURRENT content is what the passkey will approve.
    let job_url = format!("{}/api/jobs/{}", state.jobs_base, body.job_id);
    let job: Value = {
        let user_json = json!({
            "id": employee_id,
            "role": sess.role.as_deref().unwrap_or("audit-readonly"),
            "access_tier": sess.access_tier,
            "territory_account_ids": sess.territory_account_ids,
            "direct_report_ids": sess.direct_report_ids,
            "department": sess.department,
        })
        .to_string();
        let resp = state
            .request(reqwest::Method::GET, job_url)
            .header("x-boss-user", user_json)
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(v) => v,
                Err(e) => return err2(StatusCode::BAD_GATEWAY, format!("job malformed: {e}")),
            },
            Ok(r) => {
                return err2(
                    StatusCode::BAD_GATEWAY,
                    format!("job fetch: {}", r.status()),
                );
            }
            Err(e) => return err2(StatusCode::BAD_GATEWAY, format!("jobs unreachable: {e}")),
        }
    };
    let Some(step) = job["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["id"] == body.step_id.as_str()))
    else {
        return err2(StatusCode::NOT_FOUND, "no such step on that job");
    };
    let title = step["title"].as_str().unwrap_or_default();
    let metadata = step.get("metadata").cloned().unwrap_or(Value::Null);
    let shape_hash = boss_core::job::step_shape_hash(title, &metadata);

    let nonce = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = presence_challenge(&shape_hash, &nonce);

    let rows = match state.stored_passkeys(&employee_id).await {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    if rows.is_empty() {
        return err2(
            StatusCode::CONFLICT,
            "no passkey enrolled — enrol one before approving presence-gated steps",
        );
    }

    let challenge_id = Uuid::new_v4().to_string();
    let mint = state
        .request(
            reqwest::Method::POST,
            format!("{}/api/people/presence-challenges", state.people_base),
        )
        .json(&json!({
            "id": challenge_id,
            "employee_id": employee_id,
            "challenge": URL_SAFE_NO_PAD.encode(&challenge),
            "flow": "presence",
            "step_id": body.step_id,
            "shape_hash": shape_hash,
            "nonce": nonce,
        }))
        .send()
        .await;
    if !matches!(&mint, Ok(r) if r.status().is_success()) {
        return err2(StatusCode::BAD_GATEWAY, "challenge mint failed");
    }

    let allow: Vec<Value> = rows
        .iter()
        .filter_map(|r| r["credential_id"].as_str())
        .map(|id| json!({"type": "public-key", "id": id}))
        .collect();
    Json(json!({
        "challenge_id": challenge_id,
        "shape_hash": shape_hash,
        "publicKey": {
            "challenge": URL_SAFE_NO_PAD.encode(&challenge),
            "rpId": state.webauthn.get_allowed_origins().first()
                .and_then(|o| o.host_str().map(String::from)),
            "allowCredentials": allow,
            "userVerification": "required",
            "timeout": 60_000,
        }
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct AssertFinishBody {
    challenge_id: String,
    credential: PublicKeyCredential,
}

pub async fn assert_finish(
    State(state): State<Arc<PasskeyState>>,
    headers: HeaderMap,
    Json(body): Json<AssertFinishBody>,
) -> Response {
    let (_sess, employee_id) = match employee_session(&headers, &state.session_key) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let row = match consume_challenge(&state, &body.challenge_id).await {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    if row["flow"] != "presence" || row["employee_id"] != employee_id.as_str() {
        return err2(
            StatusCode::FORBIDDEN,
            "challenge was minted for someone else",
        );
    }
    let (Some(challenge_b64), Some(step_id), Some(shape_hash), Some(nonce)) = (
        row["challenge"].as_str(),
        row["step_id"].as_str(),
        row["shape_hash"].as_str(),
        row["nonce"].as_str(),
    ) else {
        return err2(
            StatusCode::BAD_GATEWAY,
            "challenge row missing presence binding",
        );
    };

    let rows = match state.stored_passkeys(&employee_id).await {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let passkeys = match PasskeyState::passkey_jsons(&rows) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    // Build the crate's own AuthenticationState through serde — the
    // documented experts-only seam for a server-supplied challenge.
    // Every verification step (origin, rpIdHash, UV, signature,
    // counter) stays inside webauthn-rs.
    let creds: Vec<Value> = passkeys.iter().map(|p| p["cred"].clone()).collect();
    let auth_state: webauthn_rs::prelude::PasskeyAuthentication =
        match serde_json::from_value(json!({
            "ast": {
                "credentials": creds,
                "policy": "required",
                "challenge": challenge_b64,
                "appid": null,
                "allow_backup_eligible_upgrade": false,
            }
        })) {
            Ok(v) => v,
            Err(e) => {
                return err2(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("authentication state rebuild failed: {e}"),
                );
            }
        };
    let result = match state
        .webauthn
        .finish_passkey_authentication(&body.credential, &auth_state)
    {
        Ok(v) => v,
        Err(e) => return err2(StatusCode::UNAUTHORIZED, format!("assertion rejected: {e}")),
    };

    // Advance the sign counter — clone detection lives in the crate,
    // the durable count lives with the credential row.
    let _ = state
        .request(
            reqwest::Method::POST,
            format!("{}/api/people/webauthn-credentials/used", state.people_base),
        )
        .json(&json!({
            "credential_id": URL_SAFE_NO_PAD.encode(result.cred_id().as_ref()),
            "sign_count": result.counter(),
        }))
        .send()
        .await;

    let ticket = PresenceTicket {
        i: employee_id,
        s: step_id.to_string(),
        h: shape_hash.to_string(),
        n: nonce.to_string(),
        e: now_epoch() + TICKET_TTL_SECONDS,
    }
    .encode(&state.session_key);
    Json(json!({
        "ticket": ticket,
        "expires_in": TICKET_TTL_SECONDS,
        "shape_hash": shape_hash,
    }))
    .into_response()
}

/// Consume a challenge row: 410 → replay/too-slow, 404 → never minted.
async fn consume_challenge(state: &PasskeyState, id: &str) -> Result<Value, ErrResp> {
    let resp = state
        .request(
            reqwest::Method::POST,
            format!(
                "{}/api/people/presence-challenges/{}/consume",
                state.people_base, id
            ),
        )
        .send()
        .await
        .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("people unreachable: {e}")))?;
    match resp.status() {
        s if s.is_success() => resp
            .json()
            .await
            .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("challenge malformed: {e}"))),
        reqwest::StatusCode::GONE => Err(err(
            StatusCode::GONE,
            "challenge already spent or expired — begin again",
        )),
        reqwest::StatusCode::NOT_FOUND => Err(err(StatusCode::NOT_FOUND, "unknown challenge")),
        s => Err(err(
            StatusCode::BAD_GATEWAY,
            format!("challenge consume: {s}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test-session-key";

    #[test]
    fn ticket_round_trips_and_expires() {
        let t = PresenceTicket {
            i: "emp-1".into(),
            s: "step-1".into(),
            h: "hash".into(),
            n: "nonce".into(),
            e: now_epoch() + 60,
        };
        let enc = t.encode(KEY);
        let dec = PresenceTicket::decode(&enc, KEY, now_epoch()).expect("valid ticket decodes");
        assert_eq!(dec, t);
        // Expired by clock: decode refuses.
        assert!(PresenceTicket::decode(&enc, KEY, t.e + 1).is_none());
        // Wrong key: decode refuses.
        assert!(PresenceTicket::decode(&enc, b"other-key", now_epoch()).is_none());
        // Tampered payload: decode refuses.
        let mut forged = enc.clone();
        forged.replace_range(0..1, if enc.starts_with('A') { "B" } else { "A" });
        assert!(PresenceTicket::decode(&forged, KEY, now_epoch()).is_none());
    }

    #[test]
    fn trusted_header_carries_the_binding() {
        let t = PresenceTicket {
            i: "emp-9".into(),
            s: "step-9".into(),
            h: "abc".into(),
            n: "def".into(),
            e: now_epoch() + 60,
        };
        let hdr = presence_header_from_ticket(&t.encode(KEY), KEY).expect("valid");
        let v: Value = serde_json::from_str(&hdr).unwrap();
        assert_eq!(v["employee_id"], "emp-9");
        assert_eq!(v["step_id"], "step-9");
        assert_eq!(v["shape_hash"], "abc");
        assert_eq!(v["nonce"], "def");
    }

    #[test]
    fn challenge_recipe_is_binding_and_nonce_sensitive() {
        let a = presence_challenge("hash-a", "n1");
        assert_eq!(a.len(), 32, "a sha256 — a valid webauthn challenge length");
        assert_ne!(
            a,
            presence_challenge("hash-b", "n1"),
            "different content, different challenge"
        );
        assert_ne!(
            a,
            presence_challenge("hash-a", "n2"),
            "same content, fresh nonce, fresh challenge"
        );
        assert_eq!(
            a,
            presence_challenge("hash-a", "n1"),
            "deterministic for the row it binds"
        );
    }

    #[test]
    fn authentication_state_builds_through_the_serde_seam() {
        // If webauthn-rs ever changes AuthenticationState's shape this
        // test names the break before a runtime 500 does.
        let auth: Result<webauthn_rs::prelude::PasskeyAuthentication, _> =
            serde_json::from_value(json!({
                "ast": {
                    "credentials": [],
                    "policy": "required",
                    "challenge": URL_SAFE_NO_PAD.encode([7u8; 32]),
                    "appid": null,
                    "allow_backup_eligible_upgrade": false,
                }
            }));
        assert!(auth.is_ok(), "state seam broke: {:?}", auth.err());
    }
}
