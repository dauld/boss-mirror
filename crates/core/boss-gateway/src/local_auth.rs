//! File-backed credential store + login/logout/me HTTP handlers.
//!
//! v1 OSS-quickstart auth. The contract:
//!
//! - Credentials live in a TOML file at `BOSS_AUTH_FILE` (default
//!   `/var/lib/boss/auth/credentials.toml`). One row per email
//!   with an Argon2id PHC hash, an optional one-time reset-token
//!   hash, and timestamps.
//! - Login: `POST /api/auth/login {email, password}` verifies
//!   against the file, looks up the Employee scope via
//!   `bootstrap_email`, mints a `boss_session` cookie via
//!   `session::Session::encode`.
//! - Logout: `POST /api/auth/logout` clears the cookie.
//! - Me: `GET /api/auth/me` decodes the session, returns the
//!   email + role + employee_id.
//! - Guest: `POST /api/auth/guest` mints a read-only session for
//!   the public demo (see below). `GET` on the same path reports
//!   whether this deployment offers it.
//!
//! ## Guest sessions
//!
//! A demo deployment wants a visitor to look around without an
//! account. The previous answer was demo mode: a middleware that
//! minted a synthetic `audit-readonly` session for anyone arriving
//! without a valid cookie. It substituted identity *silently*, and
//! the failure that killed it was exactly that — when a real
//! admin's 8-hour session expired, the next request minted a guest
//! session over it and reissued the cookie under the same name.
//! The SPA still looked signed in; every write returned 403.
//!
//! The guest session here is the same access with the substitution
//! removed. It exists only when someone clicks the button, it says
//! whose session it is, and an expiring session now expires — it
//! does not quietly become somebody else's.
//!
//! Onboarding (admin-only):
//! - `POST /api/auth/onboard {email, password}` — creates a
//!   credential row. Caller must be authenticated as a role with
//!   `policy:auth-admin` (which platform-admin / ceo / coo carry).
//! - `POST /api/auth/issue-reset {email}` — issues a one-time
//!   reset token (returns it to the admin; admin shares with the
//!   user out-of-band).
//! - `POST /api/auth/reset {email, token, password}` — consumes
//!   the token, rotates the password.
//!
//! What's deliberately small:
//! - No CSRF token. Cookie is `SameSite=Strict`. Production-grade
//!   tenants front the gateway with a proxy that handles CSRF.
//! - No account lockout / brute-force protection. Lives at the
//!   proxy tier.
//! - No password policy enforcement. Operators choose; SPA
//!   surfaces a min-length nudge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::session::{self, Session};

