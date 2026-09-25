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
//! `x-boss-*` so a client cannot forge it — verifies and forwards, still
//! signed, as `x-boss-presence`. The jobs API verifies that signature
//! AGAIN before it reads `produced = Presence`, because its machine door
//! is reachable without this gateway (backlog 72fe3640); the ticket type
//! and its check are `boss_core::presence`, one definition for both. No
//! fallback path exists anywhere in the chain, per Q3: an assurance
//! level with a bypass is a comment, not a control.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use webauthn_rs::prelude::{
    CreationChallengeResponse, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
};

use crate::session::{self, Session};

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

/// Every passkey route, the one list: the gateway's `main.rs` merges
/// this router and the tests drive it. Until backlog 3bddce66
/// (2026-09-23) `main.rs` spelled the six routes out itself and this
/// list lacked the credential removal, so what the tests mounted was
/// not what production served (CLAUDE.md 9a). Generic over the outer
/// router's state because each route carries its own.
pub fn passkey_router<S>(state: Arc<PasskeyState>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/api/auth/passkey/register/begin",
            post(register_begin).with_state(state.clone()),
        )
        .route(
            "/api/auth/passkey/register/finish",
            post(register_finish).with_state(state.clone()),
        )
        .route(
            "/api/auth/passkey/assert/begin",
            post(assert_begin).with_state(state.clone()),
        )
        .route(
            "/api/auth/passkey/assert/finish",
            post(assert_finish).with_state(state.clone()),
        )
        .route(
            "/api/auth/passkey/credentials",
            axum::routing::get(credentials_list).with_state(state.clone()),
        )
        .route(
            "/api/auth/passkey/credentials/{credential_id}",
            axum::routing::delete(credentials_remove).with_state(state),
        )
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

/// ONE PATH SEGMENT OF A PEOPLE REQUEST, OR A REFUSAL (backlog
/// a0dd9387; CodeQL rust/request-forgery on mirror PR 242). Every
/// people call here is signed as the gateway's own platform-admin actor
/// ([`sign_as_gateway`]), so a value formatted into its path decides
/// which privileged request is made. Two such values come from the
/// caller — a removal's `credential_id` (axum decodes percent-escapes,
/// so a slash arrives inside the one segment) and a finish's
/// `challenge_id` — and a traversal in either would steer the request
/// to another people path. So a segment must be an id: the base64url
/// alphabet plus `.` and `@`, which covers credential ids, challenge
/// uuids and employee ids, and never `.` or `..` on its own. Anything
/// else is refused with 400 BEFORE a request is made.
fn people_segment<'a>(value: &'a str, what: &str) -> Result<&'a str, ErrResp> {
    let well_formed = !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@'));
    if well_formed {
        Ok(value)
    } else {
        Err(err(StatusCode::BAD_REQUEST, format!("malformed {what}")))
    }
}

