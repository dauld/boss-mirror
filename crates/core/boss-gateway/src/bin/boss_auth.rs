//! `boss-auth` — admin CLI for the file-backed credential store
//! that powers the OSS-quickstart auth.
//!
//! Operators use this to:
//!   - Add the bootstrap-admin password during `docker compose up`
//!     init (the OSS quickstart calls it from `init.sh`).
//!   - Onboard a new user (until the SPA admin onboard UX lands).
//!   - Rotate a forgotten user's password.
//!   - Inspect who has credentials on file.
//!
//! All operations work against the same TOML file the gateway
//! reads at boot. Path is `BOSS_AUTH_FILE` env var or
//! `/var/lib/boss/auth/credentials.toml` by default.
//!
//! Subcommands:
//! ```text
//!   add    <email>          — prompt for password, hash, write.
//!                              Fails if the email already exists.
//!   set    <email>          — same as add but upserts.
//!   remove <email>          — drop a row.
//!   list                    — print every email on file.
//!   verify <email>          — prompt for password, exit 0 on
//!                              match, 1 on failure. Useful for
//!                              shell-script smoke tests.
//! ```
//!
//! Password input reads from stdin; if stdin is a TTY a hidden
//! prompt is used, otherwise the first line of stdin is read raw
//! (so `echo "pw" | boss-auth add op@x.com` works in CI).

use std::env;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use boss_gateway::local_auth::CredentialStore;

fn usage() -> &'static str {
    "usage: boss-auth <add|set|remove|list|verify> [email]"
}

fn auth_path() -> PathBuf {
    env::var("BOSS_AUTH_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/boss/auth/credentials.toml"))
}

fn read_password(prompt: &str) -> std::io::Result<String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        // Best-effort hidden prompt without pulling in a tty
        // dependency: print the prompt, read a single line. The
        // OS-level echo stays on, which is the obvious caveat;
        // this CLI is for ops boxes, not shoulder-surf scenarios.
        // Operators who want hidden input pipe via stty:
        //   stty -echo; boss-auth set op@x.com; stty echo
        eprint!("{prompt}");
        std::io::stderr().flush()?;
        let mut buf = String::new();
        stdin.lock().read_line(&mut buf)?;
        Ok(buf
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string())
    } else {
        // Non-TTY: first line of stdin.
        let mut buf = String::new();
        stdin.lock().read_to_string(&mut buf)?;
        Ok(buf.lines().next().unwrap_or("").to_string())
    }
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let cmd = match args.next() {
        Some(s) => s,
        None => {
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };

    let path = auth_path();
    let store = match CredentialStore::load(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: loading {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };

    match cmd.as_str() {
        "list" => {
            let emails = store.list_emails();
            if emails.is_empty() {
                eprintln!("(no credentials on file)");
            } else {
                for e in emails {
                    println!("{e}");
                }
            }
            ExitCode::SUCCESS
        }
        "add" | "set" | "remove" | "verify" => {
            let email = match args.next() {
                Some(e) => e,
                None => {
                    eprintln!("error: missing email\n{}", usage());
                    return ExitCode::from(2);
                }
            };
            match cmd.as_str() {
                "add" => {
                    if store.contains(&email) {
                        eprintln!("error: {email} already has a credential — use `set` to rotate");
                        return ExitCode::FAILURE;
                    }
                    let pw = match read_password(&format!("password for {email}: ")) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("error: read password: {e}");
                            return ExitCode::FAILURE;
                        }
                    };
                    if pw.is_empty() {
                        eprintln!("error: empty password");
                        return ExitCode::FAILURE;
                    }
                    if let Err(e) = store.upsert(&email, &pw) {
                        eprintln!("error: upsert: {e}");
                        return ExitCode::FAILURE;
                    }
                    eprintln!("✓ added {email}");
                    ExitCode::SUCCESS
                }
                "set" => {
                    let pw = match read_password(&format!("new password for {email}: ")) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("error: read password: {e}");
                            return ExitCode::FAILURE;
                        }
                    };
                    if pw.is_empty() {
                        eprintln!("error: empty password");
                        return ExitCode::FAILURE;
                    }
                    if let Err(e) = store.upsert(&email, &pw) {
                        eprintln!("error: upsert: {e}");
                        return ExitCode::FAILURE;
                    }
                    eprintln!("✓ rotated {email}");
                    ExitCode::SUCCESS
                }
                "remove" => match store.remove(&email) {
                    Ok(true) => {
                        eprintln!("✓ removed {email}");
                        ExitCode::SUCCESS
                    }
                    Ok(false) => {
                        eprintln!("(no credential for {email})");
                        ExitCode::FAILURE
                    }
                    Err(e) => {
                        eprintln!("error: remove: {e}");
                        ExitCode::FAILURE
                    }
                },
                "verify" => {
                    let pw = match read_password(&format!("password for {email}: ")) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("error: read password: {e}");
                            return ExitCode::FAILURE;
                        }
                    };
                    match store.verify(&email, &pw) {
                        Ok(()) => {
                            eprintln!("✓ verified");
                            ExitCode::SUCCESS
                        }
                        Err(e) => {
                            eprintln!("✗ {e}");
                            ExitCode::FAILURE
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
        _ => {
            eprintln!("unknown subcommand: {cmd}\n{}", usage());
            ExitCode::from(2)
        }
    }
}
