//! Break-glass — the emergency door is a hardware key you hold.
//!
//! Design: docs/design/break-glass-is-a-key-you-hold.md, Q1-Q6
//! resolved on packet e9703d8f (2026-09-03). The gateway is the
//! break-glass *verifier*: a minimal WebAuthn relying party whose
//! whole credential store is public material in an in-tree ConfigMap
//! (`infra/cluster/manifests/boss-break-glass-credentials.yaml`).
//! A verified assertion mints a `boss_session` carrying the NARROW
//! `break-glass` role (Q4) — deploy rollback, merge approval, auth
//! administration; never platform-admin.
//!
//! What each resolved question shaped here:
//!
//! - **Q1 — any attested roaming authenticator.** Registration asks
//!   the browser for `attestation: "direct"` and the finish handler
//!   refuses any credential whose verified attestation is not a
//!   certificate-chain form (`Basic` / `AttCa` / `AnonCa`). No AAGUID
//!   allowlist. Honesty about the limit: webauthn-rs verifies the
//!   attestation *statement* (signature over authData + clientData,
//!   per format), but without a curated FIDO-MDS root store the chain
//!   is not pinned to known vendor roots — a sophisticated attacker
//!   could self-sign a plausible chain. What the check DOES exclude
//!   is every real synced-passkey provider (iCloud Keychain, Google
//!   Password Manager, browser-software keys), which attest `none`,
//!   and any credential flagged backup-eligible (the BE flag every
//!   synced passkey sets). Device-bound is enforced as far as the
//!   crate allows; the residual gap is documented, not papered over.
//! - **Q2 — both keys enrolled upfront.** Two records, labels
//!   `primary` and `backup`, either sufficient to assert.
//! - **Q3 — browser ceremony only.** The self-contained ceremony page
//!   at `/break-glass` is served from a constant in this binary — no
//!   SPA bundle, no upstream service, nothing that can be down when
//!   the gateway itself is up. Terminal cases are covered by the PIV
//!   and sk-SSH applets on the same physical key, outside this module.
//! - **Q5 — credential records in an in-tree ConfigMap.** Public keys
//!   are public. Enrollment does NOT write the ConfigMap: an
//!   imperative patch to a converge-managed object is a change with
//!   an expiry (the converge loop reapplies the in-tree manifest and
//!   would silently erase enrolled keys). Instead the finish handler
//!   EMITS the complete record — response body and log line — and the
//!   operator commits it to the manifest; the kubelet propagates the
//!   ConfigMap update into the mount without a restart, and this
//!   module re-reads the directory on every ceremony. The tree stays
//!   the source of truth, which is the entire point of Q5.
//! - **Q6 — one train of soak.** `credentials.toml` and
//!   `POST /api/auth/login` are untouched; this RP lands alongside
//!   them. The PVC retirement is the follow-up car after prod proof.
//!
//! Enrollment gating: ONLY an already-authenticated break-glass
//! session (key rotation, adding the backup later) or the bootstrap
//! window — a `BOSS_BREAK_GLASS_ENROLL_TOKEN` match while ZERO
//! credentials are enrolled. The window closes by itself the moment
//! the first record lands in the ConfigMap, and does not exist at all
//! when the env var is unset.
//!
//! Sign counters, honestly: the durable counter is the `sign_count`
//! committed in the ConfigMap; this process keeps a monotonic
//! in-memory high-water mark on top of it, and webauthn-rs rejects
//! any assertion whose counter does not advance past the value the
//! credential carried at challenge time. Within a process lifetime
//! clone detection is therefore real; across a gateway restart the
//! floor falls back to the committed value, so a clone used only
//! between restarts could evade it until the operator refreshes
//! `sign_count` in the manifest (each successful assertion logs the
//! new value for exactly that purpose).
//!
//! Pending ceremony state lives in-process (single-use, five-minute
//! TTL). Deliberate: the break-glass verifier must not depend on any
//! other service being alive — the design's whole table of levers is
//! about which verifiers survive which outages — and the gateway
//! deploys single-replica (`strategy: Recreate`), so there is no
//! second instance for a ceremony to land on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use webauthn_rs::prelude::{
    AttestationMetadata, AuthenticatorAttachment, PublicKeyCredential, RegisterPublicKeyCredential,
    SecurityKey, SecurityKeyAuthentication, SecurityKeyRegistration, Url, Uuid, Webauthn,
    WebauthnBuilder,
};
use webauthn_rs_core::proto::AttestationConveyancePreference;

use crate::session::{self, Session};

/// The break-glass session's actor name. A constant, like
/// [`crate::local_auth::GUEST_EMAIL`]: nothing the caller sends
/// decides who a break-glass session is, and an actor in the audit
/// log should be a name you can look up — the credential manifest
/// documents who holds the keys.
pub const BREAK_GLASS_ACTOR: &str = "break-glass-operator";

/// One hour, not the normal session's 24: an emergency session is for
/// the emergency, and renewing it costs one touch of the key.
pub const BREAK_GLASS_TTL_SECONDS: u64 = 60 * 60;

/// How long a begun ceremony may take before its challenge expires.
/// Generous because a PIN + touch on a safe-stored backup key is not
/// a two-second interaction; still bounded because a pending
/// challenge is server state.
const CEREMONY_TTL: Duration = Duration::from_secs(300);

// --------------------------------------------------------------------
// The credential record — public material only (Q5).
// --------------------------------------------------------------------

/// Which physical key a record belongs to (Q2: both enrolled
/// upfront — primary carried, backup in a safe).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialLabel {
    Primary,
    Backup,
}

impl CredentialLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Backup => "backup",
        }
    }
}

/// One enrolled hardware credential. Every field is public by
/// construction — a leaked record is a leaked public key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BreakGlassCredential {
    /// base64url (no padding) of the raw credential id.
    pub credential_id: String,
    /// base64url (no padding) of the serialized webauthn-rs
    /// [`SecurityKey`] — public key, verified attestation, flags.
    /// Same encoding the presence-passkey rows use.
    pub public_key: String,
    /// The committed sign-counter floor. Refresh it from the value
    /// each successful assertion logs; see the module docs for what
    /// the floor does and does not guarantee across restarts.
    pub sign_count: u32,
    /// The authenticator model's AAGUID as reported by its verified
    /// attestation. All-zero for FIDO-U2F-era keys, whose attestation
    /// format predates AAGUID conveyance. Recorded for the operator's
    /// inventory — Q1 decided against gating on it.
    pub aaguid: String,
    pub enrolled_at: DateTime<Utc>,
    pub label: CredentialLabel,
}

