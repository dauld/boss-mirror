//! `estate.recover` — the half of the estate alarm that closes.
//!
//! `estate.alarm` raises an urgent packet when a HARD finding persists
//! [`PERSIST_N`] consecutive comparisons of one series, deduped on
//! `metadata.estate_finding` — and until this handler nothing ever
//! closed it. The same series keeps arriving; when the finding is
//! ABSENT for [`PERSIST_N`] consecutive comparisons the condition has
//! recovered, the system of record holds the evidence, and the packet
//! still read as an open urgent alarm until a person noticed. Three
//! were closed by hand in two days (backlog ef421cd3: a1b4f3fa
//! boss-gcp-converge `unit_unhealthy`, e3da3202, fdd10ec8
//! boss-codebase-metrics `unit_unhealthy` — the last still open after
//! its fix landed 2026-09-14 15:55), each time the operator reading the
//! recovery off the record and typing it into the triage step. An
//! alarm only a human can clear trains the operator to leave alarms
//! open, and a stale open alarm suppresses the next real raise of the
//! same finding through the dedup.
//!
//! THE RULE IS THE RAISER'S RULE, MIRRORED. Recovery is judged over
//! the recorded comparison series exactly as persistence is
//! (`/api/estate/comparisons?scope=`, newest first, the same
//! `(scope, host)` series filter — the self-scoped host series
//! interleave, and host B's clean rows must not clear host A's alarm),
//! with the same N: N readings with the finding present is a
//! condition, N readings without it is the condition gone. Two things
//! the raiser does not need, this does:
//! - **the clean readings must postdate the raise.** `opened_at` on
//!   the packet (stamped by the jobs API at admission) is the floor;
//!   a row observed before it is not evidence the condition recovered
//!   AFTER it was raised. This is also what makes the silence sweep's
//!   `unobserved:<host>` alarms safe to judge by the same rule: a
//!   series that has gone dark has no new rows, so its newest N are
//!   the pre-silence ones and "the finding is absent there" would
//!   close the alarm the moment it was raised. N rows observed after
//!   the raise IS the series arriving again.
//! - **the N are consecutive.** The newest N same-series rows after
//!   the raise must ALL lack the key — a finding that flaps
//!   present/absent is still a condition, not a recovery.
//!
//! WHY A SIBLING HANDLER AND NOT A THIRD HALF OF `estate.alarm`.
//! The raiser reads the series only when the triggering comparison
//! found something hard, and reads packets only when it has something
//! to raise; recovery needs the opposite firing — the comparison that
//! found NOTHING — and needs the open packets first, to know whether
//! any alarm belongs to the series that just ticked. Folding that in
//! would have meant reordering the raiser's reads around a case it
//! never has, inside a module already carrying two calibrations.
//! A sibling on the same event has its own rule file (its own `why`,
//! its own retirement), its own redelivery budget (a failed close
//! never NAKs a raise), and reads the shared decisions from the
//! raiser (`hard_finding_keys`, `PERSIST_N`) rather than restating
//! them. Nothing in this module names a finding class: the raiser's
//! vocabulary is the recovery vocabulary.
//!
//! THE CLOSE IS THE TRIAGE STEP, completed with `disposition = stale`
//! — the backlog-item terminal titled "Closed — the claim no longer
//! holds", and what all three hand-closes chose — plus `evidence`
//! naming the finding, the host, and the N clean comparisons with
//! their instants. `recovered_at` rides both the step and the packet's
//! metadata (PATCH, merge) so the recovery can be read off the packet
//! without opening the step; the instant is the newest clean
//! comparison's own stamp — a fact of the record, the same on every
//! redelivery — not the moment the machine noticed. The step is
//! stamped [`CLEARED_BY`] so the raiser's settled-suppression can tell
//! a machine clear from a human's answer: a finding that recovers and
//! then persists again is a new fact and must re-raise, where a human's
//! `stale` holds for a week.
//!
//! Best-effort throughout, like the raiser: every alarm that could
//! close does; any write that failed is surfaced as one error at the
//! end so the firing is redelivered, where the judgement is idempotent
//! (a closed packet is no longer open, so it is not judged again).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};

