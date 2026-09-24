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
/// A reader of any service's `/health` flags a `storage` of
/// `"in-memory"` — the signature of a service accidentally built
/// without the `postgres` feature. (A fan-out census at
/// `/api/snapshot/capabilities` was planned for boss-observability and
/// never built; the crate retired on 2026-09-23, backlog 467175e7.)
///
/// A write-roundtrip probe reads the same field as
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
    /// The git commit this IMAGE was built from — `BOSS_BUILD_COMMIT`,
    /// read from the process environment first (the image's runtime
    /// stage sets it) and from compile time second (a dev build with
    /// the variable exported). `None` in a plain dev build. This is
    /// what lets the train's `converged` step prove the RUNNING cluster
    /// serves the merge commit: an image tag proves a push happened; a
    /// self-reported build commit proves the pod restarted onto the
    /// image built from it (fdff316c / 7e5ee013, decided 2026-08-19).
    ///
    /// WHY THE ENVIRONMENT AND NOT THE COMPILE. Until 2026-09-12 the
    /// Dockerfile set `ENV BOSS_BUILD_COMMIT` before `cargo build`, so
    /// `option_env!` baked it in — and because it changed on every
    /// train, every workspace crate recompiled on every train: the
    /// converge's own stamps read `build_s=522` for a train that
    /// touched no Rust at all. Set in the runtime stage instead, the
    /// value still names the tree the image was built from, and the
    /// compiled layer is invalidated only by what it compiles.
    pub commit: Option<&'static str>,
    /// The migration level of the database this process is serving
    /// from, READ on each `/health` request (design a5323701, backlog
    /// 7c298c34). Three states, because they mean three different
    /// things and must not collapse into one:
    ///
    /// - absent — this service does not report a schema (every service
    ///   but the jobs API today: build for the one reader that exists);
    /// - `null` — this service tried and could not read the ledger.
    ///   "Could not read" is never zero, and never a pass;
    /// - an object — what `schema_migrations` held at the moment of
    ///   the request, judged against the running build's own tree.
    ///
    /// WHY. `commit` alone says a build is RUNNING, not that its
    /// migrations have RUN: a reader comparing `commit` to a sha got
    /// YES the moment the binary rolled, even when the migration the
    /// change needed had not been applied — the false-green shape, and
    /// the incident `boss-cli/src/running.rs` carries in its header.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_or_null"
    )]
    pub schema: Option<Option<Schema>>,
}

/// A service's own judgement of its database's migration level against
/// the tree it was built from — see [`Capabilities::schema`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Schema {
    /// The full id (file name) of the highest applied migration, in
    /// migration order. The whole name rather than the numeric prefix:
    /// the legacy 2- and 3-digit prefixes are not unique on their own,
    /// and `schema_migrations.id` already holds the name. Directly
    /// comparable to `infra/postgres/schema/` at any sha. `None` when
    /// the ledger is empty.
    pub head: Option<String>,
    /// How many migrations in the running build's OWN tree the ledger
    /// does not hold. `0` is "migrated to my own build"; anything else
    /// is a build running ahead of its schema. A count of applied rows
    /// was rejected: it compares to nothing.
    pub pending: u32,
    /// The first pending migration in order, so the number explains
    /// itself. `None` exactly when `pending` is 0.
    pub first_pending: Option<String>,
}

/// Deserialize a PRESENT `schema` field — `null` included — as
/// `Some(_)`, so a `null` read back stays "could not read" instead of
/// folding into "not reported" (which `default` gives an absent field).
fn present_or_null<'de, D>(d: D) -> Result<Option<Option<Schema>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <Option<Schema> as serde::Deserialize>::deserialize(d).map(Some)
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
            commit: build_commit(),
            schema: None,
        }
    }

    /// The same snapshot, reporting a schema reading: `Some(schema)`
    /// when the ledger was read, `None` when the read failed — which
    /// serialises as `null`, never as an absent field or a zero.
    pub fn with_schema(self, reading: Option<Schema>) -> Self {
        Self {
            schema: Some(reading),
            ..self
        }
    }
}

/// The build commit as [`Capabilities::commit`] reports it: the runtime
/// variable when set and non-empty, else the compile-time one, else
/// none. Resolved once per process.
pub fn build_commit() -> Option<&'static str> {
    static COMMIT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    COMMIT
        .get_or_init(|| {
            resolve_build_commit(
                std::env::var("BOSS_BUILD_COMMIT").ok(),
                option_env!("BOSS_BUILD_COMMIT"),
            )
        })
        .as_deref()
}

/// PURE: the precedence [`build_commit`] applies. An empty runtime
/// value is "unset", not a commit.
pub fn resolve_build_commit(runtime: Option<String>, compiled: Option<&str>) -> Option<String> {
    runtime
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            compiled
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
        })
}

/// Standard `/health` payload every Boss `*-api` binary returns.
///
/// `status` is `"ok"` while the process is serving; `capabilities`
/// is the service's [`Capabilities`] snapshot. Build it with
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

#[cfg(test)]
mod schema_field_tests {
    use super::{Capabilities, Schema};

    fn caps() -> Capabilities {
        Capabilities::new("boss-test-api", "0.1.0", "postgres")
    }

    fn read(schema: Option<Schema>) -> serde_json::Value {
        serde_json::to_value(caps().with_schema(schema)).unwrap()
    }

    #[test]
    fn a_service_that_does_not_report_a_schema_omits_the_field() {
        let v = serde_json::to_value(caps()).unwrap();
        assert!(v.get("schema").is_none(), "{v}");
    }

    #[test]
    fn a_ledger_that_could_not_be_read_answers_null_never_zero() {
        let v = read(None);
        assert!(v.get("schema").is_some_and(|s| s.is_null()), "{v}");
    }

    #[test]
    fn a_read_ledger_names_head_pending_and_first_pending() {
        let v = read(Some(Schema {
            head: Some("20260924000000-a.sql".into()),
            pending: 1,
            first_pending: Some("20260924000001-b.sql".into()),
        }));
        assert_eq!(
            v["schema"],
            serde_json::json!({
                "head": "20260924000000-a.sql",
                "pending": 1,
                "first_pending": "20260924000001-b.sql",
            })
        );
    }

    #[test]
    fn all_three_states_survive_a_round_trip() {
        for original in [
            caps(),
            caps().with_schema(None),
            caps().with_schema(Some(Schema {
                head: None,
                pending: 3,
                first_pending: Some("00-extensions.sql".into()),
            })),
        ] {
            let text = serde_json::to_string(&original).unwrap();
            let back: Capabilities =
                serde_json::from_str(Box::leak(text.into_boxed_str())).unwrap();
            assert_eq!(back.schema, original.schema);
        }
    }
}

#[cfg(test)]
mod build_commit_tests {
    use super::resolve_build_commit;

    #[test]
    fn the_runtime_variable_wins_over_the_compiled_one() {
        assert_eq!(
            resolve_build_commit(Some("abc123\n".into()), Some("def456")),
            Some("abc123".into())
        );
    }

    #[test]
    fn an_empty_runtime_variable_is_unset_and_the_compiled_one_answers() {
        assert_eq!(
            resolve_build_commit(Some("  ".into()), Some("def456")),
            Some("def456".into())
        );
        assert_eq!(
            resolve_build_commit(None, Some("def456")),
            Some("def456".into())
        );
    }

    #[test]
    fn a_dev_build_with_neither_reports_none() {
        assert_eq!(resolve_build_commit(None, None), None);
        assert_eq!(resolve_build_commit(Some(String::new()), Some("")), None);
    }
}
