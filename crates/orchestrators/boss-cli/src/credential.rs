//! `boss credential pull <target>` — the pull half of the credential
//! broker (packet 7ee101aa, first leg).
//!
//! The broker (dispatcher handler `credential.rotate.forgejo`) mints
//! a replacement token and installs it into a named k8s Secret. Most
//! consumers get the new value for free — their paths are Secret
//! MOUNTS the kubelet re-syncs (`/etc/forge/token`, and
//! `/etc/boss-train/forge.token` once boss-dev.yaml's mount rolls
//! out). This verb is for the residue the kubelet cannot reach:
//!   1. a token FILE on a writable path (pre-mount pods, or hosts
//!      like boss-gcp where the path is a plain file), and
//!   2. the git credential-helper CONFIG — which, on 2026-09-02, held
//!      the write token INLINE in `.git/config`, where `git config
//!      -l` prints it. That hand-placement walkthrough is the
//!      transcript exposure that opened 7ee101aa.
//! The verb reads the named Secret via the session's own kubectl
//! (the dev-session Role grants `get` on exactly that secret name —
//! boss-dev-access.yaml), writes the token file, points the global
//! credential helper AT THE FILE, and scrubs any repo-local helper
//! that carries a password inline. It prints lengths and paths, never
//! values.

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use std::process::Command;

use crate::train::env_or;

const SECRET_NAMESPACE: &str = "boss-dev";
const SECRET_NAME: &str = "boss-dev-forge-token";
const SECRET_KEY: &str = "token";

// ---------------------------------------------------------------------------
// Pure pieces — decodable, printable, scrub-decidable
// ---------------------------------------------------------------------------

/// Decode the kubectl go-template output for one secret key: base64,
/// utf-8, surrounding whitespace stripped. Errors carry no content.
pub fn decode_secret_b64(raw: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .context("secret value is not valid base64")?;
    let s = String::from_utf8(bytes).context("secret value is not utf-8")?;
    let s = s.trim().to_string();
    if s.is_empty() {
        bail!(
            "secret {SECRET_NAMESPACE}/{SECRET_NAME} key {SECRET_KEY} is empty — \
             has the broker run a rotation yet?"
        );
    }
    Ok(s)
}

/// The credential helper that reads the token FILE at use time, so
/// the value never sits in git config. Matches the shape the
/// boss-dev manifest declares for the read token.
pub fn helper_command(user: &str, token_file: &str) -> String {
    format!("!f() {{ echo username={user}; echo \"password=$(cat {token_file})\"; }}; f")
}

/// Does a configured credential-helper value embed a literal
/// password? A helper that shells out to `cat` a file does not; one
/// that says `password=<forty hex chars>` is the exposure this verb
/// exists to retire.
pub fn is_inline_password_helper(value: &str) -> bool {
    match value.split_once("password=") {
        None => false,
        Some((_, after)) => !after.trim_start().starts_with("$("),
    }
}

// ---------------------------------------------------------------------------
// The verb
// ---------------------------------------------------------------------------

fn run_cmd(mut cmd: Command, what: &str) -> Result<String> {
    let out = cmd.output().with_context(|| format!("running {what}"))?;
    if !out.status.success() {
        bail!(
            "{what} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn read_secret_via_kubectl() -> Result<String> {
    let mut cmd = Command::new("kubectl");
    cmd.args([
        "get",
        "secret",
        "-n",
        SECRET_NAMESPACE,
        SECRET_NAME,
        "-o",
        &format!("go-template={{{{index .data \"{SECRET_KEY}\"}}}}"),
    ]);
    let raw = run_cmd(
        cmd,
        &format!("kubectl get secret {SECRET_NAMESPACE}/{SECRET_NAME}"),
    )?;
    decode_secret_b64(&raw)
}

/// Land the token in the file consumers read. Three honest outcomes:
/// written; already current under a read-only Secret mount (the
/// kubelet's copy IS the distribution); or read-only AND stale, which
/// means the mount will converge on the kubelet's sync interval and
/// this verb has nothing further to add.
fn install_token_file(path: &str, token: &str) -> Result<String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        // Best-effort: an unwritable parent falls through to the
        // write attempt below, whose error names the real problem.
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(path, format!("{token}\n")) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("chmod 600 {path}"))?;
            }
            Ok(format!(
                "wrote {path} ({} bytes, mode 600)",
                token.len() + 1
            ))
        }
        Err(write_err) => match std::fs::read_to_string(path) {
            Ok(existing) if existing.trim() == token => Ok(format!(
                "{path} already current (read-only mount; the kubelet syncs it)"
            )),
            Ok(_) => Ok(format!(
                "{path} is not writable ({write_err}) and stale — if it is a Secret \
                 mount the kubelet converges it within ~1 minute; re-run to confirm"
            )),
            Err(_) => Err(write_err).with_context(|| format!("writing {path}")),
        },
    }
}

