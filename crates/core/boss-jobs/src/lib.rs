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

pub mod agent_budget;
pub mod agent_runs;
pub mod agent_spec;
pub mod agents;
pub mod audience;
pub mod bootstrap;
pub mod borders;
pub mod bundle_seed;
pub mod cadence;
pub mod cadence_seed;
pub mod calendar_hook;
pub mod car;
pub mod channels;
pub mod corrections;
pub mod credentials;
pub mod decision_record;
pub mod delivery;
pub mod delivery_policy_seed;
pub mod department;
pub mod dispatcher_firings;
pub mod escalation;
pub mod estate_seed;
pub mod events;
pub mod experiments;
pub mod flake;
pub mod http;
pub mod human_only;
pub mod in_memory;
pub mod job_edges;
pub mod jobs_config;
pub mod landing;
pub mod metadata_containment;
pub mod metadata_key;
pub mod orphan_steps;
pub mod policy_glue;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod probe;
pub mod protocol_conversion;
#[cfg(feature = "postgres")]
pub mod rebuild;
pub mod refusals;
pub mod regions;
pub mod registry;
pub mod repin;
pub mod scheduling;
pub mod station_flow;
pub mod station_lint;
pub mod station_projection;
pub mod station_quarantine;
pub mod station_queue;
pub mod station_reach;
pub mod station_seed;
pub mod stations;
pub mod step_metadata_write;
pub mod stranded;
pub mod trust;
pub mod workflow_lint;
pub mod workflow_quarantine;
pub mod workflow_seed;
pub mod yard;
// Platform Workflows live in `infra/platform/workflows/<kind>.toml`
// (`registry::platform_workflows()`, the Rust roster, is empty since
// 2026-09-11); tenant Workflows live in
// `examples/<tenant>/seeds/workflows.toml`. Both load via `seed_loader`.
// See docs/design/platform-vs-tenant-jobkinds.md.
pub mod owner_resolution;
pub mod seed_loader;
pub mod sensors;
pub mod step_plugin_seed;
pub mod step_plugins;
pub mod step_registry;
pub mod subject_existence;
pub mod surface_opens;

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
