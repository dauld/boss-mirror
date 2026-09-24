//! Marketing Asset KB — files + metadata for marketing-authored
//! content, a KB entity kind in `boss-catalog`. The example tenant
//! turns it on (`marketing-assets` in its tenant.toml modules).
//!
//! Session 2 scope: schema, CRUD adapter, HTTP surface. Retire +
//! supersedes (D9 event-sourced versioning) + KB four-section render
//! are covered here; "Insights" aggregations (download counts,
//! campaigns used in) are wired on when the asset-download event
//! stream lands (session 3).

pub mod http;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;

pub use types::{AssetKind, MarketingAsset, NewMarketingAsset, UpdateMarketingAsset};