/// Load every credential record from `dir` (the mounted ConfigMap:
/// one `<label>.json` file per key). A missing directory is an empty
/// store — that is the bootstrap state, not an error.
///
/// A malformed record is skipped LOUDLY (error log naming the file)
/// rather than failing the whole load: refusing to run the ceremony
/// because the backup record has a typo, while the primary record is
/// intact, would brick the one door this module exists to keep open.
pub fn load_store(dir: &Path) -> anyhow::Result<Vec<BreakGlassCredential>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // ConfigMap mounts carry `..data` symlink machinery; skip
        // anything hidden and anything that is not a .json record.
        if name.starts_with('.') || !name.ends_with(".json") || !path.is_file() {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|raw| {
                serde_json::from_str::<BreakGlassCredential>(&raw).map_err(anyhow::Error::from)
            });
        match parsed {
            Ok(rec) => out.push(rec),
            Err(e) => {
                tracing::error!(
                    file = %path.display(),
                    error = %e,
                    "break-glass credential record unreadable — SKIPPED; the \
                     emergency door is narrower than the manifest intends"
                );
            }
        }
    }
    Ok(out)
}

// --------------------------------------------------------------------
// Pure ceremony rules — the testable core.
// --------------------------------------------------------------------

/// Who authorized an enrollment.
#[derive(Debug, PartialEq, Eq)]
pub enum EnrollAuthz {
    /// An already-authenticated break-glass session (key rotation,
    /// adding the backup after the first key is live).
    BreakGlassSession,
    /// The first-enrollment window: bootstrap token matched and the
    /// store holds zero credentials.
    BootstrapToken,
}

/// The enrollment gate. Pure so the window logic is pinned by tests:
/// ONLY a break-glass session or the bootstrap token inside the
/// zero-credentials window may enroll. Token comparison is
/// constant-time.
pub fn enroll_gate(
    session_role: Option<&str>,
    presented_token: Option<&str>,
    configured_token: Option<&str>,
    enrolled_count: usize,
) -> Result<EnrollAuthz, &'static str> {
    if session_role == Some(boss_core::roles::BREAK_GLASS_ROLE) {
        return Ok(EnrollAuthz::BreakGlassSession);
    }
    let Some(configured) = configured_token else {
        return Err(
            "enrollment closed: no bootstrap token is configured and the caller \
             holds no break-glass session",
        );
    };
    if enrolled_count > 0 {
        return Err(
            "enrollment closed: credentials are already enrolled — assert with an \
             enrolled key, then enroll through that session",
        );
    }
    let Some(presented) = presented_token else {
        return Err("enrollment refused: bootstrap token required");
    };
    let matches = presented.as_bytes().ct_eq(configured.as_bytes());
    if matches.unwrap_u8() == 1 {
        Ok(EnrollAuthz::BootstrapToken)
    } else {
        Err("enrollment refused: bootstrap token mismatch")
    }
}

/// Q1's enforcement, applied to the serialized [`SecurityKey`] the
/// finish handler produced (the same serde seam passkey.rs uses).
/// Fail-closed: anything unrecognized is refused.
///
/// Accepted: a verified certificate-chain attestation (`Basic`,
/// `AttCa`, `AnonCa`) on a credential that is NOT backup-eligible.
/// Refused: `none` (every synced/software passkey), `Self_`
/// (surrogate self-signature — proves possession of the credential
/// key, not of vendor hardware), `Uncertain`, `ECDAA`, and any
/// backup-eligible credential regardless of its attestation.
pub fn require_attested_hardware(sk: &Value) -> Result<(), String> {
    let cred = &sk["cred"];
    if !cred.is_object() {
        return Err("credential serialization unrecognized — refusing".into());
    }
    if cred["backup_eligible"] != Value::Bool(false) {
        return Err(
            "credential is backup-eligible: the private key can leave the \
             hardware, so this is a synced passkey, not a device-bound key"
                .into(),
        );
    }
    let data = &cred["attestation"]["data"];
    let chain_form = data
        .as_object()
        .and_then(|o| (o.len() == 1).then(|| o.keys().next().cloned()).flatten())
        .filter(|k| matches!(k.as_str(), "Basic" | "AttCa" | "AnonCa"));
    match chain_form {
        Some(_) => Ok(()),
        None => Err(format!(
            "attestation required: this authenticator provided {} — a synced or \
             software passkey cannot hold the break-glass key",
            match data {
                Value::String(s) => format!("'{s}'"),
                other => other.to_string(),
            }
        )),
    }
}

/// The counter floor a credential carries into an authentication:
/// the committed manifest value, the counter inside the serialized
/// credential, and this process's high-water mark — whichever is
/// highest. webauthn-rs then refuses any assertion that does not
/// advance past it.
pub fn effective_counter(record: u32, serialized: u32, seen_this_process: Option<u32>) -> u32 {
    record.max(serialized).max(seen_this_process.unwrap_or(0))
}

/// The WebAuthn clone-detection rule, stated once: when either side
/// has a nonzero counter, a reported counter that fails to advance
/// signals a possible cloned key. (Counter-less authenticators
/// report zero forever and carry no signal.) webauthn-rs enforces
/// this inside `finish_securitykey_authentication`; this function is
/// the module's statement of the contract, pinned by tests.
pub fn counter_regressed(stored: u32, reported: u32) -> bool {
    (stored > 0 || reported > 0) && reported <= stored
}

/// Mint the narrow break-glass session (Q4). No employee id — the
/// authority is the role, and resolving an employee would make the
/// emergency door depend on boss-people being up, which is exactly
/// the dependency the design forbids the verifier to have.
pub fn mint_session(session_key: &[u8]) -> (String, Session) {
    let mut sess = Session::new(BREAK_GLASS_ACTOR, BREAK_GLASS_TTL_SECONDS);
    sess.role = Some(boss_core::roles::BREAK_GLASS_ROLE.to_string());
    let cookie_value = sess.encode(session_key);
    let set_cookie = session::set_cookie(
        session::COOKIE_NAME,
        &cookie_value,
        BREAK_GLASS_TTL_SECONDS,
        "/",
    );
    (set_cookie, sess)
}

