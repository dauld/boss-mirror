//! Is the CI host fit to receive a consist? Asked BEFORE boarding.
//!
//! On 2026-09-03 the conductor boarded two consists onto a CI host
//! whose disk was full, and each burned a full CI cycle discovering
//! it. The locomotive's own `min_free_gb` floor (default 70,
//! `infra/forge/locomotive.sh`) even PASSED at run start (01:26) and
//! the test job still died mid-run — a run-start floor cannot see the
//! consist's mid-flight consumption. So the question moves to where
//! it is cheap: the conductor asks the estate's observed series
//! whether the host has room before the first merge happens.
//!
//! The instrument is `GET /api/estate/observations?scope=host` — the
//! read half of the estate loop. Rows come back newest-first, each an
//! audit envelope whose `payload` is the observation verbatim as the
//! observer posted it. The field names bound here are the contract
//! `infra/estate/observe-host.sh` posts: `payload.scope == "host"`,
//! `payload.observed_at` (RFC 3339), and per node in `payload.nodes`
//! — `id` (the estate node row id) and `disk_free_gb` (root
//! filesystem available space, nearest GiB by the script's one stated
//! rounding rule).
//!
//! Three answers, not two, and the split is load-bearing. `Refuse`
//! means the series POSITIVELY says the host is short — the only
//! outcome that stops a boarding. `Unverifiable` means the series
//! cannot answer (absent, stale, unreadable) — and the caller
//! proceeds loudly, fail-open, because the host-scope observer is not
//! yet installed anywhere and landing this check must not stop all
//! boarding on day one. Collapsing those two into one "not ready"
//! would do exactly that.

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

/// How stale a host observation may be and still count as knowledge.
/// The cluster observer ticks every few minutes; a host observer on a
/// systemd timer will do the same. Thirty minutes is several missed
/// firings — past that the number describes a host that has since run
/// a build or two, and acting on it would be acting on a memory.
pub(crate) const MAX_OBSERVATION_AGE_MINS: i64 = 30;

pub(crate) fn max_observation_age() -> Duration {
    Duration::minutes(MAX_OBSERVATION_AGE_MINS)
}

/// The boarding verdict on the CI host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Readiness {
    /// The latest observation is fresh and the host has room.
    Proceed,
    /// The latest observation is fresh and the host is short — the
    /// reason names both numbers, because "not enough disk" without
    /// them is a chip nobody can act on.
    Refuse { reason: String },
    /// The series cannot answer: absent, stale, or unreadable. The
    /// caller decides what that means (today: proceed, loudly).
    Unverifiable { reason: String },
}

