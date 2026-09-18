//! A red verdict names its LOG — the failing-check excerpt and the red-train alert.

use super::*;

// ---------------------------------------------------------------------------
// A red verdict names its LOG, not just its check (CLAUDE.md §Diagnosis,
// "a verdict must name what failed"). Naming `test` is half the answer;
// the operator still had to go re-derive WHY. These pure helpers turn a
// failing check into the id of the job that failed and a bounded tail of
// its log, resolving the id through the ONE endpoint that answers
// correctly. The forge seam does the fetching; everything decidable
// about a JSON payload is decided here, where it can be pinned.
//
// THE TRAP these encode around (live-confirmed 2026-09-06): the job log
// lives at `/actions/jobs/{JOB_ID}/logs`, but the JOB_ID must come from
// `/actions/runs/{run}/jobs` — NOT from `/actions/tasks`. A commit
// status's `target_url` is `…/actions/runs/{run}/jobs/{index}`, so it
// carries the run AND the job's position in that run: index into the
// run's jobs array and read its `id`. Reading the entry's `task_id`
// instead (the tasks-list id) silently returns a DIFFERENT job's log —
// run 462's failing `web` was job id 2008 / task_id 1939, and
// `jobs/1939/logs` was an unrelated SUCCESS. Position is unambiguous;
// name is only a cross-check.

/// How many bytes of a job log to pull (a suffix Range request — the
/// forge answers `206 Partial Content`, so a 300KB+ log never crosses
/// the wire whole) and how much of that to keep once fetched.
/// How much of a failing job's log to pull back from the forge.
///
/// This was 16 KB for as long as the alert only ever showed a TAIL. On
/// 2026-09-09 train 864a4896 reddened on a Rust test that panicked at
/// byte ~460 K of a 778 KB log; the last 16 KB held svelte-check
/// deprecation warnings from five minutes later, so the alert named the
/// check and then showed the reader something irrelevant to it, and the
/// real cause took a hand dig through the forge Actions API to find
/// (packet 792d26be). A suffix Range large enough to contain the whole
/// of any log this pipeline has produced is what lets
/// [`failing_excerpt`] find the failure at all; the bound still exists
/// so a pathological log cannot be pulled into memory unbounded.
pub(crate) const LOG_FETCH_BYTES: u64 = 8_388_608;
pub(crate) const LOG_TAIL_LINES: usize = 40;
pub(crate) const LOG_TAIL_BYTES: usize = 4_000;
/// Ceiling on the combined-logs blob stamped onto the `ci` step.
pub(crate) const CI_STEP_LOG_BYTES: usize = 8_000;

/// Parse `…/actions/runs/{run}/jobs/{index}` (a commit status's
/// `target_url`, relative or absolute) into `(run_id, job_index)`. The
/// job index is a position WITHIN that run's jobs array, not a job id.
/// `None` when the shape is absent — a url with a run but no `/jobs/{n}`
/// cannot be resolved to one job, and we never guess.
pub(crate) fn parse_run_job_ref(target_url: &str) -> Option<(i64, usize)> {
    let after = target_url.split("actions/runs/").nth(1)?;
    let mut parts = after.splitn(2, "/jobs/");
    let run: i64 = parts.next()?.trim_matches('/').parse().ok()?;
    let idx_part = parts.next()?;
    let idx_str: String = idx_part.chars().take_while(char::is_ascii_digit).collect();
    let index: usize = idx_str.parse().ok()?;
    Some((run, index))
}

/// Does a commit-status `context` name this job? `"CI / web
/// (pull_request)"` names job `"web"`: drop the `" (event)"` suffix,
/// take the segment after the last `" / "`, compare case-insensitively.
/// A cross-check on the positional resolution, never the resolution
/// itself — a mismatch downgrades the attachment to "resolved by
/// position, note the mismatch", it does not pick a different job.
pub(crate) fn context_names_job(context: &str, job_name: &str) -> bool {
    if job_name.trim().is_empty() {
        return false;
    }
    let ctx = context.to_lowercase();
    let ctx = ctx.split(" (").next().unwrap_or(&ctx);
    let seg = ctx.rsplit(" / ").next().unwrap_or(ctx).trim();
    seg == job_name.trim().to_lowercase()
}

/// The failing job's id, read POSITIONALLY from a run's jobs array (the
/// payload of `/actions/runs/{run}/jobs`). Returns the job's own `id`
/// (NEVER `task_id` — that is the trap), plus an optional note when the
/// job at that position does not obviously match the check context, so
/// an ambiguous mapping attaches what it can and SAYS it is unsure
/// rather than guessing a wrong job. `None` only when the index is out
/// of range or the entry carries no numeric `id`.
pub(crate) fn job_id_for_index(
    jobs: &[Value],
    index: usize,
    context: &str,
) -> Option<(i64, Option<String>)> {
    let job = jobs.get(index)?;
    let id = job.get("id").and_then(Value::as_i64)?;
    let name = job.get("name").and_then(Value::as_str).unwrap_or_default();
    let note = if context_names_job(context, name) {
        None
    } else {
        Some(format!(
            "resolved by target_url position (run job index {index}); \
             job name {name:?} did not obviously match check {context:?}"
        ))
    };
    Some((id, note))
}

