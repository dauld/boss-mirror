//! Boss jobs domain — universal coordination primitive.
//!
//! A Job is a bounded unit of coordinated work that crosses team
//! boundaries: device refurb, field service, procurement, sales,
//! marketing campaigns, employee onboarding. Each Job decomposes
//! into Steps — typed units of work owned by different people, with
//! optional sign-off gates and cross-job dependency tracking.
//!
//! Hexagonal: the domain defines a `JobsRepository` port (trait).
//! Postgres, in-memory, and other adapters implement the same trait.

pub mod bootstrap;
pub mod cadence;
pub mod calendar_hook;
pub mod car;
pub mod delivery;
pub mod escalation;
pub mod events;
pub mod experiments;
pub mod http;
pub mod in_memory;
pub mod job_edges;
pub mod jobs_config;
pub mod policy_glue;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod protocol_conversion;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod refusals;
pub mod registry;
pub mod scheduling;
pub mod station_lint;
pub mod station_projection;
pub mod station_quarantine;
pub mod station_queue;
pub mod stations;
pub mod workflow_lint;
pub mod workflow_quarantine;
// Platform Workflows live in `registry::platform_workflows()` (currently
// just `workflow-design`); tenant Workflows live in
// `examples/<tenant>/seeds/workflows.toml` and load via `seed_loader`.
// See docs/design/platform-vs-tenant-jobkinds.md.
pub mod owner_resolution;
pub mod seed_loader;
pub mod step_plugins;
pub mod step_registry;
pub mod subject_existence;

pub use in_memory::InMemoryJobs;
pub use port::{JobFilter, JobsError, JobsRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgJobs;
#[cfg(feature = "postgres")]
pub use rebuild::{RebuildError, RebuildReport, rebuild_jobs_and_steps};
#[cfg(feature = "postgres")]
pub use registry::PgWorkflows;
pub use registry::{
    InMemoryWorkflows, StepSpec, Terminal, WorkflowError, WorkflowRegistry, WorkflowSpec,
    WorkflowStatus, materialize_steps, reevaluate,
};
pub use station_queue::{DisciplineKey, StationPredicate, StationQueue, evaluate_station};
#[cfg(feature = "postgres")]
pub use stations::PgStations;
pub use stations::{
    InMemoryStations, StationCapability, StationError, StationKind, StationLens, StationRegistry,
    StationSpec, StationUpstream,
};
#[cfg(feature = "postgres")]
pub use step_plugins::PgStepPlugins;
pub use step_plugins::{InMemoryStepPlugins, StepPluginError, StepPluginRegistry, StepPluginSpec};
