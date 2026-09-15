//! The epoch restart deletes the simulated company and keeps the real
//! one — and the shadow lane (packet 508cc38c).
//!
//! Sim-ness is a property of the JOB, decided from the origin of the
//! request that opened it and immutable thereafter. Everything
//! associated with a simulated Job is simulated — including a real
//! operator clicking around one. A fake brew order does not become
//! real because somebody looked at it.
//!
//! That framing is what makes the trim simple. Deciding per EVENT gave
//! a Job a mixed history, which forced the trim to preserve any Job a
//! human had touched or risk orphaning steps and aborting the rebuild
//! on `steps_job_id_fkey`. Carrying the bit on the Job removes the
//! case rather than handling it: a Job's rows all share one fate, so
//! no partial deletion is possible.
//!
//! Why any of this exists: a lap rolled mid-session and took an entire
//! day's feedback corpus, filed by a real user, with it. Nothing
//! failed and nothing was logged; the Jobs were simply gone the next
//! time the board was read.

use boss_jobs::postgres::trim_epoch_audit_log;
use boss_testing::TestDb;

async fn seed_event(db: &TestDb, kind: &str, payload: serde_json::Value) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload)
         VALUES (gen_random_uuid(), $1, 'test', now(), $2)
         RETURNING id",
    )
    .bind(kind)
    .bind(payload)
    .fetch_one(&db.pool)
    .await
    .expect("seed audit row")
}

/// Seeds the row the way every production write does (expand/contract,
/// 20260915221611): `partition` is the fact, `simulated` the derived
/// not-real bool. The trim reads `partition`.
async fn seed_job(db: &TestDb, id: &str, kind: &str, partition: &str) {
    sqlx::query(
        "INSERT INTO jobs (id, kind, workflow_version, subject_kind, subject_id, title,
                           owner_id, status, priority, opened_on, partition, simulated)
         VALUES ($1::uuid, $2, 1, 'custom', '/x', 'T', 'emp-1', 'open', 'standard',
                 '2025-06-01', $3, $3 <> 'real')",
    )
    .bind(id)
    .bind(kind)
    .bind(partition)
    .execute(&db.pool)
    .await
    .expect("seed job");
}

/// An event on `job`. The `_simulated` marker on the event is
/// deliberately WRONG in several tests — the Job decides, not the
/// event.
async fn job_event(db: &TestDb, job: &str, event_says: bool) -> i64 {
    seed_event(
        db,
        "jobs.step.completed",
        serde_json::json!({ "job_id": job, "_simulated": event_says }),
    )
    .await
}

async fn surviving(db: &TestDb) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM audit_log ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .expect("read log")
}

#[tokio::test]
async fn a_simulated_job_goes_and_a_real_one_stays() {
    let db = TestDb::new().await;
    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;

    let feedback = "11111111-1111-1111-1111-111111111111";
    let brew = "22222222-2222-2222-2222-222222222222";
    seed_job(&db, feedback, "user-feedback", "real").await;
    seed_job(&db, brew, "morning-brew", "simulated").await;

    let real_created = seed_event(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": feedback, "_simulated": false }),
    )
    .await;
    let real_step = job_event(&db, feedback, false).await;
    let sim_created = seed_event(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": brew, "_simulated": true }),
    )
    .await;
    let sim_step = job_event(&db, brew, true).await;

    let trimmed = trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");
    assert_eq!(trimmed, 2);

    let left = surviving(&db).await;
    assert!(left.contains(&baseline));
    assert!(left.contains(&real_created) && left.contains(&real_step));
    assert!(!left.contains(&sim_created) && !left.contains(&sim_step));
}

/// The whole point of the simplification. A person completing a step
/// on a simulated Job does NOT rescue it — and crucially, does not
/// leave half a Job behind, which is what orphans steps and aborts the
/// rebuild.
#[tokio::test]
async fn a_person_acting_on_a_simulated_job_does_not_make_it_real() {
    let db = TestDb::new().await;
    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;

    let brew = "33333333-3333-3333-3333-333333333333";
    seed_job(&db, brew, "morning-brew", "simulated").await;

    let created = seed_event(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": brew, "_simulated": true }),
    )
    .await;
    let by_sim = job_event(&db, brew, true).await;
    // A real operator worked this simulated Job — the event honestly
    // records that it came from a person.
    let by_person = job_event(&db, brew, false).await;

    let trimmed = trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");
    assert_eq!(trimmed, 3, "the Job's whole history shares one fate");

    let left = surviving(&db).await;
    for id in [created, by_sim, by_person] {
        assert!(
            !left.contains(&id),
            "every row of a simulated Job goes together — a partial delete is what \
             orphans steps and aborts the jobs rebuild"
        );
    }
    assert!(left.contains(&baseline));
}