/// Does a `Content-Range` header say the returned slice starts past
/// byte 0? A suffix Range fetch of a large log starts mid-line, so the
/// first line is a fragment to drop; a small log the server returned
/// whole (start 0, or no header) has a real first line to keep.
pub(crate) fn content_range_starts_past_zero(content_range: Option<&str>) -> bool {
    let Some(cr) = content_range else {
        return false;
    };
    // "bytes 303107-307106/307107" -> first number is the start.
    cr.trim()
        .strip_prefix("bytes")
        .unwrap_or(cr)
        .trim()
        .split('-')
        .next()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .is_some_and(|start| start > 0)
}

/// Reduce a raw log slice to a bounded, honest tail: drop a leading
/// partial line when the slice was a mid-file Range, keep the last
/// `max_lines`, then cap at `max_bytes` cutting on a line boundary. A
/// leading marker states it is a tail whenever anything was dropped, so
/// the words never claim to be the whole log (Orwell: the record says
/// what it is).
pub(crate) fn log_tail(
    body: &str,
    drop_partial_first: bool,
    max_lines: usize,
    max_bytes: usize,
) -> String {
    let body = body.trim_end_matches(['\n', '\r']);
    if body.is_empty() {
        return String::new();
    }
    let mut lines: Vec<&str> = body.lines().collect();
    let dropped_partial = drop_partial_first && lines.len() > 1;
    if dropped_partial {
        lines.remove(0);
    }
    let dropped_lines = lines.len() > max_lines;
    if dropped_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    let mut out = lines.join("\n");
    let mut dropped_bytes = false;
    if out.len() > max_bytes {
        let start = out.len().saturating_sub(max_bytes);
        // Prefer the next newline after `start`; else the next char
        // boundary, so the slice is always valid UTF-8.
        let cut = out[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or_else(|| {
                let mut s = start;
                while s < out.len() && !out.is_char_boundary(s) {
                    s += 1;
                }
                s
            });
        out = out[cut..].to_string();
        dropped_bytes = true;
    }
    if dropped_partial || dropped_lines || dropped_bytes {
        format!("…(log tail — earlier lines omitted)\n{out}")
    } else {
        out
    }
}

/// The lines a failing CI log is worth reading, in the order a reader
/// would grep for them. Each is unambiguous about a FAILURE: a Rust
/// panic and its location, cargo's per-target verdict, cargo's own
/// summary line, the gate runner's own refusal, a rustc error code, a
/// forge annotation, and bun's per-test failure marker.
///
/// Deliberately absent is a bare `error:`. The mocked web suite prints
/// hundreds of benign `error: Unable to connect` lines for backends it
/// does not run, and a marker that matches those points the excerpt at
/// noise — which is the defect this whole function exists to fix, in a
/// new place. Checked against both red logs of 2026-09-09: four matches
/// each across 9287 and 7300 lines, the first being the panic, and no
/// false positive.
pub(crate) const FAILURE_MARKERS: &[&str] = &[
    "panicked at",
    "test result: FAILED",
    "error: test failed",
    "GATE FAIL:",
    "error[E",
    "##[error]",
    "(fail)",
];

/// The excerpt a red-train alert should carry: the window around the
/// FIRST failure marker, or the tail when nothing matched.
///
/// A tail is what you show when you do not know what you are looking
/// for. Here the markers are known, so showing the end of the log
/// instead is a choice to hand the reader the wrong 40 lines — and on
/// 2026-09-09 it did exactly that, for a train carrying six cars that
/// had each gated green.
///
/// The first match is the one kept: a test run reports its earliest
/// failure first, and the later markers are usually that same failure
/// restated (the panic, then the target verdict, then cargo's summary,
/// then the gate's). How many matched is stated, so a reader who needs
/// the others knows they exist.
///
/// The result always says which of the two it is. A packet that shows a
/// tail while implying a diagnosis is the same defect one layer up.
pub(crate) fn failing_excerpt(
    body: &str,
    drop_partial_first: bool,
    max_lines: usize,
    max_bytes: usize,
) -> String {
    let trimmed = body.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return String::new();
    }
    let mut lines: Vec<&str> = trimmed.lines().collect();
    // A suffix Range starts mid-line; that fragment is not evidence.
    if drop_partial_first && lines.len() > 1 {
        lines.remove(0);
    }

    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| FAILURE_MARKERS.iter().any(|m| l.contains(m)))
        .map(|(i, _)| i)
        .collect();

    let Some(&first) = hits.first() else {
        // Nothing named a failure. The tail is then the honest answer,
        // and it says so in its own words.
        return log_tail(body, drop_partial_first, max_lines, max_bytes);
    };

    // Lead-in enough to carry the test's name and the lines it printed
    // before dying. The window ENDS a few lines past the LAST marker
    // rather than running out its whole line budget: a Rust failure
    // restates itself four times in six lines and is then followed by
    // whatever else the job printed, and spending the budget on that is
    // how the reader ends up looking at svelte warnings again.
    let before = max_lines / 3;
    let last = *hits.last().unwrap_or(&first);
    let start = first.saturating_sub(before);
    let end = (last + 4)
        .min(lines.len())
        .min(start + max_lines)
        .max(first + 1);
    let mut out = lines[start..end].join("\n");

    if out.len() > max_bytes {
        let mut cut = max_bytes;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }

    // Only failures the reader cannot see are worth mentioning. The
    // three or four lines a single Rust failure prints are all inside
    // the window, and announcing them as "3 more" would send someone
    // looking for a second failure that does not exist.
    let more = hits.iter().filter(|&&i| i >= end).count();
    let also = if more == 0 {
        String::new()
    } else if more == 1 {
        ", 1 more further down".to_string()
    } else {
        format!(", {more} more further down")
    };
    format!("…(the failure, with context{also})\n{out}")
}