/// A JOB OR STEP ID FROM THE CALLER, OR A REFUSAL (backlog 18b9e09d,
/// the residual of a0dd9387). `assert_begin` formats the caller's
/// `job_id` into a jobs request path, so it gets the same treatment as
/// [`people_segment`] — refused with 400 before any request — but
/// stricter, because every job and step id IS a UUID (`define_id!` over
/// a Uuid in boss-core). The parsed value is what goes on: formatted
/// back, it is hex and hyphens only, whatever spelling the caller used.
fn uuid_segment(value: &str, what: &str) -> Result<Uuid, ErrResp> {
    Uuid::parse_str(value).map_err(|_| err(StatusCode::BAD_REQUEST, format!("malformed {what}")))
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
        Err(r) => return presence_refused("credentials_remove", None, "session", r),
    };
    let refused = |reason: &str, r: ErrResp| {
        presence_refused("credentials_remove", Some(&employee_id), reason, r)
    };
    let (employee, credential) = match (
        people_segment(&employee_id, "employee id"),
        people_segment(&credential_id, "credential id"),
    ) {
        (Ok(e), Ok(c)) => (e, c),
        (Err(r), _) | (_, Err(r)) => return refused("credential removal: malformed id", r),
    };
    let url = format!(
        "{}/api/people/{employee}/webauthn-credentials/{credential}",
        state.people_base
    );
    let resp = match state.request(reqwest::Method::DELETE, url).send().await {
        Ok(r) => r,
        Err(_) => {
            return refused(
                "credential removal: people unreachable",
                err(StatusCode::BAD_GATEWAY, PEOPLE_UNREACHABLE),
            );
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let reason = format!("credential removal: {}", resp.status());
    match status {
        StatusCode::NO_CONTENT => StatusCode::NO_CONTENT.into_response(),
        StatusCode::CONFLICT | StatusCode::NOT_FOUND => {
            refused(&reason, (status, resp.text().await.unwrap_or_default()))
        }
        _ => refused(
            &reason,
            err(StatusCode::BAD_GATEWAY, "credential removal failed"),
        ),
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

/// The ticket type and its HMAC check live in `boss_core::presence`,
/// because the jobs API verifies the same ticket and must do it with
/// the same function (backlog 72fe3640; CLAUDE.md §9a).
pub use boss_core::presence::PresenceTicket;
use boss_core::presence::now_epoch;

/// The header role_headers forwards a verified ticket in, and the jobs
/// API reads (and verifies again) as `produced`.
pub const PRESENCE_HEADER: &str = boss_core::presence::HEADER;
/// The client-supplied ticket header. Deliberately NOT `x-boss-*`:
/// the edge strip removes that whole prefix from inbound traffic, and
/// the ticket must survive to be verified (forgery is caught by the
/// HMAC, not the strip).
pub const TICKET_HEADER: &str = "x-presence-ticket";

/// Verify a ticket header value and produce the header to forward.
/// Called from role_headers inside the session branch.
///
/// The value forwarded is the SIGNED TICKET ITSELF, not its fields as
/// JSON. Until backlog 72fe3640 (2026-09-24) this returned
/// `{"employee_id","step_id","shape_hash","nonce"}` in the clear, and the
/// jobs API granted presence on that JSON — so anyone reaching the jobs
/// API's machine door without this gateway could write the JSON by hand.
/// Forwarding the signature lets the jobs API verify it where it grants;
/// checking it here as well keeps a bad ticket from travelling at all.
pub fn presence_header_from_ticket(value: &str, key: &[u8]) -> Option<String> {
    PresenceTicket::decode(value, key, now_epoch())?;
    Some(value.to_string())
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

/// What the browser reads when boss-people cannot be reached. Fixed,
/// because reqwest's error names the URL it failed on, and those URLs
/// carry a challenge id (the consume) or a credential id (the removal)
/// — until backlog 56126dc7 (2026-09-23) this was `people unreachable:
/// {e}`, and since 2e893e27 and f3436d99 the browser renders it.
const PEOPLE_UNREACHABLE: &str = "people unreachable";

/// The same for boss-jobs: the step read in `assert_begin` failed on
/// the internal jobs URL, which `jobs unreachable: {e}` printed to the
/// browser until backlog 3bddce66 (2026-09-23).
const JOBS_UNREACHABLE: &str = "jobs unreachable";

/// A passkey ceremony refusal, written to the gateway log and then
/// returned unchanged. Until backlog f3436d99 (2026-09-23) `assert_begin`
/// and `assert_finish` told only the browser why they refused, so a
/// failed presence approval could not be diagnosed from the server;
/// enrolment (`register_begin`, `register_finish`) and
/// `credentials_remove` stayed silent until backlog 56126dc7 the same
/// day, and route through here too. The name predates them and stays:
/// f3436d99's recorded probe reads this function by name.
///
/// `reason` is what the log carries, and it is built ONLY from fixed
/// text, an HTTP status, or the error text webauthn-rs gives (its
/// errors are fixed strings) — never from the refusal's own message,
/// which can hold a reqwest error naming the URL it failed on, and the
/// consume URL carries the challenge id. Nothing here logs a challenge
/// id, a credential id or any credential material; the employee id is
/// the only identifier.
fn presence_refused(
    ceremony: &'static str,
    employee_id: Option<&str>,
    reason: &str,
    (status, msg): ErrResp,
) -> Response {
    tracing::warn!(
        ceremony,
        employee_id = employee_id.unwrap_or("none"),
        status = status.as_u16(),
        reason,
        "passkey ceremony refused"
    );
    (status, msg).into_response()
}

/// The gateway's own service identity: the actor its server-side
/// calls sign as, and the `owner_id` of what those calls open.
pub const GATEWAY_ACTOR: &str = "automation:gateway";

/// A server-side call signed as the gateway's own internal actor,
/// plus the machine token when the process has one. boss-people's
/// webauthn storage requires a `platform-admin` caller (its paths
/// are also browser-reachable through the /api/people proxy, so it
/// cannot trust callers by position) — this identity is how the
/// ceremony passes that gate while ordinary proxied sessions are
/// refused. The site's inquiry door (inquiries.rs) signs its
/// accounts and jobs writes the same way; one spelling, here, so the
/// two cannot drift (backlog 68126ec9).
pub fn sign_as_gateway(rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let rb = rb.header(
        "x-boss-user",
        json!({
            "id": GATEWAY_ACTOR,
            "role": "platform-admin",
            "access_tier": "operator",
        })
        .to_string(),
    );
    match boss_core::machine_token::from_env() {
        Some(token) => rb.header(boss_core::machine_token::HEADER, token),
        None => rb,
    }
}

/// A server-side call made FOR a signed-in session, carrying ONE
/// identity — the session's — plus the machine token, exactly what the
/// role_headers middleware stamps on that session's own proxied
/// traffic. The downstream policy extractor reads the first
/// `x-boss-user`, and reqwest's `header` appends, so a request built
/// from [`sign_as_gateway`] with the session's identity added after it
/// ran with the gateway's platform-admin scope: `assert_begin`'s step
/// read did exactly that until backlog 18b9e09d (2026-09-24). A read
/// made on an employee's behalf is that employee's read.
fn sign_as_session(rb: reqwest::RequestBuilder, user_json: String) -> reqwest::RequestBuilder {
    let rb = rb.header("x-boss-user", user_json);
    match boss_core::machine_token::from_env() {
        Some(token) => rb.header(boss_core::machine_token::HEADER, token),
        None => rb,
    }
}

impl PasskeyState {
    /// Machine-token-stamped server-side call as the gateway — see
    /// [`sign_as_gateway`]. Only for the ceremony's OWN storage calls;
    /// a read on the session's behalf goes through [`sign_as_session`].
    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        sign_as_gateway(self.http.request(method, url))
    }

    async fn stored_passkeys(&self, employee_id: &str) -> Result<Vec<Value>, ErrResp> {
        let employee_id = people_segment(employee_id, "employee id")?;
        let url = format!(
            "{}/api/people/{}/webauthn-credentials",
            self.people_base, employee_id
        );
        let resp = self
            .request(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(|_| err(StatusCode::BAD_GATEWAY, PEOPLE_UNREACHABLE))?;
        if !resp.status().is_success() {
            return Err(err(StatusCode::BAD_GATEWAY, "credential lookup failed"));
        }
        // Fixed text: the decode error names the URL it read and can
        // quote the value it choked on (backlog 3bddce66, 2026-09-23).
        resp.json::<Vec<Value>>()
            .await
            .map_err(|_| err(StatusCode::BAD_GATEWAY, "credential list malformed"))
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
        Err(r) => return presence_refused("register_begin", None, "session", r),
    };
    let refused = |reason: &str, r: ErrResp| {
        presence_refused("register_begin", Some(&employee_id), reason, r)
    };
    // Exclude already-registered credentials so an authenticator
    // cannot double-enrol.
    let rows = match state.stored_passkeys(&employee_id).await {
        Ok(v) => v,
        Err(r) => return refused("stored passkeys", r),
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
        Err(e) => {
            let msg = format!("webauthn: {e}");
            return refused(&msg, err(StatusCode::INTERNAL_SERVER_ERROR, msg.clone()));
        }
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
        Ok(r) => refused(
            &format!("challenge mint: {}", r.status()),
            err(
                StatusCode::BAD_GATEWAY,
                format!("challenge mint failed: {}", r.status()),
            ),
        ),
        Err(_) => refused(
            "challenge mint: people unreachable",
            err(StatusCode::BAD_GATEWAY, PEOPLE_UNREACHABLE),
        ),
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
        Err(r) => return presence_refused("register_finish", None, "session", r),
    };
    let refused = |reason: &str, r: ErrResp| {
        presence_refused("register_finish", Some(&employee_id), reason, r)
    };
    let row = match consume_challenge(&state, &body.challenge_id).await {
        Ok(v) => v,
        Err(r) => return refused("challenge consume", r),
    };
    if row["flow"] != "register" || row["employee_id"] != employee_id.as_str() {
        return refused(
            "challenge minted for someone else",
            err(
                StatusCode::FORBIDDEN,
                "challenge was minted for someone else",
            ),
        );
    }
    let state_bytes = match row["challenge"]
        .as_str()
        .and_then(|b| URL_SAFE_NO_PAD.decode(b).ok())
    {
        Some(v) => v,
        None => {
            return refused(
                "stored registration state not base64url",
                err(
                    StatusCode::BAD_GATEWAY,
                    "stored registration state unreadable",
                ),
            );
        }
    };
    let reg_state: PasskeyRegistration = match serde_json::from_slice(&state_bytes) {
        Ok(v) => v,
        // The serde error can quote the stored state; the log gets the
        // stage only.
        Err(_) => {
            return refused(
                "stored registration state not a registration",
                err(
                    StatusCode::BAD_GATEWAY,
                    "stored registration state unreadable",
                ),
            );
        }
    };
    let passkey = match state
        .webauthn
        .finish_passkey_registration(&body.credential, &reg_state)
    {
        Ok(v) => v,
        Err(e) => {
            let reason = format!("attestation rejected: {e}");
            return refused(&reason, err(StatusCode::BAD_REQUEST, reason.clone()));
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
        Ok(r) if r.status() == reqwest::StatusCode::CONFLICT => refused(
            "credential already registered",
            err(StatusCode::CONFLICT, "credential already registered"),
        ),
        Ok(r) => refused(
            &format!("credential store: {}", r.status()),
            err(
                StatusCode::BAD_GATEWAY,
                format!("credential store failed: {}", r.status()),
            ),
        ),
        Err(_) => refused(
            "credential store: people unreachable",
            err(StatusCode::BAD_GATEWAY, PEOPLE_UNREACHABLE),
        ),
    }
}

// ---------------------------------------------------------------------------
// Assertion — the presence half
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AssertBeginBody {
    job_id: String,
    step_id: String,
    /// The step content the approver was SHOWN — its title and metadata
    /// as the surface rendered them, with the surface's own writes
    /// folded in. The challenge binds the step's CURRENT shape, so a
    /// begin whose shown content does not hash to it is refused: a swap
    /// made after an honest page rendered is never signed (backlog
    /// fd7090cc, re-review of 2026-09-25). The page supplies this, so it
    /// is not proof of what was on screen. `Option` only so its absence
    /// is refused in words rather than by the extractor's.
    shown: Option<ShownStep>,
}

/// What a surface rendered of the step it is about to have signed.
#[derive(Deserialize)]
pub struct ShownStep {
    title: String,
    metadata: Value,
}

/// Why a begin's shown content cannot be what the passkey signs, or
/// `None` when it is exactly the step as it stands. Hashed with the one
/// definition — `boss_core::job::step_shape_hash`, the function the
/// jobs service binds a presence stamp with — so the browser carries no
/// copy of the canonical form (CLAUDE.md §9a).
fn shown_mismatch(shown: Option<&ShownStep>, current_shape: &str) -> Option<ErrResp> {
    let Some(shown) = shown else {
        return Some(err(
            StatusCode::BAD_REQUEST,
            "the ceremony names no shown step content — the surface must send the title \
             and metadata it rendered, so a step changed since then is refused",
        ));
    };
    if boss_core::job::step_shape_hash(&shown.title, &shown.metadata) == current_shape {
        return None;
    }
    Some(err(
        StatusCode::PRECONDITION_FAILED,
        "this step changed since it was shown — reload it and read it again before approving",
    ))
}

pub async fn assert_begin(
    State(state): State<Arc<PasskeyState>>,
    headers: HeaderMap,
    Json(body): Json<AssertBeginBody>,
) -> Response {
    let (sess, employee_id) = match employee_session(&headers, &state.session_key) {
        Ok(v) => v,
        Err(r) => return presence_refused("assert_begin", None, "session", r),
    };
    let refused =
        |reason: &str, r: ErrResp| presence_refused("assert_begin", Some(&employee_id), reason, r);
    // Both ids come from the caller's body, and the job id becomes a
    // request path: a UUID, or a refusal before any request (18b9e09d).
    let (job_id, step_id) = match (
        uuid_segment(&body.job_id, "job id"),
        uuid_segment(&body.step_id, "step id"),
    ) {
        (Ok(j), Ok(s)) => (j, s.to_string()),
        (Err(r), _) | (_, Err(r)) => return refused("malformed id", r),
    };
    // The step's CURRENT content is what the passkey will approve.
    let job_url = format!("{}/api/jobs/{job_id}", state.jobs_base);
    let job: Value = {
        let user_json = json!({
            "id": employee_id,
            // A roleless session reads as the least access, not the
            // widest (design 2830b6b7).
            "role": boss_core::roles::effective_role(sess.role.as_deref()),
            "access_tier": sess.access_tier,
            "territory_account_ids": sess.territory_account_ids,
            "direct_report_ids": sess.direct_report_ids,
            "department": sess.department,
        })
        .to_string();
        // Read as the session, never as the gateway: the employee sees
        // only what their own scope lets them (18b9e09d).
        let resp = sign_as_session(state.http.get(job_url), user_json)
            .send()
            .await;
        match resp {
            // This refusal and the unreachable one below are fixed text
            // since backlog 3bddce66 (2026-09-23): reqwest's errors
            // name the internal jobs URL, and a decode error can quote
            // the job it choked on.
            Ok(r) if r.status().is_success() => match r.json().await {
                Ok(v) => v,
                Err(_) => {
                    return refused(
                        "job malformed",
                        err(StatusCode::BAD_GATEWAY, "job malformed"),
                    );
                }
            },
            Ok(r) => {
                let reason = format!("job fetch: {}", r.status());
                return refused(&reason, err(StatusCode::BAD_GATEWAY, reason.clone()));
            }
            Err(_) => {
                return refused(
                    "jobs unreachable",
                    err(StatusCode::BAD_GATEWAY, JOBS_UNREACHABLE),
                );
            }
        }
    };
    let Some(step) = job["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["id"] == step_id.as_str()))
    else {
        return refused(
            "no such step on that job",
            err(StatusCode::NOT_FOUND, "no such step on that job"),
        );
    };
    let title = step["title"].as_str().unwrap_or_default();
    let metadata = step.get("metadata").cloned().unwrap_or(Value::Null);
    let shape_hash = boss_core::job::step_shape_hash(title, &metadata);
    // WHAT IS SIGNED IS WHAT WAS SHOWN, checked before any passkey is
    // read or challenge minted.
    if let Some(r) = shown_mismatch(body.shown.as_ref(), &shape_hash) {
        let reason = if r.0 == StatusCode::BAD_REQUEST {
            "no shown content"
        } else {
            "step changed since it was shown"
        };
        return refused(reason, r);
    }

    let nonce = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = presence_challenge(&shape_hash, &nonce);

    let rows = match state.stored_passkeys(&employee_id).await {
        Ok(v) => v,
        Err(r) => return refused("stored passkeys", r),
    };
    if rows.is_empty() {
        return refused(
            "no passkey enrolled",
            err(
                StatusCode::CONFLICT,
                "no passkey enrolled — enrol one before approving presence-gated steps",
            ),
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
            "step_id": step_id,
            "shape_hash": shape_hash,
            "nonce": nonce,
        }))
        .send()
        .await;
    if !matches!(&mint, Ok(r) if r.status().is_success()) {
        let reason = match &mint {
            Ok(r) => format!("challenge mint: {}", r.status()),
            Err(_) => "challenge mint: people unreachable".to_string(),
        };
        return refused(
            &reason,
            err(StatusCode::BAD_GATEWAY, "challenge mint failed"),
        );
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
        Err(r) => return presence_refused("assert_finish", None, "session", r),
    };
    let refused =
        |reason: &str, r: ErrResp| presence_refused("assert_finish", Some(&employee_id), reason, r);
    let row = match consume_challenge(&state, &body.challenge_id).await {
        Ok(v) => v,
        Err(r) => return refused("challenge consume", r),
    };
    if row["flow"] != "presence" || row["employee_id"] != employee_id.as_str() {
        return refused(
            "challenge minted for someone else",
            err(
                StatusCode::FORBIDDEN,
                "challenge was minted for someone else",
            ),
        );
    }
    let (Some(challenge_b64), Some(step_id), Some(shape_hash), Some(nonce)) = (
        row["challenge"].as_str(),
        row["step_id"].as_str(),
        row["shape_hash"].as_str(),
        row["nonce"].as_str(),
    ) else {
        return refused(
            "challenge row missing presence binding",
            err(
                StatusCode::BAD_GATEWAY,
                "challenge row missing presence binding",
            ),
        );
    };

    let rows = match state.stored_passkeys(&employee_id).await {
        Ok(v) => v,
        Err(r) => return refused("stored passkeys", r),
    };
    let passkeys = match PasskeyState::passkey_jsons(&rows) {
        Ok(v) => v,
        Err(r) => return refused("stored passkey rows", r),
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
            // The serde error can quote the value it choked on, and
            // that value is credential material — the log gets the
            // stage only, and since backlog 56126dc7 (2026-09-23) so
            // does the browser, which rendered it.
            Err(_) => {
                return refused(
                    "authentication state rebuild failed",
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "authentication state rebuild failed",
                    ),
                );
            }
        };
    let result = match state
        .webauthn
        .finish_passkey_authentication(&body.credential, &auth_state)
    {
        Ok(v) => v,
        Err(e) => {
            let reason = format!("assertion rejected: {e}");
            return refused(&reason, err(StatusCode::UNAUTHORIZED, reason.clone()));
        }
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

    let Some(ticket) = (PresenceTicket {
        i: employee_id.clone(),
        s: step_id.to_string(),
        h: shape_hash.to_string(),
        n: nonce.to_string(),
        e: now_epoch() + TICKET_TTL_SECONDS,
    })
    .encode(&state.session_key) else {
        let reason = "the verified assertion could not be signed as a ticket";
        return refused(reason, err(StatusCode::INTERNAL_SERVER_ERROR, reason));
    };
    Json(json!({
        "ticket": ticket,
        "expires_in": TICKET_TTL_SECONDS,
        "shape_hash": shape_hash,
    }))
    .into_response()
}

/// Consume a challenge row: 410 → replay/too-slow, 404 → never minted.
async fn consume_challenge(state: &PasskeyState, id: &str) -> Result<Value, ErrResp> {
    // The id comes back from the caller's finish body: an id, or a
    // refusal before any request (a0dd9387, `people_segment`).
    let id = people_segment(id, "challenge id")?;
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
        .map_err(|_| err(StatusCode::BAD_GATEWAY, PEOPLE_UNREACHABLE))?;
    match resp.status() {
        // Fixed text: the decode error names the consume URL, which
        // carries the challenge id (backlog 3bddce66, 2026-09-23).
        s if s.is_success() => resp
            .json()
            .await
            .map_err(|_| err(StatusCode::BAD_GATEWAY, "challenge malformed")),
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

    /// The forwarded header is the signed ticket, so the jobs API can
    /// verify it with the same key and read the same binding back out
    /// (backlog 72fe3640). The ticket's own round-trip, expiry, wrong-key
    /// and tamper cases are pinned beside its definition in
    /// `boss_core::presence`.
    #[test]
    fn the_forwarded_header_is_the_signed_ticket() {
        let t = PresenceTicket {
            i: "emp-9".into(),
            s: "step-9".into(),
            h: "abc".into(),
            n: "def".into(),
            e: now_epoch() + 60,
        };
        let enc = t.encode(KEY).unwrap();
        let hdr = presence_header_from_ticket(&enc, KEY).expect("valid");
        let back = PresenceTicket::decode(&hdr, KEY, now_epoch()).expect("still verifies");
        assert_eq!(back, t);
        // An unsigned value is never forwarded at all.
        assert!(presence_header_from_ticket(&enc, b"other-key").is_none());
        assert!(presence_header_from_ticket(r#"{"employee_id":"emp-9"}"#, KEY).is_none());
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
