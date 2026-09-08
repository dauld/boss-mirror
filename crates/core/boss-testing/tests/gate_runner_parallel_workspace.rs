//! Parallel gates are safe because the workspace is per-run — pin it.
//!
//! THE INCIDENT CLASS (packet 28de3845). Gates were strictly serial:
//! the runner Job mounted one shared RWO PVC as /gate-target, run.sh
//! wiped it per run, and `boss gate` refused a second launch. Nothing
//! but that refusal (and scheduling luck) stood between two concurrent
//! gates and 2026-08-24's crossed receipts — a receipt naming one
//! branch's head reported under another, all three results discarded.
//! On 2026-09-02 nine cars serialized through the one runner at ~14
//! minutes each; gating was the pipeline's bottleneck.
//!
//! The shape that ends it: /gate-target becomes a per-run emptyDir
//! (born with the pod, dies with it — isolation is structural, not
//! guarded), and the PVC survives only as the WARM SEED: a snapshot of
//! a target/ built at main's tip, copied into each run's workspace,
//! plus the crate cache. Every property that makes that safe and fast
//! is pinned here, because each one decays into a real, named incident
//! if it drifts:
//!
//! - workspace on a PVC again: crossed receipts (2026-08-24)
//! - seed mount gone: every gate cold, ~74G / 20+ min of rebuild
//!   (measured, boss-dev.yaml)
//! - crate cache per-run: verdicts bet on static.crates.io; a green
//!   branch was called red by the network on 2026-08-27
//! - unbounded workspace disk: "No space left on device" turned into
//!   fake code failures (2026-08-23)
//! - unlocked seed read/refresh: a torn seed — half-copied rlibs under
//!   fresh fingerprints, reds that are nobody's code

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn manifest() -> String {
    read("infra/gate-runner/gate-runner.yaml")
}

fn run_sh() -> String {
    read("infra/gate-runner/run.sh")
}

