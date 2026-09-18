//! The jobs-API blip guard and the jobs-api helpers.

use super::*;

// ---------------------------------------------------------------------------
// The jobs-API blip guard
//
// The cluster is the system of record, and it rolls. Twice on
// 2026-08-13 a reconcile hit `Connection refused` to the jobs API
// mid-converge and returned rc=1 for the whole verb — right to refuse
// to act blind, needlessly brittle about an outage that lasted
// seconds (the cadence loop's dock probe held the queue-depth rules
// for that tick on the same blip). A bounded retry covers the roll.
//
// Two rules keep it from papering over anything real: a 4xx is an
// ANSWER and is never retried, and every retry journals one line, so
// blips stay measurable instead of invisible.
// ---------------------------------------------------------------------------

/// What a failed jobs-API attempt was. The classifier reads this and
/// nothing else — pure, and pinned by tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The connection never established — refused, DNS, TLS. Proof
    /// that the request did not reach the system of record.
    Connect,
    /// A timeout, or a response that died mid-body: nothing usable
    /// came back, and whether the write happened is UNKNOWN.
    Ambiguous,
    /// The jobs API answered, with this status.
    Http(u16),
    /// The answer arrived and was unusable — an unparseable body.
    Malformed,
}

/// Retry, or surface? Two rules, and the second is the one that keeps
/// the retry honest:
///
///   - a 4xx is an ANSWER (a 422 is the SoR saying no, and asking the
///     same question three times does not change it); only transport
///     failures and 5xx are blips;
///   - a blip that leaves the write AMBIGUOUS may only be re-sent when
///     the call is idempotent. Re-POSTing an ambiguous create is how
///     one blip becomes two train Jobs. A refused connection is not
///     ambiguous — nothing was received — so anything may go again,
///     which is exactly the production case this exists for.
pub(crate) fn retryable(method: &Method, failure: &Failure) -> bool {
    let idempotent = matches!(
        *method,
        Method::GET | Method::PUT | Method::DELETE | Method::HEAD
    );
    match failure {
        Failure::Connect => true,
        Failure::Ambiguous => idempotent,
        Failure::Http(status) => idempotent && (500..600).contains(status),
        Failure::Malformed => false,
    }
}

/// A reqwest error, classified. Connect / timeout / mid-flight body
/// failures are the blips a rolling SoR produces; a builder or
/// redirect error is a misconfiguration, and retrying one just burns
/// the window three times over.
fn classify_transport(e: &reqwest::Error) -> Failure {
    if e.is_connect() {
        Failure::Connect
    } else if e.is_timeout() || e.is_request() || e.is_body() {
        Failure::Ambiguous
    } else {
        Failure::Malformed
    }
}

/// A jobs-API call that did not succeed: what it was (for the
/// classifier) and the error to surface once the retries run out.
pub(crate) struct ApiFailure {
    pub(crate) kind: Failure,
    pub(crate) cause: anyhow::Error,
}

impl ApiFailure {
    /// A reqwest failure — classified by what reqwest says went wrong.
    pub(crate) fn transport(e: reqwest::Error, context: String) -> Self {
        ApiFailure {
            kind: classify_transport(&e),
            cause: anyhow::Error::new(e).context(context),
        }
    }
}

/// The bounded retry: how many attempts in total, and the first wait
/// between them (each further wait doubles).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub(crate) attempts: u32,
    pub(crate) base: Duration,
}

/// The jobs-API policy: 3 attempts, 2s then 4s. A pod roll is over
/// inside that budget, and a jobs API still refusing after it is an
/// outage the verb should surface rather than paper over.
pub(crate) const JOBS_API_RETRY: RetryPolicy = RetryPolicy {
    attempts: 3,
    base: Duration::from_secs(2),
};

impl RetryPolicy {
    /// The wait before attempt `n + 1`, doubling from `base`.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        self.base * 2u32.pow(attempt.saturating_sub(1).min(16))
    }

    /// The same decisions with no waiting — the tests' policy, so the
    /// retry semantics get pinned without spending the backoff.
    #[cfg(test)]
    pub(crate) const fn immediate(attempts: u32) -> Self {
        RetryPolicy {
            attempts,
            base: Duration::ZERO,
        }
    }
}

/// The one-line cause of a blip: the INNERMOST error, which is where
/// the fact lives ("Connection refused (os error 61)") — the layers
/// above it just repeat the url the journal line already implies.
///
/// `budget` is policy (`delivery_policy.blip_cause_budget`); the
/// truncation is mechanism.
pub(crate) fn short_cause(err: &anyhow::Error, budget: usize) -> String {
    let innermost = err
        .chain()
        .last()
        .map(|c| c.to_string())
        .unwrap_or_default();
    let line = innermost.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= budget {
        return line.to_string();
    }
    format!("{}…", line.chars().take(budget).collect::<String>())
}

