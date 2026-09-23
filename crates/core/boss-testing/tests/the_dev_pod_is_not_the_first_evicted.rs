//! The dev pod is not the first thing the build node evicts, and it
//! comes back without re-owning the whole workspace.
//!
//! Measured 2026-09-23: the dev pod was evicted SIX times in eight days
//! (09-15, 09-18, 09-19, 09-21, 09-22, and twice on 09-23), every time
//! for `ephemeral-storage` pressure on w-1 while a gate was building.
//! The kubelet ranks eviction candidates first by "usage exceeds
//! request", then by priority. Each gate requests 90Gi; the dev pod
//! requested none, so ANY usage — its /scratch target/ alone runs to
//! ~74G cold — put it at the head of the line, ahead of the gate that
//! was actually filling the disk. The operator's live session, both
//! builders' shells and the whole warm target died each time, while
//! a gate — the one thing a retry recovers — kept running.
//!
//! The return was slow for a second reason: with `fsGroup` and no
//! change policy, the kubelet recursively re-owns every file on the
//! 40Gi workspace PVC at each mount, and on 09-23 it logged
//! "VolumePermissionChangeInProgress … taking longer than expected".
//! The ownership is already right after the first mount, so
//! `OnRootMismatch` skips the walk.

use boss_testing::repo_root;

fn manifest() -> String {
    std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("boss-dev.yaml")
}

fn document<'a>(text: &'a str, kind: &str, name: &str) -> &'a str {
    text.split("\n---\n")
        .find(|d| d.contains(&format!("kind: {kind}\n")) && d.contains(&format!("name: {name}\n")))
        .unwrap_or_else(|| panic!("no {kind} named {name} in boss-dev.yaml"))
}

/// The `dev` container's block, up to the next container.
fn dev_container(deployment: &str) -> &str {
    let start = deployment
        .find("        - name: dev\n")
        .expect("the dev container is declared");
    let rest = &deployment[start..];
    let end = rest
        .find("        - name: postgres\n")
        .expect("postgres follows dev");
    &rest[..end]
}

#[test]
fn the_dev_container_requests_the_disk_it_uses() {
    let text = manifest();
    let deployment = document(&text, "Deployment", "boss-dev");
    let dev = dev_container(deployment);
    let requests = dev
        .lines()
        .find(|l| l.trim_start().starts_with("requests:"))
        .expect("the dev container declares requests");
    assert!(
        requests.contains("ephemeral-storage"),
        "the dev container must request ephemeral-storage, or it is the first pod \
         the kubelet evicts under disk pressure: {requests}"
    );
}

#[test]
fn the_dev_pod_outranks_a_gate_under_pressure() {
    let text = manifest();
    let deployment = document(&text, "Deployment", "boss-dev");
    let class = deployment
        .lines()
        .find_map(|l| l.trim().strip_prefix("priorityClassName: "))
        .expect("the dev pod names a priorityClassName");
    let pc = document(&text, "PriorityClass", class);
    let value: i64 = pc
        .lines()
        .find_map(|l| l.strip_prefix("value: "))
        .expect("the PriorityClass declares a value")
        .trim()
        .parse()
        .expect("value is an integer");
    assert!(
        value > 0,
        "gates run at the default 0; the dev pod must rank above them"
    );
    assert!(
        pc.contains("preemptionPolicy: Never"),
        "the class orders EVICTION only — it must never preempt a running gate at scheduling"
    );
    assert!(
        pc.contains("globalDefault: false"),
        "the class is opt-in; it must not become every pod's default"
    );
}

#[test]
fn the_workspace_is_not_reowned_on_every_mount() {
    let text = manifest();
    let deployment = document(&text, "Deployment", "boss-dev");
    assert!(
        deployment.contains("fsGroupChangePolicy: OnRootMismatch"),
        "without OnRootMismatch every restart walks the whole workspace PVC"
    );
}
