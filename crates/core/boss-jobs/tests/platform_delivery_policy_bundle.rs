//! The platform delivery-policy bundle — `infra/platform/delivery-policy/`,
//! one `<name>.toml` per policy — is well-formed and complete, read
//! with no database.
//!
//! WHY (backlog 393d3234, consolidation H4, car 4 of 4). Until
//! 2026-09-18 two migrations were the only place the delivery policy
//! was declared. The row now lives here: the seed publishes it
//! insert-if-missing by (name, version) at every start, and
//! `infra/lint/migrations-declare-schema-only.sh` refuses `INSERT INTO
//! delivery_policy` in any migration newer than the cutover. The
//! equality pin against the migrations' own row is
//! `the_delivery_policy_bundle_is_the_migrations_pg.rs`; this file
//! holds the rules that need no database and range over every file.
//!
//! One name, because there is one delivery pipeline; a second policy
//! is a file dropped in and a line here.

use boss_jobs::delivery::{
    DeliveryPolicyRegistry, DeliveryPolicyRepository, DeliveryPolicySpec, InMemoryDeliveryPolicy,
};
use boss_jobs::delivery_policy_seed::platform_delivery_policy_path;
use boss_jobs::seed_loader::{bundle_files, load_delivery_policies, parse_delivery_policies};
use std::path::Path;

fn bundle() -> Vec<DeliveryPolicySpec> {
    load_delivery_policies(platform_delivery_policy_path())
        .expect("the platform delivery-policy bundle parses")
}

/// One policy per file, and the file is named for its policy, so `ls`
/// answers "which policies does a deployment seed".
#[test]
fn the_bundle_is_one_file_per_policy() {
    let dir = Path::new(platform_delivery_policy_path());
    let files = bundle_files(dir).expect("the bundle directory lists its policy files");
    assert!(!files.is_empty(), "an empty bundle would prove nothing");
    for file in &files {
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("a policy file has a UTF-8 stem");
        let rows =
            load_delivery_policies(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let names: Vec<&str> = rows.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            [stem],
            "{} holds exactly its own policy",
            file.display()
        );
    }
    let names: Vec<String> = bundle().into_iter().map(|s| s.name().to_string()).collect();
    assert_eq!(
        names,
        ["train-conductor"],
        "the one delivery policy the migrations seeded, and no other"
    );
}

/// The bundled row publishes as-is into an empty registry at its
/// declared version, and is then what the conductor's read serves —
/// `train-conductor` lands at v2 with no v1 history nobody wrote.
#[tokio::test]
async fn every_bundled_policy_is_publishable_at_its_declared_version() {
    let registry = InMemoryDeliveryPolicy::default();
    let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
    let now = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;
    for spec in bundle() {
        let declared = spec.version();
        let name = spec.name().to_string();
        let published = registry
            .publish_declared(spec, &actor, now)
            .await
            .expect("a bundle row publishes");
        assert_eq!(
            published.version(),
            declared,
            "{name}: the declared version lands"
        );
        assert_eq!(
            registry.live_versions(&name).await.expect("versions").len(),
            1,
            "{name}: no synthetic history below the declared version"
        );
    }
    let served = registry
        .active_policy("train-conductor")
        .await
        .expect("the conductor's read")
        .expect("the published policy is served active");
    assert_eq!(
        (
            served.version,
            served.ci_host_floor_gb,
            served.gate_max_concurrent
        ),
        (2, 40, 3),
        "the row the conductor reads is the row the bundle declares"
    );
}

/// A file that declares anything but an active row, a version below
/// 1, or a column the table does not have is refused by the loader
/// with the policy named — a bundle row is what a fresh deployment
/// gets, and that is an active row.
#[test]
fn a_bundle_row_must_be_active_at_a_real_version() {
    let retired = "\
[[delivery_policy]]
name = \"x\"
version = 1
status = \"retired\"
max_red_trains = 2
stall_hours = 6
consist_budget_secs = 60
consist_output_budget = 1200
consist_files_named = 6
skip_reason_file_budget = 96
blip_cause_budget = 80
ci_host_floor_gb = 40
gate_max_concurrent = 3
";
    let err =
        parse_delivery_policies(retired, "x.toml").expect_err("a retired declaration is refused");
    assert!(err.to_string().contains("`x`"), "{err}");
    assert!(err.to_string().contains("retired"), "{err}");

    let err = parse_delivery_policies(
        &retired
            .replace("version = 1", "version = 0")
            .replace("retired", "active"),
        "x.toml",
    )
    .expect_err("version 0 is refused");
    assert!(err.to_string().contains("version"), "{err}");

    // The column H9 dropped (20260918102236): a file that still
    // declares it is refused rather than silently ignored.
    let err = parse_delivery_policies(
        &format!(
            "{}consist_excluded_lints = []\n",
            retired.replace("retired", "active")
        ),
        "x.toml",
    )
    .expect_err("a column the table does not have is refused");
    assert!(err.to_string().contains("consist_excluded_lints"), "{err}");

    // Every column is required: the table has no default for a budget,
    // and a file that leaves one out would be a row the seed cannot
    // write.
    let err = parse_delivery_policies(
        &retired
            .replace("retired", "active")
            .replace("blip_cause_budget = 80\n", ""),
        "x.toml",
    )
    .expect_err("a missing column is refused");
    assert!(err.to_string().contains("blip_cause_budget"), "{err}");
}
