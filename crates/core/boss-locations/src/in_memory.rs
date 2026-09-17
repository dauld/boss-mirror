//! In-memory `LocationRepository` impl. Used by tests and for
//! service startup before the postgres feature is wired in.

use async_trait::async_trait;
use boss_core::primitives::Location;
use std::sync::RwLock;

use crate::port::{LocationError, LocationRepository};

/// Trivial in-memory store. Holds a snapshot of `Location` rows;
/// lookups are linear scans because the registry is tiny in
/// account (most tenants have <100 Locations) and tests don't
/// need indexing.
#[derive(Debug, Default)]
pub struct InMemoryLocations {
    rows: RwLock<Vec<Location>>,
}

impl InMemoryLocations {
    pub fn new(rows: Vec<Location>) -> Self {
        Self {
            rows: RwLock::new(rows),
        }
    }
}

#[async_trait]
impl LocationRepository for InMemoryLocations {
    async fn get(&self, id: &str) -> Result<Option<Location>, LocationError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        Ok(rows.iter().find(|l| l.id == id).cloned())
    }

    async fn exists_active(&self, id: &str) -> Result<bool, LocationError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        Ok(rows.iter().any(|l| l.id == id && l.retired_at.is_none()))
    }

    async fn list_for_kind(&self, kind: &str) -> Result<Vec<Location>, LocationError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        let mut out: Vec<Location> = rows
            .iter()
            .filter(|l| l.kind == kind && l.retired_at.is_none())
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn children_of(&self, parent_id: Option<&str>) -> Result<Vec<Location>, LocationError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        let mut out: Vec<Location> = rows
            .iter()
            .filter(|l| l.retired_at.is_none() && l.parent_id.as_deref() == parent_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn batch_upsert(&self, incoming: &[Location]) -> Result<u64, LocationError> {
        // Mirror the Postgres `ON CONFLICT (id) DO NOTHING`: an id
        // already present is left untouched; only new rows append.
        let mut rows = self.rows.write().expect("rwlock poisoned");
        let mut inserted: u64 = 0;
        for r in incoming {
            if !rows.iter().any(|l| l.id == r.id) {
                rows.push(r.clone());
                inserted += 1;
            }
        }
        Ok(inserted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn loc(id: &str, name: &str, kind: &str, parent_id: Option<&str>, retired: bool) -> Location {
        Location {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            parent_id: parent_id.map(String::from),
            timezone: "America/Los_Angeles".into(),
            latitude: None,
            longitude: None,
            address: None,
            account_id: None,
            metadata: json!({}),
            retired_at: if retired {
                Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            } else {
                None
            },
        }
    }

    #[tokio::test]
    async fn get_returns_retired_rows() {
        let repo = InMemoryLocations::new(vec![loc("loc-old", "Old HQ", "hq", None, true)]);
        let r = repo.get("loc-old").await.unwrap();
        assert!(r.is_some());
        assert!(r.unwrap().retired_at.is_some());
    }

    #[tokio::test]
    async fn exists_active_excludes_retired() {
        let repo = InMemoryLocations::new(vec![
            loc("loc-hq", "HQ", "hq", None, false),
            loc("loc-old", "Old HQ", "hq", None, true),
        ]);
        assert!(repo.exists_active("loc-hq").await.unwrap());
        assert!(
            !repo.exists_active("loc-old").await.unwrap(),
            "retired locations are not considered active"
        );
        assert!(!repo.exists_active("loc-bogus").await.unwrap());
    }

    #[tokio::test]
    async fn list_for_kind_filters_and_sorts_by_name() {
        let repo = InMemoryLocations::new(vec![
            loc("loc-c", "Cherry St Bakery", "storefront", None, false),
            loc("loc-a", "Almond Ave Bakery", "storefront", None, false),
            loc("loc-b", "Bay St Bakery", "storefront", None, false),
            loc("loc-hq", "HQ", "hq", None, false),
        ]);
        let out = repo.list_for_kind("storefront").await.unwrap();
        let names: Vec<&str> = out.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Almond Ave Bakery", "Bay St Bakery", "Cherry St Bakery"]
        );
    }

    #[tokio::test]
    async fn children_of_returns_direct_children_only() {
        // region(loc-west) -> city(loc-sf) -> storefront(loc-mission, loc-bay)
        // Plus a sibling region with its own storefront.
        let repo = InMemoryLocations::new(vec![
            loc("loc-west", "West Region", "field-region", None, false),
            loc(
                "loc-sf",
                "San Francisco",
                "field-region",
                Some("loc-west"),
                false,
            ),
            loc(
                "loc-mission",
                "Mission Storefront",
                "storefront",
                Some("loc-sf"),
                false,
            ),
            loc(
                "loc-bay",
                "Bay Storefront",
                "storefront",
                Some("loc-sf"),
                false,
            ),
            loc("loc-east", "East Region", "field-region", None, false),
        ]);

        let roots = repo.children_of(None).await.unwrap();
        let root_ids: Vec<&str> = roots.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(root_ids, vec!["loc-east", "loc-west"]);

        let sf_children = repo.children_of(Some("loc-sf")).await.unwrap();
        let sf_ids: Vec<&str> = sf_children.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            sf_ids,
            vec!["loc-bay", "loc-mission"],
            "only direct children, not transitive"
        );
    }

    /// Insert-if-absent by id, mirroring the Postgres `ON CONFLICT (id)
    /// DO NOTHING` (backlog 1ec8312a, 2026-09-17): a re-run of a
    /// tenant's publish leaves an existing row exactly as it was.
    #[tokio::test]
    async fn batch_upsert_inserts_if_absent_and_counts_only_new_rows() {
        let repo = InMemoryLocations::new(vec![loc("loc-hq", "HQ", "hq", None, false)]);
        let inserted = repo
            .batch_upsert(&[
                loc("loc-hq", "HQ renamed", "hq", None, false),
                loc("loc-lab", "Lab", "office", Some("loc-hq"), false),
            ])
            .await
            .unwrap();
        assert_eq!(inserted, 1, "the existing id is left untouched");
        assert_eq!(repo.get("loc-hq").await.unwrap().unwrap().name, "HQ");
        assert!(repo.exists_active("loc-lab").await.unwrap());
    }

    #[tokio::test]
    async fn children_of_excludes_retired() {
        let repo = InMemoryLocations::new(vec![
            loc("loc-hq", "HQ", "hq", None, false),
            loc(
                "loc-old-zone",
                "Old Zone",
                "warehouse-zone",
                Some("loc-hq"),
                true,
            ),
            loc(
                "loc-zone-a",
                "Zone A",
                "warehouse-zone",
                Some("loc-hq"),
                false,
            ),
        ]);
        let kids = repo.children_of(Some("loc-hq")).await.unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].id, "loc-zone-a");
    }
}
