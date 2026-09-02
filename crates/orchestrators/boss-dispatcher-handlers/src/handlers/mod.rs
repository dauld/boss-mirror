//! Step-completion side-effect handlers for the dispatcher.
//!
//! Each handler in this module is the dispatcher-native replacement
//! for one bridge in `crates/bridges/boss-*-sim-bridge`. The bridge
//! handlers live in the simulator process; the dispatcher handlers
//! live in the system. Same shape (read step metadata → build HTTP
//! body → POST/PUT) but the dispatcher handlers know nothing about
//! `SideEffectContext` or `SimOutput` — they make direct HTTP calls
//! to the public API surface, same as any real caller would.
//!
//! Migration scope per audit F15 (COMPLETE): all 13 step-completion
//! handlers live here, registered in main.rs, with matching rule
//! rows in `infra/dispatcher/rules.toml`. The boss-step-effects-runner
//! crate has been retired.

pub mod bill_payment_batch;
pub mod commerce_invoice_issue;
pub mod common;
pub mod docs_design_sweep;
pub mod docs_flush_queue;
pub mod estate_alarm;
pub mod estate_compare;
pub mod gate_resolve;
pub mod inventory_bill_approve;
pub mod inventory_overhead_absorb;
pub mod inventory_parts_consume;
pub mod inventory_parts_produce;
pub mod inventory_po_place;
pub mod inventory_receive;
pub mod jobs_auto_park;
pub mod jobs_clear_waiting;
pub mod jobs_complete_linked_step;
pub mod jobs_complete_step;
pub mod jobs_subjob_resolve;
pub mod ledger_bill_approve;
pub mod ledger_keg_deposit_settle;
pub mod ledger_payroll_run_submit;
pub mod ledger_tax_accrue;
pub mod ledger_tax_remit;
pub mod messages_expire_for_job;
pub mod messages_notify;
pub mod messages_notify_job_terminal;
pub mod network_census;
pub mod packaging_allocate;
pub mod people_hire;
pub mod people_terminate;
pub mod products_consume;
pub mod products_consume_from_invoice;
pub mod products_produce;
pub mod shipping_create;
pub mod webhook_notify;