// --------------------------------------------------------------------
// On-disk model.
// --------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credential {
    pub email: String,
    pub password_hash: String,
    pub created_at: DateTime<Utc>,
    pub last_rotated: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_token: Option<ResetToken>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetToken {
    /// Sha-256 hex of the token. The plaintext token is returned
    /// to the admin once at issue-time and never persisted.
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CredentialsFile {
    #[serde(default)]
    credential: Vec<Credential>,
}

// --------------------------------------------------------------------
// In-memory store.
// --------------------------------------------------------------------

#[derive(Clone)]
pub struct CredentialStore {
    inner: Arc<RwLock<Inner>>,
}

struct Inner {
    path: PathBuf,
    by_email: HashMap<String, Credential>,
}

impl CredentialStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let by_email = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let file: CredentialsFile =
                toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
            file.credential
                .into_iter()
                .map(|c| (c.email.to_lowercase(), c))
                .collect()
        } else {
            HashMap::new()
        };
        Ok(Self {
            inner: Arc::new(RwLock::new(Inner { path, by_email })),
        })
    }

    pub fn verify(&self, email: &str, password: &str) -> Result<()> {
        let email = email.to_lowercase();
        let inner = self.inner.read().map_err(|_| anyhow!("store poisoned"))?;
        let cred = inner
            .by_email
            .get(&email)
            .ok_or_else(|| anyhow!("invalid credentials"))?;
        let hash = PasswordHash::new(&cred.password_hash)
            .map_err(|e| anyhow!("malformed password hash: {e}"))?;
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .map_err(|_| anyhow!("invalid credentials"))?;
        Ok(())
    }

    pub fn upsert(&self, email: &str, password: &str) -> Result<()> {
        let email = email.to_lowercase();
        let now = Utc::now();
        let salt = generate_salt();
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| anyhow!("argon2 hash: {e}"))?
            .to_string();
        let mut inner = self.inner.write().map_err(|_| anyhow!("store poisoned"))?;
        let entry = inner
            .by_email
            .entry(email.clone())
            .or_insert_with(|| Credential {
                email: email.clone(),
                password_hash: String::new(),
                created_at: now,
                last_rotated: now,
                reset_token: None,
            });
        entry.password_hash = hash;
        entry.last_rotated = now;
        entry.reset_token = None;
        save_locked(&inner)
    }

    pub fn remove(&self, email: &str) -> Result<bool> {
        let email = email.to_lowercase();
        let mut inner = self.inner.write().map_err(|_| anyhow!("store poisoned"))?;
        let removed = inner.by_email.remove(&email).is_some();
        if removed {
            save_locked(&inner)?;
        }
        Ok(removed)
    }

    pub fn list_emails(&self) -> Vec<String> {
        let inner = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<String> = inner.by_email.keys().cloned().collect();
        out.sort();
        out
    }

    pub fn contains(&self, email: &str) -> bool {
        let email = email.to_lowercase();
        match self.inner.read() {
            Ok(g) => g.by_email.contains_key(&email),
            Err(_) => false,
        }
    }

    /// Issue a one-time reset token. Returns the plaintext token
    /// the admin shares out-of-band; only the sha256 hash is
    /// persisted. Tokens expire after `ttl_seconds`.
    pub fn issue_reset_token(&self, email: &str, ttl_seconds: i64) -> Result<String> {
        let email = email.to_lowercase();
        let token = random_token(24);
        let token_hash = sha256_hex(&token);
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl_seconds);
        let mut inner = self.inner.write().map_err(|_| anyhow!("store poisoned"))?;
        let entry = inner
            .by_email
            .get_mut(&email)
            .ok_or_else(|| anyhow!("no credential for {email} — onboard first"))?;
        entry.reset_token = Some(ResetToken {
            token_hash,
            expires_at,
        });
        save_locked(&inner)?;
        Ok(token)
    }

    /// Consume a one-time reset token + set a new password. Fails
    /// on missing/expired/wrong-token, with a generic error string
    /// (no enumeration leak).
    pub fn consume_reset_token(&self, email: &str, token: &str, new_password: &str) -> Result<()> {
        let email = email.to_lowercase();
        let token_hash = sha256_hex(token);
        // Re-hash the new password OUTSIDE the lock so we don't
        // hold the mutex across the (possibly slow) Argon2 call.
        let salt = generate_salt();
        let new_hash = Argon2::default()
            .hash_password(new_password.as_bytes(), &salt)
            .map_err(|e| anyhow!("argon2 hash: {e}"))?
            .to_string();
        let mut inner = self.inner.write().map_err(|_| anyhow!("store poisoned"))?;
        let entry = inner
            .by_email
            .get_mut(&email)
            .ok_or_else(|| anyhow!("invalid reset token"))?;
        let cur = entry
            .reset_token
            .as_ref()
            .ok_or_else(|| anyhow!("invalid reset token"))?;
        if cur.expires_at < Utc::now() {
            return Err(anyhow!("invalid reset token"));
        }
        if cur.token_hash != token_hash {
            return Err(anyhow!("invalid reset token"));
        }
        entry.password_hash = new_hash;
        entry.last_rotated = Utc::now();
        entry.reset_token = None;
        save_locked(&inner)?;
        Ok(())
    }
}

