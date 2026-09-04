//! The gateway joins the log — auth events for the edge
//! (Q1+Q2 resolved 2026-08-11; folded into
//! docs/architecture-decisions.md §Policy & auth).
//!
//! Q1: the gateway stages events on the transactional outbox through
//! its own small pool (recipe 3 of transactional-audit-log.md — the
//! record-only shape, `PgOutboxRecorder`), because a session is a
//! cookie, not a row: there is no domain write for the event to join,
//! and what the pool buys is membership in the one pipeline — durable
//! staging, relay ordering, replay, the ref-check trigger — not
//! atomicity. The connection is expected to run as an INSERT-only
//! Postgres role (111-gateway-audit-events.sql); the internet-facing
//! edge gets the least privilege that can stage an event.
//!
//! Q2: three kinds, registered in `event_kinds` with source
//! `gateway`. `auth.login.denied` carries a closed reason and NO
//! subject reference — no employee matched, and asserting one would
//! both lie and trip the ref-check. IdP *transport* failures
//! (discovery down, token exchange) are not auth decisions and stay
//! plain warn lines in the handlers. `auth.login.succeeded` carries
//! the method so the imminent passkey path lands as an enum value,
//! not a schema change. `auth.session.guest` counts mints of the
//! unauthenticated read-only capability.
//!
//! Failure posture (the LogTransport principle): emitting never
//! blocks and never fails a login. The handler hands the event to a
//! bounded channel; a background task stages it. Channel full,
//! channel gone, no pool configured, or the INSERT failing all
//! degrade to the structured `tracing::warn!` that was the record
//! before this module existed — never to silence.
//!
//! Timestamps are wall-clock, minted at staging time — sim time is
//! retired from the record (David, 2026-08-22, packet a7a4cae5), and
//! an auth decision is real-world activity in any clock mode. The
//! handler never awaits anything to stamp.

use std::sync::Arc;

use boss_core::event::Event;
use boss_core::port::EventRecorder;
use serde_json::json;

/// How a login was attempted. `as_str` values are payload vocabulary
/// — append variants, never rename them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Password,
    Oidc,
    /// The hardware-key WebAuthn ceremony at the gateway's own
    /// verifier (docs/design/break-glass-is-a-key-you-hold.md).
    BreakGlass,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Oidc => "oidc",
            Self::BreakGlass => "break-glass",
        }
    }
}

/// Why a login was denied. Closed set (Q2): an authentication
/// *decision* about a person — transport failures don't belong here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeniedReason {
    /// The credential itself failed verification (local 401).
    BadCredentials,
    /// The credential (or IdP assertion) verified, but no employee
    /// record matches — the fail-closed path.
    NoEmployeeRecord,
    /// The IdP itself refused the user (`error=access_denied`).
    IdpDenied,
}

impl DeniedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadCredentials => "bad_credentials",
            Self::NoEmployeeRecord => "no_employee_record",
            Self::IdpDenied => "idp_denied",
        }
    }
}

/// Depth of the hand-off channel. Logins are a handful per user per
/// day against a relay that drains thousands of rows a second; if
/// this ever fills, the DB is down and the warn backstop is the
/// honest record anyway.
const QUEUE_DEPTH: usize = 256;

