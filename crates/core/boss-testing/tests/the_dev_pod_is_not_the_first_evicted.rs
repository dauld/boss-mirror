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
//!
//! WHAT THIS DOES NOT BUY, measured 2026-09-24 (backlog 3f2a08ab): the
//! pod was evicted again at 01:27Z with both halves in force. Priority
//! orders pods only WITHIN the "exceeds its request" class, and the
//! pod's /scratch held ~430 GiB against a 100Gi request (the eviction
//! freed that much: 136 GiB free before, 567 after), while each gate
//! stayed inside its 90Gi. So whenever w-1 reaches the kubelet's 15%
//! line this pod is still first. The request and the class stay right —
//! they order the pod correctly the day its usage fits — but the
//! protection that holds today is keeping the node off that line: the
//! scratch floor, which since that date `infra/dev/wt-cargo` runs
//! before every build (`dev_scratch_reclaim_sh.rs`, `wt_cargo_sh.rs`).
//!
//! THE EVICTION MESSAGE IS NOT AN ATTRIBUTION (backlog 80f083bc,
//! measured 2026-09-25). The 01:27Z message named the postgres and
//! reclaim sidecars — 3400Ki and 124Ki, "request is 0" — and was read as
//! the cause. The kubelet builds that text by listing every container
//! whose OWN writable layer plus logs exceeds its OWN request, so with no
//! request any container writing a byte is listed, and a container whose
//! stats are missing is not: `dev` requested nothing on pods xrsjc and
//! wvsrg and is absent from both messages. The rank that chose this pod
//! compares the POD's whole usage — every container's layer and logs
//! plus the `scratch` and `pgdata` emptyDirs — with the SUM of its
//! containers' requests. So every container requests what it writes,
//! and the sum is held above what the pod was measured holding.
//!
//! AND NO CONTAINER DECLARES A DISK LIMIT. The kubelet takes the sum of
//! the containers' ephemeral-storage limits as the POD's ceiling and
//! measures the pod's whole usage against it, emptyDirs included: a 64Mi
//! limit on the reclaim sidecar alone would make ~64Mi the ceiling for a
//! pod holding ~100 GiB of /scratch, and evict the operator's session on
//! the kubelet's first pass after the roll. A limit on `dev` turns the
//! eviction ORDER into a hard kill at a fixed size — the same trade the
//! reclaim sidecar's comment defers for a scratch sizeLimit.

use boss_testing::repo_root;

/// What the pod held on 2026-09-25, in the kubelet's own accounting,
/// read without touching it: `du -s -B1 -x /scratch` = 99,474,665,472
/// bytes over nine per-worktree targets (the walk the kubelet does,
/// which counts each reflinked target at full size); pgdata 2,943,090,969
/// bytes in 191 databases plus 83,886,080 of WAL (`pg_database_size`,
/// `pg_ls_waldir`); and the dev container's writable layer at its
/// largest eviction reading, 12,324,396Ki (pod jzkgw). That is ~107.3
/// GiB — above the 100Gi the pod requested, so the live pod was already
/// in the "exceeds its request" class the day this was measured.
const MEASURED_POD_BYTES: u64 = 99_474_665_472 + 2_943_090_969 + 83_886_080 + 12_324_396 * 1024;

fn manifest() -> String {
    std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml"))
        .expect("boss-dev.yaml")
}

fn document<'a>(text: &'a str, kind: &str, name: &str) -> &'a str {
    text.split("\n---\n")
        .find(|d| d.contains(&format!("kind: {kind}\n")) && d.contains(&format!("name: {name}\n")))
        .unwrap_or_else(|| panic!("no {kind} named {name} in boss-dev.yaml"))
}

/// Every container of the Deployment, by name, with its block.
fn containers(deployment: &str) -> Vec<(&str, &str)> {
    let start = deployment
        .find("      containers:\n")
        .expect("the Deployment declares containers");
    let rest = &deployment[start..];
    let end = rest
        .find("\n      volumes:\n")
        .expect("volumes follow the containers");
    rest[..end]
        .split("\n        - name: ")
        .skip(1)
        .map(|block| (block.lines().next().unwrap_or_default().trim(), block))
        .collect()
}

