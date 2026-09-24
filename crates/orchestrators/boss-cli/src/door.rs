//! DOOR — the dev pod's ssh door, as the record last judged it
//! (backlog e6406701).
//!
//! WHY. Incident 55d001b0: both of the dev pod's ssh doors — the LAN
//! VIP and dev.algedonic.dev — were dark for ~36 hours from
//! 2026-09-22T23:23Z, and a person found it. `boss orient` is the verb
//! every session runs first, and it had no line for the door, so a
//! session could orient, work, and hand back through a pod whose front
//! door nobody could open. Now it prints one.
//!
//! FROM THE SAME RECORD THE ALARM READS. The line is the newest `door`
//! comparison (`GET /api/estate/comparisons?scope=door&limit=1`) —
//! what `estate.compare` judged of the observation the forge's
//! `infra/estate/observe-door.sh` posted, and the row `estate.alarm`
//! files from. It never probes the door itself: a verb run INSIDE the
//! pod would be judging the door by its patient, and a second judgement
//! beside the recorded one is two answers that can disagree.
//!
//! NEVER SILENT. No comparison at all says nothing is watching; a
//! reading older than its own band says the observer may be dark; a
//! failed read says so. A DOOR line that vanished on an error would
//! read exactly like a door nobody thought to watch — which is the
//! condition this line exists to end.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// The read this section makes, and nothing else.
pub(crate) const READ: &str = "/api/estate/comparisons?scope=door&limit=1";

/// `42m`, `3h05m` — an age in minutes, or hours and minutes past one.
fn span(seconds: i64) -> String {
    let m = seconds.max(0) / 60;
    if m < 60 {
        format!("{m}m")
    } else {
        format!("{}h{:02}m", m / 60, m % 60)
    }
}

/// The ids (`<door>/<half>`) under one findings field.
fn ids<'a>(comparison: &'a Value, field: &str) -> Vec<&'a str> {
    comparison
        .get("findings")
        .and_then(|f| f.get(field))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("id").and_then(Value::as_str))
        .collect()
}

/// The entry under one findings field for one `<door>/<half>` id.
fn entry<'a>(comparison: &'a Value, field: &str, id: &str) -> Option<&'a Value> {
    comparison
        .get("findings")?
        .get(field)?
        .as_array()?
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(id))
}

/// One half as printed: `lan 10.20.0.35:22 open`, or dark with how long,
/// against which band, and why.
fn half_text(comparison: &Value, door: &str, half: &Value) -> String {
    let name = half.get("half").and_then(Value::as_str).unwrap_or("?");
    let target = half.get("target").and_then(Value::as_str).unwrap_or("?");
    let id = format!("{door}/{name}");
    let judged = entry(comparison, "door_dark", &id)
        .map(|e| (e, true))
        .or_else(|| entry(comparison, "door_dimming", &id).map(|e| (e, false)));
    let Some((e, past)) = judged else {
        return format!("{name} {target} open");
    };
    let dark_for = e
        .get("dark_for_s")
        .and_then(Value::as_i64)
        .map(span)
        .map(|s| format!(" {s}"))
        .unwrap_or_default();
    let band = e
        .get("band_s")
        .and_then(Value::as_i64)
        .map(span)
        .unwrap_or_else(|| "undeclared".to_string());
    let reason = e
        .get("reason")
        .and_then(Value::as_str)
        .map(|r| format!(" ({r})"))
        .unwrap_or_default();
    if past {
        format!("{name} {target} DARK{dark_for}, past its {band} band{reason}")
    } else {
        format!("{name} {target} dark{dark_for}, inside its {band} band{reason}")
    }
}

