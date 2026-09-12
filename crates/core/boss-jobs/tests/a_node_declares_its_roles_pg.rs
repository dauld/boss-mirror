//! A node's ROLES come off the estate read as registry data — the set
//! deploy-services' `units` mode derives a host's roster from (design
//! 9e3e093f; migration 202609120300-a-node-declares-its-roles.sql).
//!
//! Two facts are pinned here because `nodes.role` and `node_roles`
//! are one fact in two places until the second stack is gone (CLAUDE.md
//! §9a): boss-gcp's primary role stays `bastion` (the estate page keys
//! its jump route on it) while its declared roles are the four the
//! design named; and a node declaring nothing reads `roles: []`, never
//! its primary role copied in — the converge treats an empty set as "no
//! roster declared, install every row", and a copied-in `bastion` would
//! silently narrow that to nothing.

use boss_jobs::PgJobs;
use boss_jobs::port::JobsRepository;
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn boss_gcp_declares_four_roles_and_keeps_its_primary() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());

    let nodes = repo.list_estate_nodes().await.expect("estate nodes");
    let gcp = nodes
        .iter()
        .find(|n| n.id == "boss-gcp")
        .expect("boss-gcp is declared");
    assert_eq!(gcp.role, "bastion");
    assert_eq!(
        gcp.roles,
        vec![
            "legacy-stack".to_string(),
            "ml-batch-host".to_string(),
            "off-cluster-observer".to_string(),
            "wireguard-bastion".to_string(),
        ],
        "sorted, so two reads of the same registry compare equal"
    );

    let w1 = nodes
        .iter()
        .find(|n| n.id == "w-1")
        .expect("w-1 is declared");
    assert!(
        w1.roles.is_empty(),
        "a node that declares no roles reads an empty set, not its primary role: {:?}",
        w1.roles
    );
}
