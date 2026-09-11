//! `boss-gateway` library exports.
//!
//! The crate is primarily a binary (the gateway daemon), but a few
//! modules need to be reachable from sibling binaries (notably
//! `boss-auth`, the credentials-file CLI). Re-exporting via lib.rs
//! keeps the implementation in one place; main.rs just `use`s
//! these instead of redeclaring them.

pub mod audit;
pub mod break_glass;
pub mod local_auth;
pub mod mail;
pub mod oidc;
pub mod passkey;
pub mod session;