/// Judge the CI host from the observations reader's response body,
/// verbatim (`{"data": [envelope, ...]}`, newest-first). Pure: `now`
/// arrives as an argument so staleness is a fact of the inputs, not
/// of when the test suite happened to run.
pub(crate) fn host_readiness(
    observations: &Value,
    host_id: &str,
    floor_gb: i64,
    max_age: Duration,
    now: DateTime<Utc>,
) -> Readiness {
    let cannot = |reason: String| Readiness::Unverifiable { reason };

    let Some(rows) = observations.get("data").and_then(Value::as_array) else {
        return cannot("the observations response carried no data array".to_string());
    };

    // Rows arrive newest-first (pinned by the reader's own tests), so
    // the first host-scope row naming this host IS its latest word.
    let latest = rows.iter().find_map(|row| {
        let payload = row.get("payload")?;
        if payload.get("scope").and_then(Value::as_str) != Some("host") {
            return None;
        }
        let node = payload
            .get("nodes")
            .and_then(Value::as_array)?
            .iter()
            .find(|n| n.get("id").and_then(Value::as_str) == Some(host_id))?;
        Some((payload, node))
    });
    let Some((payload, node)) = latest else {
        return cannot(format!(
            "no host-scope observation for {host_id} in the series"
        ));
    };

    // Freshness first: a free-space figure of unknown or expired age
    // must not gate a boarding in either direction.
    let observed_at = match payload
        .get("observed_at")
        .and_then(Value::as_str)
        .map(|s| s.parse::<DateTime<Utc>>())
    {
        Some(Ok(t)) => t,
        _ => {
            return cannot(format!(
                "latest observation for {host_id} has an unreadable observed_at"
            ));
        }
    };
    let age = now - observed_at;
    if age > max_age {
        return cannot(format!(
            "latest observation for {host_id} is stale — {}m old, max {}m",
            age.num_minutes(),
            max_age.num_minutes()
        ));
    }

    let Some(free_gb) = node.get("disk_free_gb").and_then(Value::as_i64) else {
        return cannot(format!(
            "latest observation for {host_id} carries no numeric disk_free_gb"
        ));
    };

    if free_gb < floor_gb {
        return Readiness::Refuse {
            reason: format!(
                "CI host {host_id} has {free_gb}GB free, below the {floor_gb}GB \
                 boarding floor (observed {}m ago)",
                age.num_minutes().max(0)
            ),
        };
    }
    Readiness::Proceed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        "2026-09-03T02:00:00Z".parse().unwrap()
    }

    fn max_age() -> Duration {
        Duration::minutes(30)
    }

    /// One envelope as the observations reader serves it, payload as
    /// observe-host.sh posts it.
    fn envelope(host_id: &str, observed_at: &str, free_gb: i64) -> Value {
        json!({
            "event_id": "11111111-1111-1111-1111-111111111111",
            "timestamp": observed_at,
            "source": "jobs",
            "kind": "jobs.estate.observed",
            "payload": {
                "observed_at": observed_at,
                "observer": "boss-estate-observe-host",
                "scope": "host",
                "nodes": [{
                    "id": host_id, "address": "10.20.0.15", "cpu": 8,
                    "memory_gb": 16, "disk_gb": 226, "disk_free_gb": free_gb,
                    "uptime_s": 12345, "ready": true
                }]
            }
        })
    }

    fn body(rows: Vec<Value>) -> Value {
        json!({ "data": rows })
    }

    fn verdict(rows: Vec<Value>, floor_gb: i64) -> Readiness {
        host_readiness(&body(rows), "forge-host", floor_gb, max_age(), now())
    }

    // -- the four dispositions -----------------------------------------

    #[test]
    fn fresh_and_sufficient_proceeds() {
        let r = verdict(
            vec![envelope("forge-host", "2026-09-03T01:55:00Z", 120)],
            90,
        );
        assert_eq!(r, Readiness::Proceed);
    }

    #[test]
    fn exactly_at_the_floor_proceeds() {
        // A floor is "at least this much", not "more than this much" —
        // the same reading gate.sh gives BOSS_GATE_MIN_FREE_GB.
        let r = verdict(vec![envelope("forge-host", "2026-09-03T01:55:00Z", 90)], 90);
        assert_eq!(r, Readiness::Proceed);
    }

    #[test]
    fn fresh_and_below_the_floor_refuses_naming_both_numbers() {
        let r = verdict(vec![envelope("forge-host", "2026-09-03T01:55:00Z", 41)], 90);
        let Readiness::Refuse { reason } = r else {
            panic!("41GB free under a 90GB floor must refuse, got {r:?}");
        };
        assert!(
            reason.contains("41"),
            "names the observed free space: {reason}"
        );
        assert!(
            reason.contains("90"),
            "names the floor it fell below: {reason}"
        );
        assert!(reason.contains("forge-host"), "names the host: {reason}");
    }

    #[test]
    fn a_stale_observation_is_unverifiable_not_a_refusal() {
        // 02:00 now, observed 01:15 — 45 minutes under a 30-minute
        // max age. The host may have filled OR emptied since; either
        // way the series does not know, and "does not know" must not
        // read as "knows it is short".
        let r = verdict(vec![envelope("forge-host", "2026-09-03T01:15:00Z", 30)], 90);
        let Readiness::Unverifiable { reason } = r else {
            panic!("a stale observation cannot answer, got {r:?}");
        };
        assert!(
            reason.contains("stale"),
            "says why it cannot answer: {reason}"
        );
    }

    #[test]
    fn an_absent_series_is_unverifiable() {
        let r = verdict(vec![], 90);
        let Readiness::Unverifiable { reason } = r else {
            panic!("an empty series cannot answer, got {r:?}");
        };
        assert!(
            reason.contains("forge-host"),
            "names the host nothing has observed: {reason}"
        );
    }

    #[test]
    fn a_series_that_only_knows_other_hosts_is_unverifiable() {
        let r = verdict(vec![envelope("boss-gcp", "2026-09-03T01:55:00Z", 200)], 90);
        assert!(
            matches!(r, Readiness::Unverifiable { .. }),
            "another host's abundance says nothing about this one: {r:?}"
        );
    }

    // -- latest wins ----------------------------------------------------

    #[test]
    fn the_newest_matching_row_is_the_one_judged() {
        // Rows arrive newest-first (pinned by the reader's own tests).
        // The host was short an hour ago and has room now: proceed.
        let r = verdict(
            vec![
                envelope("forge-host", "2026-09-03T01:58:00Z", 130),
                envelope("forge-host", "2026-09-03T01:00:00Z", 12),
            ],
            90,
        );
        assert_eq!(r, Readiness::Proceed);
    }

    #[test]
    fn other_hosts_rows_do_not_shadow_the_latest_for_this_one() {
        // The newest row overall is another host's; the newest row for
        // forge-host says it is short. Refuse.
        let r = verdict(
            vec![
                envelope("boss-gcp", "2026-09-03T01:59:00Z", 200),
                envelope("forge-host", "2026-09-03T01:57:00Z", 20),
            ],
            90,
        );
        assert!(matches!(r, Readiness::Refuse { .. }), "got {r:?}");
    }

    #[test]
    fn a_cluster_scope_row_is_not_a_host_observation() {
        // Same node id, wrong scope: the cluster observer's view of a
        // worker is not the host series this check binds to.
        let mut row = envelope("forge-host", "2026-09-03T01:55:00Z", 200);
        row["payload"]["scope"] = json!("kubernetes-nodes");
        let r = verdict(vec![row], 90);
        assert!(matches!(r, Readiness::Unverifiable { .. }), "got {r:?}");
    }

    // -- malformed inputs cannot answer ---------------------------------

    #[test]
    fn a_body_with_no_data_array_is_unverifiable() {
        for bad in [json!(null), json!({}), json!({"data": "rows"})] {
            let r = host_readiness(&bad, "forge-host", 90, max_age(), now());
            assert!(
                matches!(r, Readiness::Unverifiable { .. }),
                "{bad} cannot answer, got {r:?}"
            );
        }
    }

    #[test]
    fn an_observation_missing_disk_free_gb_is_unverifiable() {
        let mut row = envelope("forge-host", "2026-09-03T01:55:00Z", 120);
        row["payload"]["nodes"][0]
            .as_object_mut()
            .unwrap()
            .remove("disk_free_gb");
        let r = verdict(vec![row], 90);
        let Readiness::Unverifiable { reason } = r else {
            panic!("no free-space field, no verdict — got {r:?}");
        };
        assert!(
            reason.contains("disk_free_gb"),
            "names the missing field: {reason}"
        );
    }

    #[test]
    fn a_non_numeric_disk_free_gb_is_unverifiable() {
        let mut row = envelope("forge-host", "2026-09-03T01:55:00Z", 120);
        row["payload"]["nodes"][0]["disk_free_gb"] = json!("plenty");
        let r = verdict(vec![row], 90);
        assert!(matches!(r, Readiness::Unverifiable { .. }), "got {r:?}");
    }

    #[test]
    fn an_unreadable_observed_at_is_unverifiable() {
        // Without a timestamp, freshness cannot be judged — and a
        // number of unknown age must not gate a boarding.
        let mut row = envelope("forge-host", "2026-09-03T01:55:00Z", 30);
        row["payload"]["observed_at"] = json!("yesterday-ish");
        let r = verdict(vec![row], 90);
        let Readiness::Unverifiable { reason } = r else {
            panic!("unreadable observed_at cannot answer, got {r:?}");
        };
        assert!(
            reason.contains("observed_at"),
            "names what was unreadable: {reason}"
        );
        let mut row = envelope("forge-host", "2026-09-03T01:55:00Z", 30);
        row["payload"]
            .as_object_mut()
            .unwrap()
            .remove("observed_at");
        let r = verdict(vec![row], 90);
        assert!(matches!(r, Readiness::Unverifiable { .. }), "got {r:?}");
    }
}