use super::common::{TRIAGE_SLUG, api_client, get_json, triage_step, write_json};
use super::estate_alarm::{DEDUP_PAGE, PERSIST_N, hard_finding_keys};

/// Stamped on the triage completion this handler writes, so
/// `estate_alarm::settled_recently` can tell a machine clear from a
/// human's answer — the machine's close must not suppress the next
/// genuine raise of the same finding.
pub(super) const CLEARED_BY: &str = "estate.recover";

pub struct EstateRecover {
    client: reqwest::Client,
    jobs_base: String,
}

impl EstateRecover {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// One alarm the record says has recovered — everything the two
/// writes need, decided before either is attempted.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Recovery {
    pub job_id: String,
    pub key: String,
    pub scope: String,
    pub host: Option<String>,
    pub step_id: String,
    pub step_metadata: Map<String, Value>,
    /// The instants of the N clean comparisons, newest first.
    pub clean_at: Vec<DateTime<Utc>>,
}

/// The `estate_finding` key of one packet, if it carries one.
fn finding_key(job: &Value) -> Option<&str> {
    job.get("metadata")?.get("estate_finding")?.as_str()
}

/// The OPEN alarms raised on one series, `(scope, host)` — the same
/// identity the raiser keys persistence on. `host: None` matches the
/// cluster scope's host-less packets and nothing else.
pub(super) fn series_alarms<'a>(
    open_jobs: &'a [Value],
    scope: &str,
    host: Option<&str>,
) -> Vec<&'a Value> {
    open_jobs
        .iter()
        .filter(|j| j.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|j| finding_key(j).is_some())
        .filter(|j| {
            let m = j.get("metadata");
            m.and_then(|m| m.get("scope")).and_then(Value::as_str) == Some(scope)
                && m.and_then(|m| m.get("host")).and_then(Value::as_str) == host
        })
        .collect()
}

/// The comparison inside one recorded row: rows are event envelopes
/// with the comparison in `payload`, recorded verbatim by the dumb
/// door; the row itself is the fallback for a flattened future shape.
fn payload(row: &Value) -> &Value {
    row.get("payload").unwrap_or(row)
}

/// When one recorded comparison was observed: the observation's own
/// stamp (`observed_at`, carried onto the comparison so a replayed
/// reading says when it was taken), else the envelope's timestamp.
fn instant(row: &Value) -> Option<DateTime<Utc>> {
    payload(row)
        .get("observed_at")
        .and_then(Value::as_str)
        .or_else(|| row.get("timestamp").and_then(Value::as_str))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// The alarms of one series whose finding the record now shows
/// recovered — the decision, pure. `rows` is the recorded comparison
/// series newest-first (the API's order), every scope's rows; for each
/// open alarm on `(scope, host)` the newest `n` same-series rows
/// observed after the alarm's `opened_at` must exist and must ALL lack
/// the alarm's key. Fewer than `n` such rows is not enough evidence;
/// an alarm without `opened_at` or without a `triage` step cannot be
/// judged and is left alone.
pub(super) fn recovered(
    open_jobs: &[Value],
    rows: &[Value],
    scope: &str,
    host: Option<&str>,
    n: usize,
) -> Vec<Recovery> {
    series_alarms(open_jobs, scope, host)
        .into_iter()
        .filter_map(|alarm| {
            let key = finding_key(alarm)?;
            let opened_at = alarm
                .get("metadata")?
                .get("opened_at")?
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())?
                .with_timezone(&Utc);
            // The newest `n` rows of THIS series observed after the
            // raise, in record order — the raiser's window, floored
            // at the raise.
            let since: Vec<(&Value, DateTime<Utc>)> = rows
                .iter()
                .filter(|r| {
                    let p = payload(r);
                    p.get("scope").and_then(Value::as_str) == Some(scope)
                        && p.get("host").and_then(Value::as_str) == host
                })
                .filter_map(|r| instant(r).map(|t| (r, t)))
                .filter(|(_, t)| *t > opened_at)
                .take(n)
                .collect();
            if since.len() < n {
                return None;
            }
            if since
                .iter()
                .any(|(r, _)| hard_finding_keys(payload(r)).contains(key))
            {
                return None;
            }
            let (step_id, step_metadata) = triage_step(alarm)?;
            Some(Recovery {
                job_id: alarm.get("id")?.as_str()?.to_string(),
                key: key.to_string(),
                scope: scope.to_string(),
                host: host.map(str::to_string),
                step_id,
                step_metadata,
                clean_at: since.into_iter().map(|(_, t)| t).collect(),
            })
        })
        .collect()
}

