//! Stamp the binary with the commit it was built from, so the operator's
//! own tool can say when it is stale (895c9a3b).
//!
//! On the dev pod `boss` is whatever `cargo build -p boss-cli` last
//! produced, and NOTHING refreshes it: measured 2026-09-10, the binary
//! running every gate launch was eleven hours and two trains behind
//! main, so a guard that had landed (`--park-*` refusing without an
//! item) was unenforced on the one box that gates, silently — a removed
//! flag fails loudly, changed behaviour behind the same flag just does
//! the old thing. The floor is that the binary SAYS what it is: this
//! script reads `git rev-parse HEAD` at build time and, where there is
//! no git (an image build passes the sha as `BOSS_BUILD_COMMIT`, the
//! same variable every service already reports on /health), falls back
//! to that, and to `unknown` — never to a guess.

use std::process::Command;

fn main() {
    let from_git = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let sha = from_git
        .or_else(|| std::env::var("BOSS_BUILD_COMMIT").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BOSS_CLI_BUILT_FROM={sha}");
    println!("cargo:rerun-if-env-changed=BOSS_BUILD_COMMIT");
    // HEAD moves on checkout/commit; in a worktree `.git` is a file and
    // `--git-path HEAD` resolves it either way. No git, no rerun key —
    // the env fallback above is then the only input and is tracked.
    if let Some(head) = Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        println!("cargo:rerun-if-changed={head}");
    }
}