// --------------------------------------------------------------------
// Runtime state.
// --------------------------------------------------------------------

struct Pending<T> {
    state: T,
    expires: Instant,
}

pub struct BreakGlassState {
    pub session_key: Vec<u8>,
    pub webauthn: Webauthn,
    /// The mounted ConfigMap directory. Read on every ceremony so a
    /// committed record propagates without a restart.
    pub dir: PathBuf,
    /// The bootstrap-enrollment token, if this deployment has one
    /// configured. None → the bootstrap window does not exist.
    pub enroll_token: Option<String>,
    pub audit: crate::audit::AuthAudit,
    pending_reg: Mutex<HashMap<String, Pending<SecurityKeyRegistration>>>,
    pending_auth: Mutex<HashMap<String, Pending<SecurityKeyAuthentication>>>,
    /// Per-credential sign-counter high-water marks for this process
    /// lifetime, keyed by base64url credential id.
    counters: Mutex<HashMap<String, u32>>,
}

impl BreakGlassState {
    /// rp_id / origin derive from BOSS_PUBLIC_URL, exactly like the
    /// presence-passkey ceremony — one host, one RP identity.
    pub fn from_env(session_key: Vec<u8>, audit: crate::audit::AuthAudit) -> anyhow::Result<Self> {
        let public_url =
            std::env::var("BOSS_PUBLIC_URL").unwrap_or_else(|_| "http://localhost:8000".into());
        let origin = Url::parse(&public_url)?;
        let rp_id = origin
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("BOSS_PUBLIC_URL has no host"))?
            .to_string();
        let webauthn = WebauthnBuilder::new(&rp_id, &origin)?
            .rp_name("BOSS break-glass")
            .build()?;
        Ok(Self {
            session_key,
            webauthn,
            dir: std::env::var("BOSS_BREAK_GLASS_DIR")
                .unwrap_or_else(|_| "/etc/boss/break-glass".into())
                .into(),
            enroll_token: std::env::var("BOSS_BREAK_GLASS_ENROLL_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
            audit,
            pending_reg: Mutex::new(HashMap::new()),
            pending_auth: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
        })
    }

    fn session_role(&self, headers: &HeaderMap) -> Option<String> {
        let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
        let raw = session::find_cookie(cookie_header, session::COOKIE_NAME)?;
        Session::decode(raw, &self.session_key).ok()?.role
    }

    fn store(&self) -> Result<Vec<BreakGlassCredential>, ErrResp> {
        load_store(&self.dir).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("break-glass credential store unreadable: {e}"),
            )
        })
    }

    /// The store's records as live [`SecurityKey`]s, counters bumped
    /// to the effective floor. A record whose public_key does not
    /// decode is skipped loudly, same contract as [`load_store`].
    fn security_keys(&self, records: &[BreakGlassCredential]) -> Vec<SecurityKey> {
        let counters = lock_recover(&self.counters);
        records
            .iter()
            .filter_map(|rec| {
                let decoded = URL_SAFE_NO_PAD
                    .decode(&rec.public_key)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                let Some(mut v) = decoded else {
                    tracing::error!(
                        label = rec.label.as_str(),
                        "break-glass public_key undecodable — record SKIPPED"
                    );
                    return None;
                };
                let inner = v["cred"]["counter"].as_u64().unwrap_or(0) as u32;
                let floor = effective_counter(
                    rec.sign_count,
                    inner,
                    counters.get(&rec.credential_id).copied(),
                );
                v["cred"]["counter"] = json!(floor);
                match serde_json::from_value::<SecurityKey>(v) {
                    Ok(sk) => Some(sk),
                    Err(e) => {
                        tracing::error!(
                            label = rec.label.as_str(),
                            error = %e,
                            "break-glass public_key not a stored SecurityKey — record SKIPPED"
                        );
                        None
                    }
                }
            })
            .collect()
    }
}

fn lock_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Small error value for helper Results — converted to a Response at
/// the handler boundary (clippy::result_large_err; same idiom as the
/// presence-passkey module: these are cold refusal paths).
type ErrResp = (StatusCode, String);

fn take_pending<T>(map: &Mutex<HashMap<String, Pending<T>>>, id: &str) -> Result<T, ErrResp> {
    let mut map = lock_recover(map);
    let now = Instant::now();
    map.retain(|_, p| p.expires > now);
    match map.remove(id) {
        Some(p) => Ok(p.state),
        None => Err((
            StatusCode::GONE,
            "challenge unknown, spent or expired — begin again".into(),
        )),
    }
}

fn put_pending<T>(map: &Mutex<HashMap<String, Pending<T>>>, id: String, state: T) {
    let mut map = lock_recover(map);
    let now = Instant::now();
    map.retain(|_, p| p.expires > now);
    map.insert(
        id,
        Pending {
            state,
            expires: now + CEREMONY_TTL,
        },
    );
}

// --------------------------------------------------------------------
// Enrollment ceremony.
// --------------------------------------------------------------------

#[derive(Deserialize)]
pub struct EnrollBeginBody {
    #[serde(default)]
    pub token: Option<String>,
}