/// A staged emission: kind + payload. The drain task adds the
/// clock-routed timestamp and the `gateway` source when it builds
/// the [`Event`].
type Staged = (&'static str, serde_json::Value);

/// The gateway's auth-event emitter. Cheap to clone; handlers call
/// the `login_*`/`guest_*` methods inline and never wait on the
/// database or the clock.
#[derive(Clone)]
pub struct AuthAudit {
    tx: Option<tokio::sync::mpsc::Sender<Staged>>,
}

impl AuthAudit {
    /// No staging path configured. Every emit degrades to the
    /// structured warn line — exactly the record this deployment had
    /// before the module existed.
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    /// Spawn the drain task over a recorder. The task owns it and
    /// runs until the last `AuthAudit` clone drops.
    pub fn spawn(recorder: Arc<dyn EventRecorder>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Staged>(QUEUE_DEPTH);
        tokio::spawn(async move {
            while let Some((kind, payload)) = rx.recv().await {
                let event = Event::new("gateway", kind, payload, boss_clock_client::wall_now());
                if let Err(e) = recorder.record(&event).await {
                    warn_unrecorded(
                        event.kind.as_str(),
                        &event.payload,
                        &format!("outbox insert failed: {e}"),
                    );
                }
            }
        });
        Self { tx: Some(tx) }
    }

    /// An authentication decision went against the caller.
    /// `email_claimed` is the identity the caller *claimed*, not an
    /// employee reference; `None` when the IdP refused before any
    /// identity reached us.
    pub fn login_denied(
        &self,
        email_claimed: Option<&str>,
        method: AuthMethod,
        reason: DeniedReason,
        idp: Option<&str>,
    ) {
        let mut payload = json!({
            "method": method.as_str(),
            "reason": reason.as_str(),
        });
        if let Some(e) = email_claimed {
            payload["email_claimed"] = json!(e);
        }
        if let Some(i) = idp {
            payload["idp"] = json!(i);
        }
        self.emit("auth.login.denied", payload);
    }

    /// A session was minted for an authenticated employee.
    pub fn login_succeeded(&self, email: &str, employee_id: Option<&str>, method: AuthMethod) {
        let mut payload = json!({
            "email": email,
            "method": method.as_str(),
        });
        if let Some(id) = employee_id {
            payload["employee_id"] = json!(id);
        }
        self.emit("auth.login.succeeded", payload);
    }

    /// The unauthenticated read-only guest capability was exercised.
    pub fn guest_session(&self, email: &str) {
        self.emit("auth.session.guest", json!({ "email": email }));
    }

    /// A break-glass hardware credential passed the enrollment
    /// ceremony. Enrolling an emergency key is an auth-administration
    /// act; it gets its own kind rather than riding `login.succeeded`
    /// because no session is minted by it.
    pub fn break_glass_enrolled(&self, label: &str, credential_id: &str, aaguid: &str) {
        self.emit(
            "auth.break-glass.enrolled",
            json!({
                "label": label,
                "credential_id": credential_id,
                "aaguid": aaguid,
            }),
        );
    }

    fn emit(&self, kind: &'static str, payload: serde_json::Value) {
        match &self.tx {
            None => warn_unrecorded(kind, &payload, "no audit staging configured"),
            Some(tx) => {
                if let Err(e) = tx.try_send((kind, payload)) {
                    let ((kind, payload), why) = match e {
                        tokio::sync::mpsc::error::TrySendError::Full(v) => (v, "audit queue full"),
                        tokio::sync::mpsc::error::TrySendError::Closed(v) => {
                            (v, "audit drain task gone")
                        }
                    };
                    warn_unrecorded(kind, &payload, why);
                }
            }
        }
    }
}

/// The backstop record. Structured and greppable so a deployment
/// with no pool — or a pool that is down — still never silently
/// pretends nothing happened.
fn warn_unrecorded(kind: &str, payload: &serde_json::Value, why: &str) {
    tracing::warn!(
        kind = %kind,
        payload = %payload,
        why,
        "auth event NOT staged to the outbox — this warn line is the record"
    );
}

/// In-memory recorder + drain helper, shared by the handler tests in
/// `local_auth` and `oidc` — the port's test adapter, per the
/// no-mocks rule.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct Captured(pub(crate) Mutex<Vec<Event>>);

    #[async_trait::async_trait]
    impl EventRecorder for Captured {
        async fn record(&self, event: &Event) -> Result<(), String> {
            self.0.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    pub(crate) async fn drain(cap: &Captured, want: usize) -> Vec<Event> {
        for _ in 0..200 {
            if cap.0.lock().unwrap().len() >= want {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        cap.0.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Captured, drain};
    use super::*;

    #[tokio::test]
    async fn denied_carries_reason_and_no_subject_reference() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.login_denied(
            Some("who@example.com"),
            AuthMethod::Oidc,
            DeniedReason::NoEmployeeRecord,
            Some("https://idm.example"),
        );
        let events = drain(&cap, 1).await;
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind, "auth.login.denied");
        assert_eq!(e.source, "gateway");
        assert_eq!(e.payload["reason"], "no_employee_record");
        assert_eq!(e.payload["method"], "oidc");
        assert_eq!(e.payload["email_claimed"], "who@example.com");
        assert_eq!(e.payload["idp"], "https://idm.example");
        // The whole point of the denied event: no employee matched,
        // so the payload must not assert one (ref-check honesty).
        assert!(e.payload.get("employee_id").is_none());
    }

    #[tokio::test]
    async fn idp_refusal_needs_no_claimed_email() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.login_denied(None, AuthMethod::Oidc, DeniedReason::IdpDenied, None);
        let events = drain(&cap, 1).await;
        assert_eq!(events[0].payload["reason"], "idp_denied");
        assert!(events[0].payload.get("email_claimed").is_none());
    }

    #[tokio::test]
    async fn succeeded_names_the_method_for_the_passkey_future() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.login_succeeded("op@example.com", Some("emp-1"), AuthMethod::Password);
        let events = drain(&cap, 1).await;
        let e = &events[0];
        assert_eq!(e.kind, "auth.login.succeeded");
        assert_eq!(e.payload["method"], "password");
        assert_eq!(e.payload["employee_id"], "emp-1");
    }

    #[tokio::test]
    async fn guest_mint_is_counted_under_its_own_kind() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.guest_session("guest@algedonic.dev");
        let events = drain(&cap, 1).await;
        assert_eq!(events[0].kind, "auth.session.guest");
        assert_eq!(events[0].payload["email"], "guest@algedonic.dev");
    }

    #[tokio::test]
    async fn disabled_mode_degrades_to_the_warn_line_without_panic() {
        // No channel, no recorder: the call must be a no-op beyond
        // the warn backstop — a deployment without the pool keeps
        // exactly its pre-module behavior.
        let audit = AuthAudit::disabled();
        audit.login_denied(
            Some("who@example.com"),
            AuthMethod::Password,
            DeniedReason::BadCredentials,
            None,
        );
        audit.login_succeeded("op@example.com", None, AuthMethod::Password);
        audit.guest_session("guest@algedonic.dev");
    }

    #[tokio::test]
    async fn recorder_failure_never_reaches_the_caller() {
        struct Failing;
        #[async_trait::async_trait]
        impl EventRecorder for Failing {
            async fn record(&self, _: &Event) -> Result<(), String> {
                Err("db down".into())
            }
        }
        let audit = AuthAudit::spawn(Arc::new(Failing));
        // Emit must not error or panic; the drain task warns.
        audit.login_succeeded("op@example.com", None, AuthMethod::Password);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