/// Run `op` until it succeeds, its failure turns out to be an answer
/// rather than a blip, or the attempt budget runs out. Every retry
/// journals one line through `journal` — the caller's idiom, so the
/// conductor's blips read `conductor: ` and the cadence loop's read
/// `cadence: `. (`+ Sync` because the cadence loop's spawned verb
/// tasks record their outcome through this door, and a future that
/// crosses `tokio::spawn` must be `Send`.)
pub(crate) async fn retrying<T, F, Fut>(
    policy: &RetryPolicy,
    method: &Method,
    cause_budget: usize,
    journal: &(dyn Fn(&str) + Sync),
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, ApiFailure>>,
{
    let mut attempt = 1u32;
    loop {
        let failure = match op().await {
            Ok(v) => return Ok(v),
            Err(f) => f,
        };
        if attempt >= policy.attempts || !retryable(method, &failure.kind) {
            return Err(failure.cause);
        }
        journal(&format!(
            "jobs API blip ({attempt}/{}): {}",
            policy.attempts,
            short_cause(&failure.cause, cause_budget)
        ));
        tokio::time::sleep(policy.backoff(attempt)).await;
        attempt += 1;
    }
}

// ---------------------------------------------------------------------------
// jobs-api helpers
// ---------------------------------------------------------------------------

/// Does this open train hold the single track? Only while it is
/// PRE-MERGE — its `merged` step not yet completed. A merged train's
/// content is already on main: the next consist merges on top of it,
/// and the earlier train's `converged` step is designed to arrive by
/// ANCESTRY (`convergence_verdict` accepts a running commit that is a
/// descendant of its merge), so a later train landing first is what
/// converges it, not what strands it. Holding the track for a merged
/// train deadlocked delivery twice on 2026-09-07 (f3796323): a merged
/// train whose sha bricked its boot sat at `converged` forever, and the
/// fix-forward car could not board because the track was "occupied"
/// by the very train it would have converged.
///
/// Fails closed: a row with no `steps`, or no `merged` step, is
/// pre-merge and holds — unknown is not "clear".
///
/// Twin: `boss_jobs::yard::holds_the_track` reads the same rule off
/// typed steps for the yard board. The typed read model and this JSON
/// cannot share one signature, so each test names its twin
/// (CLAUDE.md §9a).
pub(crate) fn holds_the_track(train: &Value) -> bool {
    // `is_none_or`: no step, or no readable status, holds (fail closed);
    // only a `completed` merge releases.
    find_step(train, "merged", "Merged into main")
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_none_or(|s| s != "completed")
}

/// The train holding the track, named for the journal, or None when
/// the track is clear. Only a PRE-MERGE open train occupies it
/// (`holds_the_track`): a second consist assembled while the first is
/// still merging would merge onto a main the first is about to change
/// (a8c6773b); once the first has merged, the next consist merges on
/// top of it and converges it by ancestry. Arrived and cancelled trains
/// are closed, so both clear the track — a red train that stall-cancels
/// never blocks the next one. Names the first holder.
pub(crate) fn track_occupied_by(open_trains: &[Value]) -> Option<String> {
    open_trains.iter().find(|t| holds_the_track(t)).map(|t| {
        let title = t
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("an unnamed train");
        let id = t.get("id").and_then(Value::as_str).unwrap_or("?");
        format!("{title} ({})", &id[..id.len().min(8)])
    })
}

/// The list body, whether or not the endpoint wrapped it in
/// `{"data": [...]}`.
pub(crate) fn rows(resp: Option<Value>) -> Result<Vec<Value>> {
    let resp = resp.ok_or_else(|| anyhow!("empty response for a list call"))?;
    let list = match resp {
        Value::Object(mut o) if o.contains_key("data") => o.remove("data").unwrap_or(Value::Null),
        other => other,
    };
    match list {
        Value::Array(v) => Ok(v),
        other => bail!("expected a job list, got: {other}"),
    }
}

/// One page of a paginated `/api/jobs` read. Kept at the historical
/// 100 so a backlog that fits under a page still makes exactly one
/// call: the defect this file fixes is paging PAST a page, not making
/// the page bigger.
pub(crate) const PAGE_LIMIT: usize = 100;

/// The `total` a list response carries — the DB-wide count of rows
/// matching the filter AND the caller's policy scope, authoritative
/// over any single page's length (`http/jobs.rs` builds it beside
/// `data`). A body without it is an error, never zero: zero is what a
/// wrong deployment answers (CLAUDE.md §Doors), and this number decides
/// whether every matching car has been read.
pub(crate) fn list_total(body: &Value) -> Result<usize> {
    let total = body
        .get("total")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("list response carries no `total`: {body}"))?;
    usize::try_from(total).context("list total does not fit a usize")
}