fn save_locked(inner: &Inner) -> Result<()> {
    let mut creds: Vec<Credential> = inner.by_email.values().cloned().collect();
    creds.sort_by(|a, b| a.email.cmp(&b.email));
    let file = CredentialsFile { credential: creds };
    let body = toml::to_string_pretty(&file).context("serialize credentials")?;
    if let Some(parent) = inner.path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let tmp = inner.path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&tmp, perms)
            .with_context(|| format!("chmod {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &inner.path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), inner.path.display()))?;
    Ok(())
}

fn generate_salt() -> SaltString {
    use rand::RngExt;
    let mut bytes = [0u8; 16];
    rand::rng().fill(&mut bytes[..]);
    SaltString::encode_b64(&bytes).expect("salt encode")
}

fn random_token(len: usize) -> String {
    use rand::RngExt;
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ\
abcdefghijkmnopqrstuvwxyz23456789"; // ambiguous chars dropped
    let mut rng = rand::rng();
    (0..len)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

// --------------------------------------------------------------------
// Bootstrap-email lookup — resolves an authenticated email to the
// Employee row's id/role/scope by hitting boss-people's
// `/api/people/bootstrap-email` endpoint.
// --------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct BootstrapScope {
    pub(crate) id: String,
    pub(crate) role: String,
    pub(crate) department: Option<String>,
    #[serde(default)]
    pub(crate) territory_account_ids: Vec<String>,
    #[serde(default)]
    pub(crate) direct_report_ids: Vec<String>,
}

pub(crate) async fn bootstrap_email(http: &reqwest::Client, email: &str) -> Option<BootstrapScope> {
    let upstream =
        std::env::var("BOSS_PEOPLE_UPSTREAM").unwrap_or_else(|_| boss_ports::url("people"));
    let url = format!(
        "{}/api/people/by-email/{}/bootstrap",
        upstream.trim_end_matches('/'),
        email,
    );
    let resp = http
        .get(&url)
        .header(
            "x-boss-user",
            r#"{"id":"automation:account-provisioning","role":"system","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":null}"#,
        )
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<BootstrapScope>().await.ok()
}

// --------------------------------------------------------------------
// HTTP state.
// --------------------------------------------------------------------

#[derive(Clone)]
pub struct LocalAuthState {
    pub store: CredentialStore,
    pub session_key: Vec<u8>,
    pub http: reqwest::Client,
    /// Auth events for the edge (docs/architecture-decisions.md
    /// §Policy & auth). Always
    /// present; a deployment without the staging pool carries the
    /// disabled emitter, whose record is the structured warn line.
    pub audit: crate::audit::AuthAudit,
    /// How auth mail leaves the building. Defaults to a transport
    /// that logs and sends nothing, so a deployment with no provider
    /// configured still completes resets — the operator reads the
    /// token out of the log — rather than silently pretending.
    pub mail: std::sync::Arc<dyn crate::mail::MailTransport>,
    /// Origin the reset link points at, e.g. `https://boss.example`.
    pub public_url: String,
    /// Last accepted `forgot` per email, for rate limiting.
    ///
    /// In-process and therefore a HEURISTIC, not a guarantee: with a
    /// second gateway in front of the same store it does not hold.
    /// Said plainly because a limiter that is assumed to be airtight
    /// is worse than one known to be approximate — this raises the
    /// cost of mailbombing a known address and of probing, and it
    /// does not survive horizontal scaling.
    pub forgot_seen: std::sync::Arc<RwLock<HashMap<String, std::time::Instant>>>,
    /// OIDC runtime when the IdP is configured (idm-kanidm.md).
    /// None → the oidc routes answer honestly that they are off.
    pub oidc: Option<std::sync::Arc<crate::oidc::OidcRuntime>>,
    /// Whether this deployment offers the read-only guest session.
    /// Off unless the deployment declares itself a demo — a tenant
    /// running BOSS on real data does not hand out a session that
    /// reads every projection.
    pub guest_access: bool,
}

// --------------------------------------------------------------------
// HTTP handlers.
// --------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub email: String,
    pub employee_id: Option<String>,
    pub role: Option<String>,
    pub access_tier: String,
}

/// `POST /api/auth/login` — verify credentials, look up scope,
/// mint the boss_session cookie. Returns the resolved identity.
pub async fn login(
    State(state): State<Arc<LocalAuthState>>,
    Json(req): Json<LoginRequest>,
) -> Response {
    if let Err(e) = state.store.verify(&req.email, &req.password) {
        // The decision record (§Policy & auth): before the
        // denied event, a bad password left no trace at all.
        state.audit.login_denied(
            Some(&req.email.to_lowercase()),
            crate::audit::AuthMethod::Password,
            crate::audit::DeniedReason::BadCredentials,
            None,
        );
        return (StatusCode::UNAUTHORIZED, format!("{e}")).into_response();
    }
    let email = req.email.to_lowercase();

    let mut sess = Session::new(&email, session::DEFAULT_TTL_SECONDS);
    // The platform-admin Employee is
    // provisioned by system initialization (boss-operator-baseline-
    // seed at bootstrap time) — the gateway never auto-creates one
    // on first login, because a running system with no platform-
    // admin can't perform a privileged action if one is needed in
    // the gap before the first human shows up. The single direct
    // write the system needs to bootstrap itself is the seed
    // binary's hire-insertion, not the gateway's login handler.
    //
    // If bootstrap_email returns None here, that means the
    // credential is valid but no Employee row matches — either
    // bootstrap didn't run, or BOSS_BOOTSTRAP_ADMIN_EMAIL wasn't
    // set when it did. Surface as 403 with the operator-facing
    // remediation path. Never silently downgrade to
    // audit-readonly, which was an earlier footgun.
    let scope = match bootstrap_email(&state.http, &email).await {
        Some(s) => s,
        None => {
            state.audit.login_denied(
                Some(&email),
                crate::audit::AuthMethod::Password,
                crate::audit::DeniedReason::NoEmployeeRecord,
                None,
            );
            return (
                StatusCode::FORBIDDEN,
                format!(
                    "credential verified but no Employee record matches {email}. \
                     System initialization should have provisioned this row via \
                     boss-operator-baseline-seed with BOSS_BOOTSTRAP_ADMIN_EMAIL \
                     set to the operator email. Either rerun the bootstrap or \
                     POST /api/people manually to create the row."
                ),
            )
                .into_response();
        }
    };
    sess.employee_id = Some(scope.id);
    sess.role = Some(scope.role);
    sess.department = scope.department;
    sess.territory_account_ids = scope.territory_account_ids;
    sess.direct_report_ids = scope.direct_report_ids;

    // The mint moment IS the succeeded event (§Policy & auth);
    // `method` is how the passkey path joins without a schema
    // change.
    state.audit.login_succeeded(
        &email,
        sess.employee_id.as_deref(),
        crate::audit::AuthMethod::Password,
    );

    let cookie_value = sess.encode(&state.session_key);
    let set_cookie = session::set_cookie(
        session::COOKIE_NAME,
        &cookie_value,
        session::DEFAULT_TTL_SECONDS,
        "/",
    );
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&set_cookie) {
        headers.insert(header::SET_COOKIE, v);
    }

    (
        StatusCode::OK,
        headers,
        Json(MeResponse {
            email,
            employee_id: sess.employee_id.clone(),
            role: sess.role.clone(),
            access_tier: sess.access_tier.clone(),
        }),
    )
        .into_response()
}

