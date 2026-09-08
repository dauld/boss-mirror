//! Postgres-backed coverage for the credentials registry.
//!
//! What only a real database can answer: the migration applies, the
//! seeded rows are readable through the port every consumer reads
//! them through, the JSON-shape CHECKs refuse the rows they claim to
//! refuse, and the honest-gap posture survived the trip — the facts
//! tonight could not verify are seeded as marked-unverified, not as
//! guesses.

use boss_core::publisher::EventStamp;
use boss_jobs::credentials::types::RotationPhase;
use boss_jobs::credentials::{CredentialsError, CredentialsRegistry, PgCredentials};
use boss_testing::TestDb;

fn stamp() -> EventStamp {
    EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation(
            "rule:broker-rotates-the-boss-dev-forge-token".into(),
        ),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_seeded_credentials_are_readable_through_the_port() {
    let db = TestDb::new().await;
    let repo = PgCredentials::new(db.pool.clone());
    let rows = repo.list().await.unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "boss-credential-broker-root",
            "boss-dev-forge-token",
            "boss-machine-token",
            "dev-session-token",
        ],
        "the four known credentials are seeded, ordered by id"
    );
    for r in &rows {
        assert!(
            r.rotated_at.is_none(),
            "{}: rotations that predate the registry live on their packets, \
             so no seed may claim a rotation instant",
            r.id
        );
        assert!(
            r.consumers.as_array().is_some_and(|a| !a.is_empty()),
            "{}: a credential with no recorded consumer is one nobody dares \
             revoke — every seed names at least one",
            r.id
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forge_write_token_row_answers_the_scope_question() {
    // The lookup that used to be an experiment.
    let db = TestDb::new().await;
    let repo = PgCredentials::new(db.pool.clone());
    let row = repo.get("boss-dev-forge-token").await.unwrap().unwrap();
    assert_eq!(row.kind, "forgejo-access-token");
    assert_eq!(row.scopes, serde_json::json!(["write:repository"]));
    assert_eq!(
        row.storage_location,
        "k8s Secret boss-dev/boss-dev-forge-token key token"
    );
    assert_eq!(row.rotation_policy, "on-demand");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unverified_scope_is_an_empty_array_with_a_note_not_a_guess() {
    // The broker root is the credential tonight's 403 was about: its
    // scope was never declared beyond "admin". The seed records the
    // gap and points at the audit, rather than inventing a scope
    // string nobody verified.
    let db = TestDb::new().await;
    let repo = PgCredentials::new(db.pool.clone());
    let row = repo
        .get("boss-credential-broker-root")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.scopes, serde_json::json!([]));
    assert!(
        row.notes.contains("scope unverified"),
        "the gap must name itself: {}",
        row.notes
    );
    assert!(
        row.notes.contains("audit fills this"),
        "and name what retires it: {}",
        row.notes
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_id_is_none_not_an_error() {
    let db = TestDb::new().await;
    let repo = PgCredentials::new(db.pool.clone());
    assert!(repo.get("no-such-credential").await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn non_array_scopes_and_consumers_are_refused_by_the_schema() {
    let db = TestDb::new().await;
    for (column, value) in [("scopes", "\"write:repository\""), ("consumers", "{}")] {
        let inserted = sqlx::query(&format!(
            "INSERT INTO credentials (id, kind, issuer, principal, {column}, \
             storage_location, rotation_policy) \
             VALUES ('bad-row', 'machine-token', 'x', 'y', '{value}'::jsonb, 'z', 'on-demand')"
        ))
        .execute(&db.pool)
        .await;
        assert!(
            inserted.is_err(),
            "{column} must be a JSON array — a scalar would silently break \
             every reader that iterates it"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_install_stamps_rotated_at_and_lands_one_event() {
    // The rotation door's write, end to end at the adapter: the
    // `credential.installed` event and the `rotated_at` stamp are one
    // transaction, sharing one instant.
    let db = TestDb::new().await;
    let repo = PgCredentials::new(db.pool.clone());
    let s = stamp();
    repo.record_rotation(
        "boss-dev-forge-token",
        RotationPhase::Installed,
        serde_json::json!({
            "credential_id": "boss-dev-forge-token",
            "job_id": "7ee101aa-3267-4745-8096-06d07df7e144",
            "value_length": 40,
        }),
        &s,
    )
    .await
    .unwrap();

    let row = repo.get("boss-dev-forge-token").await.unwrap().unwrap();
    // Postgres keeps microseconds; chrono keeps nanoseconds — compare
    // at the storage's own precision.
    assert_eq!(
        row.rotated_at.map(|t| t.timestamp_micros()),
        Some(s.timestamp.timestamp_micros()),
        "the row bind and the event share ONE instant (stamp.timestamp)"
    );

    let (kind, source, payload): (String, String, serde_json::Value) = sqlx::query_as(
        "SELECT kind, source, payload FROM event_outbox WHERE kind LIKE 'credential.%'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("exactly one credential.* outbox row");
    assert_eq!(kind, "credential.installed");
    assert_eq!(
        source, "jobs",
        "the emission path is the jobs service's rotation door — the \
         event_kinds rows declare the source the stamp actually writes"
    );
    assert_eq!(payload["value_length"], 40);
    assert_eq!(
        payload["_actor"],
        "automation:rule:broker-rotates-the-boss-dev-forge-token"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_mint_leaves_rotated_at_alone() {
    let db = TestDb::new().await;
    let repo = PgCredentials::new(db.pool.clone());
    repo.record_rotation(
        "boss-dev-forge-token",
        RotationPhase::Minted,
        serde_json::json!({ "token_name": "boss-dev-forge-token-7ee101aa" }),
        &stamp(),
    )
    .await
    .unwrap();
    let row = repo.get("boss-dev-forge-token").await.unwrap().unwrap();
    assert!(
        row.rotated_at.is_none(),
        "rotated_at records when the VALUE last changed — the install moment, \
         not the mint"
    );
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'credential.minted'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rotation_against_an_unknown_credential_records_nothing() {
    let db = TestDb::new().await;
    let repo = PgCredentials::new(db.pool.clone());
    let err = repo
        .record_rotation(
            "ghost-credential",
            RotationPhase::Installed,
            serde_json::json!({}),
            &stamp(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CredentialsError::UnknownCredential(id) if id == "ghost-credential"));
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind LIKE 'credential.%'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(n, 0, "no event may detach from the row it annotates");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_rotation_phase_kind_is_declared_in_event_kinds() {
    // §9a: the phase → kind mapping lives in Rust
    // (`RotationPhase::event_kind`) and the declarations live in
    // migration 202609031830 — this is the equality test that names
    // the offending entry when the two drift. A kind emitted but
    // undeclared re-arms the audit-integrity warning; a kind declared
    // but never emitted is the maiden rotation's hole in reverse.
    let db = TestDb::new().await;
    let declared: Vec<String> = sqlx::query_scalar(
        "SELECT kind_pattern FROM event_kinds \
         WHERE kind_pattern LIKE 'credential.%' ORDER BY kind_pattern",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read event_kinds");
    let mut expected: Vec<String> = RotationPhase::ALL
        .iter()
        .map(|p| p.event_kind().to_string())
        .collect();
    expected.sort();
    assert_eq!(declared, expected);

    let sources: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT source FROM event_kinds WHERE kind_pattern LIKE 'credential.%'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        sources,
        vec!["jobs".to_string()],
        "declared source must be the one the rotation door's stamp writes"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_made_up_rotation_policy_is_refused() {
    let db = TestDb::new().await;
    let inserted = sqlx::query(
        "INSERT INTO credentials (id, kind, issuer, principal, storage_location, \
         rotation_policy) VALUES ('bad-row', 'machine-token', 'x', 'y', 'z', 'whenever')",
    )
    .execute(&db.pool)
    .await;
    assert!(
        inserted.is_err(),
        "rotation_policy is on-demand | scheduled"
    );
}
