//! A node's ROLES come off the estate read as registry data — the set
//! install-units.sh's `units` mode derives a host's roster from (design
//! 9e3e093f; migration 202609120300-a-node-declares-its-roles.sql).
//!
//! Since backlog ee368d0c (2026-09-18) the rows are the TREE's
//! declaration, not a migration's: a fresh database declares nothing
//! until the launcher publishes infra/estate/estate.toml through
//! `declare_estate_nodes`, so every test here declares first, from the
//! same file, and asserts what the read gives back.
//!
//! Two facts are pinned here because `nodes.role` and `node_roles`
//! are one fact in two places until the second stack is gone (CLAUDE.md
//! §9a): boss-gcp's primary role stays `bastion` (the estate page keys
//! its jump route on it) while its declared roles are the three the
//! design left it; and a node declaring nothing reads `roles: []`, never
//! its primary role copied in — the converge treats an empty set as "no
//! roster declared, install every row", and a copied-in `bastion` would
//! silently narrow that to nothing.

use boss_core::publisher::EventStamp;
use boss_jobs::PgJobs;
use boss_jobs::port::{EstateNode, JobsRepository};
use boss_testing::TestDb;

/// The tree's declaration, landed the way the launcher lands it.
async fn declared(db: &TestDb) -> (PgJobs, Vec<EstateNode>) {
    let repo = PgJobs::new(db.pool.clone());
    let file = boss_testing::repo_root().join("infra/estate/estate.toml");
    let rows = boss_jobs::estate_seed::load_estate_toml(&file).expect("the tree's estate parses");
    assert!(!rows.is_empty(), "{} declares nodes", file.display());
    let stamp = EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("estate-seed".into()),
    );
    let out = repo
        .declare_estate_nodes(&rows, &stamp)
        .await
        .expect("the declaration lands");
    assert_eq!(out.inserted, rows.len(), "a fresh database takes every row");
    let nodes = repo.list_estate_nodes().await.expect("estate nodes");
    (repo, nodes)
}

/// A fresh database declares no machine: the seven rows 144 and
/// 202608301900 seeded are gone (20260918063829), and what the read
/// gives back after the declaration is exactly the tree's file.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_database_declares_nothing_until_the_tree_is_published() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    assert!(
        repo.list_estate_nodes().await.unwrap().is_empty(),
        "no node before the declaration"
    );
    let (repo, nodes) = declared(&db).await;
    let file = boss_testing::repo_root().join("infra/estate/estate.toml");
    let rows = boss_jobs::estate_seed::load_estate_toml(&file).unwrap();
    let mut declared_ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    declared_ids.sort_unstable();
    let mut read_ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    read_ids.sort_unstable();
    assert_eq!(read_ids, declared_ids);
    // A second publish of the same file changes nothing and leaves no
    // new fact.
    let stamp = EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("estate-seed".into()),
    );
    let again = repo.declare_estate_nodes(&rows, &stamp).await.unwrap();
    assert_eq!((again.inserted, again.roles_inserted), (0, 0));
    let facts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'node.declared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        facts as usize,
        rows.len(),
        "one node.declared per node landed, none for the re-publish"
    );
    // The identity rows ride the declaration, so a Job may name a node.
    let subjects: i64 = sqlx::query_scalar("SELECT count(*) FROM subjects WHERE kind = 'node'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(subjects as usize, rows.len());
}

/// Three roles since 2026-09-14 (d5941ef3 car 3): `legacy-stack` was
/// deleted from node_roles — the second stack is being retired, and the
/// bounded verb that stops it refuses while the declaration stands,
/// because the converge would re-enable the chores on its next tick.
/// The Class row for the role stays; no node declares it.
#[tokio::test(flavor = "multi_thread")]
async fn boss_gcp_declares_three_roles_and_keeps_its_primary() {
    let db = TestDb::new().await;
    let (_, nodes) = declared(&db).await;

    let gcp = nodes
        .iter()
        .find(|n| n.id == "boss-gcp")
        .expect("boss-gcp is declared");
    assert_eq!(gcp.role, "bastion");
    assert_eq!(
        gcp.roles,
        vec![
            "ml-batch-host".to_string(),
            "off-cluster-observer".to_string(),
            "wireguard-bastion".to_string(),
        ],
        "sorted, so two reads of the same registry compare equal — and no legacy-stack"
    );
    assert!(
        !nodes
            .iter()
            .any(|n| n.roles.iter().any(|r| r == "legacy-stack")),
        "no node declares legacy-stack any more; the role row is vocabulary only"
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

/// The forge declares `cluster-operator` — the role that makes it the
/// host cluster management runs on, so the workstation is a terminal
/// (design 1bc4b4ed, Q1 resolved "forge now" 2026-09-12). This pins
/// that the estate read returns it, which is what forge-converge reads
/// to decide whether to install talosctl and to look for the
/// credentials.
#[tokio::test(flavor = "multi_thread")]
async fn the_forge_declares_cluster_operator() {
    let db = TestDb::new().await;
    let (_, nodes) = declared(&db).await;
    let forge = nodes
        .iter()
        .find(|n| n.id == "forge")
        .expect("the forge is declared");
    assert_eq!(
        forge.role, "forge",
        "the primary role the estate page keys on is unchanged"
    );
    assert_eq!(forge.roles, vec!["cluster-operator".to_string()]);
}

/// A role the vocabulary does not hold is refused by the schema — the
/// Class registry stays the one place a role is defined (202609120300),
/// and nothing lands from a batch that names one.
#[tokio::test(flavor = "multi_thread")]
async fn a_role_that_is_not_a_class_of_node_is_refused_and_nothing_lands() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let stamp = EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("estate-seed".into()),
    );
    let bad = vec![boss_jobs::port::EstateNodeInput {
        id: "x-1".into(),
        label: "x-1".into(),
        address: "192.0.2.99".into(),
        role: "talos-worker".into(),
        roles: vec!["made-up-role".into()],
        cpu: None,
        memory_gb: None,
        disk_gb: None,
        notes: None,
    }];
    assert!(repo.declare_estate_nodes(&bad, &stamp).await.is_err());
    assert!(repo.list_estate_nodes().await.unwrap().is_empty());
}