/// The guest's fixed identity. It is a real address on the demo
/// tenant's domain rather than something like `anonymous@local`
/// because it shows up in the audit log as an actor, and an actor
/// in the log should be a name you can look up.
pub const GUEST_EMAIL: &str = "guest@algedonic.dev";

#[derive(Serialize)]
pub struct GuestAvailability {
    pub enabled: bool,
    pub email: &'static str,
    pub role: &'static str,
}

/// `GET /api/auth/guest` — does this deployment offer guest
/// browsing? The sign-in page asks before rendering the button, so
/// a real tenant's users are never shown a control that 404s.
///
/// Unauthenticated by necessity: the caller is on the sign-in page.
/// It discloses nothing a visitor cannot learn by clicking.
pub async fn guest_available(State(state): State<Arc<LocalAuthState>>) -> Response {
    Json(GuestAvailability {
        enabled: state.guest_access,
        email: GUEST_EMAIL,
        role: boss_core::roles::AUDIT_READONLY_ROLE,
    })
    .into_response()
}

/// `POST /api/auth/guest` — mint the read-only session.
///
/// Both the identity and the role are constants: nothing the
/// caller sends influences either, because the request body of an
/// unauthenticated endpoint is not evidence of anything. Writes
/// are refused downstream by `audit-readonly`'s policy rules —
/// this handler grants a role, it does not enforce one.
///
/// `employee_id` stays `None`. A guest is not on the payroll, and
/// giving them an Employee row to satisfy a session field would
/// put a person who does not exist into the org chart, headcount
/// and directory.
pub async fn guest(State(state): State<Arc<LocalAuthState>>) -> Response {
    if !state.guest_access {
        return (
            StatusCode::NOT_FOUND,
            "guest access is not enabled on this deployment",
        )
            .into_response();
    }

    let mut sess = Session::new(GUEST_EMAIL, session::DEFAULT_TTL_SECONDS);
    sess.role = Some(boss_core::roles::AUDIT_READONLY_ROLE.to_string());

    // Counted, deliberately (§Policy & auth): an
    // unauthenticated endpoint that mints real read access gets a
    // record per mint. Constant identity — no PII rides along.
    state.audit.guest_session(GUEST_EMAIL);

    let cookie_value = sess.encode(&state.session_key);
    let set_cookie = session::set_cookie(
        session::COOKIE_NAME,
        &cookie_value,
        session::DEFAULT_TTL_SECONDS,
        "/",
    );
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&set_cookie) {
        headers.insert(header::SET_COOKIE, v);
    }

    (
        StatusCode::OK,
        headers,
        Json(MeResponse {
            email: GUEST_EMAIL.to_string(),
            employee_id: None,
            role: sess.role.clone(),
            access_tier: sess.access_tier.clone(),
        }),
    )
        .into_response()
}

/// `POST /api/auth/logout` — clear the boss_session cookie.
pub async fn logout() -> Response {
    let cookie = session::set_cookie(session::COOKIE_NAME, "", 0, "/");
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        headers.insert(header::SET_COOKIE, v);
    }
    (StatusCode::NO_CONTENT, headers).into_response()
}

