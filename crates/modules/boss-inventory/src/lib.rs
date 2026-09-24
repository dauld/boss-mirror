//! Inventory domain — parts inventory, stock levels, and purchase orders.
//!
//! Tracks on-hand / allocated stock for every part SKU, reorder
//! points, and purchase orders through their lifecycle.

pub mod http;
pub mod in_memory;
pub mod inventory_config;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod procurement;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod types;
pub mod warehouse_status;

pub use in_memory::InMemoryInventory;
pub use port::{InventoryError, InventoryRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgInventory;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_inventory};
pub use types::*;
pub mod events;