/// The mirror: a simulated ACTOR touching a real Job cannot delete it.
/// The dispatcher assigns and completes steps on Jobs a person opened,
/// and those rows must not disappear underneath the Job.
#[tokio::test]
async fn automation_acting_on_a_real_job_does_not_make_it_simulated() {
    let db = TestDb::new().await;
    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;

    let feedback = "44444444-4444-4444-4444-444444444444";
    seed_job(&db, feedback, "user-feedback", "real").await;

    let created = seed_event(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": feedback, "_simulated": false }),
    )
    .await;
    // The event claims simulated; the Job says otherwise and wins.
    let by_automation = job_event(&db, feedback, true).await;

    let trimmed = trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");
    assert_eq!(trimmed, 0);

    let left = surviving(&db).await;
    assert!(left.contains(&created) && left.contains(&by_automation));
}

/// Events with no Job — ledger postings, asset receipts — are not
/// Job-scoped, so they fall back to their own marker. Absence still
/// means keep: the conservative direction for a DELETE is to keep.
#[tokio::test]
async fn jobless_events_fall_back_to_their_own_marker() {
    let db = TestDb::new().await;
    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;

    let sim = seed_event(
        &db,
        "ledger.entry.posted",
        serde_json::json!({ "id": "je-1", "_simulated": true }),
    )
    .await;
    let real = seed_event(
        &db,
        "ledger.entry.posted",
        serde_json::json!({ "id": "je-2", "_simulated": false }),
    )
    .await;
    let unflagged = seed_event(
        &db,
        "assets.asset.received",
        serde_json::json!({ "id": "asset-1" }),
    )
    .await;

    trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");

    let left = surviving(&db).await;
    assert!(!left.contains(&sim));
    assert!(left.contains(&real));
    assert!(
        left.contains(&unflagged),
        "unflagged is kept, not destroyed"
    );
}

/// The shadow lane (packet 508cc38c, Q5): the sim never participates
/// in an experiment, so its epoch restart is not allowed to touch one.
/// A shadow packet's history survives the trim exactly as a real one's
/// does — even though its rows say `simulated = true` to an N-1 reader
/// (the derived bool is not-real, and the trim must read the word).
#[tokio::test]
async fn a_shadow_job_survives_the_sim_epoch_restart() {
    let db = TestDb::new().await;
    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;

    let shadow = "66666666-6666-6666-6666-666666666666";
    let brew = "77777777-7777-7777-7777-777777777777";
    seed_job(&db, shadow, "keg-return", "shadow").await;
    seed_job(&db, brew, "morning-brew", "simulated").await;

    let shadow_created = seed_event(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": shadow, "_partition": "shadow", "_simulated": true }),
    )
    .await;
    let shadow_step = seed_event(
        &db,
        "jobs.step.completed",
        serde_json::json!({ "job_id": shadow, "_partition": "shadow", "_simulated": true }),
    )
    .await;
    let sim_created = seed_event(
        &db,
        "jobs.job.created",
        serde_json::json!({ "id": brew, "_partition": "simulated", "_simulated": true }),
    )
    .await;
    // Jobless events carry their own marker: a shadow one is kept, a
    // simulated one goes, and a pre-shadow event with only the bool
    // still reads as the simulated company.
    let shadow_posting = seed_event(
        &db,
        "ledger.entry.posted",
        serde_json::json!({ "id": "je-s", "_partition": "shadow", "_simulated": true }),
    )
    .await;
    let sim_posting = seed_event(
        &db,
        "ledger.entry.posted",
        serde_json::json!({ "id": "je-1", "_partition": "simulated", "_simulated": true }),
    )
    .await;
    let old_sim_posting = seed_event(
        &db,
        "ledger.entry.posted",
        serde_json::json!({ "id": "je-2", "_simulated": true }),
    )
    .await;

    let trimmed = trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");
    assert_eq!(
        trimmed, 3,
        "the simulated company goes; the shadow lane stays"
    );

    let left = surviving(&db).await;
    assert!(left.contains(&shadow_created) && left.contains(&shadow_step));
    assert!(left.contains(&shadow_posting));
    assert!(!left.contains(&sim_created));
    assert!(!left.contains(&sim_posting) && !left.contains(&old_sim_posting));
}

#[tokio::test]
async fn the_baseline_is_untouchable() {
    let db = TestDb::new().await;
    let brew = "55555555-5555-5555-5555-555555555555";
    seed_job(&db, brew, "morning-brew", "simulated").await;

    let before = job_event(&db, brew, true).await;
    let baseline = seed_event(&db, "seed.marker", serde_json::json!({})).await;
    let after = job_event(&db, brew, true).await;

    trim_epoch_audit_log(&db.pool, baseline)
        .await
        .expect("trim");

    let left = surviving(&db).await;
    assert!(left.contains(&before), "below the baseline is seed data");
    assert!(left.contains(&baseline));
    assert!(!left.contains(&after));
}
