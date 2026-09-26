//! A TICKET STAMPS ITS STEP ONCE — the Pg half (backlog 3977b3d2).
//!
//! The HTTP half (`a_presence_ticket_stamps_its_step_once`) drives the
//! A-B-A replay through the sign-off door on the in-memory adapter. This
//! file pins the same refusal where the Pg adapter writes the stamp:
//! under the row lock `append_sign_off` already takes for the shape
//! check (backlog 4174c4a9), a stamp whose presence nonce is on any
//! stamp of the step, live or voided, is refused and nothing is written.
//! The step's own stamps are the consumed-nonce record, so there is no
//! table to keep and no expiry to sweep.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, SignOffStamp, Step, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::port::{JobsError, JobsRepository};
use boss_jobs::{PgJobs, events};
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

const ROLE: &str = "platform-admin";

fn es() -> EventStamp {
    EventStamp::new("jobs", ActorId::Human("emp-1".into()))
}

fn stamp(step: &Step, nonce: &str) -> SignOffStamp {
    SignOffStamp {
        authority_id: "emp-david".into(),
        role: ROLE.into(),
        stamped_at: Utc::now(),
        shape_hash: step.shape_hash(),
        assurance: boss_core::job::Assurance::Presence,
        presence_nonce: Some(nonce.into()),
        voided_at: None,
        voided_by_event: None,
    }
}

async fn stamps(pool: &sqlx::PgPool, step: &Step) -> Vec<SignOffStamp> {
    let (v,): (Value,) = sqlx::query_as("SELECT sign_offs FROM steps WHERE id = $1")
        .bind(*step.id.inner().as_uuid())
        .fetch_one(pool)
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nonce_already_on_a_voided_stamp_is_refused_under_the_lock() {
    let db = TestDb::new().await;
    let repo = PgJobs::new(db.pool.clone());
    let j = Job {
        id: JobId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-000003977b01").unwrap()),
        kind: "ops-request".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "forge"),
        title: "An approval".into(),
        owner_id: "emp-1".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    let s = es();
    repo.create_job_at(
        &j,
        Utc::now(),
        &[s.event(events::JOB_CREATED, serde_json::to_value(&j).unwrap())],
    )
    .await
    .unwrap();
    let mut step = Step::new(j.id, "sign-off", "Approve the plan", 0)
        .with_sign_offs_required(vec![ROLE.into()]);
    step.status = StepStatus::Ready;
    step.metadata = json!({"authority_role": ROLE, "decision": "approved"});
    repo.add_step_at(
        &step,
        Utc::now(),
        &[s.event(events::STEP_CREATED, events::step_state_payload(&step))],
    )
    .await
    .unwrap();

    repo.append_sign_off(&step.id, &stamp(&step, "ceremony-nonce-7"), Utc::now(), &[])
        .await
        .unwrap();

    // A → B → A: the first edit voids the stamp, the second restores
    // the shape it signed.
    for decision in ["changes-requested", "approved"] {
        let patch = json!({"decision": decision});
        repo.merge_step_metadata_at(&step.id, patch.as_object().unwrap(), &es())
            .await
            .unwrap();
    }
    let row = stamps(&db.pool, &step).await;
    assert_eq!(row.len(), 1);
    assert!(row[0].voided_at.is_some(), "{row:?}");

    // The same nonce again, on the shape it was minted over.
    let replay = repo
        .append_sign_off(&step.id, &stamp(&step, "ceremony-nonce-7"), Utc::now(), &[])
        .await;
    match replay {
        Err(JobsError::NonceSpent { nonce, .. }) => assert_eq!(nonce, "ceremony-nonce-7"),
        other => panic!("a spent nonce must be refused under the lock, got {other:?}"),
    }
    assert_eq!(stamps(&db.pool, &step).await, row, "nothing was written");

    // A fresh ceremony's nonce stamps.
    repo.append_sign_off(&step.id, &stamp(&step, "ceremony-nonce-9"), Utc::now(), &[])
        .await
        .unwrap();
    let row = stamps(&db.pool, &step).await;
    assert_eq!(row.len(), 2, "{row:?}");
    assert!(row[1].voided_at.is_none());
}