/// The `(check, log_tail)` pairs the forge managed to attach to failing
/// rollup entries. Skips entries with no `log_tail` — which is exactly
/// the state a FAILED or ambiguous fetch leaves behind, so a fetch that
/// could not resolve a log yields no pair rather than an error. Used by
/// both the `ci` step's evidence and the red-train alert body.
pub(crate) fn failing_check_logs(rollup: Option<&Value>) -> Vec<(String, String)> {
    rollup
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
        .filter_map(|c| {
            let tail = c
                .get("log_tail")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())?;
            let ctx = c
                .get("context")
                .or_else(|| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("unnamed check");
            Some((ctx.to_string(), tail.to_string()))
        })
        .collect()
}

/// Flatten the failing-check logs into one bounded blob for the `ci`
/// step's `check_logs` evidence field (`complete_step` stores strings).
/// Each block is headed by its check; the whole is capped so a step row
/// never carries megabytes.
pub(crate) fn format_check_logs(logs: &[(String, String)], max_bytes: usize) -> String {
    let mut out = String::new();
    for (ctx, tail) in logs {
        let block = format!("--- {ctx} ---\n{tail}\n");
        if !out.is_empty() && out.len() + block.len() > max_bytes {
            out.push_str("…(further check logs omitted)\n");
            break;
        }
        out.push_str(&block);
        if out.len() >= max_bytes {
            break;
        }
    }
    // A single block can still exceed the cap (a tail at its own limit
    // plus a long header). Hold the ceiling absolutely: keep the HEAD
    // here — the header names the check, and the per-check tail bound
    // already kept the log's end. Cut on a char boundary.
    if out.len() > max_bytes {
        let mut end = max_bytes;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out.trim_end().to_string()
}

/// A red train's self-announcement.
pub(crate) struct RedTrainAlert {
    pub(crate) title: String,
    pub(crate) failing: Vec<String>,
    pub(crate) refused: bool,
    /// `(check, log_tail)` for each failing check whose log the forge
    /// resolved — empty when none could be fetched, which is a missing
    /// attachment, never an error (observability is best-effort). When
    /// the red is the train GATE's, the gate-run receipt's
    /// `fails_excerpt` rides here instead, one entry per failed check
    /// labelled `gate: <check>` (5708cbd5).
    pub(crate) logs: Vec<(String, String)>,
}

/// A red train announces itself IMMEDIATELY — pure over the LIVE verdict,
/// like [`auto_cancel_reason`], but it fires on the FIRST red pass rather
/// than waiting out the stall threshold, because the point is that a red
/// train is never a surprise (d69c4274, David: "red trains shouldn't
/// surprise us"). `Some` when the live CI verdict is `failing` and the
/// train has not merged; `None` otherwise. The alert NAMES the failing
/// checks and whether any was an infrastructure refusal, so the reader
/// sees WHAT failed without re-deriving it from the forge ("a verdict
/// must name what failed").
pub(crate) fn red_train_alert(
    train: &Value,
    live_verdict: &str,
    rollup: Option<&Value>,
    gate_fails: &[String],
    gate_excerpt: &[(String, String)],
) -> Option<RedTrainAlert> {
    if live_verdict != "failing" {
        return None;
    }
    // A merged train's checks are history; the content already landed.
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let mut failing = failing_checks(rollup);
    let refused = any_failing_check_refused(rollup);
    // The OTHER half of the verdict (128b5496): a red train gate names
    // its failing checks on its receipt. Train #361's alert said "check
    // names unavailable" while the gate-run a1d0d144 held the name —
    // CI was green, the gate was red, and only the rollup was read
    // (071d8b23). A verdict must name what failed.
    let gate_red = failing.is_empty() && !gate_fails.is_empty();
    let mut logs = failing_check_logs(rollup);
    if gate_red {
        failing.extend(gate_fails.iter().cloned());
        // And WHY (5708cbd5): the receipt's excerpt of each failed
        // check — the lines the gate pod replayed and then took with
        // it — attached as a forge check's log is, labelled as the
        // gate's. Train #361's alert had the name and nothing else.
        logs.extend(
            gate_excerpt
                .iter()
                .map(|(check, text)| (format!("gate: {check}"), text.clone())),
        );
    }
    let id = train.get("id").and_then(Value::as_str).unwrap_or("");
    let named = if failing.is_empty() {
        "check names unavailable".to_string()
    } else {
        failing.join(", ")
    };
    let title = if refused {
        format!(
            "Red train {} — CI REFUSED (infrastructure, not the consist): {named}",
            id8(id)
        )
    } else if gate_red {
        format!("Red train {} — train gate failed: {named}", id8(id))
    } else {
        format!("Red train {} — CI failed: {named}", id8(id))
    };
    Some(RedTrainAlert {
        title,
        failing,
        refused,
        logs,
    })
}

/// The backlog-item body for a red-train alert. A FREE, PURE function so
/// it can be tested against the fields the jobs API demands — which is
/// exactly what the first cut of this alert got wrong: it omitted
/// `owner_id`, `status`, and `tags`, so every POST returned HTTP 422 and,
/// because reconcile filed the alert with `?`, the whole pass aborted at
/// rc=1. One red train then froze all landings for ~8h (2026-09-06). The
/// gate passed it because it only exercised `red_train_alert` (the pure
/// decision), never this body against the API. Now the body is pure and
/// pinned, and `reconcile` files it best-effort (see `announce_red_train`).
pub(crate) fn red_train_alert_body(tid: &str, alert: &RedTrainAlert, owner: &str) -> Value {
    json!({
        "kind": "backlog-item",
        "title": alert.title,
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": owner,
        "status": "open",
        "tags": [],
        "priority": "urgent",
        "metadata": {
            "title": alert.title,
            "train_alert": tid,
            "failing_checks": alert.failing,
            "refused": alert.refused,
            // The failing job's log tail — or, for a red train gate,
            // the gate receipt's excerpt of each failed check — so the
            // alert names not just WHICH check failed but WHY: no
            // hand-archaeology through the forge, no kubectl into a
            // pod log that has already been reaped. Empty when no log
            // could be resolved.
            "failing_logs": alert.logs.iter()
                .map(|(check, tail)| json!({"check": check, "log_tail": tail}))
                .collect::<Vec<_>>(),
            "reporter": "conductor",
            "source": "pipeline-failure (red train)",
        }
    })
}

/// Has this train already filed its one red-train alert? The dedup is a
/// per-train metadata FLAG (`red_alert_filed`), mirroring
/// `deploy_alarm_filed` / `converge_alarm_filed` — NOT a scan of open
/// backlog-items. The scan it replaces read `status=open&limit=200` and
/// treated a truncated page as "no alert exists": once open
/// backlog-items passed 200 the existing alert sat beyond row 200, the
/// dedup answered "not raised", and the alert re-filed every ~10-min
/// reconcile pass — a self-compounding notification flood. A flag on the
/// train the caller already holds cannot truncate: it is one boolean.
pub(crate) fn red_alert_filed(train: &Value) -> bool {
    truthy(train.get("metadata").and_then(|m| m.get("red_alert_filed")))
}

#[cfg(test)]
mod red_train_alert_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_alert_body_carries_every_field_the_jobs_api_demands() {
        // The exact regression that froze the conductor for 8h: the body
        // must carry owner_id, status, and tags, or the POST is a 422.
        let alert = RedTrainAlert {
            title: "Red train abcd1234 — CI failed: CI / web".into(),
            failing: vec!["CI / web".into()],
            refused: false,
            logs: vec![],
        };
        let b = red_train_alert_body("abcd1234-0000-0000-0000-000000000000", &alert, "emp-owner");
        for f in [
            "kind", "title", "subject", "owner_id", "status", "tags", "priority", "metadata",
        ] {
            assert!(
                b.get(f).is_some(),
                "the alert body must carry `{f}` — its absence returned HTTP 422 and froze reconcile"
            );
        }
        assert_eq!(b["status"], "open");
        assert_eq!(b["owner_id"], "emp-owner", "the owner is the one handed in");
        assert_eq!(b["tags"], json!([]));
        assert_eq!(
            b["metadata"]["train_alert"], "abcd1234-0000-0000-0000-000000000000",
            "the packet still names its train; dedup is the train's red_alert_filed flag"
        );
    }

    fn train(verdict_merged: bool) -> Value {
        let merged = if verdict_merged {
            "completed"
        } else {
            "pending"
        };
        json!({
            "id": "e799e241-aaaa-bbbb-cccc-000000000000",
            "steps": [{"metadata": {"spec_slug": "merged"}, "title": "Merged into main", "status": merged}]
        })
    }
    /// c186d63d: the sparing of innocent cars on a CI refusal rests on
    /// the rollup carrying each check's `description` — the same adapter
    /// layer that once dropped `context`. From the forge's statuses, as
    /// its API spells them, through the entry mapping, to the verdict.
    #[test]
    fn a_refusal_survives_the_rollup_and_spares_the_cars() {
        let forge_statuses = json!([
            {"context": "CI / build-image (pull_request)", "status": "success", "description": ""},
            {"context": "CI / locomotive refusal", "status": "failure",
             "description": "refused: 65GB free on the workspace filesystem, need 70GB",
             "target_url": "http://10.20.0.15:3000/david/boss/actions/runs/9/jobs/1"},
            {"context": "CI / test (pull_request)", "status": "pending", "description": ""},
        ]);
        let entries: Vec<Value> = forge_statuses
            .as_array()
            .unwrap()
            .iter()
            .map(super::rollup_entry)
            .collect();
        let refusal = &entries[1];
        assert_eq!(refusal["context"], "CI / locomotive refusal");
        assert_eq!(refusal["conclusion"], "FAILURE");
        assert_eq!(
            refusal["description"], "refused: 65GB free on the workspace filesystem, need 70GB",
            "the description is the refusal channel; dropping it turns the refusal into a strike"
        );
        assert_eq!(
            refusal["target_url"],
            "http://10.20.0.15:3000/david/boss/actions/runs/9/jobs/1"
        );
        assert_eq!(entries[2]["status"], "PENDING");
        let rollup = json!(entries);
        assert!(super::any_failing_check_refused(Some(&rollup)));
        assert!(
            !super::verdict_strikes_cars("failing", Some(&rollup)),
            "an infrastructure refusal must not strike the cars aboard"
        );
        // And the same statuses with the description lost DO strike — so
        // this test fails the moment the adapter drops the field again.
        let stripped: Vec<Value> = entries
            .iter()
            .map(|e| {
                let mut e = e.clone();
                e["description"] = json!("");
                e
            })
            .collect();
        assert!(super::verdict_strikes_cars(
            "failing",
            Some(&json!(stripped))
        ));
    }

    fn rollup(entries: Value) -> Value {
        entries
    }

    #[test]
    fn a_red_train_announces_the_failing_check() {
        let r = red_train_alert(
            &train(false),
            "failing",
            Some(&rollup(json!([
                {"context": "CI / build-image", "conclusion": "SUCCESS"},
                {"context": "CI / test", "conclusion": "FAILURE"}
            ]))),
            &[],
            &[],
        )
        .expect("a red train alerts");
        assert_eq!(r.failing, vec!["CI / test".to_string()]);
        assert!(!r.refused);
        assert!(
            r.title.contains("CI / test"),
            "title names the check: {}",
            r.title
        );
    }

    /// Train #361, 2026-09-14 17:00Z: CI green, train gate RED, and the
    /// alert (071d8b23) read "check names unavailable" because only the
    /// forge rollup was consulted. The gate's receipt named the check.
    #[test]
    fn a_red_train_gate_announces_its_failing_check_when_ci_is_green() {
        let green_ci = rollup(json!([
            {"context": "CI / build-image", "conclusion": "SUCCESS"},
            {"context": "CI / web", "conclusion": "SUCCESS"}
        ]));
        let fails = vec![
            "test: the_real_run_refuses_while_the_host_declares_legacy_stack - FAILED".to_string(),
        ];
        // WHY, from the receipt's `fails_excerpt` (5708cbd5): the same
        // lines the gate pod replayed and then took with it.
        let excerpt = vec![(
            "test".to_string(),
            "---- the_real_run_refuses_while_the_host_declares_legacy_stack stdout ----\n\
             assertion `left == right` failed: the host declares legacy-stack"
                .to_string(),
        )];
        let r = red_train_alert(&train(false), "failing", Some(&green_ci), &fails, &excerpt)
            .expect("a red gate is a red train");
        assert_eq!(r.failing, fails);
        assert!(!r.refused);
        assert_eq!(
            r.logs,
            vec![("gate: test".to_string(), excerpt[0].1.clone())],
            "the gate's excerpt rides the alert the way a forge check's log does, labelled as \
             the gate's so the two are never confused"
        );
        let body = red_train_alert_body("abcd1234-0000-0000-0000-000000000000", &r, "emp-owner");
        assert_eq!(body["metadata"]["failing_logs"][0]["check"], "gate: test");
        assert!(
            body["metadata"]["failing_logs"][0]["log_tail"]
                .as_str()
                .unwrap_or("")
                .contains("the host declares legacy-stack"),
            "the alert body carries the assertion text, not just the test's name: {}",
            body["metadata"]["failing_logs"]
        );
        assert!(
            r.title.contains("train gate failed")
                && r.title
                    .contains("the_real_run_refuses_while_the_host_declares_legacy_stack"),
            "the title names the gate and the check: {}",
            r.title
        );
        // When the forge itself names a failing check, that name leads
        // and the title stays CI's.
        let both = red_train_alert(
            &train(false),
            "failing",
            Some(&rollup(
                json!([{"context": "CI / web", "conclusion": "FAILURE"}]),
            )),
            &fails,
            &excerpt,
        )
        .unwrap();
        assert_eq!(both.failing, vec!["CI / web".to_string()]);
        assert!(both.title.contains("CI failed"), "{}", both.title);
        assert!(
            both.logs.is_empty(),
            "when the forge names the failure, the gate's excerpt is not attached beside it \
             (the forge's own log is, by attach_failing_logs): {:?}",
            both.logs
        );
    }

    #[test]
    fn a_green_or_pending_verdict_is_no_alert() {
        assert!(red_train_alert(&train(false), "green", None, &[], &[]).is_none());
        assert!(red_train_alert(&train(false), "pending", None, &[], &[]).is_none());
    }

    #[test]
    fn a_merged_train_is_no_alert_whatever_the_verdict() {
        assert!(red_train_alert(&train(true), "failing", None, &[], &[]).is_none());
    }

    #[test]
    fn the_red_alert_flag_suppresses_a_second_file() {
        // A train with no flag has not filed yet; the flag, once
        // stamped, makes `announce_red_train` a no-op. This is the whole
        // dedup — no scan of open backlog-items, so no page to truncate.
        let mut t = train(false);
        assert!(!red_alert_filed(&t), "an unflagged train has not alerted");
        t["metadata"] = json!({"red_alert_filed": true});
        assert!(
            red_alert_filed(&t),
            "the flag on the train must suppress a second file"
        );
    }

    #[test]
    fn an_infrastructure_refusal_is_named_as_such() {
        let r = red_train_alert(
            &train(false),
            "failing",
            Some(&rollup(json!([
                {"context": "CI / locomotive", "conclusion": "FAILURE", "description": "refused: disk floor"}
            ]))),
            &[],
            &[],
        )
        .expect("a refusal still alerts");
        assert!(r.refused);
        assert!(
            r.title.to_lowercase().contains("refused"),
            "title says refused: {}",
            r.title
        );
    }
}

