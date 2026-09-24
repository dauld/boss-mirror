//! Subject-existence validation for `POST /api/jobs`.
//!
//! Closes the "you can create a Job pointing at an account that
//! doesn't exist" hole captured in the create-Job UX walkthrough.
//! The handler asks the `subjects` table ("does account
//! `acc-bigseed-9999` exist?") before persisting the Job, returning
//! `400 Bad Request` for ghost ids instead of letting them land as
//! dangling references. The binary wires `PgSubjectExistence`
//! whenever it runs on Postgres; `None` skips the check (the
//! in-memory path, which has no identity table to ask).

use async_trait::async_trait;
use boss_core::job::Subject;

#[derive(Debug, thiserror::Error)]
pub enum SubjectExistenceError {
    #[error("subject not found: {0}")]
    NotFound(String),
    /// The check could not be answered (a storage error). The create
    /// handler fails CLOSED on it with a 503 (subject-model design Q2).
    #[error("upstream unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait SubjectExistenceCheck: Send + Sync {
    /// Return `Ok(())` if the subject exists, `NotFound` if it
    /// definitively doesn't, `Unavailable` if we couldn't tell.
    async fn check(&self, subject: &Subject) -> Result<(), SubjectExistenceError>;
}

/// The uniform adapter (subject-model design R1, approved
/// 2026-07-15): one indexed lookup against the `subjects` identity
/// table, for EVERY kind — platform, tenant-defined, and the
/// previously unreachable ones (vendor, campaign, product, …).
/// Replaces the five-endpoint HTTP prober: no per-kind URL
/// templates, no fall-through kinds, no cross-service fan-out.
/// A storage error maps to `Unavailable`, which the create handler
/// fails CLOSED on (Q2: abort by default).
#[cfg(feature = "postgres")]
pub struct PgSubjectExistence {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PgSubjectExistence {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl SubjectExistenceCheck for PgSubjectExistence {
    async fn check(&self, subject: &Subject) -> Result<(), SubjectExistenceError> {
        // Birth-by-job kinds (SubjectKind registry rows carrying
        // `metadata.birth = "job"`: `workflow`, `custom`) pass without
        // an identity row — the Job being created IS the subject's
        // birth record, and `create_job_at` mints the identity inside
        // the job-create transaction. A registry property, not a gate
        // bypass: domain kinds stay fail-closed.
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM subjects WHERE kind = $1 AND id = $2) \
             OR EXISTS(SELECT 1 FROM subject_kinds \
                        WHERE kind = $1 AND retired_at IS NULL \
                          AND metadata->>'birth' = 'job')",
        )
        .bind(&subject.kind)
        .bind(&subject.id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| SubjectExistenceError::Unavailable(e.to_string()))?;
        if exists {
            Ok(())
        } else {
            Err(SubjectExistenceError::NotFound(format!(
                "{}/{}",
                subject.kind, subject.id
            )))
        }
    }
}

#[cfg(test)]
pub mod test_helpers {
    //! In-memory implementation for unit tests. Pre-populated with
    //! a known-good id set; everything else is NotFound.

    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    /// Kind-keyed id sets, so tests can seed any Subject kind
    /// (including tenant-defined ones) without growing this struct.
    pub struct InMemorySubjectExistenceCheck {
        pub sets: Mutex<HashMap<String, HashSet<String>>>,
    }

    impl InMemorySubjectExistenceCheck {
        pub fn new() -> Self {
            Self {
                sets: Mutex::new(HashMap::new()),
            }
        }

        /// Seed an existing subject of any kind.
        pub fn with_subject(self, kind: &str, id: &str) -> Self {
            self.sets
                .lock()
                .unwrap()
                .entry(kind.to_string())
                .or_default()
                .insert(id.to_string());
            self
        }

        pub fn with_account(self, id: &str) -> Self {
            self.with_subject("account", id)
        }
        pub fn with_employee(self, id: &str) -> Self {
            self.with_subject("employee", id)
        }
        pub fn with_asset(self, id: &str) -> Self {
            self.with_subject("asset", id)
        }
    }

    impl Default for InMemorySubjectExistenceCheck {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl SubjectExistenceCheck for InMemorySubjectExistenceCheck {
        async fn check(&self, subject: &Subject) -> Result<(), SubjectExistenceError> {
            // Kinds the test never seeded pass through, so a test
            // exercises only the kinds it seeds. Seeded kinds answer
            // from their id set.
            let sets = self.sets.lock().unwrap();
            match sets.get(subject.kind.as_str()) {
                None => Ok(()),
                Some(set) if set.contains(&subject.id) => Ok(()),
                Some(_) => Err(SubjectExistenceError::NotFound(subject.id.clone())),
            }
        }
    }
}
