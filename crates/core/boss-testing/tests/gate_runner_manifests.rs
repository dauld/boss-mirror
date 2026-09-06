//! The gate rig is infrastructure, and infrastructure that only exists
//! on a cluster is infrastructure nobody can reproduce.
//!
//! TWO FAILURES ON 2026-08-26, both silent, both found by reading.
//!
//! THE SCRIPT DRIFTED. The gate Job cannot run `run.sh` out of the
//! clone, because `run.sh` is what performs the clone — so it reaches
//! the pod through ConfigMap `gate-runner-script`. The manifest carried
//! the regeneration command in a COMMENT and asked whoever applied it
//! to remember. The live ConfigMap held the script from
//! `perf/the-gate-uses-the-cpu-it-was-given`, a branch measured 16%
//! slower and deliberately never parked, while main carried something
//! else. Ten gates ran that day and every receipt was produced by a
//! script that is not in this repository. CLAUDE.md §9a: a comment
//! asking the next person to keep two copies in sync is not a
//! mechanism. `apply-script-configmap.sh` is the mechanism, and
//! `--check` answers "is the thing that ran the thing I have?".
//!
//! THE MANIFEST WAS NEVER TRACKED AT ALL. The variant that made six
//! concurrent gates possible existed only in an agent job's scratch
//! directory — one `rm -rf` from taking the capability with it. It is
//! `gate-runner-local.yaml` now.
//!
//! What these tests defend is the DIFFERENCE between the two manifests.
//! It looks like duplication and invites unification, and unifying them
//! is exactly the mistake: a claimed workspace cannot be shared, and a
//! per-pod one is the only reason gates can run side by side.

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

const SHARED: &str = "infra/gate-runner/gate-runner.yaml";
const LOCAL: &str = "infra/gate-runner/gate-runner-local.yaml";
const APPLY: &str = "infra/gate-runner/apply-script-configmap.sh";
const CONFIGMAP: &str = "gate-runner-script";

/// The per-pod workspace is the whole reason concurrent gates are safe.
#[test]
fn the_local_runner_keeps_its_workspace_per_pod() {
    let local = read(LOCAL);
    assert!(
        local.contains("emptyDir: {sizeLimit:"),
        "{LOCAL} must give /gate-target an emptyDir. A claimed workspace cannot be shared \
         by two gates: each `git checkout -f -B` yanks the tree from under the other and \
         both write the same receipt path, which crossed three verdicts on 2026-08-24."
    );
    assert!(
        !local.contains("persistentVolumeClaim"),
        "{LOCAL} must not claim a volume — that is what {SHARED} is for"
    );
}

/// And the sibling must stay distinguishable, or someone unifies them
/// and quietly deletes the reason both exist.
#[test]
fn the_shared_runner_still_claims_its_disk() {
    assert!(
        read(SHARED).contains("persistentVolumeClaim"),
        "{SHARED} is the variant whose workspace outlives the Job. If it no longer claims \
         a volume, the two manifests have collapsed into one and the choice they encode \
         has been lost."
    );
}

/// THE BUILD ROLE IS A LABEL, NOT A MACHINE.
///
/// The scratch copy this was recovered from pinned
/// `kubernetes.io/hostname: w-1`. When w-1 was cordoned for a suspected
/// NVMe fault, a hard pin would have left the gate with no schedulable
/// node and stopped gating entirely; preferred affinity degrades to a
/// slower build on a control plane instead of no build at all.
#[test]
fn neither_runner_pins_a_hostname() {
    for rel in [SHARED, LOCAL] {
        let text = read(rel);
        assert!(
            !text.contains("kubernetes.io/hostname"),
            "{rel} pins a specific node. Replacing the build machine must be a label move, \
             not a car."
        );
        assert!(
            text.contains("boss.dev/purpose"),
            "{rel} must select the build node by role label"
        );
    }
}

/// The pair that actually drifted: the ConfigMap the Jobs mount and the
/// one the apply script creates have to be the same object.
#[test]
fn the_script_configmap_has_one_name_everywhere() {
    let apply = read(APPLY);
    assert!(
        apply.contains(CONFIGMAP),
        "{APPLY} must create the ConfigMap named {CONFIGMAP}"
    );
    for rel in [SHARED, LOCAL] {
        assert!(
            read(rel).contains(CONFIGMAP),
            "{rel} mounts a script ConfigMap that {APPLY} does not create — the gate would \
             run whatever happened to be in the cluster, which is the failure this file \
             exists for"
        );
    }
}

/// The mechanism has to be runnable, not merely present. A regeneration
/// step that is documented and not executable is the comment we already
/// had.
#[test]
fn the_apply_script_is_executable_and_offers_a_drift_check() {
    let path = repo_root().join(APPLY);
    let apply = read(APPLY);
    assert!(
        apply.contains("--check"),
        "{APPLY} must offer --check: the question that went unasked for a day is 'has the \
         live script drifted from the tree?', and it needs an answer that changes nothing"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("apply script exists")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "{APPLY} is not executable (mode {mode:o})"
        );
    }
}

/// The manifests must stop TELLING people to regenerate the ConfigMap
/// by hand and point at the thing that does it.
#[test]
fn the_manifests_point_at_the_mechanism_not_at_a_ritual() {
    for rel in [SHARED, LOCAL] {
        assert!(
            read(rel).contains("apply-script-configmap.sh"),
            "{rel} must name {APPLY} where it explains how the script reaches the pod. It \
             used to inline the kubectl command as a comment, and the copy in the cluster \
             drifted to an unlanded branch's version without anyone noticing."
        );
    }
}
