//! `maintenance.sweep.inspect` (deploy-convergence target) — the sweep
//! that reads the trains' own stamps instead of an operator doing it.
//!
//! Every pr-train records the instant it merged
//! (`merged.metadata.completed_at`) and the instant the cluster was
//! observed running that commit (`converged.metadata.completed_at`,
//! written by the conductor's ancestry check). The deploy-convergence
//! sweep's question — did every merge since yesterday reach the
//! cluster, and how long did each take — is a pure function of those
//! two stamps over the trains in the window. On 2026-09-11 and again
//! on 2026-09-12 an operator answered it by hand from the same packets
//! (backlog 8e8311f5); this module is that reading, done by the
//! handler on the sweep's own Inspect step.
//!
//! Pure: the handler fetches the trains and the sweep's open instant
//! and hands both here. No wall clock — `now` is the sweep's
//! `opened_at`, so a replay reads the same answer.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

/// The convergence arm: the estate raises after a merge has gone this
/// long without the cluster running it (infra/estate, 30 min). The
/// sweep judges by the same line so its findings and the alarm agree.
pub(crate) const ARM: Duration = Duration::minutes(30);

/// Where one merged train stands against the arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Standing {
    /// Converged inside the arm.
    Converged { lag_s: i64 },
    /// Converged, but later than the arm — a lag worth a look even
    /// though it resolved itself.
    Late { lag_s: i64 },
    /// Open, merged less than the arm ago, converge not yet observed.
    Converging { waited_s: i64 },
    /// Open, past the arm, still not converged.
    Overdue { waited_s: i64 },
    /// Closed with `merged` completed and `converged` never completed:
    /// the train reached a terminal without the cluster catching up.
    ClosedUnconverged,
}