/// The triage completion that closes one recovered alarm: the step's
/// existing keys carried through (PUT replaces metadata wholesale),
/// the two fields the backlog-item workflow requires at done
/// (`disposition`, `evidence`), and the machine's stamps.
pub(super) fn clear_step_body(r: &Recovery, n: usize) -> Value {
    let mut metadata = r.step_metadata.clone();
    let instants: Vec<String> = r.clean_at.iter().map(DateTime::to_rfc3339).collect();
    let host = r.host.as_deref().unwrap_or(&r.scope);
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "estate.recover read the recorded `{scope}` series back: the finding \
             `{key}` on `{host}` has been absent from {n} consecutive comparisons \
             observed after this alarm was raised, at {instants}. The condition \
             has recovered, so the claim this alarm carried no longer holds. \
             Closed by machine from the record, not by judgement (backlog \
             ef421cd3: three of these were read off the record and typed in by \
             hand). The series rides /api/estate/comparisons?scope={scope}.",
            scope = r.scope,
            key = r.key,
            instants = instants.join(", "),
        )),
    );
    metadata.insert("cleared_by".into(), json!(CLEARED_BY));
    metadata.insert("recovered_at".into(), json!(recovered_at(r)));
    json!({"status": "completed", "metadata": metadata})
}

/// When the record showed the recovery: the newest clean comparison's
/// own instant. A fact of the data — identical on every redelivery —
/// rather than the moment the machine happened to notice.
fn recovered_at(r: &Recovery) -> String {
    r.clean_at
        .first()
        .map(DateTime::to_rfc3339)
        .unwrap_or_default()
}

/// The merge onto the packet's own metadata: the recovery, readable
/// without opening the step.
pub(super) fn recovered_patch(r: &Recovery) -> Value {
    json!({"recovered_at": recovered_at(r)})
}

