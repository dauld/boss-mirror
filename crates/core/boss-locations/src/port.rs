//! Hexagonal port: `LocationRepository` defines what the domain
//! needs from the Location persistence layer.

use async_trait::async_trait;
use boss_core::primitives::Location;

#[derive(Debug, thiserror::Error)]
pub enum LocationError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

/// Persistence port for the Locations registry.
///
/// Reads, plus the ONE write a tenant needs to declare its sites.
/// Authoring (edit / retire) lands when the admin UI does. The
/// methods below cover what downstream services + the read-side UI
/// need today:
///
/// - `get` returns a single Location by id (including retired
///   rows), so audit / history surfaces can resolve old ids.
/// - `exists_active` is the hot-path validation primitive — every
///   write that sets a `*_location_id` column on another table
///   calls this first.
/// - `list_for_kind` powers the "what locations of this kind
///   exist?" dropdown the admin / picker UIs will use.
/// - `children_of` powers hierarchy walks (region → city →
///   storefront → kitchen-zone, etc.).
/// - `batch_upsert` seeds rows insert-if-absent by id — the door
///   `boss tenant publish` sends `seeds/locations.toml` through
///   (backlog 1ec8312a, 2026-09-17: until then the only rows were
///   the schema's, so an adopter with two sites could not declare
///   them and the company's HQ was a platform row wearing its name).
#[async_trait]
pub trait LocationRepository: Send + Sync {
    /// Fetch a single Location by id. Returns `None` if no row
    /// matches. **Retired rows are returned** — callers that want
    /// to refuse retired ids should prefer `exists_active`.
    async fn get(&self, id: &str) -> Result<Option<Location>, LocationError>;

    /// True iff a non-retired Location with the given id exists.
    /// The hot-path validation primitive.
    async fn exists_active(&self, id: &str) -> Result<bool, LocationError>;

    /// All non-retired Locations of a given `kind`, ordered by
    /// `name` ascending. `kind` matches a Class registry code
    /// under `(subject_kind='location', member_attribute='kind')`
    /// — e.g. `"storefront"`, `"warehouse-zone"`, `"hq"`.
    async fn list_for_kind(&self, kind: &str) -> Result<Vec<Location>, LocationError>;

    /// Direct children of a parent Location. `parent_id = None`
    /// returns roots. Ordered by `name` ascending.
    async fn children_of(&self, parent_id: Option<&str>) -> Result<Vec<Location>, LocationError>;

    /// Insert every row whose `id` is not already present; a row
    /// whose id exists is left exactly as it was (never overwritten:
    /// a re-run of a tenant's publish must not clobber an operator's
    /// edit). Returns the count actually inserted. One transaction,
    /// so a parent listed after its child in the same batch lands.
    async fn batch_upsert(&self, rows: &[Location]) -> Result<u64, LocationError>;
}
