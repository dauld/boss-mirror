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

/// Container blocks of the pod spec, as (name, block text).
///
/// Hand-scanned rather than YAML-parsed because the workspace carries no
/// YAML parser and one dependency for one lint is a poor trade; the
/// structure needed here is shallow. `volumes:` entries look exactly
/// like containers at this indentation, so the scan is bounded to the
/// `containers:` / `initContainers:` sections — a postgres native
/// sidecar lives in the latter, and a check that silently skipped it
/// would pass on the very container the warning named.
fn pod_containers(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent == 6 && trimmed.ends_with(':') {
            in_section = matches!(trimmed, "containers:" | "initContainers:");
            continue;
        }
        if !in_section {
            continue;
        }
        if indent == 8
            && let Some(name) = trimmed.strip_prefix("- name: ")
        {
            out.push((name.trim().to_string(), String::new()));
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    out
}

/// THE GATE MUST MEET THE RESTRICTED PROFILE, NOT WARN ABOUT IT.
///
/// Every launch printed `Warning: would violate PodSecurity
/// "restricted:latest"` — the postgres sidecar dropped only NET_RAW and
/// nothing set runAsNonRoot. A warning is the whole consequence while
/// the cluster default enforces `baseline`, which is what made this a
/// CLIFF rather than a slope: tighten that default, or label boss-dev
/// `enforce: restricted`, and every gate Job becomes uncreatable at
/// admission. The gate is the only path a car has to a green receipt,
/// so that is all delivery stopping at once, in a shape (pods refused
/// before any container starts) that reads like a code defect and gets
/// diagnosed as one (backlog bb5a902e).
///
/// `infra/gate.sh` never renders or applies this manifest, so nothing
/// in the gate's own check roster can catch a regression here — a car
/// that re-opened the gap would go green. This test is where that gets
/// read. It asserts the properties, not their formatting, so the
/// manifests stay free to explain themselves.
#[test]
fn both_runners_meet_the_restricted_profile() {
    for rel in [SHARED, LOCAL] {
        let text = read(rel);
        assert!(
            text.contains("runAsNonRoot: true"),
            "{rel} must set runAsNonRoot: true on the POD, so a container added later \
             inherits it instead of silently re-opening the gap"
        );
        assert!(
            text.contains("seccompProfile: {type: RuntimeDefault}"),
            "{rel} must keep the RuntimeDefault seccomp profile the restricted profile requires"
        );
        let containers = pod_containers(&text);
        assert!(
            containers.iter().any(|(n, _)| n == "gate")
                && containers.iter().any(|(n, _)| n == "postgres"),
            "{rel}: the scan found {:?}, not the gate + postgres pair — either a container was \
             renamed or this scan no longer sees the sections it must cover",
            containers.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        for (name, block) in &containers {
            assert!(
                block.contains("allowPrivilegeEscalation: false"),
                "{rel}: container {name} must set allowPrivilegeEscalation: false"
            );
            assert!(
                block.contains(r#"capabilities: {drop: ["ALL"]}"#),
                "{rel}: container {name} must drop ALL capabilities. NET_RAW-only was the \
                 right read of the postgres entrypoint's root-then-gosu hop and the wrong \
                 fix: name uid 999 and the root phase never happens, so nothing needs \
                 CHOWN/SETUID/SETGID"
            );
            let uid = block
                .lines()
                .find_map(|l| l.trim().strip_prefix("runAsUser: ").map(str::trim))
                .unwrap_or_else(|| {
                    panic!(
                        "{rel}: container {name} names no runAsUser. runAsNonRoot alone makes \
                         the kubelet REFUSE an image whose user is root, and boss-ci declares \
                         no USER — so the uid has to be explicit here or the pod never starts"
                    )
                });
            assert_ne!(
                uid, "0",
                "{rel}: container {name} runs as uid 0, which runAsNonRoot forbids"
            );
        }
    }
}

/// A named uid needs a writable HOME, and boss-ci has no passwd entry
/// for one.
///
/// `run.sh`'s first act is `git config --global credential.helper`,
/// under `set -e`: it writes `$HOME/.gitconfig`. containerd hands an
/// unresolvable uid `HOME=/`, which is not writable, so the gate would
/// die before the clone — on EVERY branch, with a message about git
/// config rather than about a uid. The fix has to be an env var in the
/// manifest and not a `mkdir` in run.sh, because the script reaches the
/// pod through a separately-applied ConfigMap: a manifest depending on
/// a run.sh change would be live before the change was.
#[test]
fn the_non_root_gate_is_given_a_writable_home() {
    for rel in [SHARED, LOCAL] {
        let text = read(rel);
        let (_, gate) = pod_containers(&text)
            .into_iter()
            .find(|(n, _)| n == "gate")
            .unwrap_or_else(|| panic!("{rel} has no container named gate"));
        assert!(
            gate.contains("{name: HOME, value: /gate-target}"),
            "{rel}: the gate container must set HOME to the workspace mount. It exists before \
             the container starts and fsGroup has already made it group-writable, which an \
             arbitrary path would not be"
        );
    }
}