/// `POST /api/auth/break-glass/enroll/begin`
pub async fn enroll_begin(
    State(state): State<Arc<BreakGlassState>>,
    headers: HeaderMap,
    Json(body): Json<EnrollBeginBody>,
) -> Response {
    let records = match state.store() {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    if let Err(refusal) = enroll_gate(
        state.session_role(&headers).as_deref(),
        body.token.as_deref(),
        state.enroll_token.as_deref(),
        records.len(),
    ) {
        return (StatusCode::FORBIDDEN, refusal).into_response();
    }

    let exclude = if records.is_empty() {
        None
    } else {
        Some(
            records
                .iter()
                .filter_map(|r| URL_SAFE_NO_PAD.decode(&r.credential_id).ok())
                .map(webauthn_rs::prelude::CredentialID::from)
                .collect(),
        )
    };
    // A stable user handle: break-glass has exactly one "user" — the
    // deployment's emergency operator identity.
    let user_uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, BREAK_GLASS_ACTOR.as_bytes());
    let (mut ccr, reg_state) = match state.webauthn.start_securitykey_registration(
        user_uuid,
        BREAK_GLASS_ACTOR,
        "BOSS break-glass",
        exclude,
        // Q1: no CA allowlist. The chain-form requirement is enforced
        // at finish by `require_attested_hardware`.
        None,
        Some(AuthenticatorAttachment::CrossPlatform),
    ) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("webauthn: {e}")).into_response();
        }
    };
    // The security-key flow only requests `direct` conveyance when a
    // CA list is supplied; we require the statement without the
    // allowlist, so raise the ask ourselves. The registration state
    // does not pin the preference — verification is unaffected.
    ccr.public_key.attestation = Some(AttestationConveyancePreference::Direct);

    let challenge_id = Uuid::new_v4().to_string();
    put_pending(&state.pending_reg, challenge_id.clone(), reg_state);
    Json(json!({ "challenge_id": challenge_id, "options": ccr })).into_response()
}

#[derive(Deserialize)]
pub struct EnrollFinishBody {
    pub challenge_id: String,
    #[serde(default)]
    pub token: Option<String>,
    pub label: CredentialLabel,
    pub credential: RegisterPublicKeyCredential,
}

/// The manifest a finished enrollment tells the operator to commit
/// into — named in code so the response can never drift from the
/// repo layout silently.
pub const CREDENTIALS_MANIFEST: &str = "infra/cluster/manifests/boss-break-glass-credentials.yaml";

/// `POST /api/auth/break-glass/enroll/finish` — verifies the
/// attestation and EMITS the credential record for the operator to
/// commit (see the module docs for why enrollment never writes the
/// ConfigMap itself).
pub async fn enroll_finish(
    State(state): State<Arc<BreakGlassState>>,
    headers: HeaderMap,
    Json(body): Json<EnrollFinishBody>,
) -> Response {
    let records = match state.store() {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    if let Err(refusal) = enroll_gate(
        state.session_role(&headers).as_deref(),
        body.token.as_deref(),
        state.enroll_token.as_deref(),
        records.len(),
    ) {
        return (StatusCode::FORBIDDEN, refusal).into_response();
    }
    let reg_state = match take_pending(&state.pending_reg, &body.challenge_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let sk = match state
        .webauthn
        .finish_securitykey_registration(&body.credential, &reg_state)
    {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("registration rejected: {e}"),
            )
                .into_response();
        }
    };
    let sk_value = match serde_json::to_value(&sk) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("credential serialization failed: {e}"),
            )
                .into_response();
        }
    };
    if let Err(reason) = require_attested_hardware(&sk_value) {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }

    let aaguid = match &sk.attestation().metadata {
        AttestationMetadata::Packed { aaguid } | AttestationMetadata::Tpm { aaguid, .. } => {
            aaguid.to_string()
        }
        // FIDO-U2F attestation predates AAGUID conveyance; the
        // record keeps the all-zero id rather than inventing one.
        _ => Uuid::nil().to_string(),
    };
    let record = BreakGlassCredential {
        credential_id: URL_SAFE_NO_PAD.encode(sk.cred_id().as_slice()),
        public_key: URL_SAFE_NO_PAD.encode(serde_json::to_vec(&sk_value).unwrap_or_default()),
        sign_count: sk_value["cred"]["counter"].as_u64().unwrap_or(0) as u32,
        aaguid,
        // Wall time via the sanctioned stamp source: enrolling an
        // emergency key is real-world activity in any clock mode,
        // same decision the auth-audit drain records under.
        enrolled_at: boss_clock_client::wall_now(),
        label: body.label,
    };
    state
        .audit
        .break_glass_enrolled(record.label.as_str(), &record.credential_id, &record.aaguid);
    // The record is public material; logging it whole is the
    // operator's second copy if the browser tab is lost.
    tracing::info!(
        label = record.label.as_str(),
        credential_id = %record.credential_id,
        record = %serde_json::to_string(&record).unwrap_or_default(),
        "break-glass credential enrolled — commit this record to {CREDENTIALS_MANIFEST}"
    );
    (
        StatusCode::CREATED,
        Json(json!({
            "credential": record,
            "commit_to": CREDENTIALS_MANIFEST,
            "config_map_key": format!("{}.json", record.label.as_str()),
        })),
    )
        .into_response()
}

// --------------------------------------------------------------------
// Assertion ceremony — the emergency door itself.
// --------------------------------------------------------------------

/// `POST /api/auth/break-glass/assert/begin`
pub async fn assert_begin(State(state): State<Arc<BreakGlassState>>) -> Response {
    let records = match state.store() {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    if records.is_empty() {
        return (
            StatusCode::CONFLICT,
            "no break-glass credential enrolled — see the enrollment ceremony in \
             the credential manifest",
        )
            .into_response();
    }
    let keys = state.security_keys(&records);
    if keys.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "no enrolled credential record is readable — the store is present but \
             every record failed to decode",
        )
            .into_response();
    }
    let (rcr, auth_state) = match state.webauthn.start_securitykey_authentication(&keys) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("webauthn: {e}")).into_response();
        }
    };
    let challenge_id = Uuid::new_v4().to_string();
    put_pending(&state.pending_auth, challenge_id.clone(), auth_state);
    Json(json!({ "challenge_id": challenge_id, "options": rcr })).into_response()
}

#[derive(Deserialize)]
pub struct AssertFinishBody {
    pub challenge_id: String,
    pub credential: PublicKeyCredential,
}

