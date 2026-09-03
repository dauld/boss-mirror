//! Postgres-backed coverage for the credentials registry.
//!
//! What only a real database can answer: the migration applies, the
//! seeded rows are readable through the port every consumer reads
//! them through, the JSON-shape CHECKs refuse the rows they claim to
//! refuse, and the honest-gap posture survived the trip — the facts
//! tonight could not verify are seeded as marked-unverified, not as
//! guesses.

use boss_jobs::credentials::{CredentialsRegistry, PgCredentials};
use boss_testing::TestDb;

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
