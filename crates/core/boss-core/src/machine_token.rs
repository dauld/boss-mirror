//! The machine door's shared secret (feedback 7fcd78fa, phase 1; design
//! 6805c764, car 2).
//!
//! The service ports read identity from the caller-supplied
//! `x-boss-user` header and trust it verbatim — measured from a laptop
//! with a made-up header, any host that can route to a port may declare
//! itself any role at any tier (backlog 2710c8fc). The gate
//! (`machine_gate.rs`) is how a port refuses such a caller; this module
//! is how a legitimate caller proves it is not one.
//!
//! ONE SOURCE, THE MOUNTED FILE. Callers and the gate read the token
//! from the same place: the `current` file in the directory the
//! `boss-machine-token` Secret is mounted at ([`TOKEN_DIR_ENV`], default
//! [`DEFAULT_TOKEN_DIR`]). Until car 2 of design 6805c764 the callers
//! read an env var, `BOSS_MACHINE_TOKEN`, while car 1's gate read this
//! directory — two sources for one fact (CLAUDE.md §9a), and an env var
//! from a `secretKeyRef` is fixed at process start, so a rotation would
//! have meant restarting every caller in order against the gates, the
//! ordering that takes a system of record dark (design choice 2). The
//! env var is deleted, not kept as a fallback, for the same reason.
//! Callers always SEND `current`; the gate ACCEPTS `current`, `next` and
//! `previous`, which is what lets the broker rotate without a restart.
//!
//! DEPLOY-ORDER SAFETY: everything is inert until the Secret is mounted
//! (car 4). No file means no token, and no token means no header is
//! attached — and every gate is in mode `off` until then.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

/// Header the token rides in. Lowercase because axum/reqwest header
/// names are; the gateway's edge strip covers it via the `x-boss-`
/// prefix rule, so a browser can never smuggle one through.
pub const HEADER: &str = "x-boss-machine-token";

/// The directory holding the `current`, `next` and `previous` slots —
/// the `boss-machine-token` Secret's mount (design 6805c764, car 4).
/// ONE definition, read by the gate and by every caller.
pub const TOKEN_DIR_ENV: &str = "BOSS_MACHINE_TOKEN_DIR";
pub const DEFAULT_TOKEN_DIR: &str = "/etc/boss/machine-token";

/// The slot a caller sends.
pub const CURRENT: &str = "current";

/// The most a slot file may hold. A token is 32 random bytes,
/// base64url — 43 characters; anything past this is not a token, and a
/// reader must not pull an arbitrary file into memory on every request
/// (review of car 1, a159e1ee).
pub const MAX_SLOT_BYTES: u64 = 4096;

/// How often a [`Source`] re-reads its file. Kubelet refreshes a mounted
/// Secret in about a minute, so five seconds adds nothing a rotation
/// would notice — the same interval the gate re-reads at.
pub const REREAD: Duration = Duration::from_secs(5);

/// The mounted directory, from the environment or its default.
pub fn token_dir() -> PathBuf {
    std::env::var_os(TOKEN_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TOKEN_DIR))
}

/// One slot's value from `dir`: whitespace-trimmed, absent when the file
/// is missing, unreadable, blank, or larger than [`MAX_SLOT_BYTES`]. A
/// blank value reads as absent rather than as a token every empty header
/// would match. Blocking; the file is a Secret key on a tmpfs mount.
pub fn read_slot(dir: &Path, slot: &str) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(dir.join(slot)).ok()?;
    let mut raw = String::new();
    file.take(MAX_SLOT_BYTES + 1)
        .read_to_string(&mut raw)
        .ok()?;
    clean(raw)
}

fn clean(raw: String) -> Option<String> {
    if raw.len() as u64 > MAX_SLOT_BYTES {
        return None;
    }
    Some(raw.trim().to_string()).filter(|v| !v.is_empty())
}

/// The token this process sends: the `current` slot of the mounted
/// directory, read now. For a client built once (its default headers)
/// or a rare server-side call; a per-request stamp holds a [`Source`].
pub fn current() -> Option<String> {
    read_slot(&token_dir(), CURRENT)
}

/// Insert the token header when the process has one mounted. Callers
/// hand this their client's `default_headers` map so every request the
/// client ever makes carries the token — attaching per-request is how
/// one call site gets missed.
pub fn attach(headers: &mut reqwest::header::HeaderMap) {
    attach_value(headers, current());
}

fn attach_value(headers: &mut reqwest::header::HeaderMap, token: Option<String>) {
    if let Some(token) = token
        && let Ok(v) = reqwest::header::HeaderValue::from_str(&token)
    {
        headers.insert(HEADER, v);
    }
}