#[async_trait]
impl Handler for EstateRecover {
    fn name(&self) -> &'static str {
        "estate.recover"
    }

    async fn invoke(
        &self,
        _args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let comparison = &ctx.event_payload;
        let scope = comparison
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // An unscoped payload is not an estate comparison at all.
        if scope.is_empty() {
            return Ok(());
        }
        let host = comparison.get("host").and_then(Value::as_str);

        // The open packets first: only an alarm on the series that just
        // ticked can have changed state, and most firings find none.
        // A failed read is the one condition that fails the whole pass
        // — there is nothing to judge without it — and the redelivery
        // repeats a read, not a write.
        let listing = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind=backlog-item&status=open&limit={DEDUP_PAGE}",
                self.base()
            ),
            &ctx.rule_name,
        )
        .await?;
        let open_rows: Vec<Value> = listing
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // A truncated page is not a safety problem here the way it is
        // for the raiser's dedup: every close is judged on the series,
        // not on a packet's absence, and an alarm beyond the page is
        // judged on the next firing. Say so, though.
        if let Some(total) = listing.get("total").and_then(Value::as_u64)
            && usize::try_from(total).unwrap_or(usize::MAX) > open_rows.len()
        {
            tracing::warn!(
                rows = open_rows.len(),
                total,
                "estate.recover: open-packet read truncated; alarms beyond the page wait for the next firing"
            );
        }
        let alarms = series_alarms(&open_rows, scope, host);
        if alarms.is_empty() {
            return Ok(());
        }
        // A finding the triggering comparison still carries cannot be
        // absent from the newest N — no series read needed to know.
        let present = hard_finding_keys(comparison);
        if alarms
            .iter()
            .all(|a| finding_key(a).is_some_and(|k| present.contains(k)))
        {
            return Ok(());
        }

        // The recorded series IS the state, read exactly as the raiser
        // reads it: scoped, newest first, twenty rows.
        let recent = get_json(
            &self.client,
            &format!(
                "{}/api/estate/comparisons?scope={scope}&limit=20",
                self.base()
            ),
            &ctx.rule_name,
        )
        .await?;
        let rows: Vec<Value> = recent
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // Best-effort writes: every alarm that could close does, and
        // whatever failed is one aggregated error at the end so the
        // firing is redelivered. The judgement is idempotent under
        // redelivery — a closed packet is no longer open.
        let mut errors: Vec<String> = Vec::new();
        for r in recovered(&open_rows, &rows, scope, host, PERSIST_N) {
            let key = &r.key;
            if let Err(e) = write_json(
                &self.client,
                reqwest::Method::PUT,
                &format!("{}/api/jobs/{}/steps/{}", self.base(), r.job_id, r.step_id),
                &clear_step_body(&r, PERSIST_N),
                &ctx.rule_name,
            )
            .await
            {
                errors.push(format!(
                    "close of the alarm for {key} (its `{TRIAGE_SLUG}` step) failed; others still closed: {e}"
                ));
                continue;
            }
            if let Err(e) = write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{}/metadata", self.base(), r.job_id),
                &recovered_patch(&r),
                &ctx.rule_name,
            )
            .await
            {
                errors.push(format!(
                    "recovered_at annotation on the closed alarm for {key} failed (the step carries it): {e}"
                ));
                continue;
            }
            tracing::info!(
                finding = %key,
                scope,
                packet = %r.job_id,
                "estate.recover closed an alarm — the finding has been absent {PERSIST_N} consecutive comparisons"
            );
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(HandlerError::Downstream(format!(
                "estate.recover: {} write(s) failed this pass (every alarm that could close did; the rest retry): {}",
                errors.len(),
                errors.join(" | ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const RAISED_AT: &str = "2026-09-12T05:26:17+00:00";

    /// An open alarm as the raiser files it and the jobs API returns
    /// it: `estate_finding` + `scope` + `host` on the packet,
    /// `opened_at` stamped at admission, and a `triage` step carrying
    /// `authority_role`.
    fn alarm(id: &str, key: &str, scope: &str, host: Option<&str>, status: &str) -> Value {
        let mut metadata = json!({
            "area": "estate",
            "estate_finding": key,
            "scope": scope,
            "opened_at": RAISED_AT,
        });
        if let Some(h) = host {
            metadata["host"] = json!(h);
        }
        json!({
            "id": id,
            "kind": "backlog-item",
            "status": status,
            "metadata": metadata,
            "steps": [
                {"id": format!("{id}-filed"), "spec_slug": "filed", "status": "completed", "metadata": {}},
                {"id": format!("{id}-triage"), "spec_slug": "triage", "status": "ready",
                 "metadata": {"authority_role": "platform-admin"}},
                {"id": format!("{id}-stale"), "spec_slug": "stale", "status": "pending", "metadata": {}},
            ],
        })
    }

    fn at(minutes_after_raise: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 12, 5, 26, 17).unwrap()
            + chrono::Duration::minutes(minutes_after_raise)
    }

    /// One recorded `host-units` comparison row as the API returns
    /// it: an event envelope whose payload is the comparison
    /// `estate.compare` recorded — scope + host stamp + findings.
    fn units_row(host: &str, unhealthy: &[&str], observed: DateTime<Utc>) -> Value {
        json!({
            "event_id": "e", "timestamp": observed.to_rfc3339(), "source": "jobs",
            "kind": "jobs.estate.compared",
            "payload": {
                "scope": "host-units",
                "host": host,
                "observed_at": observed.to_rfc3339(),
                "findings": {
                    "units_unhealthy": unhealthy.iter().map(|u| json!({
                        "host": host, "unit": u,
                        "load_state": "loaded", "active_state": "failed",
                        "sub_state": "failed", "result": "exit-code",
                    })).collect::<Vec<_>>(),
                }
            }
        })
    }

    fn cluster_row(not_ready: &[&str], observed: DateTime<Utc>) -> Value {
        json!({
            "event_id": "e", "timestamp": observed.to_rfc3339(), "source": "jobs",
            "kind": "jobs.estate.compared",
            "payload": {
                "scope": "kubernetes-nodes",
                "observed_at": observed.to_rfc3339(),
                "findings": {"not_ready": not_ready, "declared_not_observed": []},
            }
        })
    }

    const KEY: &str = "unit_unhealthy:boss-gcp/boss-codebase-metrics.service";
    const UNIT: &str = "boss-codebase-metrics.service";

    #[test]
    fn a_finding_absent_n_times_after_the_raise_closes_its_alarm() {
        // fdd10ec8's shape: raised at 05:26, fix landed, the 5-minute
        // series has read clean three times since.
        let open = [alarm(
            "fdd10ec8",
            KEY,
            "host-units",
            Some("boss-gcp"),
            "open",
        )];
        let rows = [
            units_row("boss-gcp", &[], at(45)),
            units_row("boss-gcp", &[], at(40)),
            units_row("boss-gcp", &[], at(35)),
            units_row("boss-gcp", &[UNIT], at(-5)),
            units_row("boss-gcp", &[UNIT], at(-10)),
        ];
        let out = recovered(&open, &rows, "host-units", Some("boss-gcp"), 3);
        assert_eq!(out.len(), 1, "one alarm, one recovery");
        let r = &out[0];
        assert_eq!(r.job_id, "fdd10ec8");
        assert_eq!(r.key, KEY);
        assert_eq!(r.host.as_deref(), Some("boss-gcp"));
        assert_eq!(
            r.step_id, "fdd10ec8-triage",
            "the close is the triage step, addressed by id"
        );
        assert_eq!(
            r.step_metadata.get("authority_role"),
            Some(&json!("platform-admin")),
            "the step's existing metadata rides back so the PUT does not drop it"
        );
        assert_eq!(
            r.clean_at,
            vec![at(45), at(40), at(35)],
            "the N clean instants, newest first — the evidence the close names"
        );
    }

    #[test]
    fn absent_n_minus_one_times_is_not_yet_a_recovery() {
        let open = [alarm("a", KEY, "host-units", Some("boss-gcp"), "open")];
        let rows = [
            units_row("boss-gcp", &[], at(40)),
            units_row("boss-gcp", &[], at(35)),
            units_row("boss-gcp", &[UNIT], at(30)),
        ];
        assert!(recovered(&open, &rows, "host-units", Some("boss-gcp"), 3).is_empty());
    }

    #[test]
    fn a_finding_that_flaps_is_still_a_condition() {
        // Absent, present, absent: three clean readings exist in the
        // window but they are not consecutive. One flapped reading is
        // weather in both directions.
        let open = [alarm("a", KEY, "host-units", Some("boss-gcp"), "open")];
        let rows = [
            units_row("boss-gcp", &[], at(50)),
            units_row("boss-gcp", &[UNIT], at(45)),
            units_row("boss-gcp", &[], at(40)),
            units_row("boss-gcp", &[], at(35)),
        ];
        assert!(recovered(&open, &rows, "host-units", Some("boss-gcp"), 3).is_empty());
    }

    #[test]
    fn an_alarm_a_human_already_closed_is_untouched() {
        let closed = [alarm("a", KEY, "host-units", Some("boss-gcp"), "closed")];
        let rows = [
            units_row("boss-gcp", &[], at(45)),
            units_row("boss-gcp", &[], at(40)),
            units_row("boss-gcp", &[], at(35)),
        ];
        assert!(recovered(&closed, &rows, "host-units", Some("boss-gcp"), 3).is_empty());
    }

    #[test]
    fn two_hosts_series_do_not_clear_each_other() {
        // The raiser's per-(scope, host) rule, applied to recovery:
        // boss-gcp and the forge both post host-units every five
        // minutes. The forge's clean rows interleave with boss-gcp's
        // still-sick ones and must not read as boss-gcp recovering.
        let open = [
            alarm("gcp", KEY, "host-units", Some("boss-gcp"), "open"),
            alarm(
                "forge",
                "unit_unhealthy:forge-host/boss-train.service",
                "host-units",
                Some("forge-host"),
                "open",
            ),
        ];
        let rows = [
            units_row("forge-host", &[], at(46)),
            units_row("boss-gcp", &[UNIT], at(45)),
            units_row("forge-host", &[], at(41)),
            units_row("boss-gcp", &[UNIT], at(40)),
            units_row("forge-host", &[], at(36)),
            units_row("boss-gcp", &[UNIT], at(35)),
        ];
        assert!(
            recovered(&open, &rows, "host-units", Some("boss-gcp"), 3).is_empty(),
            "boss-gcp is still sick; the forge's clean rows are not its evidence"
        );
        // And the forge's own firing closes the forge's alarm only.
        let out = recovered(&open, &rows, "host-units", Some("forge-host"), 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].job_id, "forge");
    }

    #[test]
    fn readings_before_the_raise_are_not_evidence_of_recovery() {
        // Three clean rows exist but every one predates `opened_at`:
        // the record shows the condition before it was raised, not
        // after. This is what keeps an `unobserved:<host>` alarm —
        // whose series has, by definition, stopped producing rows —
        // from closing itself against the pre-silence readings.
        let open = [alarm(
            "quiet",
            "unobserved:boss-gcp",
            "host-units",
            Some("boss-gcp"),
            "open",
        )];
        let before = [
            units_row("boss-gcp", &[], at(-5)),
            units_row("boss-gcp", &[], at(-10)),
            units_row("boss-gcp", &[], at(-15)),
        ];
        assert!(recovered(&open, &before, "host-units", Some("boss-gcp"), 3).is_empty());
        // Once the series arrives again — three rows after the raise —
        // the same rule closes it: observed rows ARE the recovery.
        let after = [
            units_row("boss-gcp", &[], at(15)),
            units_row("boss-gcp", &[], at(10)),
            units_row("boss-gcp", &[], at(5)),
            units_row("boss-gcp", &[], at(-5)),
        ];
        assert_eq!(
            recovered(&open, &after, "host-units", Some("boss-gcp"), 3).len(),
            1
        );
    }

    #[test]
    fn an_alarm_without_a_raise_instant_cannot_be_judged() {
        let mut a = alarm("a", KEY, "host-units", Some("boss-gcp"), "open");
        a["metadata"].as_object_mut().unwrap().remove("opened_at");
        let rows = [
            units_row("boss-gcp", &[], at(45)),
            units_row("boss-gcp", &[], at(40)),
            units_row("boss-gcp", &[], at(35)),
        ];
        assert!(recovered(&[a], &rows, "host-units", Some("boss-gcp"), 3).is_empty());
    }

    #[test]
    fn the_cluster_series_is_host_less_and_judged_on_its_own_rows() {
        // `not_ready:cp-2` on the kubernetes-nodes scope: the packet
        // carries no host, the rows carry no host stamp, and a
        // host-scoped firing must not judge it.
        let open = [alarm(
            "cp2",
            "not_ready:cp-2",
            "kubernetes-nodes",
            None,
            "open",
        )];
        let rows = [
            cluster_row(&[], at(45)),
            units_row("boss-gcp", &[], at(44)),
            cluster_row(&[], at(30)),
            cluster_row(&[], at(15)),
            cluster_row(&["cp-2"], at(-15)),
        ];
        assert_eq!(
            recovered(&open, &rows, "kubernetes-nodes", None, 3).len(),
            1
        );
        assert!(recovered(&open, &rows, "host-units", Some("boss-gcp"), 3).is_empty());
    }

    #[test]
    fn only_the_series_that_ticked_is_judged() {
        let open = [
            alarm("gcp", KEY, "host-units", Some("boss-gcp"), "open"),
            alarm("cp2", "not_ready:cp-2", "kubernetes-nodes", None, "open"),
            json!({"id": "plain", "status": "open", "metadata": {"claim": "no estate key"}}),
        ];
        let gcp = series_alarms(&open, "host-units", Some("boss-gcp"));
        assert_eq!(gcp.len(), 1);
        assert_eq!(gcp[0]["id"], "gcp");
        assert_eq!(series_alarms(&open, "kubernetes-nodes", None).len(), 1);
        assert!(series_alarms(&open, "host", Some("boss-gcp")).is_empty());
    }

    fn a_recovery() -> Recovery {
        let mut step_metadata = Map::new();
        step_metadata.insert("authority_role".into(), json!("platform-admin"));
        Recovery {
            job_id: "fdd10ec8".into(),
            key: KEY.into(),
            scope: "host-units".into(),
            host: Some("boss-gcp".into()),
            step_id: "fdd10ec8-triage".into(),
            step_metadata,
            clean_at: vec![at(45), at(40), at(35)],
        }
    }

    #[test]
    fn the_close_completes_triage_with_every_field_the_workflow_requires() {
        // infra/platform/workflows/backlog-item.toml, step `triage`:
        // `disposition` (enum, required) and `evidence` (string,
        // required) at done. `stale` is the terminal the three hand
        // closes chose — "the claim no longer holds".
        let body = clear_step_body(&a_recovery(), 3);
        assert_eq!(body["status"], "completed");
        let m = &body["metadata"];
        assert_eq!(m["disposition"], "stale");
        let evidence = m["evidence"].as_str().expect("evidence is a string");
        assert!(evidence.contains(KEY), "names the finding");
        assert!(evidence.contains("boss-gcp"), "names the host");
        assert!(evidence.contains("3 consecutive"), "names N");
        for t in [at(45), at(40), at(35)] {
            assert!(
                evidence.contains(&t.to_rfc3339()),
                "names each clean comparison's instant: {}",
                t.to_rfc3339()
            );
        }
        assert_eq!(
            m["authority_role"], "platform-admin",
            "existing step metadata survives the PUT"
        );
        assert_eq!(m["cleared_by"], CLEARED_BY);
        assert_eq!(
            m["recovered_at"],
            at(45).to_rfc3339(),
            "the newest clean comparison's own instant, not the machine's clock"
        );
    }

    #[test]
    fn the_packet_patch_carries_the_recovery_instant() {
        let patch = recovered_patch(&a_recovery());
        assert_eq!(patch["recovered_at"], at(45).to_rfc3339());
        assert_eq!(
            patch.as_object().map(Map::len),
            Some(1),
            "a merge of exactly the recovery instant — who closed it lives once, on the step"
        );
    }

    // ----- the writes, witnessed against a stub jobs API -----

    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::sync::Mutex;

    type Writes = Arc<Mutex<Vec<(String, Value)>>>;

    /// A jobs API that answers the open-packet listing and the
    /// comparison series from fixtures and records every PUT and
    /// PATCH — the two writes a recovery is.
    async fn stub(open: Vec<Value>, rows: Vec<Value>) -> (String, Writes, Writes) {
        let puts: Writes = Arc::new(Mutex::new(Vec::new()));
        let patches: Writes = Arc::new(Mutex::new(Vec::new()));
        let total = open.len();
        let open = Arc::new(open);
        let rows = Arc::new(rows);
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<Map<String, Value>>| {
                    let open = open.clone();
                    async move {
                        assert_eq!(
                            q.get("status").and_then(Value::as_str),
                            Some("open"),
                            "recovery reads OPEN packets only"
                        );
                        Json(json!({"data": *open, "total": total}))
                    }
                }),
            )
            .route(
                "/api/estate/comparisons",
                get(move |Query(q): Query<Map<String, Value>>| {
                    let rows = rows.clone();
                    async move {
                        assert!(q.contains_key("scope"), "the series read is scoped");
                        Json(json!({"data": *rows}))
                    }
                }),
            )
            .route("/api/jobs/{id}/steps/{step_id}", {
                let puts = puts.clone();
                axum::routing::put(
                    move |Path((_id, step_id)): Path<(String, String)>, Json(body): Json<Value>| {
                        let puts = puts.clone();
                        async move {
                            puts.lock().unwrap().push((step_id, body));
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                )
            })
            .route("/api/jobs/{id}/metadata", {
                let patches = patches.clone();
                axum::routing::patch(move |Path(id): Path<String>, Json(body): Json<Value>| {
                    let patches = patches.clone();
                    async move {
                        patches.lock().unwrap().push((id, body));
                        axum::http::StatusCode::NO_CONTENT
                    }
                })
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), puts, patches)
    }

    fn firing(comparison: Value) -> InvocationContext {
        InvocationContext {
            rule_name: "estate-recover-on-comparison".into(),
            triggering_event_id: "evt-test".into(),
            triggering_topic: "jobs.estate.compared".into(),
            event_payload: comparison,
        }
    }

    #[tokio::test]
    async fn a_recovered_alarm_is_closed_through_its_triage_step_and_stamped() {
        let clean = units_row("boss-gcp", &[], at(45));
        let rows = vec![
            clean.clone(),
            units_row("boss-gcp", &[], at(40)),
            units_row("boss-gcp", &[], at(35)),
            units_row("boss-gcp", &[UNIT], at(-5)),
        ];
        let open = vec![alarm(
            "fdd10ec8",
            KEY,
            "host-units",
            Some("boss-gcp"),
            "open",
        )];
        let (base, puts, patches) = stub(open, rows).await;
        let handler = EstateRecover::new(&base);

        handler
            .invoke(&[], &firing(payload(&clean).clone()))
            .await
            .expect("both writes answered");

        let puts = puts.lock().unwrap();
        assert_eq!(puts.len(), 1, "one recovery, one step completion");
        assert_eq!(puts[0].0, "fdd10ec8-triage");
        assert_eq!(puts[0].1["status"], "completed");
        assert_eq!(puts[0].1["metadata"]["disposition"], "stale");
        assert_eq!(puts[0].1["metadata"]["cleared_by"], CLEARED_BY);
        let patches = patches.lock().unwrap();
        assert_eq!(patches.len(), 1, "one recovery, one packet annotation");
        assert_eq!(patches[0].0, "fdd10ec8");
        assert_eq!(patches[0].1["recovered_at"], at(45).to_rfc3339());
    }

    #[tokio::test]
    async fn a_still_present_finding_writes_nothing() {
        let sick = units_row("boss-gcp", &[UNIT], at(45));
        let rows = vec![
            sick.clone(),
            units_row("boss-gcp", &[UNIT], at(40)),
            units_row("boss-gcp", &[UNIT], at(35)),
        ];
        let open = vec![alarm(
            "fdd10ec8",
            KEY,
            "host-units",
            Some("boss-gcp"),
            "open",
        )];
        let (base, puts, patches) = stub(open, rows).await;
        let handler = EstateRecover::new(&base);

        handler
            .invoke(&[], &firing(payload(&sick).clone()))
            .await
            .expect("a no-op is not an error");

        assert!(puts.lock().unwrap().is_empty());
        assert!(patches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unscoped_payload_is_not_a_comparison() {
        let (base, puts, _) = stub(vec![], vec![]).await;
        let handler = EstateRecover::new(&base);
        handler
            .invoke(&[], &firing(json!({"hello": "world"})))
            .await
            .expect("ignored, not failed");
        assert!(puts.lock().unwrap().is_empty());
    }
}
