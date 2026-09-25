//! Both adapters stamp a new step with the plugin version that serves
//! its kind (backlog 82448947).
//!
//! The Pg step INSERT has always snapshotted `step_plugins`' active
//! version into a step written with `step_plugin_version = 0`, so a
//! plugin republished later does not change which bundle an open step
//! renders against. The port stated none of it and the in-memory
//! adapter stored the caller's zero, so a test against the port saw a
//! different step than production wrote. One body of assertions, run
//! against each adapter over the step-plugin registry it reads, so the
//! two cannot drift apart again without this file naming which one
//! moved (CLAUDE.md §9a).

use std::sync::Arc;

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::events::{STEP_CREATED, step_state_payload};
use boss_jobs::{
    InMemoryJobs, InMemoryStepPlugins, JobsRepository, PgJobs, PgStepPlugins, StepPluginRegistry,
    StepPluginSpec,
};
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

/// A kind a plugin is published for in the test, and one nothing
/// serves — neither exists in the migrations' own plugin rows.
const SERVED: &str = "emerald-inspection";
const UNSERVED: &str = "a-kind-no-plugin-serves";

fn job(id: JobId) -> Job {
    Job {
        id,
        kind: "field-service".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "SYS-1"),
        title: "Inspect".into(),
        owner_id: "emp-owner".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

async fn publish_a_version(registry: &dyn StepPluginRegistry) -> i32 {
    let spec = StepPluginSpec::draft(
        SERVED,
        "Emerald inspection",
        "qa",
        "emerald-inspection.js",
        serde_json::json!({}),
    );
    registry
        .create_draft(spec, &author(), Utc::now())
        .await
        .expect("draft");
    registry
        .publish(SERVED, &author(), Utc::now())
        .await
        .expect("publish")
        .version
}

async fn stored_version<R: JobsRepository>(repo: &R, step: &Step) -> i32 {
    repo.get_step(&step.id)
        .await
        .expect("read step")
        .expect("step exists")
        .step_plugin_version
}

async fn a_new_step_is_stamped_with_the_version_that_serves_its_kind<R: JobsRepository>(
    repo: &R,
    registry: &dyn StepPluginRegistry,
    adapter: &str,
) {
    // Two published versions, so the stamp cannot pass as a default 1.
    publish_a_version(registry).await;
    let active = publish_a_version(registry).await;
    assert_eq!(active, 2, "{adapter}: the fixture publishes v2");

    // Admission: the whole-packet path.
    let job_id = JobId::from_uuid(Uuid::new_v4());
    let admitted = Step::new(job_id, SERVED, "Inspect at admission", 0);
    let unserved = Step::new(job_id, UNSERVED, "Nothing serves this kind", 1);
    let mut chosen = Step::new(job_id, SERVED, "Carries its own version", 2);
    chosen.step_plugin_version = 7;
    let steps = vec![admitted.clone(), unserved.clone(), chosen.clone()];
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let step_events: Vec<_> = steps
        .iter()
        .map(|s| stamp.event(STEP_CREATED, step_state_payload(s)))
        .collect();
    repo.create_job_with_steps_at(&job(job_id), &steps, Utc::now(), &[], &step_events)
        .await
        .expect("admit the packet");

    assert_eq!(
        stored_version(repo, &admitted).await,
        active,
        "{adapter}: a step written at version 0 is stamped with the active plugin version"
    );
    assert_eq!(
        stored_version(repo, &unserved).await,
        0,
        "{adapter}: a kind no plugin serves stays at 0"
    );
    assert_eq!(
        stored_version(repo, &chosen).await,
        7,
        "{adapter}: a caller-supplied non-zero version wins over the lookup"
    );

    // The single-step path writes through the same rule.
    let added = Step::new(job_id, SERVED, "Added after admission", 3);
    repo.add_step_at(&added, Utc::now(), &[])
        .await
        .expect("add step");
    assert_eq!(
        stored_version(repo, &added).await,
        active,
        "{adapter}: add_step_at stamps as admission does"
    );

    // The stamp is a snapshot: retiring the plugin moves no existing
    // step, and a step written after it finds nothing to stamp.
    registry
        .retire(SERVED, &author(), Utc::now())
        .await
        .expect("retire");
    let after_retire = Step::new(job_id, SERVED, "Added after the retire", 4);
    repo.add_step_at(&after_retire, Utc::now(), &[])
        .await
        .expect("add step");
    assert_eq!(
        stored_version(repo, &admitted).await,
        active,
        "{adapter}: a stamped step keeps its version when the plugin moves"
    );
    assert_eq!(
        stored_version(repo, &after_retire).await,
        0,
        "{adapter}: with no active plugin the step is written at 0"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_in_memory_adapter_stamps_a_steps_plugin_version() {
    let registry = Arc::new(InMemoryStepPlugins::new());
    let repo = InMemoryJobs::new().with_step_plugins(registry.clone());
    a_new_step_is_stamped_with_the_version_that_serves_its_kind(&repo, &*registry, "in-memory")
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pg_adapter_stamps_a_steps_plugin_version() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let registry = PgStepPlugins::new(db.pool.clone());
    a_new_step_is_stamped_with_the_version_that_serves_its_kind(&repo, &registry, "postgres").await;
}
