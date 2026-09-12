//! WebAuthn ceremony storage — credentials and single-use challenges.
//!
//! The presence design (docs/design/presence.md; BOSS-native call
//! accepted on packet 7218c3f1) splits the ceremony across two
//! services: the GATEWAY runs the cryptographic ceremony against the
//! browser, and this module is the storage it leans on — the
//! credential registry (who owns which authenticator) and the
//! challenge ledger (what was minted, for which step content, and
//! whether it has been spent).
//!
//! These endpoints are internal machinery: the gateway's ceremony is
//! the only legitimate caller. They cannot rely on that being true —
//! the gateway also proxies `/api/people/{*rest}` for the SPA, so any
//! session could reach these paths — and a credential row planted for
//! someone else is an account takeover. So every handler requires a
//! `platform-admin` actor: the gateway's server-side ceremony calls
//! identify as `automation:gateway` with that role, operator tooling
//! (boss-api) already carries it, and every ordinary proxied session
//! is refused. Humans never call these directly; they go through the
//! gateway's `/api/auth/passkey/*` ceremony, which enforces
//! session-to-employee binding before it ever gets here.
//!
//! Bytes (credential ids, public keys, challenges) travel as
//! base64url-no-pad strings — the same alphabet the browser's
//! WebAuthn API speaks — and are stored as BYTEA.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::Row;

use boss_clock_client::{ClockClient, now_from};
use boss_policy_client::CurrentUser;

#[derive(Clone)]
pub struct WebauthnState {
    pub pool: Arc<PgPool>,
    pub clock: Arc<dyn ClockClient>,
}

/// Platform machinery only — see the module doc for why this cannot
/// be open to ordinary sessions even behind the gateway.
/// Small error value for helper Results (clippy::result_large_err —
/// Response is a big payload and refusals are cold paths); converted
/// at the handler boundary.
type ErrResp = (StatusCode, &'static str);

fn operator_gate(user: &boss_policy::User) -> Result<(), ErrResp> {
    if user.role == "platform-admin" {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "webauthn storage is platform machinery — enrolment and assertions go \
             through the gateway's /api/auth/passkey ceremony",
        ))
    }
}

pub fn webauthn_router(pool: PgPool, clock: Arc<dyn ClockClient>) -> Router {
    let state = WebauthnState {
        pool: Arc::new(pool),
        clock,
    };
    Router::new()
        .route(
            "/api/people/{id}/webauthn-credentials",
            get(list_credentials).post(register_credential),
        )
        .route(
            "/api/people/{id}/webauthn-credentials/{credential_id}",
            delete(remove_credential),
        )
        .route(
            "/api/people/webauthn-credentials/used",
            post(record_credential_use),
        )
        .route("/api/people/presence-challenges", post(mint_challenge))
        .route(
            "/api/people/presence-challenges/{id}/consume",
            post(consume_challenge),
        )
        .with_state(state)
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(field: &str, s: &str) -> Result<Vec<u8>, (StatusCode, String)> {
    URL_SAFE_NO_PAD.decode(s).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            format!("{field} is not base64url-no-pad"),
        )
    })
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RegisterCredentialBody {
    credential_id: String,
    public_key: String,
    #[serde(default = "default_label")]
    label: String,
    #[serde(default = "default_tier")]
    access_tier: String,
}

fn default_label() -> String {
    "default".into()
}
fn default_tier() -> String {
    "user".into()
}

