//! Procurement — vendor CRM + intelligence, namespaced under
//! `boss-inventory` beside the parts and POs it buys. Backs /vendors
//! (the example tenant seeds its vendors from seeds/vendors.toml).
//!
//! Session 1 scope: contacts, interactions, account-team, contracts
//! — the Vendor Knowledge Base four-section plumbing. Jobs + plugins
//! + crawl intelligence land in subsequent sessions.

pub mod http;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;

pub use types::{
    NewVendorAccountTeamMember, NewVendorContact, NewVendorContract, NewVendorInteraction,
    VendorAccountTeamMember, VendorContact, VendorContract, VendorInteraction,
};