#[cfg(test)]
mod red_verdict_log_tests {
    //! A red verdict names its LOG, not just its check. These pin the
    //! decidable parts: the run/job-id resolution off `target_url`, the
    //! log-tail truncation, and that a fetch that resolves nothing yields
    //! no attachment rather than an error. The effectful fetch itself
    //! (`fetch_job_log_tail` / `attach_failing_logs`) stays thin.
    use super::*;
    use serde_json::json;

    // ---- parse_run_job_ref -------------------------------------------

    #[test]
    fn a_target_url_yields_its_run_and_job_index() {
        // The exact shape this forge posts (relative), live-confirmed.
        assert_eq!(
            parse_run_job_ref("/david/boss/actions/runs/467/jobs/2"),
            Some((467, 2))
        );
        // Absolute form, and a trailing query/fragment after the index.
        assert_eq!(
            parse_run_job_ref("http://10.20.0.15:3000/david/boss/actions/runs/462/jobs/0?x=1"),
            Some((462, 0))
        );
    }

    #[test]
    fn a_url_without_a_job_index_resolves_to_nothing() {
        // A run alone cannot name ONE job — we never guess an index.
        assert_eq!(parse_run_job_ref("/david/boss/actions/runs/467"), None);
        assert_eq!(parse_run_job_ref("/david/boss/commit/abc"), None);
        assert_eq!(parse_run_job_ref(""), None);
    }