/// The section as printed, from the newest door comparison row (an
/// event envelope whose `payload` is the comparison), `None` when the
/// series is empty, or the read's error.
pub(crate) fn lines(latest: &Result<Option<Value>, String>, now: DateTime<Utc>) -> Vec<String> {
    let row = match latest {
        Err(e) => return vec![format!("  DOOR — unavailable: {e}")],
        Ok(None) => {
            return vec![
                "  DOOR — NOT WATCHED: no door comparison is recorded, so nothing outside the pod is probing it".to_string(),
                "    the observer is infra/estate/observe-door.sh, run on the forge by cluster-watchdog.service".to_string(),
            ];
        }
        Ok(Some(row)) => row,
    };
    let c = row.get("payload").unwrap_or(row);
    let doors: Vec<&Value> = c
        .get("doors")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if doors.is_empty() {
        return vec![
            "  DOOR — UNREADABLE: the newest door comparison carries no doors".to_string(),
        ];
    }
    let observed = c
        .get("observed_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc));
    let age_s = observed.map(|t| (now - t).num_seconds());
    let age = age_s
        .map(|s| format!("observed {} ago", span(s)))
        .unwrap_or_else(|| "observed at an unreadable time".to_string());
    let dark = ids(c, "door_dark");
    let dimming = ids(c, "door_dimming");
    doors
        .iter()
        .map(|d| {
            let id = d.get("id").and_then(Value::as_str).unwrap_or("?");
            let mine = |list: &[&str]| list.iter().any(|x| x.starts_with(&format!("{id}/")));
            let word = if mine(&dark) {
                "DARK"
            } else if mine(&dimming) {
                "dimming"
            } else {
                "open"
            };
            let halves: Vec<String> = d
                .get("halves")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|h| half_text(c, id, h))
                .collect();
            // A reading older than the band it is judged against cannot
            // say the door is open NOW: the observer may be the thing
            // that went dark (estate.alarm's `unobserved:door` is the
            // alarm; this is the line that makes it visible here).
            let stale = match (age_s, d.get("band_s").and_then(Value::as_i64)) {
                (Some(a), Some(b)) if a > b => {
                    " — STALE: older than the band, is the observer running?"
                }
                (None, _) => " — STALE: no readable observation time",
                _ => "",
            };
            format!(
                "  DOOR — {id} {word}: {}  ({age}){stale}",
                halves.join(" · ")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 4, 0).unwrap()
    }

    fn row(findings: Value) -> Value {
        json!({
            "event_id": "e", "kind": "jobs.estate.compared",
            "payload": {
                "scope": "door",
                "observed_at": "2026-09-24T12:00:00Z",
                "doors": [{"id": "dev-ssh", "band_s": 900, "halves": [
                    {"half": "lan", "target": "10.20.0.35:22"},
                    {"half": "public", "target": "dev.algedonic.dev"},
                ]}],
                "findings": findings,
            }
        })
    }

    #[test]
    fn an_open_door_names_both_halves_and_the_reading_age() {
        let out = lines(
            &Ok(Some(row(json!({"door_dark": [], "door_dimming": []})))),
            now(),
        );
        assert_eq!(
            out,
            vec![
                "  DOOR — dev-ssh open: lan 10.20.0.35:22 open · public dev.algedonic.dev open  (observed 4m ago)"
            ]
        );
    }

    #[test]
    fn a_door_dark_past_its_band_says_which_half_how_long_and_why() {
        let out = lines(
            &Ok(Some(row(json!({
                "door_dark": [{"id": "dev-ssh/lan", "dark_for_s": 2520, "band_s": 900,
                               "reason": "connection refused"}],
                "door_dimming": [],
            })))),
            now(),
        );
        assert_eq!(
            out,
            vec![
                "  DOOR — dev-ssh DARK: lan 10.20.0.35:22 DARK 42m, past its 15m band (connection refused) · public dev.algedonic.dev open  (observed 4m ago)"
            ]
        );
    }

    #[test]
    fn a_half_inside_its_band_is_dimming_not_dark() {
        let out = lines(
            &Ok(Some(row(json!({
                "door_dark": [],
                "door_dimming": [{"id": "dev-ssh/public", "dark_for_s": 360, "band_s": 900,
                                  "reason": "does not resolve"}],
            })))),
            now(),
        );
        assert!(out[0].starts_with("  DOOR — dev-ssh dimming: "), "{out:?}");
        assert!(
            out[0].contains(
                "public dev.algedonic.dev dark 6m, inside its 15m band (does not resolve)"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn a_reading_older_than_its_band_says_the_observer_may_be_dark() {
        let later = Utc.with_ymd_and_hms(2026, 9, 24, 13, 0, 0).unwrap();
        let out = lines(
            &Ok(Some(row(json!({"door_dark": [], "door_dimming": []})))),
            later,
        );
        assert!(
            out[0].ends_with(
                "(observed 1h00m ago) — STALE: older than the band, is the observer running?"
            ),
            "{out:?}"
        );
    }

    #[test]
    fn no_comparison_at_all_says_nothing_is_watching() {
        let out = lines(&Ok(None), now());
        assert!(out[0].contains("NOT WATCHED"), "{out:?}");
        assert!(out[1].contains("infra/estate/observe-door.sh"), "{out:?}");
    }

    #[test]
    fn a_failed_read_says_so_rather_than_dropping_the_line() {
        let out = lines(&Err("HTTP 502".into()), now());
        assert_eq!(out, vec!["  DOOR — unavailable: HTTP 502"]);
    }

    #[test]
    fn a_comparison_with_no_doors_is_unreadable_not_open() {
        let out = lines(
            &Ok(Some(json!({"payload": {"scope": "door", "findings": {}}}))),
            now(),
        );
        assert_eq!(
            out,
            vec!["  DOOR — UNREADABLE: the newest door comparison carries no doors"]
        );
    }
}
