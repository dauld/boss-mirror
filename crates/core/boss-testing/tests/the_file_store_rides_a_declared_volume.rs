//! The file_refs store is switched on, on a DECLARED volume, and every
//! pod that touches its bytes mounts the same one (backlog 6280be03,
//! measured 2026-09-23 on the tree at 509f165f).
//!
//! WHAT WAS TRUE. The content-addressed blob store (schema
//! 07-content.sql, crates/core/boss-content/src/files/, the /api/files
//! surface, its rebuilder and its e2e tests) was complete and OFF: the
//! config generator never wrote a `[files]` block, so boss-content-api
//! mounted its 503 "unconfigured" fallback, and boss-files-gc no-opped
//! nightly saying so in its own comment.
//!
//! WHY ONE ROOT IN FOUR FILES IS A PIN AND NOT A COLLAPSE. The root is
//! one fact — `/var/lib/boss/files` on the `boss-files` claim — read by
//! four consumers that cannot share a definition: the Deployment that
//! runs boss-content-api (and hands the generator BOSS_FILES_ROOT), the
//! files-gc CronJob that deletes bytes under it, the backup CronJob
//! that archives it, and the quickstart's compose file. Kubernetes has
//! no include; so the equality is held here (CLAUDE.md §9a), and the
//! entry that drifts is named. The path is also what boss-content-api
//! records as every row's `bucket`, so it must never move quietly.
//!
//! WHY EACH CRONJOB PINS ITSELF TO THE boss POD'S NODE. The claim is a
//! ReadWriteOnce Longhorn volume: it attaches to ONE node, and any
//! number of pods on that node may mount it. A CronJob pod scheduled
//! elsewhere would wedge in ContainerCreating on multi-attach — so both
//! declare required podAffinity to `app: boss` by hostname. What that
//! costs (a backup that waits while the boss pod is down) is stated in
//! the manifests.
//!
//! The backup's archive is EXECUTED, not read, by
//! backup_files_its_packet.rs; this file holds only the wiring.

use boss_testing::repo_root;

const FILES_ROOT: &str = "/var/lib/boss/files";
const CLAIM: &str = "boss-files";

const DEPLOYMENT: &str = "infra/cluster/manifests/boss.yaml";
const GC: &str = "infra/cluster/manifests/boss-files-gc.yaml";
const BACKUP: &str = "infra/cluster/manifests/boss-backup.yaml";
const COMPOSE: &str = "infra/oss-quickstart/docker-compose.yml";
const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The YAML document in `rel` of `kind` whose metadata names `name`.
fn doc(rel: &str, kind: &str, name: &str) -> String {
    read(rel)
        .split("\n---\n")
        .find(|d| {
            d.lines().any(|l| l.trim() == format!("kind: {kind}"))
                && d.lines().any(|l| l.trim() == format!("name: {name}"))
        })
        .unwrap_or_else(|| panic!("{rel} declares no {kind} named {name}"))
        .to_string()
}

/// A line carrying every one of `parts` (flow-style YAML puts a whole
/// mount or env entry on one line, which is how this tree writes them).
fn has_line(text: &str, parts: &[&str]) -> bool {
    text.lines().any(|l| parts.iter().all(|p| l.contains(p)))
}

/// A volume named `volume` backed by the claim.
fn mounts_the_claim(text: &str, volume: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    lines.iter().enumerate().any(|(i, l)| {
        l.trim() == format!("- name: {volume}")
            && lines[i + 1..]
                .iter()
                .take(3)
                .any(|n| n.contains(&format!("claimName: {CLAIM}")))
    })
}

fn pinned_to_the_boss_pod(text: &str) -> bool {
    text.contains("podAffinity:")
        && text.contains("requiredDuringSchedulingIgnoredDuringExecution:")
        && text.contains("topologyKey: kubernetes.io/hostname")
        && has_line(text, &["app: boss"])
}

#[test]
fn the_claim_is_declared_in_the_manifest_not_made_by_hand() {
    let pvc = doc(DEPLOYMENT, "PersistentVolumeClaim", CLAIM);
    assert!(
        pvc.contains("storageClassName: longhorn") && pvc.contains("ReadWriteOnce"),
        "{DEPLOYMENT}: the {CLAIM} claim must be a replicated Longhorn volume, declared here so \
         the converge creates it (an imperative change has an expiry):\n{pvc}"
    );
}