/// The offset to request next, or `None` once the rows already gathered
/// cover `total`. The pure pagination decision behind `list_all_pages`.
///
/// A limit is not a filter (memory: a-limit-is-not-a-filter). The job
/// list is `ORDER BY opened_on DESC`, so a car opened days ago but
/// gated and parked today has an OLD `opened_on` and sorts to the tail.
/// Once more than one page of open cars exist (in-flight + parked +
/// landed-but-unclosed residue), a parked-ready car falls off page one
/// and, read with a bare `limit=`, never boards — silently, and exactly
/// when a backlog builds. Looping on this until it returns `None` makes
/// a read see every matching row.
pub(crate) fn next_offset(total: usize, fetched: usize) -> Option<usize> {
    if fetched >= total {
        None
    } else {
        Some(fetched)
    }
}

/// Every row of a paginated `/api/jobs` list, not just page one.
///
/// `fetch` is handed the offset to request and returns that page's body
/// (with `data` and `total`); this pages on `offset` — via
/// [`next_offset`] over the response's [`list_total`] — until the rows
/// gathered cover `total`. Shared by every whole-open-set read in the
/// conductor: boarding (`candidates`), the branch-sweep guard
/// (`open_car_branches`), the dock's merge preview (`preview_dock`),
/// and the cadence loop's dock-depth probe (`cadence::probe_dock_depth`)
/// — one paginator so none of them can under-read the dock again.
///
/// Terminates: each page advances `offset` by the rows it returned, and
/// a page that returns nothing stops the loop, so a `total` that shrinks
/// mid-read (a car closing between pages) cannot spin it.
pub(crate) async fn list_all_pages<F, Fut>(fetch: F) -> Result<Vec<Value>>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let mut out: Vec<Value> = Vec::new();
    loop {
        let body = fetch(out.len())
            .await?
            .ok_or_else(|| anyhow!("empty response for a list call"))?;
        let total = list_total(&body)?;
        let page = rows(Some(body))?;
        if page.is_empty() {
            break;
        }
        out.extend(page);
        if next_offset(total, out.len()).is_none() {
            break;
        }
    }
    Ok(out)
}

pub(crate) fn find_step<'a>(job: &'a Value, slug: &str, title: &str) -> Option<&'a Value> {
    // One lookup, defined in core beside the car builders: the parkers
    // read the same review step this file boards (CLAUDE.md 9a).
    boss_jobs::car::find_step(job, slug, title)
}

pub(crate) fn step_done(step: Option<&Value>) -> bool {
    step.and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|s| s == "completed" || s == "skipped")
}

/// `spec_slug or title` — the label the python conductor logged.
pub(super) fn step_label(step: &Value) -> String {
    step.get("spec_slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| step.get("title").and_then(Value::as_str))
        .unwrap_or("?")
        .to_string()
}

pub(crate) fn id8(id: &str) -> String {
    id.chars().take(8).collect()
}

pub(super) fn job_id(job: &Value) -> Result<&str> {
    job.get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("job without an id"))
}

/// Python truthiness for the metadata fields the conductor reads —
/// absent, null, "", 0 and empty containers are all "not set".
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// How long a gate-run may stay active before it is presumed dead: the
/// gate Job's own `activeDeadlineSeconds` (10800 = 3h) from
/// `infra/gate-runner/gate-runner.yaml`. Past it Kubernetes has killed
/// the Job. CLAUDE.md §9a — the manifest is the authority; if that
/// deadline moves, move this. A CEILING, not an expectation (a normal
/// gate finishes in ~15-90 min), so it can only settle runs truly gone.
pub(crate) const GATE_DEADLINE_HOURS: i64 = 3;

