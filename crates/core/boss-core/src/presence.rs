//! The presence ticket — ONE definition for the side that mints it and
//! every side that grants on it.
//!
//! The gateway's passkey ceremony (docs/design/presence.md) ends in a
//! short-lived HMAC ticket: a verified WebAuthn assertion, bound to one
//! step's id and shape hash, one employee and one single-use nonce. The
//! jobs API grants `Assurance::Presence` on it.
//!
//! WHY THIS LIVES IN boss-core (backlog 72fe3640, 2026-09-24). The type
//! used to live in boss-gateway, the only place its HMAC was checked;
//! the gateway then re-injected the ticket's fields as PLAIN JSON in
//! `x-boss-presence`, and the jobs API granted presence on that JSON.
//! The gateway strips every inbound `x-boss-*` header, so through the
//! front door that held — but the jobs API's machine door (:7900)
//! trusts headers verbatim, and every machine-token holder (every
//! agent's `boss-api`, the conductor, the dispatcher, the ops runner)
//! could write the JSON by hand and stamp a passkey's presence on any
//! step as any person. So the header now carries the SIGNED ticket
//! itself, and the jobs API verifies it with [`PresenceTicket::decode`]
//! — this function, not a copy of it, so the check at the grant can
//! never be weaker than the check at the mint (CLAUDE.md §9a).
//!
//! The key is the gateway's session key. Both processes run in the one
//! `boss` container and read the same read-only Secret mount, named by
//! [`SESSION_KEY_ENV`]; [`parse_session_key`] is the one reading of that
//! file's format.

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// The header the jobs API reads a presence claim from. The gateway
/// writes it only after verifying a ticket, and its VALUE is that
/// signed ticket — so the jobs API can, and does, verify it again.
pub const HEADER: &str = "x-boss-presence";

/// The env var naming the session-key FILE (a path, not a key — see
/// infra/lint/session-key-persists.sh for what confusing the two cost).
pub const SESSION_KEY_ENV: &str = "BOSS_SESSION_KEY";

/// Where the key file is when [`SESSION_KEY_ENV`] is unset.
pub const DEFAULT_SESSION_KEY_PATH: &str = "/var/lib/boss-gateway/session.key";

/// The shortest key the file may hold, in bytes.
pub const MIN_SESSION_KEY_BYTES: usize = 32;

/// The key file's path, as every reader resolves it.
pub fn session_key_path() -> PathBuf {
    std::env::var(SESSION_KEY_ENV)
        .unwrap_or_else(|_| DEFAULT_SESSION_KEY_PATH.into())
        .into()
}

/// Why a key file's contents are not a key.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SessionKeyError {
    #[error("session key file is not valid hex")]
    NotHex,
    #[error("session key must be at least {MIN_SESSION_KEY_BYTES} bytes")]
    TooShort,
}

/// The key file's format, read once for every reader: hex, surrounding
/// whitespace ignored, at least [`MIN_SESSION_KEY_BYTES`] bytes.
pub fn parse_session_key(text: &str) -> Result<Vec<u8>, SessionKeyError> {
    let bytes = hex::decode(text.trim()).map_err(|_| SessionKeyError::NotHex)?;
    if bytes.len() < MIN_SESSION_KEY_BYTES {
        return Err(SessionKeyError::TooShort);
    }
    Ok(bytes)
}

/// Seconds since the epoch, by the wall clock — the clock the ticket's
/// expiry is minted against, never the simulation clock.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A verified assertion, portable for as long as its `e` allows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

fn mac(key: &[u8], payload_b64: &str) -> Option<Vec<u8>> {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).ok()?;
    mac.update(payload_b64.as_bytes());
    Some(mac.finalize().into_bytes().to_vec())
}

impl PresenceTicket {
    /// `<base64url(json)>.<base64url(hmac_sha256(key, base64url(json)))>`.
    /// `None` only if the ticket cannot be serialized or keyed, which
    /// HMAC-SHA256 over a plain struct never is.
    pub fn encode(&self, key: &[u8]) -> Option<String> {
        let payload = serde_json::to_vec(self).ok()?;
        let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
        let sig_b64 = URL_SAFE_NO_PAD.encode(mac(key, &payload_b64)?);
        Some(format!("{payload_b64}.{sig_b64}"))
    }

    /// The ticket, iff `value` was signed with `key` and is unexpired at
    /// `now_epoch`. The signature is compared in constant time and
    /// checked BEFORE the payload is parsed, so nothing an unsigned
    /// value says is ever read.
    pub fn decode(value: &str, key: &[u8], now_epoch: u64) -> Option<Self> {
        let (payload_b64, sig_b64) = value.split_once('.')?;
        let sig = URL_SAFE_NO_PAD.decode(sig_b64).ok()?;
        let expected = mac(key, payload_b64)?;
        if expected.ct_eq(&sig).unwrap_u8() != 1 {
            return None;
        }
        let ticket: PresenceTicket =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_b64).ok()?).ok()?;
        (ticket.e > now_epoch).then_some(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test-session-key";

    fn ticket(e: u64) -> PresenceTicket {
        PresenceTicket {
            i: "emp-1".into(),
            s: "step-1".into(),
            h: "hash".into(),
            n: "nonce".into(),
            e,
        }
    }

    #[test]
    fn ticket_round_trips_and_expires() {
        let t = ticket(now_epoch() + 60);
        let enc = t.encode(KEY).unwrap();
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

    /// The shape the jobs API used to accept (backlog 72fe3640): the
    /// ticket's fields as plain JSON, no signature. It must not decode,
    /// whatever it says.
    #[test]
    fn plain_json_is_not_a_ticket() {
        let json = serde_json::json!({
            "employee_id": "emp-1", "step_id": "step-1",
            "shape_hash": "hash", "nonce": "nonce",
        })
        .to_string();
        assert!(PresenceTicket::decode(&json, KEY, now_epoch()).is_none());
        // Nor a payload with a made-up signature.
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&ticket(u64::MAX)).unwrap());
        let made_up = format!("{payload}.{}", URL_SAFE_NO_PAD.encode([0u8; 32]));
        assert!(PresenceTicket::decode(&made_up, KEY, now_epoch()).is_none());
    }

    #[test]
    fn the_key_file_is_hex_of_at_least_thirty_two_bytes() {
        let hex64 = "ab".repeat(32);
        assert_eq!(
            parse_session_key(&format!("  {hex64}\n")).unwrap(),
            vec![0xab; 32]
        );
        assert_eq!(parse_session_key("abc"), Err(SessionKeyError::NotHex));
        assert_eq!(parse_session_key("zz"), Err(SessionKeyError::NotHex));
        assert_eq!(
            parse_session_key(&"ab".repeat(31)),
            Err(SessionKeyError::TooShort)
        );
    }
}