/// `POST /api/auth/break-glass/assert/finish` — a verified assertion
/// mints the narrow break-glass session. A failed one refuses
/// loudly, like every refusal in this system.
pub async fn assert_finish(
    State(state): State<Arc<BreakGlassState>>,
    Json(body): Json<AssertFinishBody>,
) -> Response {
    let auth_state = match take_pending(&state.pending_auth, &body.challenge_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let result = match state
        .webauthn
        .finish_securitykey_authentication(&body.credential, &auth_state)
    {
        Ok(v) => v,
        Err(e) => {
            state.audit.login_denied(
                Some(BREAK_GLASS_ACTOR),
                crate::audit::AuthMethod::BreakGlass,
                crate::audit::DeniedReason::BadCredentials,
                None,
            );
            return (StatusCode::UNAUTHORIZED, format!("assertion rejected: {e}")).into_response();
        }
    };

    // Advance this process's counter floor and tell the operator the
    // durable one is behind (module docs: the committed sign_count is
    // the restart-surviving floor).
    let cred_id = URL_SAFE_NO_PAD.encode(result.cred_id().as_slice());
    {
        let mut counters = lock_recover(&state.counters);
        let entry = counters.entry(cred_id.clone()).or_insert(0);
        *entry = (*entry).max(result.counter());
    }
    tracing::info!(
        credential_id = %cred_id,
        sign_count = result.counter(),
        "break-glass assertion verified — refresh sign_count in \
         {CREDENTIALS_MANIFEST} to carry this floor across restarts"
    );
    state.audit.login_succeeded(
        BREAK_GLASS_ACTOR,
        None,
        crate::audit::AuthMethod::BreakGlass,
    );

    let (set_cookie, sess) = mint_session(&state.session_key);
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&set_cookie) {
        headers.insert(header::SET_COOKIE, v);
    }
    (
        StatusCode::OK,
        headers,
        Json(json!({
            "actor": BREAK_GLASS_ACTOR,
            "role": sess.role,
            "expires_in": BREAK_GLASS_TTL_SECONDS,
        })),
    )
        .into_response()
}

// --------------------------------------------------------------------
// The ceremony page — self-contained, served from this binary.
// --------------------------------------------------------------------

/// `GET /break-glass`. Unlinked from the SPA on purpose — the
/// understated posture the login page documents is preserved: this
/// is a typed URL, not a button. It is served from a constant in the
/// gateway binary so it exists exactly when the verifier does.
pub async fn ceremony_page() -> Response {
    Html(CEREMONY_HTML).into_response()
}

