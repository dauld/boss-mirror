//! Hexagonal port: `ClassRepository` defines what the domain needs
//! from the Class registry's persistence layer.

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::primitives::{Class, ClassRef};
use boss_core::publisher::EventStamp;

/// The fact a tenant's declaration leaves: one per Class row the
/// batch INSERTED (backlog d9409039, 2026-09-17). Never per kept row
/// — a row the registry already held changed nothing, so there is
/// nothing to record — and never per batch: the rebuilders reproduce
/// rows, not requests.
pub const CLASS_DECLARED: &str = "class.declared";

/// Build the `class.declared` event for one inserted row: the row as
/// inserted, plus `declared_by` — the actor the request signed with,
/// read from the stamp so it is the same value `_actor` carries. One
/// builder for both adapters, so the in-memory double records exactly
/// what the Pg adapter stages on the outbox.
pub fn declared_event(stamp: &EventStamp, row: &Class) -> Result<Event, ClassError> {
    let mut payload = serde_json::to_value(row).map_err(|e| ClassError::Storage(e.to_string()))?;
    if let serde_json::Value::Object(map) = &mut payload {
        map.insert(
            "declared_by".to_string(),
            serde_json::Value::String(stamp.actor().to_string()),
        );
    }
    Ok(stamp.event(CLASS_DECLARED, payload))
}

#[derive(Debug, thiserror::Error)]
pub enum ClassError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0:?}")]
    NotFound(ClassRef),
    #[error("conflict: {0}")]
    Conflict(String),
    /// A class row named a subject_kind the SubjectKind registry
    /// doesn't carry — "Classes are typed reference data each
    /// Subject kind owns", so the kind must exist first. Enforced by
    /// the classes.subject_kind FK.
    #[error("unregistered subject kind: {0} — register it in the SubjectKind registry first")]
    UnregisteredKind(String),
}

/// Persistence port for the Class registry.
///
/// v1 is read-only: the registry seed lives in the migration, and
/// authoring (CRUD) lands when the admin UI does. The methods here
/// cover what downstream services and the read-side UI need today:
///
/// - `list_for_subject_kind` powers the admin list view and the
///   "what roles exist?" dropdown the create-employee form will eventually
///   replace its hardcoded options with.
/// - `get` returns a single Class by its composite key (including
///   retired rows), so audit / history surfaces can resolve old codes.
/// - `exists_active` is the fast write-time validation primitive that
///   replaces today's CHECK constraints (e.g. `employees.role IN (...)`).
#[async_trait]
pub trait ClassRepository: Send + Sync {
    /// All non-retired Classes for a given `subject_kind`, ordered by
    /// `sort_order` ascending then `code` ascending.
    async fn list_for_subject_kind(&self, subject_kind: &str) -> Result<Vec<Class>, ClassError>;

    /// Fetch a single Class by its composite (`subject_kind`, `code`)
    /// key. Returns `None` if no row matches. **Retired rows are
    /// returned** — callers that want to refuse retired codes should
    /// prefer `exists_active`.
    async fn get(&self, class_ref: &ClassRef) -> Result<Option<Class>, ClassError>;

    /// True iff a non-retired Class with the given `(subject_kind,
    /// code)` exists. The hot-path validation primitive.
    async fn exists_active(&self, class_ref: &ClassRef) -> Result<bool, ClassError>;

    /// Idempotent batch upsert of Class rows. Each row inserts with
    /// `ON CONFLICT (subject_kind, code) DO NOTHING`, mirroring the
    /// seed `classes.sql`'s semantics: re-running is a no-op, existing
    /// rows are left untouched. Returns the number of rows that were
    /// newly inserted (conflicts excluded).
    ///
    /// Every row inserted records one [`CLASS_DECLARED`] event built
    /// from `stamp` ([`declared_event`]) in the same transaction as
    /// the insert; a kept row records nothing. Until backlog d9409039
    /// (2026-09-17) this write left no audit-log fact at all, on the
    /// reasoning that `classes` is a reference table the rebuilders
    /// never touch — but "every state-changing operation publishes an
    /// event" (CLAUDE.md §Events) has no reference-table exception,
    /// and a tenant's declarations are exactly the state an operator
    /// later asks "when did this appear, and who put it there" about.
    async fn batch_upsert(&self, rows: &[Class], stamp: &EventStamp) -> Result<u64, ClassError>;

    /// Replace an existing Class's editable body — display name,
    /// parent, member attribute, metadata, sort order. Returns `false`
    /// if no row matches the composite key; the key itself is never
    /// rewritten, because a code is an identity that other rows point
    /// at (`employees.role`, `subject_edges.target_kind`) and renaming
    /// it in place would silently orphan them.
    ///
    /// Distinct from `batch_upsert` on purpose. That one is
    /// insert-if-absent by design — re-running a seed must not clobber
    /// operator edits — which also meant nothing could edit a Class at
    /// all once seeded. A registry the tenant cannot change is not
    /// data, it is a hardcoded list with extra steps, and the whole
    /// point of the Class registry (CLAUDE.md §9) is that taxonomies
    /// are tenant-editable without forking core.
    async fn update(&self, class: &Class) -> Result<bool, ClassError>;

    /// Withdraw a Class from active use by stamping `retired_at`. The
    /// row STAYS — existing rows point at the code (`employees.role`,
    /// step metadata), so retirement removes it from `list` and
    /// `exists_active` without orphaning anything. Idempotent: the
    /// first call stamps the timestamp, a repeat is a no-op that keeps
    /// the original stamp (when it was withdrawn is a fact). Returns
    /// `false` only when no row matches the composite key.
    async fn retire(&self, class_ref: &ClassRef) -> Result<bool, ClassError>;
}
