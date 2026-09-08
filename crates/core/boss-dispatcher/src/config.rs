//! Runtime configuration for boss-dispatcher.

use serde::Deserialize;
use tracing::warn;

/// How the dispatcher distributes a ready Step across the active holders
/// of its required role. Selected by data (`BOSS_DISPATCH_STRATEGY`), not
/// baked in — per the registries/data-over-hardcoded-paths rule, the
/// work-dispatch *behavior* is config-selectable. New strategies are named
/// here and gated in `pick_employee`, never forked into a caller's `match`.
///
/// Both variants are deterministic: the same (strategy, sorted roster,
/// step id) always selects the same employee, so an assignment replays
/// identically across a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssignmentStrategy {
    /// Legacy behavior: index 0 of the id-sorted candidates (the
    /// lowest-id holder). Kept selectable for parity/debugging.
    LowestId,
    /// Default: spread deterministically across the role's holders by a
    /// stable hash of the step id, so load fans out instead of piling
    /// onto one employee.
    #[default]
    Spread,
}

impl AssignmentStrategy {
    /// Parse the `BOSS_DISPATCH_STRATEGY` value. `"lowest-id"` → `LowestId`,
    /// `"spread"` → `Spread`. An unknown or empty value falls back to the
    /// default (`Spread`) with a `warn!` — a typo must not hard-fail the
    /// dispatcher, only nudge the operator. Case/whitespace-insensitive.
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "lowest-id" | "lowest_id" => Self::LowestId,
            "spread" => Self::Spread,
            "" => Self::default(),
            other => {
                warn!(
                    value = %other,
                    "unknown BOSS_DISPATCH_STRATEGY; falling back to default `spread`"
                );
                Self::default()
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DispatcherConfig {
    pub nats_url: String,
    pub jobs_api_url: String,
    pub people_api_url: String,
    pub inventory_api_url: String,
    pub commerce_api_url: String,
    pub products_api_url: String,
    pub shipping_api_url: String,
    pub ledger_api_url: String,
    pub messages_api_url: String,
    /// Docs service base URL — the `docs.flush_queue` handler POSTs
    /// its flush-jobs endpoint (a recorded decision queues its flush).
    pub docs_api_url: String,
    /// Clock service base URL. The schedule runner consumes its SSE tick
    /// feed (`GET /api/clock/ticks`) to drive sim-day-boundary firing of
    /// schedule-triggered rules.
    pub clock_api_url: String,
    /// Calendar service base URL. The schedule runner fetches the
    /// business calendars its schedule rules reference at startup.
    pub calendar_api_url: String,
    pub http_bind: String,
    /// Postgres URL — the dispatcher loads its rule registry (the
    /// append-only versioned `dispatcher_rules` table) from here at
    /// startup and serves it at `/api/dispatcher/rules`. Replaces the
    /// legacy `BOSS_DISPATCHER_RULES` rules.toml file path.
    pub postgres_url: String,
    /// External webhook URL for the `webhook.notify` handler to forward
    /// matched events to. `None` (the normal deployment) makes
    /// `webhook.notify` a no-op; a regen sets it to the brewery-engine's
    /// callback server so its CounterpartyEngine (banks, suppliers,
    /// courier, tax authority) reacts to live events as an external party.
    pub webhook_url: Option<String>,
    /// Which step-assignment distribution strategy the dispatcher applies
    /// (data-selected via `BOSS_DISPATCH_STRATEGY`, default `spread`).
    /// Threaded into `DispatcherCtx` so `pick_employee` can gate the
    /// index it takes into the sorted candidate list. `#[serde(skip)]`:
    /// the field is sourced from env in `Default`, not from a serde
    /// document, and the strategy enum is intentionally not `Deserialize`.
    #[serde(skip)]
    pub assignment_strategy: AssignmentStrategy,
    /// The credential broker's issuer endpoint — the forge whose
    /// tokens the `credential.rotate.forgejo` handler mints and
    /// revokes. Defaults to the deployment's own forge.
    pub broker_forge_url: String,
    /// The broker's Forgejo root credential (admin token). Sourced
    /// from the `boss-credential-broker-root` k8s Secret via env;
    /// `None` leaves the handler registered but unconfigured, so a
    /// rotation rule firing without it dead-letters with the knob's
    /// name instead of tripping UnknownHandler. Never logged.
    #[serde(skip)]
    pub broker_forgejo_token: Option<String>,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            nats_url: std::env::var("BOSS_NATS_URL")
                .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string()),
            jobs_api_url: std::env::var("BOSS_JOBS_URL")
                .unwrap_or_else(|_| boss_ports::url("jobs")),
            people_api_url: std::env::var("BOSS_PEOPLE_URL")
                .unwrap_or_else(|_| boss_ports::url("people")),
            inventory_api_url: std::env::var("BOSS_INVENTORY_URL")
                .unwrap_or_else(|_| boss_ports::url("inventory")),
            commerce_api_url: std::env::var("BOSS_COMMERCE_URL")
                .unwrap_or_else(|_| boss_ports::url("commerce")),
            products_api_url: std::env::var("BOSS_PRODUCTS_URL")
                .unwrap_or_else(|_| boss_ports::url("products")),
            shipping_api_url: std::env::var("BOSS_SHIPPING_URL")
                .unwrap_or_else(|_| boss_ports::url("shipping")),
            ledger_api_url: std::env::var("BOSS_LEDGER_URL")
                .unwrap_or_else(|_| boss_ports::url("ledger")),
            docs_api_url: std::env::var("BOSS_DOCS_URL")
                .unwrap_or_else(|_| boss_ports::url("docs")),
            messages_api_url: std::env::var("BOSS_MESSAGES_URL")
                .unwrap_or_else(|_| boss_ports::url("messages")),
            clock_api_url: std::env::var("BOSS_CLOCK_URL")
                .unwrap_or_else(|_| boss_ports::url("clock")),
            calendar_api_url: std::env::var("BOSS_CALENDAR_URL")
                .unwrap_or_else(|_| boss_ports::url("calendar")),
            // Loopback: the gateway is the sole trust boundary and is
            // co-located in every deployment (SECURITY.md §Deployment
            // trust model). BOSS_DISPATCHER_BIND widens deliberately.
            http_bind: std::env::var("BOSS_DISPATCHER_BIND")
                .unwrap_or_else(|_| format!("127.0.0.1:{}", boss_ports::prod("dispatcher"))),
            postgres_url: std::env::var("BOSS_POSTGRES_URL")
                .unwrap_or_else(|_| "postgres://boss:boss@127.0.0.1/boss".to_string()),
            webhook_url: std::env::var("BOSS_EVENT_WEBHOOK_URL").ok(),
            assignment_strategy: AssignmentStrategy::parse(
                &std::env::var("BOSS_DISPATCH_STRATEGY").unwrap_or_default(),
            ),
            broker_forge_url: std::env::var("BOSS_BROKER_FORGE_URL")
                .unwrap_or_else(|_| "http://10.20.0.15:3000".to_string()),
            broker_forgejo_token: std::env::var("BOSS_BROKER_FORGEJO_TOKEN")
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AssignmentStrategy;

    /// The data-selected default IS `Spread` — both via the explicit
    /// `Default` impl (what `#[serde(skip)]` falls back to) and via parsing
    /// an empty `BOSS_DISPATCH_STRATEGY`, which is what an unset env var
    /// resolves to in `DispatcherConfig::default`.
    #[test]
    fn default_strategy_is_spread() {
        assert_eq!(AssignmentStrategy::default(), AssignmentStrategy::Spread);
        assert_eq!(AssignmentStrategy::parse(""), AssignmentStrategy::Spread);
        assert_eq!(AssignmentStrategy::parse("   "), AssignmentStrategy::Spread);
    }

    /// Known values parse to their variant, case- and whitespace-insensitive,
    /// accepting both the kebab and snake spelling.
    #[test]
    fn known_strategies_parse() {
        assert_eq!(
            AssignmentStrategy::parse("spread"),
            AssignmentStrategy::Spread
        );
        assert_eq!(
            AssignmentStrategy::parse("  SPREAD  "),
            AssignmentStrategy::Spread
        );
        assert_eq!(
            AssignmentStrategy::parse("lowest-id"),
            AssignmentStrategy::LowestId
        );
        assert_eq!(
            AssignmentStrategy::parse("Lowest_Id"),
            AssignmentStrategy::LowestId
        );
    }

    /// A typo must NOT hard-fail (no panic, no error type) — it falls back to
    /// the default. The `warn!` is a side effect; the value contract is that
    /// an unknown string still yields a usable strategy.
    #[test]
    fn unknown_strategy_falls_back_to_default() {
        assert_eq!(
            AssignmentStrategy::parse("round-robin"),
            AssignmentStrategy::Spread
        );
        assert_eq!(
            AssignmentStrategy::parse("sprad-typo"),
            AssignmentStrategy::Spread
        );
    }
}