/// run.sh with comment lines dropped — the pins below are about what
/// the script DOES, and a comment explaining the old world must stay
/// legal prose (same rule as run_sh_verdict.rs).
fn printed_run_sh() -> String {
    run_sh()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Job document out of the multi-doc manifest.
fn job_doc() -> String {
    manifest()
        .split("\n---")
        .find(|doc| doc.contains("kind: Job"))
        .expect("gate-runner.yaml carries a Job document")
        .to_string()
}

/// The volume name mounted at `mount_path` in the gate container,
/// read from the inline mount style the manifest uses
/// (`- {name: x, mountPath: /y}`).
fn volume_mounted_at(job: &str, mount_path: &str) -> Option<String> {
    job.lines()
        .filter(|l| l.contains("mountPath:"))
        .find(|l| {
            l.split("mountPath:")
                .nth(1)
                .map(|rest| {
                    rest.trim_start()
                        .trim_end_matches('}')
                        .split([',', ' '])
                        .next()
                        == Some(mount_path)
                })
                .unwrap_or(false)
        })
        .and_then(|l| {
            let after = l.split("name:").nth(1)?;
            Some(
                after
                    .trim_start()
                    .split([',', '}'])
                    .next()?
                    .trim()
                    .to_string(),
            )
        })
}

/// What backs a named volume in the Job's `volumes:` list.
fn volume_backing(job: &str, name: &str) -> String {
    let mut in_entry = false;
    for line in job.lines() {
        let t = line.trim_start();
        if t.starts_with("- name:") {
            in_entry = t.split(':').nth(1).map(str::trim) == Some(name);
            continue;
        }
        if in_entry {
            if t.starts_with("emptyDir") {
                return "emptyDir".into();
            }
            if t.starts_with("persistentVolumeClaim") {
                return "persistentVolumeClaim".into();
            }
            if t.starts_with("configMap") || t.starts_with("secret") {
                return "other".into();
            }
        }
    }
    format!("volume `{name}` not found in the Job's volumes list")
}

/// THE ISOLATION PIN. Two gates that can see each other's workspace
/// cross their receipts; two that cannot are safe by construction.
#[test]
fn the_workspace_is_per_run_and_the_pvc_is_only_a_seed() {
    let job = job_doc();

    let workspace = volume_mounted_at(&job, "/gate-target")
        .expect("the gate container must mount a workspace at /gate-target");
    assert_eq!(
        volume_backing(&job, &workspace),
        "emptyDir",
        "/gate-target must be a per-run emptyDir. On a PVC, two concurrent gates \
         yank the tree from under each other and write one receipt path — the \
         2026-08-24 crossed-receipts incident, made possible again."
    );

    let seed = volume_mounted_at(&job, "/gate-seed")
        .expect("the gate container must mount the warm seed at /gate-seed");
    assert_eq!(
        volume_backing(&job, &seed),
        "persistentVolumeClaim",
        "the seed must OUTLIVE the pod — an emptyDir seed is empty by definition, \
         and every gate goes cold: ~74G of target rebuilt, 20+ minutes each."
    );
}

/// THE DISK BOUND. A gate that can fill the node's disk manufactures
/// failures for every tenant of that node (2026-08-23: shared-disk
/// exhaustion read as code failures). The workspace emptyDir carries a
/// sizeLimit (kubelet evicts the one offender), and the gate container
/// requests ephemeral-storage (the scheduler admits only what fits —
/// the disk half of the concurrency bound, enforced by the node).
#[test]
fn the_workspace_disk_is_bounded() {
    let job = job_doc();
    let workspace = volume_mounted_at(&job, "/gate-target").expect("workspace mount exists");

    let mut in_entry = false;
    let mut limited = false;
    for line in job.lines() {
        let t = line.trim_start();
        if t.starts_with("- name:") {
            in_entry = t.split(':').nth(1).map(str::trim) == Some(workspace.as_str());
            continue;
        }
        if in_entry && t.contains("sizeLimit") {
            limited = true;
        }
    }
    assert!(
        limited,
        "the workspace emptyDir must carry a sizeLimit — without one, a runaway \
         run exhausts the NODE and every concurrent gate reds with it"
    );
    assert!(
        job.contains("ephemeral-storage"),
        "the gate container must request ephemeral-storage, so the scheduler \
         stops admitting gates when the node's disk cannot hold another workspace"
    );
}

/// Concurrent Jobs must be tellable apart in `kubectl get jobs` — a
/// refusal that names `gate-8kx2p` three times names nothing.
#[test]
fn concurrent_jobs_carry_the_branch_in_name_and_label() {
    let job = job_doc();
    assert!(
        job.contains("generateName: gate-$GATE_NAME_HINT-"),
        "the Job name must carry the branch hint (gate-<branch>-<rand>); \
         `boss gate` fills $GATE_NAME_HINT when it renders the manifest"
    );
    assert!(
        job.contains("boss.dev/branch: $GATE_NAME_HINT"),
        "a branch label makes `kubectl get jobs -l boss.dev/branch=...` answer \
         which gate is whose without parsing generated names"
    );
}

/// THE CRATE CACHE SURVIVES THE RUN — on the seed volume, not the
/// per-run workspace. This is a correctness fix before a speed one:
/// with a per-run CARGO_HOME every gate re-downloads every dependency,
/// and on 2026-08-27 a green branch was recorded as a clippy FAILURE
/// because static.crates.io dropped one fetch of crc32fast.
#[test]
fn the_crate_cache_lives_on_the_seed_volume() {
    let sh = printed_run_sh();
    assert!(
        sh.contains("/gate-seed/cargo")
            || sh.contains("$SEED/cargo")
            || sh.contains("${SEED}/cargo"),
        "CARGO_HOME must point at the seed volume so the registry cache outlives \
         the pod — a per-run cache bets every verdict on several hundred \
         consecutive crates.io fetches"
    );
}

/// A stale checkout renders the OLD manifest (no /gate-seed mount)
/// against the NEW ConfigMap script. That skew must cost speed, not
/// gates: run.sh degrades to a cold, per-run cache and says so.
#[test]
fn a_missing_seed_mount_degrades_to_cold_not_dead() {
    let sh = printed_run_sh();
    assert!(
        sh.contains(r#"[ -d "$SEED" ]"#),
        "run.sh must probe whether /gate-seed is mounted before using it"
    );
    assert!(
        sh.contains("/gate-target/cargo"),
        "the no-seed fallback must keep a usable CARGO_HOME on the workspace — \
         cold and loud beats dead"
    );
}

/// SEED READS AND SEED WRITES ARE LOCK-DISCIPLINED. Readers copy under
/// a shared flock; the refresher rewrites under an exclusive,
/// non-blocking one. Without this, a refresh racing a seeding copy
/// hands the new gate half-copied rlibs under fresh-looking
/// fingerprints — reds that are nobody's code, the exact class the
/// gate exists to never produce.
#[test]
fn seed_copy_and_refresh_take_opposite_locks() {
    let sh = printed_run_sh();
    assert!(
        sh.contains("flock -s"),
        "the seed copy must hold a SHARED lock — concurrent readers are fine, \
         a reader racing the refresher is not"
    );
    assert!(
        sh.contains("flock -x -n"),
        "the refresh must hold an EXCLUSIVE lock and skip when busy (-n): \
         refreshing is best-effort housekeeping, blocking a verdict is not its job"
    );
}

/// The refresh stages into target.partial and renames. A pod that dies
/// mid-refresh (w-1 has reset mid-gate before) must leave a MISSING
/// seed — the next gate runs cold, slow and correct — never a torn one.
#[test]
fn the_refresh_stages_then_renames_so_death_leaves_cold_not_torn() {
    let sh = printed_run_sh();
    assert!(
        sh.contains("target.partial"),
        "the refresh must copy into a staging dir, not into the live seed path"
    );
    let stage = sh.find("target.partial").expect("staging dir present");
    let swap = sh
        .rfind("mv \"$SEED/target.partial\" \"$SEED/target\"")
        .expect("the staged copy must be renamed into place as the last step");
    assert!(
        stage < swap,
        "staging must happen before the rename that publishes it"
    );
}

/// The seed refreshes only from a GREEN run at/near main's tip.
/// Refresh from a red run and the seed inherits a broken tree's
/// artifacts; refresh from a stale-based branch and every later gate
/// rebuilds the distance to main anyway.
#[test]
fn the_seed_refreshes_only_on_a_green_near_tip_run() {
    let sh = printed_run_sh();
    assert!(
        sh.contains(r#"[ "$VERDICT" = "green" ]"#),
        "the refresh must be gated on the verdict being green"
    );
    assert!(
        sh.contains("rev-list --count HEAD..origin/main"),
        "the refresh must measure distance to origin/main — 'near the tip' is a \
         count, not a feeling"
    );
}
