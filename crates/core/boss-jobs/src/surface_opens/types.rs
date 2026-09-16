//! The surface-open record's shapes: what the SPA posts, what the
//! roll-up answers, what the sweep reports.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How long a row lives, in days. A CONSTANT until the retention
/// registry lands (backlog 16115a17) — at which point this becomes a
/// retention row keyed on the table and the constant is deleted, not
/// kept beside it (CLAUDE.md §9a). Thirty days: the reading is "the
/// last seven days per actor", the chore rolls up twenty-four hours at
/// a time, and a month of raw rows covers both with room for a missed
/// week without keeping an attention log for longer than it is read.
pub const RETENTION_DAYS: i64 = 30;

/// The longest route pattern the door accepts. A pattern is a path of
/// a few segments; anything longer is not a route the SPA minted.
pub const MAX_ROUTE_LEN: usize = 200;

/// What the SPA posts: the route pattern and when. Nothing names the
/// actor — the door takes that off the session.
#[derive(Debug, Clone, Deserialize)]
pub struct NewSurfaceOpen {
    pub route: String,
    /// When the operator opened it. Optional so a client with no clock
    /// still records; the door stamps the wall clock in its place.
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
}

/// Why a route pattern is refused. Named so the refusal can say which
/// test failed rather than that one did.
pub fn validate_route(route: &str) -> Result<(), String> {
    if route.is_empty() {
        return Err("route is required — the pattern the SPA opened, e.g. /it/codebase".into());
    }
    if !route.starts_with('/') {
        return Err(format!(
            "route `{route}` must be an absolute path starting with `/`"
        ));
    }
    if route.len() > MAX_ROUTE_LEN {
        return Err(format!(
            "route is {} characters; a route pattern is at most {MAX_ROUTE_LEN}",
            route.len()
        ));
    }
    if route.contains('?') || route.contains('#') {
        return Err(format!(
            "route `{route}` carries a query or fragment — post the pattern, not the URL"
        ));
    }
    if route.chars().any(char::is_whitespace) {
        return Err(format!("route `{route}` contains whitespace"));
    }
    Ok(())
}

/// One roll-up row: how many times this actor opened this route inside
/// the window, and the last time they did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteCount {
    pub actor_id: String,
    pub route: String,
    pub opens: i64,
    pub last_at: DateTime<Utc>,
}

/// The roll-up over a window: every (actor, route) pair with a count,
/// actor-ordered then most-opened first. The window rides with the
/// rows so a reader cannot mistake a day for a week.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollup {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub rows: Vec<RouteCount>,
}

impl Rollup {
    /// Total opens across every row.
    pub fn opens(&self) -> i64 {
        self.rows.iter().map(|r| r.opens).sum()
    }
}

/// What a retention sweep did: how many rows it deleted and the cutoff
/// it deleted before, so the chore's row states the sweep's facts and
/// not just that one happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sweep {
    pub deleted: u64,
    pub before: DateTime<Utc>,
    pub retention_days: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_is_an_absolute_path_without_query_or_id_noise() {
        assert!(validate_route("/it/codebase").is_ok());
        assert!(validate_route("/ux/jobs/:jobId").is_ok());
        assert!(validate_route("/").is_ok());
        assert!(validate_route("").unwrap_err().contains("required"));
        assert!(
            validate_route("it/codebase")
                .unwrap_err()
                .contains("absolute")
        );
        assert!(
            validate_route("/ux/jobs?kind=x")
                .unwrap_err()
                .contains("query")
        );
        assert!(
            validate_route("/ux/a b")
                .unwrap_err()
                .contains("whitespace")
        );
        let long = format!("/{}", "x".repeat(MAX_ROUTE_LEN));
        assert!(validate_route(&long).unwrap_err().contains("at most"));
    }

    #[test]
    fn the_rollup_total_is_the_sum_of_its_rows() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-09-16T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let r = Rollup {
            since: at,
            until: at,
            rows: vec![
                RouteCount {
                    actor_id: "emp-david".into(),
                    route: "/it".into(),
                    opens: 3,
                    last_at: at,
                },
                RouteCount {
                    actor_id: "emp-david".into(),
                    route: "/it/codebase".into(),
                    opens: 2,
                    last_at: at,
                },
            ],
        };
        assert_eq!(r.opens(), 5);
    }
}