/// The flow-mapping line a container declares for `key` (`requests:` or
/// `limits:`), e.g. `requests: {cpu: 50m, memory: 64Mi}`.
fn resource_line<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    block
        .lines()
        .map(str::trim_start)
        .find(|l| l.starts_with(key))
}

/// The ephemeral-storage quantity on a flow-mapping resource line.
fn ephemeral(line: &str) -> Option<&str> {
    line.split(['{', '}', ','])
        .filter_map(|kv| kv.split_once(':'))
        .find(|(k, _)| k.trim() == "ephemeral-storage")
        .map(|(_, v)| v.trim().trim_matches('"'))
}

/// A Kubernetes quantity in the binary suffixes this file uses, in bytes.
fn bytes(quantity: &str) -> u64 {
    let (digits, unit) = quantity.split_at(
        quantity
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(quantity.len()),
    );
    let n: u64 = digits
        .parse()
        .unwrap_or_else(|_| panic!("not a quantity: {quantity}"));
    n * match unit {
        "" => 1,
        "Ki" => 1 << 10,
        "Mi" => 1 << 20,
        "Gi" => 1 << 30,
        "Ti" => 1 << 40,
        other => panic!("unit {other} in {quantity} is not one this pin reads"),
    }
}

#[test]
fn every_container_requests_the_disk_it_writes() {
    let text = manifest();
    let deployment = document(&text, "Deployment", "boss-dev");
    let all = containers(deployment);
    let names: Vec<&str> = all.iter().map(|(n, _)| *n).collect();
    assert!(
        ["dev", "postgres", "reclaim"]
            .iter()
            .all(|n| names.contains(n)),
        "the container reader lost a container: {names:?}"
    );
    let missing: Vec<&str> = all
        .iter()
        .filter(|(_, block)| {
            resource_line(block, "requests:")
                .and_then(ephemeral)
                .is_none()
        })
        .map(|(n, _)| *n)
        .collect();
    assert!(
        missing.is_empty(),
        "every boss-dev container must request ephemeral-storage — the pod's rank \
         under disk pressure is its whole usage against the SUM of these; missing: {missing:?}"
    );
}

#[test]
fn no_container_limits_its_disk() {
    let text = manifest();
    let deployment = document(&text, "Deployment", "boss-dev");
    let limited: Vec<&str> = containers(deployment)
        .into_iter()
        .filter(|(_, block)| {
            resource_line(block, "limits:")
                .and_then(ephemeral)
                .is_some()
        })
        .map(|(n, _)| n)
        .collect();
    assert!(
        limited.is_empty(),
        "no boss-dev container may declare an ephemeral-storage LIMIT: the kubelet \
         sums them into the pod's ceiling and measures /scratch against it, so a \
         sidecar's small limit evicts the live session on the first pass: {limited:?}"
    );
}

#[test]
fn the_pod_requests_what_it_was_measured_holding() {
    let text = manifest();
    let deployment = document(&text, "Deployment", "boss-dev");
    let requested: u64 = containers(deployment)
        .iter()
        .filter_map(|(_, block)| resource_line(block, "requests:").and_then(ephemeral))
        .map(bytes)
        .sum();
    assert!(
        requested > MEASURED_POD_BYTES,
        "the pod requests {requested} bytes of ephemeral-storage, not more than the \
         {MEASURED_POD_BYTES} it was measured holding on 2026-09-25 — it is first in \
         line under pressure before any gate is"
    );
}

#[test]
fn the_quantity_reader_reads_the_units_this_file_uses() {
    assert_eq!(bytes("64Mi"), 64 << 20);
    assert_eq!(bytes("150Gi"), 150 << 30);
    assert_eq!(
        ephemeral(r#"requests: {cpu: "4", memory: 8Gi, ephemeral-storage: 100Gi}"#),
        Some("100Gi")
    );
    assert_eq!(ephemeral("limits: {cpu: \"16\", memory: 32Gi}"), None);
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
