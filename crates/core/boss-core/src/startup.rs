//! Boot-time helpers every BOSS service binary uses: the masked
//! database URL for logs, and the `/health` capability snapshot.
//!
//! THERE IS NO IN-MEMORY SERVING BRANCH. Until 2026-09-12 this module
//! carried `require_postgres_or_explicit_inmemory`, a guard for a
//! binary built without the `postgres` feature that would refuse to
//! serve from a process-local map unless `BOSS_ALLOW_INMEMORY=1` was
//! set. Measured (backlog be793304): nothing in the tree set that
//! variable, every service binary declares `required-features =
//! ["postgres"]` so cargo will not build one without the feature, and
//! infra/check-binary-build-coverage.sh classified REACHING the guard
//! as a defect. A branch whose sanctioned outcome is "never reached"
//! is not a safety net; it is code that cannot run. The thirteen
//! `cfg(not(feature = "postgres"))` arms that called it went with it,
//! and the two binaries that chose storage at RUNTIME from an optional
//! `postgres_url` (boss-jobs-api, boss-assets-api) now refuse to start
//! without one. The in-memory repositories themselves stay: they are
//! what the unit tests run against.

/// Redact the password in a database URL for safe logging.
///
/// `postgres://user:pass@host/db` → `postgres://user:***@host/db`.
/// Finds the **first** `:` after the `://` scheme separator, so a
/// password that itself contains `:` is masked in full (no prefix
/// leak). Returns the input unchanged when there's no userinfo
/// `password` segment to redact.
pub fn mask_password(url: &str) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(scheme_end), Some(at)) => {
            let scheme_end = scheme_end + 3;
            if at > scheme_end
                && let Some(colon) = url[scheme_end..at].find(':')
            {
                let user_end = scheme_end + colon;
                return format!("{}:***{}", &url[..user_end], &url[at..]);
            }
            url.to_string()
        }
        _ => url.to_string(),
    }
}

/// Self-reported capability snapshot returned by every service's
/// `/health` endpoint.
///
/// The aggregator at `/api/snapshot/capabilities` (boss-observability)
/// fans out, collects these, and flags any service whose `storage`
/// is `"in-memory"` — the signature of a service accidentally built
/// without the `postgres` feature.
///
/// `infra/check-service-write-roundtrip.sh` reads the same field as
/// a defense-in-depth check.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    /// Service name for log/audit reference (e.g. `"boss-docs-api"`).
    pub service: &'static str,
    /// `"postgres"` when the binary was compiled with the `postgres`
    /// feature, `"in-memory"` otherwise. The latter is a yellow flag
    /// in production — see `require_postgres_or_explicit_inmemory`.
    pub storage: &'static str,
    /// Crate version (`CARGO_PKG_VERSION`).
    pub version: &'static str,
    /// The git commit this binary was BUILT from — `BOSS_BUILD_COMMIT`
    /// at compile time, which the image build passes as a build arg.
    /// `None` in dev builds. This is what lets the train's `converged`
    /// step prove the RUNNING cluster binary serves the merge commit:
    /// an image tag proves a push happened; a self-reported build
    /// commit proves the pod restarted onto it (fdff316c / 7e5ee013,
    /// decided 2026-08-19).
    pub commit: Option<&'static str>,
}

impl Capabilities {
    /// Build capabilities for the calling service. The `service` arg
    /// is the binary name (`"boss-docs-api"`); the `version` arg is
    /// usually `env!("CARGO_PKG_VERSION")` at the call site. The
    /// `storage` arg is the storage backend the caller actually
    /// wired up — typically `"postgres"` from a
    /// `#[cfg(feature = "postgres")]` arm, `"in-memory"` from the
    /// fallback arm. The build commit is read here rather than at
    /// call sites so every service reports it identically for free.
    pub fn new(service: &'static str, version: &'static str, storage: &'static str) -> Self {
        Self {
            service,
            storage,
            version,
            commit: option_env!("BOSS_BUILD_COMMIT"),
        }
    }
}

/// Standard `/health` payload every Boss `*-api` binary returns.
///
/// `status` is `"ok"` while the process is serving; `capabilities`
/// is the [`Capabilities`] snapshot the aggregator at
/// `/api/snapshot/capabilities` fans out to collect. Build it with
/// [`health_response`] — the handler is a pure const response, so a
/// service's whole health triplet collapses to one call:
///
/// ```ignore
/// async fn health() -> axum::Json<boss_core::startup::HealthResponse> {
///     axum::Json(boss_core::startup::health_response(
///         "boss-docs-api",
///         env!("CARGO_PKG_VERSION"),
///         STORAGE,
///     ))
/// }
/// ```
///
/// Services that carry extra health fields (boss-clock's `mode`,
/// boss-cybernetics' `vm_id`/`timestamp`) or a flatter wire shape
/// (boss-classes/locations/subject-kinds) keep their own structs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HealthResponse {
    /// `"ok"` while the process is serving requests.
    pub status: &'static str,
    /// Self-reported capability snapshot for this service.
    pub capabilities: Capabilities,
}

/// Build the standard [`HealthResponse`] for the calling service.
///
/// `status` is fixed to `"ok"`; the args feed straight into
/// [`Capabilities::new`]. See [`HealthResponse`] for the call-site
/// shape.
pub fn health_response(
    service: &'static str,
    version: &'static str,
    storage: &'static str,
) -> HealthResponse {
    HealthResponse {
        status: "ok",
        capabilities: Capabilities::new(service, version, storage),
    }
}