/// How long a gate-run has been active with no verdict, when that is
/// long enough to call it dead — `None` means leave it alone.
///
/// Pure so the decision is testable without an API: a run is dead when
/// it has NOT reported a verdict AND its `opened_at` predates the Job
/// deadline. A run with no `opened_at` yields `None` — absence of a
/// stamp is not evidence of death, and settling on a guess would put a
/// verdict nobody observed into the audit log.
pub(crate) fn dead_gate_run_hours(run: &Value, now: DateTime<Utc>) -> Option<i64> {
    let verdict_step = find_step(run, "record-verdict", "Record the gate verdict");
    if step_done(verdict_step) {
        return None;
    }
    let opened = metadata_map(run)
        .get("opened_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))?;
    let hours = (now - opened).num_hours();
    (hours >= GATE_DEADLINE_HOURS).then_some(hours)
}

/// Should this closed gate-run's verdict be buried if its sha landed?
/// Returns the (sha, verdict) to check when the run is closed with a
/// `failed` or `lost` verdict, names a sha, is not already superseded,
/// and was opened within the yard's approach window — the same two days
/// after which the lens calls a closed gate archaeology, so nothing
/// older is touched. Everything else is None: a green needs no burial,
/// an open run is live activity, and an annotated run is settled.
pub(crate) const APPROACH_FRESH_DAYS: i64 = 2;

pub(crate) fn verdict_to_bury(run: &Value, now: DateTime<Utc>) -> Option<(String, String)> {
    if run.get("status").and_then(Value::as_str) != Some("closed") {
        return None;
    }
    let md = metadata_map(run);
    if md
        .get("superseded")
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    {
        return None;
    }
    let verdict_step = find_step(run, "record-verdict", "Record the gate verdict");
    let verdict = verdict_step
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("verdict"))
        .and_then(Value::as_str)
        .or_else(|| md.get("outcome").and_then(Value::as_str))?;
    if verdict != "failed" && verdict != "lost" {
        return None;
    }
    let sha = md.get("sha").and_then(Value::as_str)?.to_string();
    if sha.is_empty() {
        return None;
    }
    let opened = md
        .get("opened_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))?;
    if (now - opened).num_days() > APPROACH_FRESH_DAYS {
        return None;
    }
    Some((sha, verdict.to_string()))
}

pub(crate) fn metadata_map(v: &Value) -> Map<String, Value> {
    match v.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// The overlay half of `merge_job_metadata`, pure: jobs-api's PATCH
/// semantics stop at the top level — a PUT replaces `metadata`
/// wholesale — so every update must carry the record's existing keys
/// forward. A `Value::Null` value REMOVES the key: how a boarding car
/// sheds a stale `skip_reason` instead of carrying "" forever.
/// Has this train's arrival report already been filed?
///
/// Reads the JOB's metadata, not the `arrived` step's. The report moved
/// there when terminal steps became immutable (f402a681) — and the
/// idempotence check has to move with it, or every reconcile re-files a
/// report it already wrote. That is the failure mode the 2026-08-13
/// journal records as "re-file its arrival report" making the conductor
/// look broken while the trains had in fact landed.
pub(crate) fn arrival_already_filed(train: &Value) -> bool {
    train
        .get("metadata")
        .and_then(|m| m.get("arrival_report"))
        .is_some_and(|v| !v.is_null())
}

pub(crate) fn overlay_metadata(container: &Value, kv: Vec<(&str, Value)>) -> Map<String, Value> {
    let mut md = metadata_map(container);
    for (k, v) in kv {
        match v {
            Value::Null => {
                md.remove(k);
            }
            v => {
                md.insert(k.to_string(), v);
            }
        }
    }
    md
}

#[cfg(test)]
mod track_tests {
    use super::{holds_the_track, track_occupied_by};
    use serde_json::{Value, json};

    /// An open train whose `merged` step sits at `status`.
    fn train(id: &str, title: &str, merged: &str) -> Value {
        json!({
            "id": id,
            "title": title,
            "status": "open",
            "steps": [
                {"spec_slug": "collect", "title": "Collect what is ready to board", "status": "completed"},
                {"spec_slug": "merged", "title": "Merged into main", "status": merged},
                {"spec_slug": "converged", "title": "Cluster converged", "status": "pending"}
            ]
        })
    }

    #[test]
    fn an_open_train_occupies_the_track_and_is_named() {
        let open = vec![train(
            "48b67f2e-9970-456f-817a-d085975b915f",
            "PR train 2026-09-05 07:26",
            "ready",
        )];
        assert_eq!(
            track_occupied_by(&open).as_deref(),
            Some("PR train 2026-09-05 07:26 (48b67f2e)")
        );
    }

    #[test]
    fn no_open_train_means_a_clear_track() {
        // The caller lists status=open only; an arrived or cancelled
        // train is closed and never reaches this list.
        assert_eq!(track_occupied_by(&[]), None);
    }

    /// Twin of boss-jobs `yard::tests::a_merged_train_waiting_to_converge_does_not_hold_the_track`
    /// — the board counts the track by the same rule off typed steps.
    #[test]
    fn a_merged_train_waiting_to_converge_does_not_hold_the_track() {
        // 2026-09-07 (f3796323), twice: a merged train whose sha bricked
        // its boot sat at `converged`, and the fix-forward car could not
        // board because the track was "occupied" by the very train it
        // would have converged. Its content is on main; the next consist
        // merges on top and converges it by ancestry.
        let mut merged = train(
            "a1b2c3d4-0000-0000-0000-000000000000",
            "PR train 2026-09-07 21:00",
            "completed",
        );
        merged["steps"][2]["status"] = json!("ready");
        assert!(!holds_the_track(&merged));
        assert_eq!(track_occupied_by(&[merged]), None);
    }

    /// Twin of boss-jobs `yard::tests::only_the_pre_merge_train_counts_as_the_track`.
    #[test]
    fn the_pre_merge_train_is_named_when_a_merged_one_is_also_open() {
        let merged = train(
            "a1b2c3d4-0000-0000-0000-000000000000",
            "PR train 2026-09-07 21:00",
            "completed",
        );
        let pre_merge = train(
            "e5f6a7b8-0000-0000-0000-000000000000",
            "PR train 2026-09-07 22:51",
            "ready",
        );
        assert_eq!(
            track_occupied_by(&[merged, pre_merge]).as_deref(),
            Some("PR train 2026-09-07 22:51 (e5f6a7b8)")
        );
    }

    #[test]
    fn a_train_with_no_steps_fails_closed_as_pre_merge() {
        // Unknown is not "clear": a row the list did not enrich, or a
        // train with no merged step at all, holds the track.
        let bare = json!({
            "id": "48b67f2e-9970-456f-817a-d085975b915f",
            "title": "PR train 2026-09-05 07:26",
            "status": "open"
        });
        assert!(holds_the_track(&bare));
        assert_eq!(
            track_occupied_by(&[bare]).as_deref(),
            Some("PR train 2026-09-05 07:26 (48b67f2e)")
        );
        let no_merged = json!({
            "id": "48b67f2e-9970-456f-817a-d085975b915f",
            "title": "PR train 2026-09-05 07:26",
            "steps": [{"spec_slug": "collect", "title": "Collect what is ready to board", "status": "ready"}]
        });
        assert!(holds_the_track(&no_merged));
    }
}

#[cfg(test)]
mod burial_tests {
    use super::verdict_to_bury;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn run(
        status: &str,
        verdict: &str,
        opened: &str,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut md = json!({"branch": "fix/lean-ci-builds", "sha": "6c0e31a34a7659aebd63c07218cfddc1e3542fb9", "opened_at": opened});
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                md[k] = v.clone();
            }
        }
        json!({
            "id": "335d5f6d-0000-0000-0000-000000000000", "kind": "gate-run", "status": status, "metadata": md,
            "steps": [{"title": "Record the gate verdict", "spec_slug": "record-verdict", "status": "completed", "metadata": {"verdict": verdict}}]
        })
    }

    #[test]
    fn a_fresh_lost_or_failed_verdict_with_a_sha_is_a_candidate() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 17, 0, 0).unwrap();
        for v in ["lost", "failed"] {
            let r = run("closed", v, "2026-09-04T00:20:43Z", json!({}));
            assert_eq!(
                verdict_to_bury(&r, now),
                Some((
                    "6c0e31a34a7659aebd63c07218cfddc1e3542fb9".to_string(),
                    v.to_string()
                ))
            );
        }
    }

    #[test]
    fn a_green_an_open_run_an_annotated_run_and_archaeology_are_left_alone() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 17, 0, 0).unwrap();
        assert_eq!(
            verdict_to_bury(
                &run("closed", "green", "2026-09-04T00:20:43Z", json!({})),
                now
            ),
            None
        );
        assert_eq!(
            verdict_to_bury(&run("open", "lost", "2026-09-04T00:20:43Z", json!({})), now),
            None
        );
        assert_eq!(
            verdict_to_bury(
                &run(
                    "closed",
                    "lost",
                    "2026-09-04T00:20:43Z",
                    json!({"superseded": "by hand"})
                ),
                now
            ),
            None
        );
        assert_eq!(
            verdict_to_bury(
                &run("closed", "lost", "2026-09-01T00:20:43Z", json!({})),
                now
            ),
            None,
            "older than the approach window"
        );
        assert_eq!(
            verdict_to_bury(
                &run("closed", "lost", "2026-09-04T00:20:43Z", json!({"sha": ""})),
                now
            ),
            None,
            "no sha, nothing to ask git"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 2026-09-04: two gate-runs whose pods were evicted sat at
    /// `record-verdict` for 17 hours, each holding one of three gate
    /// slots and rendering as a live gate, while their branches had long
    /// since landed. A third died the same way and silently ate a car —
    /// the change was never gated and nobody noticed until a census.
    /// gate-runner.yaml already promised "a runner that dies anyway
    /// leaves an overdue packet"; nothing was listening.
    #[test]
    fn a_gate_run_past_the_job_deadline_is_dead() {
        let now = DateTime::parse_from_rfc3339("2026-09-04T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let run = |opened: &str, verdict_done: bool| {
            serde_json::json!({
                "id": "11111111-1111-1111-1111-111111111111",
                "metadata": { "branch": "feat/x", "opened_at": opened },
                "steps": [{
                    "spec_slug": "record-verdict",
                    "title": "Record the gate verdict",
                    "status": if verdict_done { "completed" } else { "ready" },
                    "metadata": {}
                }]
            })
        };
        // 17h with no verdict: dead, and it reports how long.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-03T20:00:00Z", false), now),
            Some(17)
        );
        // Inside the deadline it is simply a gate that is running.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-04T11:30:00Z", false), now),
            None
        );
        // A run that REPORTED is never ours to touch, however old.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-03T20:00:00Z", true), now),
            None
        );
        // No stamp, no claim — settling on a guess would write a verdict
        // nobody observed into the audit log.
        let unstamped = serde_json::json!({
            "id": "22222222-2222-2222-2222-222222222222",
            "metadata": { "branch": "feat/y" },
            "steps": [{"spec_slug":"record-verdict","title":"Record the gate verdict","status":"ready","metadata":{}}]
        });
        assert_eq!(dead_gate_run_hours(&unstamped, now), None);
    }

    // -- the conductor reads all its cars, not just page one -----------

    #[test]
    fn next_offset_pages_past_the_first_hundred() {
        // A backlog under one page needs no second read.
        assert_eq!(next_offset(0, 0), None);
        assert_eq!(next_offset(42, 42), None);
        assert_eq!(next_offset(PAGE_LIMIT, PAGE_LIMIT), None);
        // 150 open cars, 100 read: page two starts at offset 100.
        assert_eq!(next_offset(150, 100), Some(100));
        // page two read: the whole backlog is covered.
        assert_eq!(next_offset(150, 150), None);
        // defensive — a `total` that shrank mid-read never asks for more.
        assert_eq!(next_offset(150, 160), None);
    }

    #[test]
    fn list_total_reads_total_not_the_page() {
        let body = json!({"data": [{"id": "car-1"}], "limit": 100, "offset": 0, "total": 150});
        assert_eq!(list_total(&body).unwrap(), 150);
        // no `total` is an error, never zero: zero is a wrong deployment.
        assert!(list_total(&json!({"data": []})).is_err());
    }

    /// THE STARVATION REGRESSION. A parked-ready car opened days ago
    /// sorts to the tail of `opened_on DESC`; with 150 open cars it sits
    /// on page two (offset 100). The old bare `limit=100` read left it
    /// off page one forever. `list_all_pages` — the read every whole-dock
    /// caller now shares (candidates, open_car_branches, preview_dock,
    /// probe_dock_depth) — must gather it.
    #[tokio::test]
    async fn list_all_pages_reads_the_car_on_page_two() {
        let all: Vec<Value> = (0..150)
            .map(|i| json!({"id": format!("car-{i}")}))
            .collect();
        let all_ref = &all;
        let calls = std::cell::Cell::new(0u32);
        let gathered = list_all_pages(|offset| {
            calls.set(calls.get() + 1);
            async move {
                let page: Vec<Value> = all_ref
                    .iter()
                    .skip(offset)
                    .take(PAGE_LIMIT)
                    .cloned()
                    .collect();
                anyhow::Ok(Some(json!({
                    "data": page,
                    "total": all_ref.len(),
                    "offset": offset,
                    "limit": PAGE_LIMIT,
                })))
            }
        })
        .await
        .unwrap();
        assert_eq!(gathered.len(), 150, "every open car must be read");
        assert!(
            gathered.iter().any(|j| j["id"] == "car-149"),
            "the car on page two must be gathered, not left off page one"
        );
        assert_eq!(calls.get(), 2, "150 cars is two pages of 100");
    }

    /// A backlog that fits under a page still makes exactly one call —
    /// the fix is paging PAST a page, never changing behaviour below it.
    #[tokio::test]
    async fn list_all_pages_makes_one_call_below_a_page() {
        let calls = std::cell::Cell::new(0u32);
        let gathered = list_all_pages(|offset| {
            calls.set(calls.get() + 1);
            async move {
                assert_eq!(offset, 0, "a sub-page backlog never asks for page two");
                anyhow::Ok(Some(json!({
                    "data": (0..42).map(|i| json!({"id": i})).collect::<Vec<_>>(),
                    "total": 42,
                    "offset": offset,
                    "limit": PAGE_LIMIT,
                })))
            }
        })
        .await
        .unwrap();
        assert_eq!(gathered.len(), 42);
        assert_eq!(calls.get(), 1);
    }

    // -- metadata overlays merge, never clobber ----------------------------
    //
    // jobs-api PUT replaces top-level `metadata` wholesale; every
    // update must carry the existing keys forward, and clearing a key
    // means removing it, not writing "".

    #[test]
    fn a_metadata_overlay_preserves_existing_keys() {
        let job = json!({"metadata": {"branch": "feat/x", "queue": "q-1"}});
        let md = overlay_metadata(&job, vec![("skip_reason", json!("conflict: a.rs"))]);
        assert_eq!(md.get("branch"), Some(&json!("feat/x")));
        assert_eq!(md.get("queue"), Some(&json!("q-1")));
        assert_eq!(md.get("skip_reason"), Some(&json!("conflict: a.rs")));
    }

    #[test]
    fn a_null_overlay_removes_the_key() {
        // Boarding stamps `train` and sheds the stale skip note in one
        // update; the key goes away rather than lingering as "".
        let job = json!({"metadata": {"branch": "feat/x", "skip_reason": "conflict: a.rs"}});
        let md = overlay_metadata(
            &job,
            vec![("train", json!("t-1")), ("skip_reason", Value::Null)],
        );
        assert!(!md.contains_key("skip_reason"));
        assert_eq!(md.get("train"), Some(&json!("t-1")));
        assert_eq!(md.get("branch"), Some(&json!("feat/x")));
    }

    #[test]
    fn an_overlay_on_a_bare_job_starts_fresh() {
        let job = json!({"id": "j-1"});
        let md = overlay_metadata(&job, vec![("skip_reason", json!("x"))]);
        assert_eq!(md.len(), 1);
        // Removing a key that was never there is a quiet no-op.
        let md = overlay_metadata(&job, vec![("skip_reason", Value::Null)]);
        assert!(md.is_empty());
    }

    /// f402a681: the report moved to the job when terminal steps became
    /// immutable, so the "already filed" check has to read the job too.
    /// Reading the step instead means every reconcile re-files a report
    /// it already wrote.
    #[test]
    fn arrival_filed_is_read_from_the_job_not_the_step() {
        let unfiled = json!({
            "metadata": {"boarded_jobs": []},
            "steps": [{"title": "Train arrived", "status": "completed", "metadata": {}}],
        });
        assert!(!arrival_already_filed(&unfiled));

        let filed = json!({
            "metadata": {"arrival_report": {"consist": []}, "arrival_summary": "2 cars"},
            "steps": [{"title": "Train arrived", "status": "completed", "metadata": {}}],
        });
        assert!(arrival_already_filed(&filed));

        // A report on the STEP is the OLD location. It must not count as
        // filed, or trains that pre-date the move never get a job-level
        // report and their branch sweep stays blocked.
        let old_location = json!({
            "metadata": {},
            "steps": [{
                "title": "Train arrived",
                "status": "completed",
                "metadata": {"arrival_report": {"consist": []}},
            }],
        });
        assert!(
            !arrival_already_filed(&old_location),
            "a report on the step is not a report on the job"
        );
    }

    /// PATCH merges, and a null value DELETES a key — so a null report
    /// must read as "not filed" rather than as a filed one.
    #[test]
    fn a_null_report_is_not_filed() {
        let nulled = json!({"metadata": {"arrival_report": null}});
        assert!(!arrival_already_filed(&nulled));
    }

    // -- the jobs-API retry classifier -------------------------------------
    //
    // The cluster is the system of record and it rolls. Twice on
    // 2026-08-13 a reconcile hit `Connection refused` to the jobs API
    // mid-converge and failed the whole verb; the blip lasted seconds.
    // A bounded retry covers the roll — but only for failures that are
    // blips, and only where re-sending is safe.

    #[test]
    fn a_refused_connection_is_a_blip_under_any_method() {
        // Nothing was received, so nothing was done: even a create may
        // go again.
        assert!(retryable(&Method::GET, &Failure::Connect));
        assert!(retryable(&Method::PUT, &Failure::Connect));
        assert!(retryable(&Method::POST, &Failure::Connect));
    }

    #[test]
    fn an_ambiguous_blip_only_retries_an_idempotent_call() {
        // A timeout leaves the write UNKNOWN — re-POSTing an ambiguous
        // create is how one blip becomes two train Jobs.
        assert!(retryable(&Method::GET, &Failure::Ambiguous));
        assert!(retryable(&Method::PUT, &Failure::Ambiguous));
        assert!(!retryable(&Method::POST, &Failure::Ambiguous));
    }

    #[test]
    fn a_5xx_is_a_blip_and_a_4xx_is_an_answer() {
        for status in [500, 502, 503, 504] {
            assert!(
                retryable(&Method::GET, &Failure::Http(status)),
                "{status} is the SoR failing to answer"
            );
            assert!(
                !retryable(&Method::POST, &Failure::Http(status)),
                "{status} leaves a create ambiguous"
            );
        }
        // A 422 is the jobs API telling the conductor no. Retrying an
        // answer just asks the same question three times — including
        // 429, which is an answer about rate, not a transport blip.
        for status in [400, 404, 409, 422, 429] {
            assert!(!retryable(&Method::GET, &Failure::Http(status)), "{status}");
            assert!(!retryable(&Method::PUT, &Failure::Http(status)), "{status}");
        }
        // 2xx/3xx never reach the classifier, and are not blips either.
        assert!(!retryable(&Method::GET, &Failure::Http(200)));
        assert!(!retryable(&Method::GET, &Failure::Http(301)));
    }

    #[test]
    fn an_unusable_answer_is_never_a_blip() {
        // The SoR answered; the body was garbage. Retrying re-reads
        // the same garbage.
        assert!(!retryable(&Method::GET, &Failure::Malformed));
        assert!(!retryable(&Method::POST, &Failure::Malformed));
    }

    #[test]
    fn the_backoff_doubles_from_the_base() {
        assert_eq!(JOBS_API_RETRY.attempts, 3);
        assert_eq!(JOBS_API_RETRY.backoff(1), Duration::from_secs(2));
        assert_eq!(JOBS_API_RETRY.backoff(2), Duration::from_secs(4));
        // The tests' policy makes the same decisions and never waits.
        assert_eq!(RetryPolicy::immediate(3).backoff(1), Duration::ZERO);
    }

    #[test]
    fn a_blip_cause_reads_the_innermost_error() {
        // "GET /api/jobs: error sending request: ... : Connection
        // refused" — the fact is at the bottom; the url is already
        // implied by the line around it.
        let e = anyhow!("Connection refused (os error 61)")
            .context("error sending request for url (http://10.20.0.34:7900/api/jobs)")
            .context("GET /api/jobs?kind=pr-train");
        assert_eq!(
            short_cause(&e, policy().blip_cause_budget),
            "Connection refused (os error 61)"
        );
        // A bare error is its own innermost cause.
        assert_eq!(
            short_cause(&anyhow!("HTTP 503"), policy().blip_cause_budget),
            "HTTP 503"
        );
        // And it stays journal-sized.
        let long = short_cause(&anyhow!("{}", "x".repeat(500)), policy().blip_cause_budget);
        assert!(long.chars().count() <= 81, "{} chars", long.chars().count());
        assert!(long.ends_with('…'), "says it truncated: {long}");
    }

    #[test]
    fn a_real_refused_connection_classifies_as_a_blip() {
        // The production failure end to end: reqwest's own error for a
        // refused connect must land on a retryable Failure, or the
        // classifier above is pinning a shape the wire never produces.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(async {
            reqwest::Client::builder()
                .timeout(Duration::from_millis(250))
                .build()
                .unwrap()
                // Port 1 refuses; a filtered port times out. Both are
                // blips, and neither is an answer.
                .get("http://127.0.0.1:1/api/jobs")
                .send()
                .await
                .expect_err("nothing serves port 1")
        });
        let kind = classify_transport(&err);
        assert!(
            matches!(kind, Failure::Connect | Failure::Ambiguous),
            "a refused/timed-out connect must be a transport failure, got {kind:?}"
        );
        assert!(retryable(&Method::GET, &kind));
    }

    // -- the retry driver --------------------------------------------------

    fn blip(kind: Failure) -> ApiFailure {
        ApiFailure {
            kind,
            cause: anyhow!("Connection refused (os error 61)"),
        }
    }

    /// A journal that counts its lines instead of printing them.
    /// Atomic rather than `Cell` because `retrying` now takes a
    /// `Sync` journal (the cadence loop's spawned verb tasks report
    /// through it).
    fn counting_journal(lines: &AtomicU32) -> impl Fn(&str) + Sync {
        move |_| {
            lines.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn a_blip_retries_to_the_attempt_budget_then_surfaces() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<()> = retrying(
            &RetryPolicy::immediate(3),
            &Method::GET,
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                async { Err(blip(Failure::Connect)) }
            },
        )
        .await;
        assert!(out.is_err(), "the verb still surfaces a real outage");
        assert_eq!(calls, 3, "three attempts, not more");
        assert_eq!(
            lines.load(Ordering::Relaxed),
            2,
            "one line per retry — blips stay countable"
        );
    }

    #[tokio::test]
    async fn a_recovered_blip_costs_nothing_but_a_line() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<u8> = retrying(
            &RetryPolicy::immediate(3),
            &Method::PUT,
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                let attempt = calls;
                async move {
                    if attempt == 1 {
                        Err(blip(Failure::Ambiguous))
                    } else {
                        Ok(7)
                    }
                }
            },
        )
        .await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls, 2, "stops the moment the SoR answers");
        assert_eq!(lines.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn an_answer_is_surfaced_on_the_first_attempt() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<()> = retrying(
            &RetryPolicy::immediate(3),
            &Method::PUT,
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                async {
                    Err(ApiFailure {
                        kind: Failure::Http(422),
                        cause: anyhow!("PUT /api/jobs/x: HTTP 422: metadata_schema"),
                    })
                }
            },
        )
        .await;
        assert!(
            out.unwrap_err().to_string().contains("422"),
            "the answer reaches the operator unchanged"
        );
        assert_eq!(calls, 1, "a 422 is an answer — asked once");
        assert_eq!(
            lines.load(Ordering::Relaxed),
            0,
            "an answer is not a blip and journals none"
        );
    }
}
