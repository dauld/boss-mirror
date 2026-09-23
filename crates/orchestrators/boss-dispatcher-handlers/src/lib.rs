//! Side-effect handlers for the boss-dispatcher engine.
//!
//! The core `boss-dispatcher` crate owns the generic, rule-driven engine
//! (the [`Handler`](boss_dispatcher::rules::handler::Handler) trait, the
//! rule registry, the durable NATS consumers, topic routing). This crate
//! owns the **handlers** — the actual side effects a step transition
//! produces — plus the binary that wires engine + handlers together.
//!
//! Handlers are generic mechanisms parameterized by data: they read step
//! metadata + live state and call the domain HTTP APIs. They do NOT import
//! the module crates, so tenant behavior stays in seed data and this
//! library stays decoupled. New handlers land here as the model grows —
//! core never changes.
//!
//! # Before you write a fact twice
//!
//! The shortcut this crate keeps reinventing is "write the fact at the
//! JOB level so a reader need not fetch its steps". It is not needed:
//! `GET /api/jobs?…` has always returned each row WITH its steps
//! embedded, so a reader of a listed packet already has them — and the
//! `exit` a handler wrote beside the execute step's `exit_code` for
//! exactly that reason was a second spelling nothing ever read
//! (backlog 50fede8b collapsed it). Two statements of one fact, written
//! by one act and held equal by nothing, is what CLAUDE.md §9a refuses.
//! The contract is held by
//! `crates/core/boss-jobs/tests/a_listed_packet_carries_its_steps.rs`,
//! which also carries the reasoning; read it before adding a job-level
//! copy of anything a step already states.
pub mod cascade;
pub mod handlers;
