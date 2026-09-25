//! The key this jobs API verifies presence tickets with (backlog
//! 72fe3640, 2026-09-24).
//!
//! `Assurance::Presence` is granted on the gateway's signed ticket
//! (`boss_core::presence`), and the only way to know a ticket is the
//! gateway's is to check its HMAC with the gateway's key. That key is
//! the session key: the gateway and this API run in the one `boss`
//! container and read the same read-only Secret mount, named by
//! `BOSS_SESSION_KEY` — so no new secret exists for this, and none was
//! invented.
//!
//! LOADED ON FIRST USE, NOT AT START. Every service in the quickstart
//! launcher starts in parallel and the gateway CREATES the key file when
//! it finds none, so a jobs API that read once at start could find no
//! file and refuse presence until its next restart. So the file is read
//! the first time a presence claim arrives, and once it has read a
//! valid key it keeps it; until then each claim tries again. It is never
//! created here — only the gateway mints a key.
//!
//! FAIL CLOSED. No key — unconfigured, unreadable, malformed — means no
//! ticket verifies, so no request is granted presence. There is no
//! fallback to reading the header's fields on trust: that fallback WAS
//! the defect.

use std::path::PathBuf;

use tokio::sync::OnceCell;

/// Where the verification key comes from, and the key once read.
pub struct PresenceKey {
    /// `None` for a key handed over whole ([`PresenceKey::fixed`]).
    path: Option<PathBuf>,
    key: OnceCell<Vec<u8>>,
}

impl PresenceKey {
    /// A key given directly — tests, and any wiring that already holds
    /// the bytes.
    pub fn fixed(key: Vec<u8>) -> Self {
        Self {
            path: None,
            key: OnceCell::new_with(Some(key)),
        }
    }

    /// The gateway's key file at `path`, read on first use.
    pub fn from_file(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            key: OnceCell::new(),
        }
    }

    /// The gateway's key file where the gateway itself looks for it
    /// (`BOSS_SESSION_KEY`, else its default path).
    pub fn from_env() -> Self {
        Self::from_file(boss_core::presence::session_key_path())
    }

    /// The key, or `None` when there is none to be had — in which case
    /// nothing verifies. Says why on the log each time it cannot read
    /// one, because a presence refusal with a silent cause is the
    /// failure this whole change exists to remove.
    pub async fn get(&self) -> Option<&[u8]> {
        if let Some(key) = self.key.get() {
            return Some(key);
        }
        let path = self.path.as_ref()?;
        let text = match tokio::fs::read_to_string(path).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "presence: cannot read the gateway's key file; every presence claim \
                     is refused until it can be read"
                );
                return None;
            }
        };
        match boss_core::presence::parse_session_key(&text) {
            Ok(key) => {
                // A concurrent first read may have won; either way the
                // cell now holds the file's key.
                let _ = self.key.set(key);
                self.key.get().map(Vec::as_slice)
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "presence: the gateway's key file holds no usable key; every presence \
                     claim is refused"
                );
                None
            }
        }
    }
}

/// The key to judge `headers` with: looked up only when a presence
/// claim is actually present, so an ordinary write never touches the
/// file.
pub(super) async fn key_for<'a>(
    key: Option<&'a PresenceKey>,
    headers: &axum::http::HeaderMap,
) -> Option<&'a [u8]> {
    if !headers.contains_key(boss_core::presence::HEADER) {
        return None;
    }
    key?.get().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fixed_key_is_the_key() {
        let k = PresenceKey::fixed(vec![7; 32]);
        assert_eq!(k.get().await, Some(&[7u8; 32][..]));
    }

    /// The quickstart race: the jobs API asks before the gateway has
    /// written the file. It refuses then, and verifies once the file
    /// exists — without a restart.
    #[tokio::test]
    async fn a_key_file_written_after_start_is_picked_up() {
        let dir = boss_testing::scratch_dir("presence-key");
        let path = dir.join("session.key");
        let k = PresenceKey::from_file(path.clone());
        assert_eq!(k.get().await, None, "no file yet: nothing verifies");

        std::fs::write(&path, "ab".repeat(32)).unwrap();
        assert_eq!(k.get().await, Some(&[0xab; 32][..]));
    }

    #[tokio::test]
    async fn a_malformed_key_file_verifies_nothing() {
        let dir = boss_testing::scratch_dir("presence-key-bad");
        let path = dir.join("session.key");
        std::fs::write(&path, "not-hex").unwrap();
        assert_eq!(PresenceKey::from_file(path).get().await, None);
    }
}