    // ---- job_id_for_index: the id, not the task_id -------------------

    fn run_jobs() -> Value {
        // A real /actions/runs/{run}/jobs payload: a plain array where
        // `id` is the JOB id (the logs key) and `task_id` is the TRAP.
        json!([
            {"id": 2031, "name": "build-image", "task_id": 1962, "status": "success"},
            {"id": 2032, "name": "locomotive",  "task_id": 1963, "status": "success"},
            {"id": 2033, "name": "web",         "task_id": 1964, "status": "failure"},
            {"id": 2034, "name": "fast",        "task_id": 1965, "status": "success"},
            {"id": 2035, "name": "test",        "task_id": 1966, "status": "failure"},
        ])
    }

    #[test]
    fn the_job_id_comes_from_position_and_is_the_id_not_the_task_id() {
        let jobs = run_jobs();
        let jobs = jobs.as_array().unwrap();
        // Index 2 == "web": id 2033 (the log key), NOT task_id 1964 —
        // passing 1964 to /actions/jobs/{id}/logs returns another job's
        // log (live-confirmed). Context matches the name, so no note.
        let (id, note) = job_id_for_index(jobs, 2, "CI / web (pull_request)").unwrap();
        assert_eq!(id, 2033, "the job id, never the task_id");
        assert!(note.is_none(), "name matched the context: {note:?}");

        let (id, note) = job_id_for_index(jobs, 4, "CI / test (pull_request)").unwrap();
        assert_eq!(id, 2035);
        assert!(note.is_none());
    }