/// The `current` slot, re-read in the background every [`REREAD`], for a
/// caller that stamps the token on every request (the gateway): a read
/// of the last value is a lock, never file I/O on the request path, and
/// a rotation reaches it without a restart.
#[derive(Debug, Default)]
pub struct Source {
    value: RwLock<Option<String>>,
}

impl Source {
    /// A fixed value that is never re-read. For tests, and for a process
    /// that has no runtime to watch from.
    pub fn fixed(value: Option<String>) -> Self {
        Source {
            value: RwLock::new(value.and_then(clean)),
        }
    }

    /// Read `dir`'s `current` now, then every [`REREAD`] on the current
    /// runtime. Outside a runtime it is read once and says so.
    pub fn watch(dir: PathBuf) -> Arc<Self> {
        let source = Arc::new(Source::fixed(read_slot(&dir, CURRENT)));
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                let watched = Arc::clone(&source);
                rt.spawn(async move {
                    loop {
                        tokio::time::sleep(REREAD).await;
                        let d = dir.clone();
                        let next = tokio::task::spawn_blocking(move || read_slot(&d, CURRENT))
                            .await
                            .ok()
                            .flatten();
                        *watched
                            .value
                            .write()
                            .unwrap_or_else(PoisonError::into_inner) = next;
                    }
                });
            }
            Err(_) => tracing::error!(
                "machine token source built outside a runtime: its `current` slot will not be re-read"
            ),
        }
        source
    }

    /// The last value read.
    pub fn current(&self) -> Option<String> {
        self.value
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Does the provided header value match the expected token?
///
/// Byte-wise constant-time over the compared length: the accumulator
/// folds every byte rather than returning at the first mismatch, so
/// response timing does not leak a prefix. The length check short-
/// circuits, which leaks only the token's length — acceptable for a
/// high-entropy random value.
pub fn verify(expected: &str, provided: Option<&str>) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    let (a, b) = (expected.as_bytes(), provided.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "boss-core-machine-token-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn verify_requires_exact_match() {
        assert!(verify("s3cret", Some("s3cret")));
        assert!(!verify("s3cret", Some("s3creT")));
        assert!(!verify("s3cret", Some("s3cre")));
        assert!(!verify("s3cret", Some("")));
        assert!(!verify("s3cret", None));
    }

    #[test]
    fn a_caller_sends_the_current_slot_of_the_mounted_directory() {
        let d = dir();
        // Nothing mounted: no token, and no header. This is every pod
        // until car 4 mounts the Secret — the deploy-order safety.
        assert_eq!(read_slot(&d, CURRENT), None);
        let mut h = reqwest::header::HeaderMap::new();
        attach_value(&mut h, read_slot(&d, CURRENT));
        assert!(!h.contains_key(HEADER));

        std::fs::write(d.join("current"), "cur-value\n").unwrap();
        std::fs::write(d.join("next"), "next-value\n").unwrap();
        assert_eq!(read_slot(&d, CURRENT).as_deref(), Some("cur-value"));
        attach_value(&mut h, read_slot(&d, CURRENT));
        assert_eq!(h.get(HEADER).unwrap(), "cur-value");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_blank_or_oversized_slot_is_no_token() {
        let d = dir();
        std::fs::write(d.join("current"), "  \n").unwrap();
        assert_eq!(read_slot(&d, CURRENT), None);
        std::fs::write(d.join("current"), "x".repeat(MAX_SLOT_BYTES as usize + 1)).unwrap();
        assert_eq!(read_slot(&d, CURRENT), None);
        std::fs::write(d.join("current"), "x".repeat(MAX_SLOT_BYTES as usize)).unwrap();
        assert!(read_slot(&d, CURRENT).is_some());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_env_var_is_no_longer_a_source() {
        // Two sources for one fact drift (CLAUDE.md §9a): the token is
        // the mounted file's, and a process whose environment still
        // carries the old variable sends nothing because of it.
        let src = include_str!("machine_token.rs");
        let old = ["BOSS_MACHINE", "_TOKEN\""].concat();
        assert!(
            !src.contains(&old),
            "machine_token.rs reads the env var again"
        );
    }

    #[tokio::test]
    async fn a_source_follows_its_file_without_a_restart() {
        let d = dir();
        std::fs::write(d.join("current"), "first\n").unwrap();
        let s = Source::watch(d.clone());
        assert_eq!(s.current().as_deref(), Some("first"));
        std::fs::write(d.join("current"), "second\n").unwrap();
        tokio::time::sleep(REREAD + Duration::from_millis(500)).await;
        assert_eq!(s.current().as_deref(), Some("second"));
        assert_eq!(Source::fixed(Some(" \n".into())).current(), None);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
