//! The conductor can launch a gate: design 128b5496 (David, 2026-09-12,
//! all three questions accepted as proposed) moves the train's Rust
//! checks from the forge's CI to a gate-run of the train branch on the
//! cluster, filed by the conductor when it opens the PR. Today the
//! conductor has no ServiceAccount, no token and no kubectl, so the
//! first car is the CAPABILITY, pinned here against the files that
//! grant it: the conductor's manifest binds a ServiceAccount to the
//! gates Role the dev pod already uses and mounts its token; the
//! cluster image carries kubectl (copied from the mirrored alpine/k8s
//! image the converge runner already uses — no public download, no sha to
//! hunt) and the gate-runner manifest `boss gate` renders.

use boss_testing::repo_root;

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn the_conductor_runs_as_a_service_account_bound_to_the_gates_role() {
    let m = read("infra/cluster/manifests/boss-conductor.yaml");
    assert!(
        m.contains("kind: ServiceAccount")
            && m.contains("name: boss-conductor\n  namespace: boss-dev"),
        "no boss-conductor ServiceAccount in boss-dev"
    );
    assert!(
        m.contains("serviceAccountName: boss-conductor"),
        "the Deployment must run as the boss-conductor ServiceAccount"
    );
    assert!(
        !m.contains("automountServiceAccountToken: false"),
        "the token must be mounted — a launcher without a token cannot create a Job"
    );
    assert!(
        m.contains("kind: RoleBinding") && m.contains("name: dev-session-gates\nsubjects:"),
        "the binding must grant the gates Role (jobs create/get/list/watch/delete, pods, pods/log, pvc) that infra/cluster/manifests/boss-dev.yaml declares"
    );
    assert!(
        m.contains("kind: ServiceAccount\n    name: boss-conductor\n    namespace: boss-dev"),
        "the binding's subject must be the conductor's ServiceAccount"
    );
}

#[test]
fn the_gates_role_the_conductor_binds_to_is_the_one_the_tree_declares() {
    let dev = read("infra/cluster/manifests/boss-dev.yaml");
    assert!(
        dev.contains("kind: Role\nmetadata:\n  name: dev-session-gates\n  namespace: boss-dev"),
        "dev-session-gates must still be declared in boss-dev.yaml — the conductor binds to it by name"
    );
}

#[test]
fn the_cluster_image_carries_kubectl_and_the_gate_runner_manifest() {
    let d = read("infra/oss-quickstart/Dockerfile");
    assert!(
        d.contains("COPY --from=10.20.0.15:3000/david/alpine-k8s:1.33.3 /usr/bin/kubectl /usr/local/bin/kubectl"),
        "kubectl must come from the mirrored alpine/k8s tag the converge runner already uses"
    );
    assert!(
        d.contains("COPY infra/gate-runner /opt/boss/infra/gate-runner"),
        "boss gate renders infra/gate-runner/gate-runner.yaml; the conductor's image must carry it"
    );
    let mirror = read("infra/forge/mirror-base-images.sh");
    assert!(
        mirror.contains("alpine-k8s:1.33.3"),
        "the tag the image copies kubectl from must be one the mirror list puts on the forge"
    );
}