const CEREMONY_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="robots" content="noindex">
<title>BOSS break-glass</title>
<style>
  body { font: 15px/1.5 system-ui, sans-serif; max-width: 34rem;
         margin: 4rem auto; padding: 0 1rem; color: #222; }
  h1 { font-size: 1.2rem; }
  button { font: inherit; padding: .5rem 1rem; cursor: pointer; }
  input, select { font: inherit; padding: .35rem; width: 100%;
                  box-sizing: border-box; margin: .2rem 0 .8rem; }
  pre { background: #f4f4f4; padding: .8rem; overflow-x: auto;
        white-space: pre-wrap; word-break: break-all; }
  .err { color: #a00; }
  .ok { color: #060; }
  details { margin-top: 2.5rem; }
</style>
</head>
<body>
<h1>Break-glass</h1>
<p>The emergency door. Touch your enrolled security key to open an
emergency session carrying the narrow break-glass role.</p>
<button id="assert">Touch key &amp; sign in</button>
<p id="assert-out"></p>

<details>
<summary>Enrollment (bootstrap or key rotation)</summary>
<p>Runs only inside the first-enrollment window (bootstrap token set,
zero credentials committed) or under an existing break-glass session.
The finished record must be committed to
<code>infra/cluster/manifests/boss-break-glass-credentials.yaml</code>
— nothing is stored until it lands there.</p>
<label>Bootstrap token (blank when using a break-glass session)</label>
<input id="token" type="password" autocomplete="off">
<label>Label</label>
<select id="label"><option>primary</option><option>backup</option></select>
<button id="enroll">Enroll this key</button>
<p id="enroll-out"></p>
<pre id="record" hidden></pre>
</details>

<script>
"use strict";
const b64uToBuf = (s) => Uint8Array.from(
  atob(s.replace(/-/g, "+").replace(/_/g, "/")), c => c.charCodeAt(0));
const bufToB64u = (b) => btoa(String.fromCharCode(...new Uint8Array(b)))
  .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const post = async (url, body) => {
  const r = await fetch(url, { method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body ?? {}) });
  const text = await r.text();
  if (!r.ok) throw new Error(r.status + ": " + text);
  return JSON.parse(text);
};
const say = (id, msg, cls) => {
  const el = document.getElementById(id);
  el.textContent = msg; el.className = cls || "";
};

document.getElementById("assert").addEventListener("click", async () => {
  try {
    say("assert-out", "requesting challenge…");
    const begin = await post("/api/auth/break-glass/assert/begin");
    const pk = begin.options.publicKey;
    pk.challenge = b64uToBuf(pk.challenge);
    (pk.allowCredentials || []).forEach(c => { c.id = b64uToBuf(c.id); });
    say("assert-out", "touch your key…");
    const cred = await navigator.credentials.get({ publicKey: pk });
    const out = await post("/api/auth/break-glass/assert/finish", {
      challenge_id: begin.challenge_id,
      credential: {
        id: cred.id, rawId: bufToB64u(cred.rawId), type: cred.type,
        extensions: cred.getClientExtensionResults(),
        response: {
          authenticatorData: bufToB64u(cred.response.authenticatorData),
          clientDataJSON: bufToB64u(cred.response.clientDataJSON),
          signature: bufToB64u(cred.response.signature),
          userHandle: cred.response.userHandle
            ? bufToB64u(cred.response.userHandle) : null,
        },
      },
    });
    say("assert-out", "session open (" + out.role + ", " +
        out.expires_in + "s) — redirecting…", "ok");
    setTimeout(() => { window.location.href = "/"; }, 800);
  } catch (e) { say("assert-out", String(e), "err"); }
});

document.getElementById("enroll").addEventListener("click", async () => {
  try {
    const token = document.getElementById("token").value || null;
    const label = document.getElementById("label").value;
    say("enroll-out", "requesting challenge…");
    const begin = await post("/api/auth/break-glass/enroll/begin", { token });
    const pk = begin.options.publicKey;
    pk.challenge = b64uToBuf(pk.challenge);
    pk.user.id = b64uToBuf(pk.user.id);
    (pk.excludeCredentials || []).forEach(c => { c.id = b64uToBuf(c.id); });
    say("enroll-out", "touch your key…");
    const cred = await navigator.credentials.create({ publicKey: pk });
    const out = await post("/api/auth/break-glass/enroll/finish", {
      challenge_id: begin.challenge_id, token, label,
      credential: {
        id: cred.id, rawId: bufToB64u(cred.rawId), type: cred.type,
        extensions: cred.getClientExtensionResults(),
        response: {
          attestationObject: bufToB64u(cred.response.attestationObject),
          clientDataJSON: bufToB64u(cred.response.clientDataJSON),
        },
      },
    });
    say("enroll-out", "enrolled. Commit the record below as ConfigMap key '" +
        out.config_map_key + "' in " + out.commit_to, "ok");
    const rec = document.getElementById("record");
    rec.hidden = false;
    rec.textContent = JSON.stringify(out.credential, null, 2);
  } catch (e) { say("enroll-out", String(e), "err"); }
});
</script>
</body>
</html>
"#;

// --------------------------------------------------------------------
// Tests.
// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tempfile::TempDir;

    const KEY: &[u8] = b"break-glass-test-session-key-32b";

    /// A real serialized credential, taken verbatim from
    /// webauthn-rs-core 0.5.5's own test suite (MPL-2.0) — a Yubico
    /// security key with a verified `Basic` attestation chain. Public
    /// material only. Used to build synthetic-but-deserializable
    /// [`SecurityKey`] values without an authenticator in the loop.
    const YUBIKEY_CRED: &str = r#"{"cred_id":"uZcVDBVS68E_MtAgeQpElJxldF_6cY9sSvbWqx_qRh8wiu42lyRBRmh5yFeD_r9k130dMbFHBHI9RTFgdJQIzQ","cred":{"type_":"ES256","key":{"EC_EC2":{"curve":"SECP256R1","x":[194,126,127,109,252,23,131,21,252,6,223,99,44,254,140,27,230,17,94,5,133,28,104,41,144,69,171,149,161,26,200,243],"y":[143,123,183,156,24,178,21,248,117,159,162,69,171,52,188,252,26,59,6,47,103,92,19,58,117,103,249,0,219,8,95,196]}}},"counter":2,"user_verified":false,"backup_eligible":false,"backup_state":false,"registration_policy":"preferred","extensions":{"cred_protect":"NotRequested","hmac_create_secret":"NotRequested"},"attestation":{"data":{"Basic":["MIICvTCCAaWgAwIBAgIEK_F8eDANBgkqhkiG9w0BAQsFADAuMSwwKgYDVQQDEyNZdWJpY28gVTJGIFJvb3QgQ0EgU2VyaWFsIDQ1NzIwMDYzMTAgFw0xNDA4MDEwMDAwMDBaGA8yMDUwMDkwNDAwMDAwMFowbjELMAkGA1UEBhMCU0UxEjAQBgNVBAoMCVl1YmljbyBBQjEiMCAGA1UECwwZQXV0aGVudGljYXRvciBBdHRlc3RhdGlvbjEnMCUGA1UEAwweWXViaWNvIFUyRiBFRSBTZXJpYWwgNzM3MjQ2MzI4MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEdMLHhCPIcS6bSPJZWGb8cECuTN8H13fVha8Ek5nt-pI8vrSflxb59Vp4bDQlH8jzXj3oW1ZwUDjHC6EnGWB5i6NsMGowIgYJKwYBBAGCxAoCBBUxLjMuNi4xLjQuMS40MTQ4Mi4xLjcwEwYLKwYBBAGC5RwCAQEEBAMCAiQwIQYLKwYBBAGC5RwBAQQEEgQQxe9V_62aS5-1gK3rr-Am0DAMBgNVHRMBAf8EAjAAMA0GCSqGSIb3DQEBCwUAA4IBAQCLbpN2nXhNbunZANJxAn_Cd-S4JuZsObnUiLnLLS0FPWa01TY8F7oJ8bE-aFa4kTe6NQQfi8-yiZrQ8N-JL4f7gNdQPSrH-r3iFd4SvroDe1jaJO4J9LeiFjmRdcVa-5cqNF4G1fPCofvw9W4lKnObuPakr0x_icdVq1MXhYdUtQk6Zr5mBnc4FhN9qi7DXqLHD5G7ZFUmGwfIcD2-0m1f1mwQS8yRD5-_aDCf3vutwddoi3crtivzyromwbKklR4qHunJ75LGZLZA8pJ_mXnUQ6TTsgRqPvPXgQPbSyGMf2z_DIPbQqCD_Bmc4dj9o6LozheBdDtcZCAjSPTAd_ui"]},"metadata":"None"},"attestation_format":"Packed"}"#;

    fn yubikey_sk_value() -> Value {
        json!({ "cred": serde_json::from_str::<Value>(YUBIKEY_CRED).unwrap() })
    }

    fn record_from(sk: &Value, label: CredentialLabel, sign_count: u32) -> BreakGlassCredential {
        BreakGlassCredential {
            credential_id: sk["cred"]["cred_id"].as_str().unwrap().to_string(),
            public_key: URL_SAFE_NO_PAD.encode(serde_json::to_vec(sk).unwrap()),
            sign_count,
            aaguid: Uuid::nil().to_string(),
            enrolled_at: Utc::now(),
            label,
        }
    }

    fn write_record(dir: &Path, name: &str, rec: &BreakGlassCredential) {
        std::fs::write(dir.join(name), serde_json::to_vec_pretty(rec).unwrap()).unwrap();
    }

    fn state_with(dir: &Path, enroll_token: Option<&str>) -> Arc<BreakGlassState> {
        let origin = Url::parse("https://boss.test").unwrap();
        let webauthn = WebauthnBuilder::new("boss.test", &origin)
            .unwrap()
            .rp_name("BOSS break-glass")
            .build()
            .unwrap();
        Arc::new(BreakGlassState {
            session_key: KEY.to_vec(),
            webauthn,
            dir: dir.to_path_buf(),
            enroll_token: enroll_token.map(String::from),
            audit: crate::audit::AuthAudit::disabled(),
            pending_reg: Mutex::new(HashMap::new()),
            pending_auth: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
        })
    }

    fn break_glass_headers() -> HeaderMap {
        let (_, sess) = mint_session(KEY);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{}={}", session::COOKIE_NAME, sess.encode(KEY)))
                .unwrap(),
        );
        headers
    }

    async fn body_string(resp: Response) -> String {
        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    // ---- store ------------------------------------------------------

    #[test]
    fn a_missing_directory_is_an_empty_store_not_an_error() {
        let td = TempDir::new().unwrap();
        let store = load_store(&td.path().join("nope")).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn records_round_trip_through_the_store() {
        let td = TempDir::new().unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 7);
        write_record(td.path(), "primary.json", &rec);
        let store = load_store(td.path()).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store[0].credential_id, rec.credential_id);
        assert_eq!(store[0].sign_count, 7);
        assert_eq!(store[0].label, CredentialLabel::Primary);
    }

    /// A ConfigMap mount carries `..data` machinery and an operator
    /// may leave a stray note; neither is a credential.
    #[test]
    fn non_record_files_are_ignored() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join(".hidden.json"), "junk").unwrap();
        std::fs::write(td.path().join("README.txt"), "notes").unwrap();
        std::fs::create_dir(td.path().join("..data")).unwrap();
        assert!(load_store(td.path()).unwrap().is_empty());
    }

    /// One broken record must not brick the door the intact record
    /// still opens.
    #[test]
    fn a_malformed_record_is_skipped_not_fatal() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("backup.json"), "{not json").unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        let store = load_store(td.path()).unwrap();
        assert_eq!(store.len(), 1, "the intact record survives");
        assert_eq!(store[0].label, CredentialLabel::Primary);
    }

    #[test]
    fn a_label_outside_primary_backup_is_refused_at_parse() {
        let mut rec = serde_json::to_value(record_from(
            &yubikey_sk_value(),
            CredentialLabel::Primary,
            0,
        ))
        .unwrap();
        rec["label"] = json!("skeleton");
        assert!(serde_json::from_value::<BreakGlassCredential>(rec).is_err());
    }

    // ---- enrollment gate --------------------------------------------

    #[test]
    fn bootstrap_window_is_open_only_at_zero_credentials_with_the_token() {
        // The window: token configured, token presented, store empty.
        assert_eq!(
            enroll_gate(None, Some("t0k3n"), Some("t0k3n"), 0),
            Ok(EnrollAuthz::BootstrapToken)
        );
        // First record committed → window closed, same token refused.
        assert!(enroll_gate(None, Some("t0k3n"), Some("t0k3n"), 1).is_err());
        // Wrong token → refused even in the window.
        assert!(enroll_gate(None, Some("wrong"), Some("t0k3n"), 0).is_err());
        // No token presented → refused.
        assert!(enroll_gate(None, None, Some("t0k3n"), 0).is_err());
        // No token configured → the window does not exist at all.
        assert!(enroll_gate(None, Some("t0k3n"), None, 0).is_err());
    }

    #[test]
    fn a_break_glass_session_may_always_enroll() {
        // Post-bootstrap rotation path: no token anywhere, records
        // already enrolled — the session is the authorization.
        assert_eq!(
            enroll_gate(Some("break-glass"), None, None, 2),
            Ok(EnrollAuthz::BreakGlassSession)
        );
    }

    #[test]
    fn no_other_session_role_may_enroll() {
        for role in ["platform-admin", "audit-readonly", "ceo", ""] {
            assert!(
                enroll_gate(Some(role), None, None, 0).is_err(),
                "role {role:?} must not enroll a break-glass key — not even \
                 platform-admin: the key ceremony is its own authority"
            );
        }
    }

    // ---- attestation policy (Q1) ------------------------------------

    #[test]
    fn a_basic_attestation_chain_is_accepted() {
        assert!(require_attested_hardware(&yubikey_sk_value()).is_ok());
    }

    #[test]
    fn none_attestation_is_rejected() {
        let mut sk = yubikey_sk_value();
        sk["cred"]["attestation"]["data"] = json!("None");
        let err = require_attested_hardware(&sk).unwrap_err();
        assert!(err.contains("attestation required"), "{err}");
    }

    /// Surrogate self-attestation proves possession of the credential
    /// key, not of vendor hardware — a software authenticator can
    /// produce it freely.
    #[test]
    fn self_and_uncertain_attestation_are_rejected() {
        for variant in ["Self_", "Uncertain", "ECDAA"] {
            let mut sk = yubikey_sk_value();
            sk["cred"]["attestation"]["data"] = json!(variant);
            assert!(
                require_attested_hardware(&sk).is_err(),
                "{variant} must be refused"
            );
        }
    }

    /// The BE flag is what every synced passkey sets: the private key
    /// can leave the device. Attestation form does not matter then.
    #[test]
    fn a_backup_eligible_credential_is_rejected_even_with_a_chain() {
        let mut sk = yubikey_sk_value();
        sk["cred"]["backup_eligible"] = json!(true);
        let err = require_attested_hardware(&sk).unwrap_err();
        assert!(err.contains("backup-eligible"), "{err}");
    }

    /// Fail closed: a shape this check does not recognize is a
    /// refusal, not a shrug.
    #[test]
    fn unrecognized_shapes_are_refused() {
        assert!(require_attested_hardware(&json!({})).is_err());
        let mut sk = yubikey_sk_value();
        sk["cred"]["attestation"]["data"] = json!({ "Novel": [] });
        assert!(require_attested_hardware(&sk).is_err());
        // A missing backup_eligible is not "false".
        let mut sk = yubikey_sk_value();
        sk["cred"]
            .as_object_mut()
            .unwrap()
            .remove("backup_eligible");
        assert!(require_attested_hardware(&sk).is_err());
    }

    // ---- counters ---------------------------------------------------

    #[test]
    fn effective_counter_is_the_highest_floor_available() {
        assert_eq!(effective_counter(5, 2, None), 5, "manifest wins");
        assert_eq!(effective_counter(1, 9, None), 9, "serialized wins");
        assert_eq!(effective_counter(3, 2, Some(11)), 11, "process memory wins");
        assert_eq!(effective_counter(0, 0, None), 0, "counter-less key");
    }

    #[test]
    fn counter_regression_is_the_clone_signal() {
        assert!(counter_regressed(5, 5), "no advance = possible clone");
        assert!(counter_regressed(5, 3), "backwards = possible clone");
        assert!(!counter_regressed(5, 6), "advancing is healthy");
        assert!(
            !counter_regressed(0, 0),
            "a counter-less authenticator carries no signal"
        );
        assert!(!counter_regressed(0, 1), "first count on a fresh record");
    }

    /// The floor actually reaches the credential handed to webauthn:
    /// the SecurityKey built for authentication carries the merged
    /// counter, which is what makes the crate's regression check
    /// enforce OUR floor rather than a stale one.
    #[test]
    fn security_keys_carry_the_merged_counter_floor() {
        let td = TempDir::new().unwrap();
        // Serialized counter inside public_key is 2; manifest says 40.
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 40);
        write_record(td.path(), "primary.json", &rec);
        let state = state_with(td.path(), None);
        // And the process has seen 90 since.
        lock_recover(&state.counters).insert(rec.credential_id.clone(), 90);
        let keys = state.security_keys(&load_store(td.path()).unwrap());
        assert_eq!(keys.len(), 1);
        let v = serde_json::to_value(&keys[0]).unwrap();
        assert_eq!(v["cred"]["counter"], json!(90));
    }

    // ---- session mint -----------------------------------------------

    #[test]
    fn the_minted_session_is_narrow_and_expiring() {
        let (set_cookie, sess) = mint_session(KEY);
        assert_eq!(sess.role.as_deref(), Some("break-glass"));
        assert_eq!(sess.username, BREAK_GLASS_ACTOR);
        assert!(
            sess.employee_id.is_none(),
            "the emergency session must not depend on (or invent) an employee row"
        );
        assert_eq!(sess.access_tier, "user", "no tier elevation rides along");
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains(&format!("Max-Age={BREAK_GLASS_TTL_SECONDS}")));
        // And it decodes as a valid session under the same key.
        let cookie_value = set_cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1;
        let decoded = Session::decode(cookie_value, KEY).unwrap();
        assert_eq!(decoded.role.as_deref(), Some("break-glass"));
    }

    // ---- handlers: the refusal and happy-begin paths ----------------

    #[tokio::test]
    async fn enroll_begin_refuses_outside_the_window() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path(), Some("t0k3n"));
        let resp = enroll_begin(
            State(state),
            HeaderMap::new(),
            Json(EnrollBeginBody {
                token: Some("wrong".into()),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn enroll_begin_in_the_window_asks_for_direct_attestation() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path(), Some("t0k3n"));
        let resp = enroll_begin(
            State(state),
            HeaderMap::new(),
            Json(EnrollBeginBody {
                token: Some("t0k3n".into()),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(v["challenge_id"].is_string());
        assert_eq!(
            v["options"]["publicKey"]["attestation"],
            json!("direct"),
            "Q1: the ceremony must ask the authenticator to convey its statement"
        );
    }

    /// After the first record lands, the token path is dead but a
    /// break-glass session still enrolls (the backup / rotation path)
    /// — and the enrolled key is excluded from re-enrollment.
    #[tokio::test]
    async fn enroll_begin_after_bootstrap_requires_the_session() {
        let td = TempDir::new().unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        let state = state_with(td.path(), Some("t0k3n"));

        let denied = enroll_begin(
            State(state.clone()),
            HeaderMap::new(),
            Json(EnrollBeginBody {
                token: Some("t0k3n".into()),
            }),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let allowed = enroll_begin(
            State(state),
            break_glass_headers(),
            Json(EnrollBeginBody { token: None }),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(allowed).await).unwrap();
        let excluded = v["options"]["publicKey"]["excludeCredentials"]
            .as_array()
            .expect("enrolled credentials are excluded");
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0]["id"], json!(rec.credential_id));
    }

    #[tokio::test]
    async fn assert_begin_with_no_credentials_is_a_conflict() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path(), None);
        let resp = assert_begin(State(state)).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn assert_begin_offers_every_enrolled_key() {
        let td = TempDir::new().unwrap();
        // Q2: two records, either sufficient — both must be offered.
        let primary = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 3);
        write_record(td.path(), "primary.json", &primary);
        let mut sk2 = yubikey_sk_value();
        sk2["cred"]["cred_id"] = json!(URL_SAFE_NO_PAD.encode([9u8; 32]));
        let backup = record_from(&sk2, CredentialLabel::Backup, 0);
        write_record(td.path(), "backup.json", &backup);

        let state = state_with(td.path(), None);
        let resp = assert_begin(State(state)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let allow = v["options"]["publicKey"]["allowCredentials"]
            .as_array()
            .expect("allowCredentials present");
        let ids: Vec<&str> = allow.iter().filter_map(|c| c["id"].as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&primary.credential_id.as_str()));
        assert!(ids.contains(&backup.credential_id.as_str()));
    }

    /// A challenge is single-use and unknown ids answer GONE — the
    /// replay surface of the ceremony state.
    #[tokio::test]
    async fn a_spent_or_unknown_challenge_is_gone() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path(), None);
        let resp = assert_finish(
            State(state),
            Json(AssertFinishBody {
                challenge_id: "never-minted".into(),
                credential: serde_json::from_value(json!({
                    "id": "x", "rawId": "eA",
                    "response": {
                        "authenticatorData": "eA", "clientDataJSON": "eA",
                        "signature": "eA", "userHandle": null,
                    },
                    "type": "public-key",
                }))
                .unwrap(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::GONE);
    }

    /// The store is re-read per ceremony, so a record committed while
    /// the gateway runs opens the door without a restart — the kubelet
    /// half of the Q5 write path.
    #[tokio::test]
    async fn a_record_committed_after_boot_is_seen_without_restart() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path(), None);
        assert_eq!(
            assert_begin(State(state.clone())).await.status(),
            StatusCode::CONFLICT
        );
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        assert_eq!(assert_begin(State(state)).await.status(), StatusCode::OK);
    }

    /// The ceremony page is self-contained: no external script, no
    /// SPA asset, nothing that can be down when the gateway is up.
    #[tokio::test]
    async fn the_ceremony_page_is_self_contained() {
        let resp = ceremony_page().await;
        assert_eq!(resp.status(), StatusCode::OK);
        let html = body_string(resp).await;
        assert!(html.contains("/api/auth/break-glass/assert/begin"));
        assert!(html.contains("/api/auth/break-glass/enroll/begin"));
        assert!(
            !html.contains("src=\"http") && !html.contains("href=\"http"),
            "the page must not reference any external asset"
        );
    }
}