/// Point the GLOBAL credential helper for the forge host at the token
/// file, and scrub any repo-local helper carrying an inline password.
fn repair_git_config(forge_url: &str, user: &str, token_file: &str) -> Result<Vec<String>> {
    let mut report = Vec::new();
    let key = format!("credential.{forge_url}.helper");
    let helper = helper_command(user, token_file);
    let mut set = Command::new("git");
    set.args(["config", "--global", &key, &helper]);
    run_cmd(set, &format!("git config --global {key}"))?;
    report.push(format!("global {key} -> reads {token_file}"));

    // Repo-local scrub, only meaningful inside a work tree.
    let mut get = Command::new("git");
    get.args(["config", "--local", "--get-all", "credential.helper"]);
    if let Ok(out) = get.output()
        && out.status.success()
    {
        let values = String::from_utf8_lossy(&out.stdout);
        let inline = values
            .lines()
            .filter(|v| is_inline_password_helper(v))
            .count();
        if inline > 0 {
            let mut unset = Command::new("git");
            unset.args(["config", "--local", "--unset-all", "credential.helper"]);
            run_cmd(unset, "git config --local --unset-all credential.helper")?;
            report.push(format!(
                "scrubbed {inline} repo-local credential.helper entr{} carrying an \
                 inline password",
                if inline == 1 { "y" } else { "ies" }
            ));
        }
    }
    Ok(report)
}

pub async fn pull(target: &str) -> Result<()> {
    if target != "forge" {
        bail!("unknown credential target {target:?}; this verb knows: forge");
    }
    let forge_url = env_or("BOSS_TRAIN_FORGE_URL", "http://10.20.0.15:3000");
    let forge_user = env_or("BOSS_TRAIN_FORGE_USER", "david");
    let token_file = env_or("BOSS_TRAIN_FORGE_TOKEN_FILE", "/etc/boss-train/forge.token");

    let token = read_secret_via_kubectl()?;
    println!(
        "pulled secret {SECRET_NAMESPACE}/{SECRET_NAME} key {SECRET_KEY}: {} bytes",
        token.len()
    );
    println!("{}", install_token_file(&token_file, &token)?);
    for line in repair_git_config(&forge_url, &forge_user, &token_file)? {
        println!("{line}");
    }
    println!("done — no value printed, by design");
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Top-level registration for `boss credential` — see `merged::Cmd`
/// for why the variant lives in its verb's module (84f9fbc0).
#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Credential distribution — pull a broker-rotated credential
    /// into this host's declared consumer paths.
    Credential {
        #[command(subcommand)]
        action: Action,
    },
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Pull the named credential from its k8s Secret: write the token
    /// file, point the git credential helper at it, scrub any inline-
    /// password helper. Prints lengths, never values.
    Pull {
        /// Which credential (today: `forge`).
        target: String,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Credential {
            action: Action::Pull { target },
        } => pull(&target).await,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_trims_a_secret_value() {
        // base64("abc123token\n")
        let raw = base64::engine::general_purpose::STANDARD.encode("abc123token\n");
        assert_eq!(decode_secret_b64(&raw).unwrap(), "abc123token");
        // kubectl output may carry surrounding whitespace of its own.
        assert_eq!(
            decode_secret_b64(&format!(" {raw}\n")).unwrap(),
            "abc123token"
        );
    }

    #[test]
    fn an_empty_secret_is_an_error_naming_the_bootstrap_question() {
        let raw = base64::engine::general_purpose::STANDARD.encode("\n");
        let err = decode_secret_b64(&raw).unwrap_err().to_string();
        assert!(err.contains("broker"), "got: {err}");
    }

    #[test]
    fn garbage_is_rejected_without_echoing_content() {
        let err = decode_secret_b64("!!not-base64!!").unwrap_err().to_string();
        assert!(
            !err.contains("!!not-base64!!"),
            "error echoed content: {err}"
        );
    }

    #[test]
    fn helper_reads_the_file_rather_than_embedding_a_value() {
        let h = helper_command("david", "/etc/boss-train/forge.token");
        assert_eq!(
            h,
            "!f() { echo username=david; echo \"password=$(cat /etc/boss-train/forge.token)\"; }; f"
        );
        assert!(
            !is_inline_password_helper(&h),
            "the file-reading helper is not inline"
        );
    }

    #[test]
    fn an_inline_password_helper_is_recognized_for_scrubbing() {
        // The 2026-09-02 exposure shape: the literal token in the
        // helper. The fixture value is assembled at runtime so no
        // token-shaped literal sits in the source — the no-secrets
        // lint (correctly) cannot tell a synthetic 40-hex assignment
        // from a real one, and an allow-list entry would teach it to
        // skip exactly the pattern this test exists to recognize.
        let fake = "0123456789abcdef".repeat(2) + "01234567";
        let exposed = format!("!f() {{ echo username=david; echo password={fake}; }}; f");
        assert!(is_inline_password_helper(&exposed));
        // No password at all: nothing to scrub.
        assert!(!is_inline_password_helper(
            "store --file ~/.git-credentials"
        ));
    }
}