#[derive(Serialize)]
struct CredentialOut {
    credential_id: String,
    public_key: String,
    sign_count: i32,
    label: String,
    access_tier: String,
    registered_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

async fn list_credentials(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Path(employee_id): Path<String>,
) -> Response {
    if let Err(r) = operator_gate(&user) {
        return r.into_response();
    }
    let rows = sqlx::query(
        "SELECT credential_id, public_key, sign_count, label, access_tier,
                registered_at, last_used_at
           FROM webauthn_credentials
          WHERE employee_id = $1
          ORDER BY registered_at",
    )
    .bind(&employee_id)
    .fetch_all(state.pool.as_ref())
    .await;
    match rows {
        Ok(rows) => {
            let out: Vec<CredentialOut> = rows
                .iter()
                .map(|r| CredentialOut {
                    credential_id: b64(&r.get::<Vec<u8>, _>("credential_id")),
                    public_key: b64(&r.get::<Vec<u8>, _>("public_key")),
                    sign_count: r.get("sign_count"),
                    label: r.get("label"),
                    access_tier: r.get("access_tier"),
                    registered_at: r.get("registered_at"),
                    last_used_at: r.get("last_used_at"),
                })
                .collect();
            Json(out).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn register_credential(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Path(employee_id): Path<String>,
    Json(body): Json<RegisterCredentialBody>,
) -> Response {
    if let Err(r) = operator_gate(&user) {
        return r.into_response();
    }
    if !["operator", "user"].contains(&body.access_tier.as_str()) {
        return (StatusCode::BAD_REQUEST, "access_tier must be operator|user").into_response();
    }
    let cred_id = match unb64("credential_id", &body.credential_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let pub_key = match unb64("public_key", &body.public_key) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let now = now_from(&state.clock).await;
    let res = sqlx::query(
        "INSERT INTO webauthn_credentials
           (employee_id, credential_id, public_key, label, access_tier, registered_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&employee_id)
    .bind(&cred_id)
    .bind(&pub_key)
    .bind(&body.label)
    .bind(&body.access_tier)
    .bind(now)
    .execute(state.pool.as_ref())
    .await;
    match res {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => (
            StatusCode::CONFLICT,
            "credential_id already registered — a credential binds to one authenticator forever",
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct CredentialUsedBody {
    credential_id: String,
    sign_count: i32,
}

/// `DELETE /api/people/{id}/webauthn-credentials/{credential_id}` —
/// a lost or retired authenticator can go; the LAST one stays.
///
/// David, feedback 16414d99: "Important so users can add a backup key,
/// but don't let them delete all their keys." A person with no passkey
/// cannot pass a presence-gated step, so the surface that let them
/// remove the last one would be the one that locked them out. The
/// refusal is a 409 in the user's terms — add a backup, then remove —
/// and a credential that is not this employee's is a 404, never a
/// silent 204 that reads as "removed". One statement does the count
/// and the delete together, so two removals racing cannot both see
/// "two keys" and leave none.
async fn remove_credential(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Path((employee_id, credential_id)): Path<(String, String)>,
) -> Response {
    if let Err(r) = operator_gate(&user) {
        return r.into_response();
    }
    let cred = match unb64("credential_id", &credential_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let deleted = sqlx::query(
        "DELETE FROM webauthn_credentials
          WHERE employee_id = $1 AND credential_id = $2
            AND (SELECT count(*) FROM webauthn_credentials WHERE employee_id = $1) > 1",
    )
    .bind(&employee_id)
    .bind(&cred)
    .execute(state.pool.as_ref())
    .await;
    match deleted {
        Ok(r) if r.rows_affected() == 1 => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM webauthn_credentials
                                WHERE employee_id = $1 AND credential_id = $2)",
            )
            .bind(&employee_id)
            .bind(&cred)
            .fetch_one(state.pool.as_ref())
            .await
            .unwrap_or(false);
            if exists {
                (
                    StatusCode::CONFLICT,
                    "this is the last passkey on the account and it stays — add a backup \
                     key first, then remove this one",
                )
                    .into_response()
            } else {
                (StatusCode::NOT_FOUND, "no such passkey on this account").into_response()
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn record_credential_use(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CredentialUsedBody>,
) -> Response {
    if let Err(r) = operator_gate(&user) {
        return r.into_response();
    }
    let cred_id = match unb64("credential_id", &body.credential_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let now = now_from(&state.clock).await;
    let res = sqlx::query(
        "UPDATE webauthn_credentials
            SET sign_count = $2, last_used_at = $3
          WHERE credential_id = $1",
    )
    .bind(&cred_id)
    .bind(body.sign_count)
    .bind(now)
    .execute(state.pool.as_ref())
    .await;
    match res {
        Ok(r) if r.rows_affected() == 0 => {
            (StatusCode::NOT_FOUND, "unknown credential").into_response()
        }
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Challenges
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MintChallengeBody {
    id: String,
    employee_id: String,
    challenge: String,
    flow: String,
    #[serde(default)]
    step_id: Option<String>,
    #[serde(default)]
    shape_hash: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    /// Seconds until expiry; default 300 (the table's original 5 min).
    #[serde(default)]
    ttl_seconds: Option<i64>,
}

#[derive(Serialize)]
struct ChallengeOut {
    id: String,
    employee_id: String,
    challenge: String,
    flow: String,
    step_id: Option<String>,
    shape_hash: Option<String>,
    nonce: Option<String>,
}

async fn mint_challenge(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<MintChallengeBody>,
) -> Response {
    if let Err(r) = operator_gate(&user) {
        return r.into_response();
    }
    if !["register", "authenticate", "presence"].contains(&body.flow.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            "flow must be register|authenticate|presence",
        )
            .into_response();
    }
    let challenge = match unb64("challenge", &body.challenge) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let now = now_from(&state.clock).await;
    let expires = now + chrono::Duration::seconds(body.ttl_seconds.unwrap_or(300));
    let res = sqlx::query(
        "INSERT INTO webauthn_challenges
           (id, employee_id, challenge, flow, step_id, shape_hash, nonce,
            created_at, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&body.id)
    .bind(&body.employee_id)
    .bind(&challenge)
    .bind(&body.flow)
    .bind(&body.step_id)
    .bind(&body.shape_hash)
    .bind(&body.nonce)
    .bind(now)
    .bind(expires)
    .execute(state.pool.as_ref())
    .await;
    match res {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            (StatusCode::CONFLICT, "challenge id already minted").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Atomic single-use consumption: the UPDATE claims the row only if it
/// is unspent and unexpired, so two racing verifications cannot both
/// succeed. A spent or expired row answers 410 (it existed; it is no
/// longer redeemable), an unknown id 404 — the caller can tell "replay
/// or too slow" from "never minted".
async fn consume_challenge(
    State(state): State<WebauthnState>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = operator_gate(&user) {
        return r.into_response();
    }
    let now = now_from(&state.clock).await;
    let row = sqlx::query(
        "UPDATE webauthn_challenges
            SET used_at = $2
          WHERE id = $1 AND used_at IS NULL AND expires_at > $2
      RETURNING employee_id, challenge, flow, step_id, shape_hash, nonce",
    )
    .bind(&id)
    .bind(now)
    .fetch_optional(state.pool.as_ref())
    .await;
    match row {
        Ok(Some(r)) => Json(ChallengeOut {
            id,
            employee_id: r
                .get::<Option<String>, _>("employee_id")
                .unwrap_or_default(),
            challenge: b64(&r.get::<Vec<u8>, _>("challenge")),
            flow: r.get("flow"),
            step_id: r.get("step_id"),
            shape_hash: r.get("shape_hash"),
            nonce: r.get("nonce"),
        })
        .into_response(),
        Ok(None) => {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM webauthn_challenges WHERE id = $1",
            )
            .bind(&id)
            .fetch_one(state.pool.as_ref())
            .await
            .unwrap_or(0);
            if exists > 0 {
                (StatusCode::GONE, "challenge spent or expired").into_response()
            } else {
                (StatusCode::NOT_FOUND, "unknown challenge").into_response()
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
