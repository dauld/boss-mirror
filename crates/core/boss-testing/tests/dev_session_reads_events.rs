//! The dev-session ServiceAccount can read the EVENTS in its own
//! namespace (backlog 27eacc14).
//!
//! Three times in the week of 2026-09-12 the cause of a pod stuck at
//! ContainerCreating was on the pod's events and the session that
//! launched the pod could not read it: `kubectl describe pod` printed
//! `Events: <none>` because the Role grants jobs, pods, pods/log and
//! PVCs — not events. Each time a human ran the same describe from a
//! workstation and pasted it back; the 9-hour gate-seed mount
//! (d42d4967) was diagnosed that way. Reading events is a read, scoped
//! to boss-dev like every other rule here; this pins the grant so a
//! later tidy of the Role cannot drop it silently.

use boss_testing::repo_root;

#[test]
fn the_dev_session_role_grants_events_in_both_api_groups() {
    let text = std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("boss-dev.yaml");
    let role = text
        .split("\n---\n")
        .find(|d| d.contains("kind: Role\n") && d.contains("name: dev-session-gates"))
        .expect("the dev-session-gates Role is declared");
    // kubectl describe reads core-group events; `kubectl get events`
    // and newer clients read events.k8s.io — grant both, or one door
    // stays blind.
    for group in [r#"apiGroups: [""]"#, r#"apiGroups: ["events.k8s.io"]"#] {
        let rule = role
            .split("\n  - ")
            .find(|r| r.contains(group) && r.contains(r#"resources: ["events"]"#))
            .unwrap_or_else(|| panic!("no events rule for {group} in the dev-session Role"));
        for verb in ["get", "list", "watch"] {
            assert!(
                rule.contains(&format!("\"{verb}\"")),
                "the events rule for {group} must grant {verb}: {rule}"
            );
        }
        assert!(
            !rule.contains("create") && !rule.contains("delete") && !rule.contains("patch"),
            "events are read-only for the session: {rule}"
        );
    }
}