impl Standing {
    pub(crate) fn is_finding(&self) -> bool {
        matches!(
            self,
            Standing::Late { .. } | Standing::Overdue { .. } | Standing::ClosedUnconverged
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrainRow {
    pub train_id: String,
    pub title: String,
    pub merged_at: DateTime<Utc>,
    pub standing: Standing,
}

fn step<'a>(train: &'a Value, slug: &str) -> Option<&'a Value> {
    train
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
}

fn completed_at(step: Option<&Value>) -> Option<DateTime<Utc>> {
    let s = step?;
    if s.get("status").and_then(Value::as_str) != Some("completed") {
        return None;
    }
    s.pointer("/metadata/completed_at")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// One row per train whose `merged` step completed at or after
/// `since`, oldest merge first. Trains that never merged (cancelled,
/// still in CI) are not the sweep's question and are skipped.
pub(crate) fn convergence_rows(
    trains: &[Value],
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Vec<TrainRow> {
    let mut rows: Vec<TrainRow> = trains
        .iter()
        .filter_map(|t| {
            let merged_at = completed_at(step(t, "merged"))?;
            if merged_at < since {
                return None;
            }
            let standing = match completed_at(step(t, "converged")) {
                Some(c) => {
                    let lag_s = (c - merged_at).num_seconds();
                    if c - merged_at > ARM {
                        Standing::Late { lag_s }
                    } else {
                        Standing::Converged { lag_s }
                    }
                }
                None if t.get("status").and_then(Value::as_str) == Some("open") => {
                    let waited_s = (now - merged_at).num_seconds().max(0);
                    if now - merged_at > ARM {
                        Standing::Overdue { waited_s }
                    } else {
                        Standing::Converging { waited_s }
                    }
                }
                None => Standing::ClosedUnconverged,
            };
            Some(TrainRow {
                train_id: t
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                title: t
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                merged_at,
                standing,
            })
        })
        .collect();
    rows.sort_by_key(|r| r.merged_at);
    rows
}

/// What the Inspect checklist records: routing, the per-train items,
/// the finding lines and the one-line measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inspection {
    pub action_needed: bool,
    pub findings: Vec<String>,
    pub measured: String,
    pub items: Vec<Value>,
}

fn minutes(s: i64) -> String {
    format!("{} min", (s + 30) / 60)
}

fn describe(row: &TrainRow) -> String {
    let merged = row.merged_at.format("%H:%MZ");
    match &row.standing {
        Standing::Converged { lag_s } => {
            format!(
                "{} merged {merged} → converged +{}",
                row.title,
                minutes(*lag_s)
            )
        }
        Standing::Late { lag_s } => format!(
            "{} merged {merged} → converged +{} — LATE, past the {}-min arm",
            row.title,
            minutes(*lag_s),
            ARM.num_minutes()
        ),
        Standing::Converging { waited_s } => format!(
            "{} merged {merged} → converging, {} so far (within the arm)",
            row.title,
            minutes(*waited_s)
        ),
        Standing::Overdue { waited_s } => format!(
            "{} merged {merged} → NOT CONVERGED after {}, past the {}-min arm",
            row.title,
            minutes(*waited_s),
            ARM.num_minutes()
        ),
        Standing::ClosedUnconverged => format!(
            "{} merged {merged} → closed without the cluster ever observed running it",
            row.title
        ),
    }
}

/// The inspection of `trains` for a sweep opened at `now`, over the
/// day before it. `actor` stamps each checklist item.
pub(crate) fn inspect(trains: &[Value], now: DateTime<Utc>, actor: &str) -> Inspection {
    let since = now - Duration::hours(24);
    let rows = convergence_rows(trains, since, now);
    let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let findings: Vec<String> = rows
        .iter()
        .filter(|r| r.standing.is_finding())
        .map(|r| format!("{}: {}", r.train_id, describe(r)))
        .collect();
    let lags: Vec<i64> = rows
        .iter()
        .filter_map(|r| match r.standing {
            Standing::Converged { lag_s } | Standing::Late { lag_s } => Some(lag_s),
            _ => None,
        })
        .collect();
    let converging = rows
        .iter()
        .filter(|r| matches!(r.standing, Standing::Converging { .. }))
        .count();
    let lag_summary = match (lags.iter().min(), lags.iter().max()) {
        (Some(min), Some(max)) => format!(
            "{} converged, merge→converged {}–{}",
            lags.len(),
            minutes(*min),
            minutes(*max)
        ),
        _ => "none converged".to_string(),
    };
    let measured = format!(
        "{} train(s) merged since {}: {}, {} converging, {} finding(s). Read off the pr-train merged/converged step stamps at {stamp}.",
        rows.len(),
        since.format("%Y-%m-%d %H:%MZ"),
        lag_summary,
        converging,
        findings.len(),
    );
    let items: Vec<Value> = if rows.is_empty() {
        vec![json!({
            "label": format!("no train merged since {}", since.format("%Y-%m-%d %H:%MZ")),
            "checked": true, "checked_by": actor, "checked_at": stamp,
        })]
    } else {
        rows.iter()
            .map(|r| {
                json!({
                    "label": describe(r),
                    "checked": true, "checked_by": actor, "checked_at": stamp,
                })
            })
            .collect()
    };
    Inspection {
        action_needed: !findings.is_empty(),
        findings,
        measured,
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(t).unwrap().with_timezone(&Utc)
    }
    fn train(id: &str, status: &str, merged: Option<&str>, converged: Option<&str>) -> Value {
        let mk = |slug: &str, when: Option<&str>| match when {
            Some(t) => {
                json!({ "spec_slug": slug, "status": "completed", "metadata": { "completed_at": t } })
            }
            None => json!({ "spec_slug": slug, "status": "pending", "metadata": {} }),
        };
        json!({
            "id": id, "kind": "pr-train", "status": status, "title": format!("PR train {id}"),
            "steps": [mk("merged", merged), mk("converged", converged)]
        })
    }
    const NOW: &str = "2026-09-12T20:00:00Z";

    #[test]
    fn a_train_that_converged_inside_the_arm_is_not_a_finding() {
        let rows = convergence_rows(
            &[train(
                "t1",
                "closed",
                Some("2026-09-12T18:50:41Z"),
                Some("2026-09-12T19:10:02Z"),
            )],
            at("2026-09-11T20:00:00Z"),
            at(NOW),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].standing, Standing::Converged { lag_s: 1161 });
        assert!(!rows[0].standing.is_finding());
    }

    #[test]
    fn a_converge_past_the_arm_is_late_and_a_finding() {
        let rows = convergence_rows(
            &[train(
                "t1",
                "closed",
                Some("2026-09-12T18:00:00Z"),
                Some("2026-09-12T18:45:00Z"),
            )],
            at("2026-09-11T20:00:00Z"),
            at(NOW),
        );
        assert_eq!(rows[0].standing, Standing::Late { lag_s: 2700 });
        assert!(rows[0].standing.is_finding());
    }

    #[test]
    fn an_open_train_merged_recently_is_converging_not_overdue() {
        let rows = convergence_rows(
            &[train("t1", "open", Some("2026-09-12T19:50:00Z"), None)],
            at("2026-09-11T20:00:00Z"),
            at(NOW),
        );
        assert_eq!(rows[0].standing, Standing::Converging { waited_s: 600 });
        assert!(!rows[0].standing.is_finding());
    }

    #[test]
    fn an_open_train_past_the_arm_without_a_converge_is_overdue() {
        let rows = convergence_rows(
            &[train("t1", "open", Some("2026-09-12T19:00:00Z"), None)],
            at("2026-09-11T20:00:00Z"),
            at(NOW),
        );
        assert_eq!(rows[0].standing, Standing::Overdue { waited_s: 3600 });
        assert!(rows[0].standing.is_finding());
    }

    #[test]
    fn a_closed_train_that_merged_but_never_converged_is_a_finding() {
        let rows = convergence_rows(
            &[train("t1", "closed", Some("2026-09-12T19:00:00Z"), None)],
            at("2026-09-11T20:00:00Z"),
            at(NOW),
        );
        assert_eq!(rows[0].standing, Standing::ClosedUnconverged);
        assert!(rows[0].standing.is_finding());
    }

    #[test]
    fn a_train_that_never_merged_and_one_before_the_window_are_not_the_question() {
        let rows = convergence_rows(
            &[
                train("cancelled", "closed", None, None),
                train("in-ci", "open", None, None),
                train(
                    "yesterday",
                    "closed",
                    Some("2026-09-10T12:00:00Z"),
                    Some("2026-09-10T12:10:00Z"),
                ),
            ],
            at("2026-09-11T20:00:00Z"),
            at(NOW),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn rows_come_oldest_merge_first_whatever_order_the_api_gave() {
        let rows = convergence_rows(
            &[
                train(
                    "later",
                    "closed",
                    Some("2026-09-12T19:00:00Z"),
                    Some("2026-09-12T19:10:00Z"),
                ),
                train(
                    "earlier",
                    "closed",
                    Some("2026-09-12T17:00:00Z"),
                    Some("2026-09-12T17:10:00Z"),
                ),
            ],
            at("2026-09-11T20:00:00Z"),
            at(NOW),
        );
        assert_eq!(rows[0].train_id, "earlier");
    }

    #[test]
    fn a_clean_day_routes_to_clear_with_one_item_per_train_and_the_lags_measured() {
        let insp = inspect(
            &[
                train(
                    "t1",
                    "closed",
                    Some("2026-09-12T17:02:00Z"),
                    Some("2026-09-12T17:12:00Z"),
                ),
                train(
                    "t2",
                    "closed",
                    Some("2026-09-12T18:50:41Z"),
                    Some("2026-09-12T19:10:02Z"),
                ),
                train("t3", "open", Some("2026-09-12T19:50:00Z"), None),
            ],
            at(NOW),
            "automation:test",
        );
        assert!(!insp.action_needed);
        assert!(insp.findings.is_empty());
        assert_eq!(insp.items.len(), 3);
        assert!(insp.measured.starts_with("3 train(s) merged since 2026-09-11 20:00Z: 2 converged, merge→converged 10 min–19 min, 1 converging, 0 finding(s)."), "{}", insp.measured);
        assert_eq!(
            insp.items[0]["label"],
            "PR train t1 merged 17:02Z → converged +10 min"
        );
        assert_eq!(insp.items[0]["checked_by"], "automation:test");
        assert_eq!(insp.items[0]["checked_at"], NOW);
    }

    #[test]
    fn an_overdue_train_routes_to_remediate_and_is_named() {
        let insp = inspect(
            &[train("t9", "open", Some("2026-09-12T18:00:00Z"), None)],
            at(NOW),
            "automation:test",
        );
        assert!(insp.action_needed);
        assert_eq!(insp.findings.len(), 1);
        assert!(
            insp.findings[0].starts_with(
                "t9: PR train t9 merged 18:00Z → NOT CONVERGED after 120 min, past the 30-min arm"
            ),
            "{}",
            insp.findings[0]
        );
    }

    #[test]
    fn a_day_with_no_merge_records_that_in_one_item() {
        let insp = inspect(&[], at(NOW), "automation:test");
        assert!(!insp.action_needed);
        assert_eq!(insp.items.len(), 1);
        assert_eq!(
            insp.items[0]["label"],
            "no train merged since 2026-09-11 20:00Z"
        );
    }
}