    #[test]
    fn a_context_that_does_not_match_the_positioned_job_is_attached_with_a_note() {
        let jobs = run_jobs();
        let jobs = jobs.as_array().unwrap();
        // Positional resolution still yields a real job id, but the note
        // says the mapping was uncertain — attach what you can, don't
        // guess a different job.
        let (id, note) = job_id_for_index(jobs, 2, "CI / test (pull_request)").unwrap();
        assert_eq!(
            id, 2033,
            "position wins; we do not go hunting a 'better' job"
        );
        let note = note.expect("a mismatch is noted");
        assert!(note.contains("did not obviously match"), "note: {note}");
    }

    #[test]
    fn an_out_of_range_index_resolves_to_no_job() {
        let jobs = run_jobs();
        let jobs = jobs.as_array().unwrap();
        assert!(job_id_for_index(jobs, 9, "CI / whatever").is_none());
        assert!(job_id_for_index(&[], 0, "CI / web").is_none());
    }

    #[test]
    fn context_names_job_strips_workflow_and_event() {
        assert!(context_names_job("CI / web (pull_request)", "web"));
        assert!(context_names_job("CI / build-image (push)", "build-image"));
        assert!(context_names_job("CI / TEST (pull_request)", "test")); // case-insensitive
        assert!(!context_names_job("CI / web (pull_request)", "test"));
        assert!(!context_names_job("CI / web", ""));
    }

    // ---- content_range_starts_past_zero ------------------------------

    #[test]
    fn a_mid_file_range_is_detected_a_whole_body_is_not() {
        // A suffix Range of a big log starts past 0 -> first line is a
        // fragment to drop.
        assert!(content_range_starts_past_zero(Some(
            "bytes 303107-307106/307107"
        )));
        // The whole small file (start 0), or no Range at all (a 200):
        // the first line is real.
        assert!(!content_range_starts_past_zero(Some("bytes 0-4999/5000")));
        assert!(!content_range_starts_past_zero(None));
    }

    // ---- failing_excerpt: the failure, not the end -------------------

    /// The shape of a real red `test` job: thousands of lines of
    /// passing output, the panic in the middle, and five more minutes
    /// of unrelated warnings after it. Both red logs of 2026-09-09
    /// looked exactly like this.
    fn a_red_test_log() -> String {
        let mut l = String::new();
        for i in 0..3000 {
            l.push_str(&format!("2026-09-09T02:00:00Z passing line {i}\n"));
        }
        l.push_str("2026-09-09T02:04:46Z running 5 tests\n");
        l.push_str("2026-09-09T02:04:46Z test the_error_line ... FAILED\n");
        l.push_str("2026-09-09T02:04:46Z thread 'the_error_line' panicked at crates/core/boss-jobs/tests/station_boot_quarantine.rs:199:5:\n");
        l.push_str("2026-09-09T02:04:46Z an unviable active station is logged at ERROR: \n");
        l.push_str("2026-09-09T02:04:46Z test result: FAILED. 4 passed; 1 failed\n");
        l.push_str("2026-09-09T02:04:47Z GATE FAIL: test\n");
        for i in 0..2000 {
            l.push_str(&format!(
                "2026-09-09T02:06:13Z Warn: Using `on:submit` is deprecated {i}\n"
            ));
        }
        l
    }