/// `GET /api/auth/me` — decode the cookie + return the identity of
/// the current session. 401 if there is no cookie or the signature
/// is invalid.
///
/// This used to 401 a session with no `employee_id` and role
/// `audit-readonly` as well. That was demo mode's signature, and
/// the 401 existed to keep the sign-in form reachable: the
/// middleware minted a session for the form's own GET, so without
/// the exclusion `/me` answered 200 and the SPA redirected a
/// signed-out visitor to the home page.
///
/// Demo mode is gone, so nothing mints a session behind the
/// caller's back and the exclusion has no work left to do — but a
/// guest session has that exact signature, so leaving it in place
/// would 401 every guest and bounce them back to the page they
/// just came from. It reports the session it finds.
pub async fn me(State(state): State<Arc<LocalAuthState>>, headers: HeaderMap) -> Response {
    let session = match extract_session(&headers, &state.session_key) {
        Some(s) => s,
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };
    Json(MeResponse {
        email: session.username.clone(),
        employee_id: session.employee_id.clone(),
        role: session.role.clone(),
        access_tier: session.access_tier.clone(),
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct OnboardRequest {
    pub email: String,
    pub password: String,
}

/// `POST /api/auth/onboard` — admin-only. Creates a credential
/// row for an existing Employee email. Verified via the caller's
/// role (must be platform-admin / ceo / coo).
pub async fn onboard(
    State(state): State<Arc<LocalAuthState>>,
    headers: HeaderMap,
    Json(req): Json<OnboardRequest>,
) -> Response {
    if !is_admin(&headers, &state.session_key) {
        return (StatusCode::FORBIDDEN, "admin only").into_response();
    }
    if let Err(e) = state.store.upsert(&req.email, &req.password) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response();
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"email": req.email})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct IssueResetRequest {
    pub email: String,
}

#[derive(Serialize)]
pub struct IssueResetResponse {
    /// Where the token went. NOT the token — see `issue_reset`.
    pub sent_to: String,
    pub expires_at: DateTime<Utc>,
}

/// `POST /api/auth/issue-reset` — admin-only. Returns a one-time
/// reset token to share with the user.
pub async fn issue_reset(
    State(state): State<Arc<LocalAuthState>>,
    headers: HeaderMap,
    Json(req): Json<IssueResetRequest>,
) -> Response {
    if !is_admin(&headers, &state.session_key) {
        return (StatusCode::FORBIDDEN, "admin only").into_response();
    }
    let ttl = 60 * 60; // 1h
    match state.store.issue_reset_token(&req.email, ttl) {
        Ok(token) => {
            // Mail it; do not return it. Returning the token was
            // correct while this was admin-only and the admin was the
            // one conveying it — and it is exactly what must not
            // happen now that `/api/auth/forgot` exists, because a
            // handler that answers with a credential is one routing
            // mistake away from handing anyone anyone else's reset.
            let mail = crate::mail::reset_mail(&req.email, &token, &state.public_url);
            if let Err(e) = state.mail.send(&mail).await {
                tracing::warn!(error = %e, "reset token issued but mail failed");
                return (
                    StatusCode::BAD_GATEWAY,
                    "token issued but could not be sent; check the mail transport",
                )
                    .into_response();
            }
            Json(IssueResetResponse {
                sent_to: req.email,
                expires_at: Utc::now() + chrono::Duration::seconds(ttl),
            })
            .into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

#[derive(Deserialize)]
pub struct ForgotRequest {
    pub email: String,
}

/// `POST /api/auth/forgot` — public. Self-service password reset.
///
/// Always answers 204, whatever happened. That is the whole design:
///
/// - **Enumeration.** A public endpoint that distinguishes "no such
///   account" from "sent" is an account-discovery oracle. Anyone
///   could walk a list of addresses and learn who works here.
/// - **Rate limiting.** A public endpoint that sends mail is a
///   mailbomb aimed at a victim's inbox and at our own sending
///   reputation. Silently ignoring a repeat inside the window is
///   better than an error, which would itself distinguish a known
///   address from an unknown one.
///
/// The operator-facing signal lives in the logs, where it can be
/// specific without telling the caller anything.
pub async fn forgot(
    State(state): State<Arc<LocalAuthState>>,
    Json(req): Json<ForgotRequest>,
) -> Response {
    let email = req.email.trim().to_lowercase();
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

    {
        let mut seen = match state.forgot_seen.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let now = std::time::Instant::now();
        seen.retain(|_, t| now.duration_since(*t) < WINDOW);
        if seen.contains_key(&email) {
            tracing::info!(%email, "forgot: within the rate-limit window; ignoring");
            return StatusCode::NO_CONTENT.into_response();
        }
        seen.insert(email.clone(), now);
    }

    let ttl = 60 * 60;
    match state.store.issue_reset_token(&email, ttl) {
        Ok(token) => {
            let mail = crate::mail::reset_mail(&email, &token, &state.public_url);
            match state.mail.send(&mail).await {
                Ok(()) => {
                    tracing::info!(%email, delivered = state.mail.delivers(), "forgot: reset sent")
                }
                Err(e) => tracing::warn!(%email, error = %e, "forgot: mail failed"),
            }
        }
        // No such account. Logged, never surfaced — the caller gets
        // the same 204 either way.
        Err(e) => tracing::info!(%email, reason = %e, "forgot: no token issued"),
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct ResetRequest {
    pub email: String,
    pub token: String,
    pub password: String,
}

/// `POST /api/auth/reset` — public. User consumes the token an
/// admin shared with them, sets a new password.
pub async fn reset(
    State(state): State<Arc<LocalAuthState>>,
    Json(req): Json<ResetRequest>,
) -> Response {
    if let Err(e) = state
        .store
        .consume_reset_token(&req.email, &req.token, &req.password)
    {
        return (StatusCode::UNAUTHORIZED, format!("{e}")).into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

// --------------------------------------------------------------------
// Helpers.
// --------------------------------------------------------------------

fn extract_session(headers: &HeaderMap, key: &[u8]) -> Option<Session> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    let raw = session::find_cookie(cookie_header, session::COOKIE_NAME)?;
    Session::decode(raw, key).ok()
}

fn is_admin(headers: &HeaderMap, key: &[u8]) -> bool {
    let Some(s) = extract_session(headers, key) else {
        return false;
    };
    // `can_administer_auth`, not `has_global_read`: the same set plus
    // the narrow break-glass role, whose auth-administration lever is
    // exactly these endpoints (break-glass-is-a-key-you-hold.md Q4).
    s.role
        .as_deref()
        .map(boss_core::roles::can_administer_auth)
        .unwrap_or(false)
}

// --------------------------------------------------------------------
// Tests.
// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, CredentialStore) {
        let td = TempDir::new().unwrap();
        let path = td.path().join("credentials.toml");
        let store = CredentialStore::load(path).unwrap();
        (td, store)
    }

    /// Captures instead of sending, so a test can assert what was
    /// composed and — more importantly — how many times.
    #[derive(Default)]
    struct CapturingTransport {
        sent: std::sync::Mutex<Vec<crate::mail::OutboundMail>>,
    }

    #[async_trait::async_trait]
    impl crate::mail::MailTransport for CapturingTransport {
        async fn send(
            &self,
            mail: &crate::mail::OutboundMail,
        ) -> Result<(), crate::mail::MailError> {
            self.sent.lock().expect("lock").push(mail.clone());
            Ok(())
        }
        fn delivers(&self) -> bool {
            true
        }
    }

    fn state_with(
        transport: Arc<CapturingTransport>,
    ) -> (TempDir, Arc<LocalAuthState>, Arc<CapturingTransport>) {
        let (td, store) = temp_store();
        let st = Arc::new(LocalAuthState {
            store,
            session_key: vec![7u8; 32],
            http: reqwest::Client::new(),
            audit: crate::audit::AuthAudit::disabled(),
            guest_access: false,
            oidc: None,
            mail: transport.clone(),
            public_url: "https://boss.test".into(),
            forgot_seen: Default::default(),
        });
        (td, st, transport)
    }

    /// The reset token must never come back in a response body. It was
    /// returned while this was admin-only and the admin conveyed it by
    /// hand; with a public `forgot` endpoint in the same file, a
    /// handler that answers with a credential is one routing mistake
    /// from handing anyone anyone else's reset.
    #[tokio::test]
    async fn issue_reset_mails_the_token_and_never_returns_it() {
        let (_td, st, cap) = state_with(Arc::new(CapturingTransport::default()));
        st.store.upsert("op@example.com", "pw").expect("seed");

        let resp = forgot(
            State(st.clone()),
            Json(ForgotRequest {
                email: "op@example.com".into(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let sent = cap.sent.lock().expect("lock");
        assert_eq!(sent.len(), 1, "exactly one mail");
        assert_eq!(sent[0].to, "op@example.com");
        assert!(
            sent[0].body.contains("https://boss.test/login?reset="),
            "the mail must carry a usable link: {}",
            sent[0].body
        );
    }

    /// A public endpoint that answers differently for a known and an
    /// unknown address is an account-discovery oracle. Same status,
    /// same empty body, either way — and no mail for the unknown one.
    #[tokio::test]
    async fn forgot_does_not_reveal_whether_an_account_exists() {
        let (_td, st, cap) = state_with(Arc::new(CapturingTransport::default()));
        st.store.upsert("real@example.com", "pw").expect("seed");

        let known = forgot(
            State(st.clone()),
            Json(ForgotRequest {
                email: "real@example.com".into(),
            }),
        )
        .await;
        let unknown = forgot(
            State(st.clone()),
            Json(ForgotRequest {
                email: "ghost@example.com".into(),
            }),
        )
        .await;

        assert_eq!(known.status(), unknown.status());
        assert_eq!(known.status(), StatusCode::NO_CONTENT);
        let sent = cap.sent.lock().expect("lock");
        assert_eq!(sent.len(), 1, "only the real account gets mail");
        assert_eq!(sent[0].to, "real@example.com");
    }

    /// A public endpoint that sends mail is a mailbomb aimed at a
    /// victim's inbox and at our sending reputation. The repeat is
    /// ignored SILENTLY — an error would itself distinguish a known
    /// address from an unknown one, undoing the test above.
    #[tokio::test]
    async fn forgot_rate_limits_without_saying_so() {
        let (_td, st, cap) = state_with(Arc::new(CapturingTransport::default()));
        st.store.upsert("op@example.com", "pw").expect("seed");

        let mut statuses = vec![];
        for _ in 0..4 {
            let r = forgot(
                State(st.clone()),
                Json(ForgotRequest {
                    email: "op@example.com".into(),
                }),
            )
            .await;
            statuses.push(r.status());
        }
        assert!(
            statuses.iter().all(|s| *s == StatusCode::NO_CONTENT),
            "every call answers the same: {statuses:?}"
        );
        assert_eq!(
            cap.sent.lock().expect("lock").len(),
            1,
            "four requests, one mail"
        );
    }

    fn guest_state(enabled: bool) -> (TempDir, Arc<LocalAuthState>) {
        let (td, store) = temp_store();
        let st = Arc::new(LocalAuthState {
            store,
            session_key: vec![7u8; 32],
            http: reqwest::Client::new(),
            audit: crate::audit::AuthAudit::disabled(),
            guest_access: enabled,
            oidc: None,
            mail: Arc::new(crate::mail::LogTransport),
            public_url: "https://boss.test".into(),
            forgot_seen: Default::default(),
        });
        (td, st)
    }

    /// Architecture decisions, §Policy & auth: a bad local password is an
    /// authentication decision, and today it leaves no record at
    /// all. The denied event is the fix; `bad_credentials` is its
    /// closed reason.
    #[tokio::test]
    async fn a_bad_password_lands_a_denied_event() {
        let cap = std::sync::Arc::new(crate::audit::testing::Captured::default());
        let (_td, store) = temp_store();
        // Runtime-generated: no credential-shaped literal in the test
        // binary (CodeQL flags them), and the test can't quietly grow
        // a dependency on a magic value.
        let good_pw = format!("pw-{}", {
            use rand::RngExt;
            rand::rng().random::<u64>()
        });
        store.upsert("op@example.com", &good_pw).expect("seed");
        let st = Arc::new(LocalAuthState {
            store,
            session_key: vec![7u8; 32],
            http: reqwest::Client::new(),
            audit: crate::audit::AuthAudit::spawn(cap.clone()),
            guest_access: false,
            oidc: None,
            mail: Arc::new(crate::mail::LogTransport),
            public_url: "https://boss.test".into(),
            forgot_seen: Default::default(),
        });

        let resp = login(
            State(st.clone()),
            Json(LoginRequest {
                email: "op@example.com".into(),
                password: format!("{good_pw}-wrong"),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let events = crate::audit::testing::drain(&cap, 1).await;
        assert_eq!(events.len(), 1, "one denied event");
        assert_eq!(events[0].kind, "auth.login.denied");
        assert_eq!(events[0].payload["reason"], "bad_credentials");
        assert_eq!(events[0].payload["method"], "password");
        assert_eq!(events[0].payload["email_claimed"], "op@example.com");
        assert!(
            events[0].payload.get("employee_id").is_none(),
            "denied asserts no employee"
        );
    }

    /// Q2: the guest mint is an unauthenticated capability; counting
    /// it is the minimum honest record.
    #[tokio::test]
    async fn a_guest_mint_lands_its_own_event_kind() {
        let cap = std::sync::Arc::new(crate::audit::testing::Captured::default());
        let (_td, store) = temp_store();
        let st = Arc::new(LocalAuthState {
            store,
            session_key: vec![7u8; 32],
            http: reqwest::Client::new(),
            audit: crate::audit::AuthAudit::spawn(cap.clone()),
            guest_access: true,
            oidc: None,
            mail: Arc::new(crate::mail::LogTransport),
            public_url: "https://boss.test".into(),
            forgot_seen: Default::default(),
        });

        let resp = guest(State(st.clone())).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let events = crate::audit::testing::drain(&cap, 1).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "auth.session.guest");
        assert_eq!(events[0].payload["email"], GUEST_EMAIL);
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    /// Pull the cookie value out of a `Set-Cookie` header: the first
    /// `;`-separated segment is `name=value`, the rest are attributes.
    fn cookie_value(resp: &Response) -> String {
        let raw = resp
            .headers()
            .get(header::SET_COOKIE)
            .expect("Set-Cookie present")
            .to_str()
            .expect("ascii cookie");
        let first = raw.split(';').next().expect("a first segment");
        let (name, value) = first.split_once('=').expect("name=value");
        assert_eq!(name, session::COOKIE_NAME);
        value.to_string()
    }

    /// Nothing the caller sends decides who a guest is, so the only
    /// thing to assert is that the constants land on the session.
    #[tokio::test]
    async fn a_guest_session_is_audit_readonly_and_not_an_employee() {
        let (_td, st) = guest_state(true);
        let resp = guest(State(st.clone())).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let sess = Session::decode(&cookie_value(&resp), &st.session_key).expect("decodes");
        assert_eq!(sess.username, GUEST_EMAIL);
        assert_eq!(sess.role.as_deref(), Some("audit-readonly"));
        assert!(
            sess.employee_id.is_none(),
            "a guest must not carry an Employee identity — that would put a \
             person who does not exist into the org chart"
        );
    }

    /// A tenant running BOSS on their own company's data has not asked
    /// to hand out a session that reads every projection.
    #[tokio::test]
    async fn guest_access_is_refused_unless_the_deployment_offers_it() {
        let (_td, st) = guest_state(false);
        let resp = guest(State(st.clone())).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(
            resp.headers().get(header::SET_COOKIE).is_none(),
            "a refused mint must not set a cookie"
        );

        let avail = body_json(guest_available(State(st)).await).await;
        assert_eq!(avail["enabled"], serde_json::json!(false));
    }

    /// The sign-in page renders its button from this, so the answer
    /// has to track the deployment rather than be assumed.
    #[tokio::test]
    async fn availability_reports_the_identity_it_would_mint() {
        let (_td, st) = guest_state(true);
        let avail = body_json(guest_available(State(st)).await).await;
        assert_eq!(avail["enabled"], serde_json::json!(true));
        assert_eq!(avail["email"], serde_json::json!(GUEST_EMAIL));
        assert_eq!(avail["role"], serde_json::json!("audit-readonly"));
    }

    /// Regression. `me` used to 401 a session with no `employee_id`
    /// and role `audit-readonly` — demo mode's signature, and now
    /// exactly a guest's. With that branch still in place a guest
    /// signs in, the SPA asks who it is, gets a 401, and returns them
    /// to the sign-in page they just left.
    #[tokio::test]
    async fn me_answers_for_a_guest_session() {
        let (_td, st) = guest_state(true);
        let minted = guest(State(st.clone())).await;

        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!(
                "{}={}",
                session::COOKIE_NAME,
                cookie_value(&minted)
            ))
            .unwrap(),
        );

        let resp = me(State(st), headers).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let me_body = body_json(resp).await;
        assert_eq!(me_body["email"], serde_json::json!(GUEST_EMAIL));
        assert!(
            me_body["employee_id"].is_null(),
            "the SPA distinguishes a guest from a real login by the absence \
             of an employee_id, so it has to be reported, not omitted"
        );
    }

    #[test]
    fn upsert_then_verify_round_trip() {
        let (_td, store) = temp_store();
        store
            .upsert("op@example.com", "correct horse battery")
            .unwrap();
        assert!(
            store
                .verify("op@example.com", "correct horse battery")
                .is_ok()
        );
        assert!(store.verify("op@example.com", "wrong").is_err());
        assert!(store.verify("missing@example.com", "anything").is_err());
    }

    #[test]
    fn email_lookup_is_case_insensitive() {
        let (_td, store) = temp_store();
        store.upsert("Alice@Example.com", "pw").unwrap();
        assert!(store.verify("alice@example.com", "pw").is_ok());
        assert!(store.verify("ALICE@EXAMPLE.COM", "pw").is_ok());
    }

    #[test]
    fn reload_persists_across_instances() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("credentials.toml");
        let s1 = CredentialStore::load(&path).unwrap();
        s1.upsert("op@example.com", "pw").unwrap();
        let s2 = CredentialStore::load(&path).unwrap();
        assert!(s2.verify("op@example.com", "pw").is_ok());
    }

    #[test]
    fn remove_drops_the_credential() {
        let (_td, store) = temp_store();
        store.upsert("a@b.com", "pw").unwrap();
        assert!(store.contains("a@b.com"));
        assert!(store.remove("a@b.com").unwrap());
        assert!(!store.contains("a@b.com"));
        assert!(store.verify("a@b.com", "pw").is_err());
    }

    #[test]
    fn reset_token_round_trip() {
        let (_td, store) = temp_store();
        store.upsert("op@example.com", "old-pw").unwrap();
        let token = store.issue_reset_token("op@example.com", 60).unwrap();
        // Old password still works until the reset is consumed.
        assert!(store.verify("op@example.com", "old-pw").is_ok());
        // Wrong token rejected, doesn't burn the token.
        assert!(
            store
                .consume_reset_token("op@example.com", "wrong", "x")
                .is_err()
        );
        // Correct consumption rotates the password.
        store
            .consume_reset_token("op@example.com", &token, "new-pw")
            .unwrap();
        assert!(store.verify("op@example.com", "new-pw").is_ok());
        assert!(store.verify("op@example.com", "old-pw").is_err());
        // Token can't be reused.
        assert!(
            store
                .consume_reset_token("op@example.com", &token, "yet-pw")
                .is_err()
        );
    }

    #[test]
    fn reset_token_for_unknown_email_fails_cleanly() {
        let (_td, store) = temp_store();
        assert!(store.issue_reset_token("ghost@example.com", 60).is_err());
        assert!(
            store
                .consume_reset_token("ghost@example.com", "x", "y")
                .is_err()
        );
    }
}
