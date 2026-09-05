//! The conductor's forge credential rides on its OWN git commands — as
//! configuration only those child processes can see — and is written
//! into nobody's `~/.gitconfig`.
//!
//! WHY. The conductor moved into the cluster with only a token file
//! (`BOSS_TRAIN_FORGE_TOKEN_FILE`), and its clone, fetch and push all
//! need it on a private forge. The first fix wrote it with `git config
//! --global http.extraHeader` on every preflight, reasoning that the
//! container's HOME is its own and does not survive a roll. Both true —
//! and on 2026-09-05 an operator ran one `boss train cancel` on the
//! shared dev pod with a different token file, and that write replaced
//! the pod's working push credential with a read-only token: every
//! `git push` from the pod answered 403 on the receive-pack handshake
//! (reads fine) until the header was found and removed by hand
//! (packet 10bb1e1a). A verb given a credential for ITS clone must not
//! reach into the operator's global git config with it.
//!
//! HOW. git reads `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_n` /
//! `GIT_CONFIG_VALUE_n` from the environment as configuration of the
//! highest precedence — the same precedence as `git -c`, above every
//! file. [`command`] returns a `git` `Command` carrying the header as
//! exactly that, scoped to the forge URL (`http.<forge>/.extraHeader`,
//! so the token never rides to another host), computed from the token
//! file each time. Nothing is written anywhere; a fresh pod has the
//! credential the moment it has the token file; an operator's own git
//! config is never touched. A parent that already exports
//! `GIT_CONFIG_COUNT` keeps its entries — ours is appended after them.
//!
//! Best-effort by design: no token file, or an empty one, means the
//! command runs anonymous and fails loudly at the next remote op rather
//! than silently pretending to be configured.

use std::fs;
use std::process::Command;

use crate::train::env_or;

/// One git configuration entry: the URL-scoped extra header carrying
/// the forge token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForgeAuth {
    pub(crate) key: String,
    pub(crate) value: String,
}

/// The credential from the environment the conductor runs in, or None
/// when there is no usable token file.
pub(crate) fn forge_auth() -> Option<ForgeAuth> {
    forge_auth_from(
        &env_or("BOSS_TRAIN_FORGE_TOKEN_FILE", "/etc/boss-train/forge.token"),
        &env_or("BOSS_TRAIN_FORGE_URL", "http://10.20.0.15:3000"),
    )
}

/// The credential from a token file and a forge URL. Pure apart from
/// the file read, so the shape is pinned without touching the process
/// environment.
pub(crate) fn forge_auth_from(token_file: &str, forge_url: &str) -> Option<ForgeAuth> {
    let token = fs::read_to_string(token_file).ok()?;
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Some(ForgeAuth {
        key: format!("http.{}/.extraHeader", forge_url.trim_end_matches('/')),
        value: format!("Authorization: token {token}"),
    })
}

/// A `git` command carrying the forge credential, if there is one.
pub(crate) fn command() -> Command {
    let mut cmd = Command::new("git");
    if let Some(auth) = forge_auth() {
        let base = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        apply_at(&mut cmd, &auth, base);
    }
    cmd
}

/// Put `auth` on `cmd` as the `n`-th `GIT_CONFIG_*` entry, keeping any
/// entries `0..n` the parent already exports.
pub(crate) fn apply_at(cmd: &mut Command, auth: &ForgeAuth, n: usize) {
    cmd.env("GIT_CONFIG_COUNT", (n + 1).to_string());
    cmd.env(format!("GIT_CONFIG_KEY_{n}"), &auth.key);
    cmd.env(format!("GIT_CONFIG_VALUE_{n}"), &auth.value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_file(contents: &str) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        fs::write(f.path(), contents).unwrap();
        f
    }

    #[test]
    fn the_credential_is_a_header_scoped_to_the_forge_url() {
        let f = token_file("abc123\n");
        let auth = forge_auth_from(f.path().to_str().unwrap(), "http://10.20.0.15:3000/").unwrap();
        assert_eq!(auth.key, "http.http://10.20.0.15:3000/.extraHeader");
        assert_eq!(auth.value, "Authorization: token abc123");
    }

    #[test]
    fn no_token_means_no_credential_not_a_broken_one() {
        assert_eq!(
            forge_auth_from("/nonexistent/forge.token", "http://f"),
            None
        );
        let empty = token_file("  \n");
        assert_eq!(
            forge_auth_from(empty.path().to_str().unwrap(), "http://f"),
            None
        );
    }

    #[test]
    fn git_reads_the_credential_off_the_command_and_nothing_is_written() {
        // The behavioural pin: a git child of this command sees the
        // header as configuration — and only that child does. The
        // process's own environment and every config file stay as
        // they were, which is the whole point.
        let f = token_file("secret-token");
        let auth = forge_auth_from(f.path().to_str().unwrap(), "http://forge.test").unwrap();
        let mut cmd = Command::new("git");
        apply_at(&mut cmd, &auth, 0);
        let out = cmd.args(["config", "--get", &auth.key]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "Authorization: token secret-token"
        );
        // A plain git in the same process sees nothing of it.
        let plain = Command::new("git")
            .args(["config", "--get", &auth.key])
            .output()
            .unwrap();
        assert!(
            !plain.status.success(),
            "the header leaked outside the command"
        );
    }

    #[test]
    fn a_parents_git_config_entries_are_kept_and_ours_appended() {
        let f = token_file("t");
        let auth = forge_auth_from(f.path().to_str().unwrap(), "http://forge.test").unwrap();
        let mut cmd = Command::new("git");
        cmd.env("GIT_CONFIG_COUNT", "1");
        cmd.env("GIT_CONFIG_KEY_0", "boss.parent");
        cmd.env("GIT_CONFIG_VALUE_0", "kept");
        apply_at(&mut cmd, &auth, 1);
        let out = cmd
            .args([
                "config",
                "--get-regexp",
                "^(boss\\.parent|http\\..*extraheader)$",
            ])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("boss.parent kept"), "{text}");
        assert!(text.contains("Authorization: token t"), "{text}");
    }
}