    #[test]
    fn the_excerpt_carries_the_failure_not_the_end_of_the_log() {
        let out = failing_excerpt(&a_red_test_log(), false, 40, 4000);
        assert!(
            out.starts_with("…(the failure"),
            "says which of the two it is: {out}"
        );
        assert!(
            out.contains("panicked at crates/core/boss-jobs/tests/station_boot_quarantine.rs"),
            "the panic and its file are in the excerpt: {out}"
        );
        assert!(
            out.contains("test result: FAILED"),
            "cargo's verdict is in the excerpt: {out}"
        );
        // A few lines past the last marker are deliberate: a bun
        // failure prints its assertion diff there, and a gate its next
        // step. What must not happen is the excerpt BEING those lines,
        // which is what a 40-line tail of this log was.
        let warnings = out.matches("deprecated").count();
        assert!(
            warnings <= 3,
            "the warnings five minutes later are context at most, not the excerpt ({warnings} lines): {out}"
        );
        assert!(
            out.lines().filter(|l| l.contains("deprecated")).count() * 2 < out.lines().count(),
            "the failure, not the noise, is the bulk of what the reader sees: {out}"
        );
    }

    #[test]
    fn the_excerpt_leads_in_far_enough_to_name_the_test() {
        // The panic line names a file; the lines above it name the test
        // that produced it, and a reader needs both.
        let out = failing_excerpt(&a_red_test_log(), false, 40, 100_000);
        assert!(out.contains("running 5 tests"), "lead-in kept: {out}");
        assert!(out.contains("test the_error_line ... FAILED"));
    }

    #[test]
    fn how_many_failures_matched_is_stated() {
        let out = failing_excerpt(&a_red_test_log(), false, 40, 100_000);
        // All three markers of this one failure are inside the window,
        // so there is nothing further down to announce.
        assert!(
            !out.contains("further down"),
            "no phantom second failure is announced: {out}"
        );
        assert!(
            out.contains("GATE FAIL: test"),
            "the last marker is shown: {out}"
        );
    }

    #[test]
    fn a_second_failure_far_below_the_window_is_announced() {
        let mut body = String::new();
        body.push_str("panicked at the first place\n");
        for i in 0..200 {
            body.push_str(&format!("noise {i}\n"));
        }
        body.push_str("panicked at the second place\n");
        let out = failing_excerpt(&body, false, 40, 100_000);
        assert!(out.contains("the first place"), "the first is shown: {out}");
        assert!(
            out.contains("1 more further down"),
            "the second is announced, not hidden: {out}"
        );
        assert!(!out.contains("the second place"), "and not shown: {out}");
    }

    #[test]
    fn a_log_with_no_failure_marker_falls_back_to_the_tail_and_says_so() {
        let body: String = (0..500).map(|i| format!("row {i}\n")).collect();
        let out = failing_excerpt(&body, false, 40, 100_000);
        assert!(
            out.starts_with("…(log tail"),
            "an excerpt that is really a tail says it is a tail: {out}"
        );
        assert!(out.contains("row 499"), "the tail is the end: {out}");
    }

    #[test]
    fn the_benign_connection_errors_of_the_mocked_suite_are_not_a_failure() {
        // The mocked web suite prints hundreds of these for backends it
        // does not run, and every one of its tests passes. A marker set
        // that matched them would point the excerpt at noise.
        let mut body = String::new();
        for i in 0..200 {
            body.push_str("error: Unable to connect. Is the computer able to access the url?\n");
            body.push_str(&format!(
                "  ✓  {i} [chromium] › tests/mocked/thing.spec.ts\n"
            ));
        }
        let out = failing_excerpt(&body, false, 40, 100_000);
        assert!(
            out.starts_with("…(log tail"),
            "no failure was claimed where none is: {out}"
        );
    }

    #[test]
    fn the_excerpt_holds_its_byte_cap() {
        let mut body = String::new();
        body.push_str("panicked at somewhere\n");
        for i in 0..50 {
            body.push_str(&format!("{i}:{}\n", "x".repeat(500)));
        }
        let out = failing_excerpt(&body, false, 40, 1_000);
        assert!(out.len() <= 1_000 + 60, "byte-capped: {} bytes", out.len());
    }

    #[test]
    fn a_range_fetch_still_drops_the_partial_first_line() {
        let body = "ed at nothing\npanicked at real place\nafter\n";
        let out = failing_excerpt(body, true, 40, 4000);
        assert!(!out.contains("ed at nothing"), "fragment dropped: {out}");
        assert!(out.contains("panicked at real place"));
    }

    #[test]
    fn an_empty_log_stays_empty() {
        assert_eq!(failing_excerpt("", false, 40, 4000), "");
        assert_eq!(failing_excerpt("\n\n", false, 40, 4000), "");
    }

    // ---- log_tail: bounded and honest --------------------------------

    #[test]
    fn a_short_whole_log_is_returned_verbatim() {
        let body = "line a\nline b\nline c\n";
        let out = log_tail(body, false, 40, 4000);
        assert_eq!(out, "line a\nline b\nline c", "no marker on a whole log");
        assert!(!out.contains("omitted"));
    }

