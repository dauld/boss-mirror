//! A calendar-feed token, and the only form of it the server keeps.
//!
//! KEEP A HASH, NEVER THE TOKEN (backlog 4aaff4dc, design 3101c506,
//! 2026-09-26). The token in `/ics/{token}/calendar.ics` is the whole
//! authentication of a sessionless feed, so holding it IS reading that
//! employee's schedule. Until this module the rotate wrote it raw into
//! `tech_calendar_tokens` AND into the payload of
//! `scheduling.calendar-token.rotated` — which the outbox copies to
//! `audit_log` and to the bus, and which the event tail, export and
//! stream hand to every global-read role, the guest login's
//! `audit-readonly` among them. Redacting the readers could not reach
//! the bus, the backups, or an export already taken; a hash at rest
//! makes every one of those safe at once.
//!
//! So the token exists in exactly two places: the response to the
//! employee's own rotate, once, and the URL their calendar app holds.
//! Everything the server writes — the table, the event, the log — is
//! [`CalendarTokenSha256`]. SHA-256 with no salt: the token is 256 bits
//! of randomness, so a salt or a slow hash adds nothing against a guess.
//!
//! The migration that re-hashed the rows written before this module
//! computes the same digest in SQL; `SQL_DIGEST_OF_TOKEN_COLUMN` is
//! that expression, and a database-backed test holds the two equal
//! (a_calendar_token_rests_as_its_hash_pg.rs).

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The SQL the re-hash migration applies to the legacy `token` column.
/// Spelled here so the test that pins it to [`CalendarTokenSha256::of`]
/// and the migration file can be compared byte for byte.
pub const SQL_DIGEST_OF_TOKEN_COLUMN: &str = "encode(sha256(convert_to(token, 'UTF8')), 'hex')";

/// Mint a fresh feed token: two v4 UUIDs, 256 bits of randomness,
/// 64 URL-safe hex characters. The caller hands it to the employee
/// once and keeps only [`CalendarTokenSha256::of`] it.
pub fn mint_calendar_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// The lowercase-hex SHA-256 of a feed token — the only value the
/// table, the event and the log ever hold. The port takes this type,
/// never a `&str`, so an adapter cannot be handed the token itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarTokenSha256(String);

impl CalendarTokenSha256 {
    /// Digest a token — one just minted, or one presented in a feed URL.
    pub fn of(token: &str) -> Self {
        Self(hex::encode(Sha256::digest(token.as_bytes())))
    }

    /// The digest of a fresh token nobody keeps. Writing it over a
    /// feed is a revocation: the old URL stops matching, and no URL
    /// matches the new row until the employee rotates their own.
    pub fn of_a_discarded_token() -> Self {
        Self::of(&mint_calendar_token())
    }

    /// A digest read back from the record — a replayed event's
    /// `token_sha256` — taken as it was written. Not for a presented
    /// token: that is [`Self::of`], or the digest would open the feed.
    pub fn from_stored(digest: &str) -> Self {
        Self(digest.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_lowercase_hex_sha256_of_the_token_bytes() {
        // FIPS 180-2 test vector for "abc".
        assert_eq!(
            CalendarTokenSha256::of("abc").as_str(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_minted_token_is_64_hex_characters_and_never_repeats() {
        let a = mint_calendar_token();
        let b = mint_calendar_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn the_digest_is_not_the_token() {
        let t = mint_calendar_token();
        let h = CalendarTokenSha256::of(&t);
        assert_ne!(h.as_str(), t);
        assert!(!h.as_str().contains(&t));
        assert_eq!(h, CalendarTokenSha256::of(&t), "the digest is a function");
    }

    #[test]
    fn a_discarded_token_digest_matches_no_minted_token() {
        let t = mint_calendar_token();
        assert_ne!(
            CalendarTokenSha256::of_a_discarded_token(),
            CalendarTokenSha256::of(&t)
        );
    }
}