#[test]
fn the_content_service_runs_on_the_store() {
    let dep = doc(DEPLOYMENT, "Deployment", "boss");
    assert!(
        has_line(
            &dep,
            &["name: BOSS_FILES_ROOT", &format!("value: {FILES_ROOT}")]
        ),
        "{DEPLOYMENT}: the boss container must hand the config generator BOSS_FILES_ROOT={FILES_ROOT} \
         — without it the generator writes no [files] block and the store stays off"
    );
    assert!(
        mounts_the_claim(&dep, CLAIM),
        "{DEPLOYMENT}: the pod must carry a volume `{CLAIM}` backed by claimName {CLAIM}"
    );
    assert!(
        has_line(
            &dep,
            &[
                &format!("name: {CLAIM}"),
                &format!("mountPath: {FILES_ROOT}")
            ]
        ),
        "{DEPLOYMENT}: the boss container must mount {CLAIM} at {FILES_ROOT}, or the bytes land \
         on the container's writable layer and die with it"
    );
}

#[test]
fn files_gc_sweeps_the_same_store() {
    let gc = read(GC);
    assert!(
        has_line(&gc, &["name: BOSS_FILES_ROOT"]) && gc.contains(&format!("value: {FILES_ROOT}")),
        "{GC}: files-gc generates its config the same way, so it needs BOSS_FILES_ROOT={FILES_ROOT} \
         to have a [files] block to sweep"
    );
    assert!(
        mounts_the_claim(&gc, CLAIM)
            && has_line(
                &gc,
                &[
                    &format!("name: {CLAIM}"),
                    &format!("mountPath: {FILES_ROOT}")
                ]
            ),
        "{GC}: a sweep that deletes under {FILES_ROOT} without the volume mounted there deletes \
         nothing and reports success"
    );
    assert!(
        pinned_to_the_boss_pod(&gc),
        "{GC}: {CLAIM} is ReadWriteOnce, so files-gc must be scheduled on the boss pod's node \
         (required podAffinity to app: boss by kubernetes.io/hostname)"
    );
}

#[test]
fn the_backup_reads_the_same_store() {
    let backup = read(BACKUP);
    assert!(
        mounts_the_claim(&backup, "file-store")
            && has_line(
                &backup,
                &[
                    "name: file-store",
                    &format!("mountPath: {FILES_ROOT}"),
                    "readOnly: true"
                ]
            ),
        "{BACKUP}: the backup must mount {CLAIM} read-only at {FILES_ROOT}"
    );
    assert!(
        pinned_to_the_boss_pod(&backup),
        "{BACKUP}: {CLAIM} is ReadWriteOnce, so the backup must be scheduled on the boss pod's \
         node (required podAffinity to app: boss by kubernetes.io/hostname)"
    );
}

/// The quickstart gets the store too, on a named volume — the same way
/// it already keeps Postgres and the credential store across restarts.
#[test]
fn the_quickstart_keeps_its_files_on_a_named_volume() {
    let compose = read(COMPOSE);
    assert!(
        has_line(&compose, &["BOSS_FILES_ROOT:", FILES_ROOT]),
        "{COMPOSE}: boss-services must name BOSS_FILES_ROOT={FILES_ROOT}"
    );
    assert!(
        has_line(&compose, &[&format!("- {CLAIM}:{FILES_ROOT}")]),
        "{COMPOSE}: boss-services must mount the named volume {CLAIM} at {FILES_ROOT}"
    );
    assert!(
        compose.lines().any(|l| l == format!("  {CLAIM}:")),
        "{COMPOSE}: the top-level volumes: must declare {CLAIM}"
    );
    // A named volume copies the ownership of the directory it mounts
    // over; absent from the image, Docker creates it root-owned and
    // the boss uid cannot write an upload.
    assert!(
        has_line(&read(DOCKERFILE), &["mkdir -p", FILES_ROOT]),
        "{DOCKERFILE}: the image must create {FILES_ROOT} (chowned with the rest of /var/lib/boss) \
         so the named volume inherits the boss uid"
    );
}
