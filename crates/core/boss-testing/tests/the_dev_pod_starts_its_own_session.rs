//! The dev pod starts its own Claude Code session (David, 2026-09-15:
//! "restart the dev pod into the Claude Code session, so we can at least
//! use remote connect without needing to SSH into the pod").
//!
//! Until this, a fresh pod came up with claude INSTALLED and nothing
//! running it — the lost day of 2026-09-10 — so every restart cost an ssh
//! hop to type `claude`. The manifest's postStart now starts the durable
//! tmux session `dev` (the one ssh logins attach to through
//! dev-session.sh) running `claude --remote-control`, guarded on the
//! binary AND the login being present on the PVC, and says which when it
//! does not start. Pinned by reading the manifest: the boot path is the
//! definition, and a human `claude` after a roll is what this ends.

use boss_testing::repo_root;

fn manifest() -> String {
    std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("infra/cluster/manifests/boss-dev.yaml")
}

#[test]
fn the_post_start_starts_the_durable_session_with_remote_control() {
    let m = manifest();
    assert!(
        m.contains("exec claude --remote-control boss-dev"),
        "postStart must exec claude with Remote Control so the session registers on its own"
    );
    assert!(
        m.contains("tmux new-session -d -s dev -c /work/boss /work/dev-claude.sh"),
        "the session must be the durable tmux session `dev` — the one dev-session.sh attaches ssh logins to"
    );
    assert!(
        m.contains("exec tmux new-session -A -s dev"),
        "dev-session.sh must still ATTACH (-A) so an ssh login lands inside the running session, not beside it"
    );
}

#[test]
fn the_session_start_is_guarded_on_the_binary_and_the_login_and_says_which() {
    let m = manifest();
    assert!(
        m.contains(
            "[ -x /work/home/.local/bin/claude ] && [ -s /work/home/.claude/.credentials.json ]"
        ),
        "start only when claude AND its login are on the PVC — the boot never writes a credential"
    );
    assert!(
        m.contains("dev session: not started"),
        "a session that does not start says so in postStart.log rather than leaving a silent pod"
    );
}