    #[test]
    fn a_range_fetch_drops_the_partial_first_line_and_marks_the_tail() {
        // The Range started mid-line, so "ne b" is a fragment.
        let body = "ne b\nline c\nline d";
        let out = log_tail(body, true, 40, 4000);
        assert!(out.starts_with("…(log tail"), "marked as a tail: {out}");
        assert!(!out.contains("ne b"), "partial first line dropped: {out}");
        assert!(out.contains("line c") && out.contains("line d"));
    }

    #[test]
    fn a_long_log_is_capped_to_the_last_lines() {
        let body: String = (0..500).map(|i| format!("row {i}\n")).collect();
        let out = log_tail(&body, false, 40, 100_000);
        let kept: Vec<&str> = out.lines().filter(|l| l.starts_with("row ")).collect();
        assert_eq!(kept.len(), 40, "kept the last 40 rows");
        assert_eq!(*kept.last().unwrap(), "row 499", "the tail, not the head");
        assert!(!kept.contains(&"row 459") || kept[0] == "row 460");
        assert!(out.contains("omitted"));
    }

    #[test]
    fn the_byte_cap_holds_even_when_the_line_count_is_fine() {
        let body: String = (0..10)
            .map(|i| format!("{i}:{}\n", "x".repeat(500)))
            .collect();
        let out = log_tail(&body, false, 40, 1_000);
        assert!(out.len() <= 1_000 + 40, "byte-capped: {} bytes", out.len());
        assert!(out.contains("omitted"));
        // The END is kept: the last line survives, the first does not.
        assert!(out.contains("9:"));
        assert!(!out.contains("0:xxxx"));
    }

    // ---- failing_check_logs / format_check_logs ----------------------

    #[test]
    fn failing_check_logs_reads_only_failing_entries_that_carry_a_tail() {
        let rollup = json!([
            {"context": "CI / build-image", "conclusion": "SUCCESS", "log_tail": "irrelevant"},
            {"context": "CI / web", "conclusion": "FAILURE", "log_tail": "boom on web"},
            {"context": "CI / test", "conclusion": "FAILURE"}, // fetch attached nothing
        ]);
        let logs = failing_check_logs(Some(&rollup));
        assert_eq!(
            logs,
            vec![("CI / web".to_string(), "boom on web".to_string())],
            "only the failing check WITH a tail; the success and the \
             no-tail failure contribute nothing"
        );
    }

    #[test]
    fn a_fetch_that_attached_nothing_is_a_missing_log_not_an_error() {
        // Exactly the rollup a FAILED or ambiguous log fetch leaves: a
        // FAILURE entry with no `log_tail`. The alert must still build,
        // carry the check name, and simply have no log — never error.
        let train = json!({
            "id": "e799e241-aaaa-bbbb-cccc-000000000000",
            "steps": [{"metadata": {"spec_slug": "merged"}, "title": "Merged into main", "status": "pending"}]
        });
        let rollup = json!([{"context": "CI / test", "conclusion": "FAILURE"}]);
        let alert = red_train_alert(&train, "failing", Some(&rollup), &[], &[])
            .expect("still an alert without a log");
        assert_eq!(alert.failing, vec!["CI / test".to_string()]);
        assert!(alert.logs.is_empty(), "no attachment, not an error");
        let body =
            red_train_alert_body("e799e241-aaaa-bbbb-cccc-000000000000", &alert, "emp-owner");
        assert_eq!(
            body["metadata"]["failing_logs"],
            json!([]),
            "the body still forms, with an empty failing_logs"
        );
    }

    #[test]
    fn a_red_alert_carries_the_resolved_log_tail() {
        let train = json!({
            "id": "abcd1234-0000-0000-0000-000000000000",
            "steps": [{"metadata": {"spec_slug": "merged"}, "title": "Merged into main", "status": "pending"}]
        });
        let rollup = json!([
            {"context": "CI / test", "conclusion": "FAILURE", "log_tail": "assertion failed at line 9"}
        ]);
        let alert = red_train_alert(&train, "failing", Some(&rollup), &[], &[]).unwrap();
        assert_eq!(alert.logs.len(), 1);
        let body =
            red_train_alert_body("abcd1234-0000-0000-0000-000000000000", &alert, "emp-owner");
        assert_eq!(body["metadata"]["failing_logs"][0]["check"], "CI / test");
        assert_eq!(
            body["metadata"]["failing_logs"][0]["log_tail"],
            "assertion failed at line 9"
        );
    }

    #[test]
    fn format_check_logs_heads_each_block_and_stays_bounded() {
        let logs = vec![
            ("CI / test".to_string(), "boom".to_string()),
            ("CI / web".to_string(), "kaboom".to_string()),
        ];
        let out = format_check_logs(&logs, 8_000);
        assert!(out.contains("--- CI / test ---"));
        assert!(out.contains("boom"));
        assert!(out.contains("--- CI / web ---"));

        // A tiny cap keeps the first block and stops, rather than
        // stamping megabytes onto the step.
        let big = vec![
            ("CI / test".to_string(), "x".repeat(5_000)),
            ("CI / web".to_string(), "y".repeat(5_000)),
        ];
        let out = format_check_logs(&big, 4_000);
        assert!(out.len() <= 4_000 + 80, "bounded: {} bytes", out.len());
        assert!(out.contains("--- CI / test ---"));
        assert!(!out.contains("--- CI / web ---"), "second block omitted");
    }
}
