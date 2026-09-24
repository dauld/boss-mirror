//! A JUDGED red releases the track at once — the auto-cancel's fast arm.
//!
//! Backlog a2d4d842 (David, 2026-09-24: "we don't want to be in the
//! habit of letting red linger and train us to ignore it"). Two trains
//! that day — f7bd1e9d (17:17) and 02801b05 (18:31) — went red on
//! `CI / web` with the failing check NAMED, their train gates finished
//! red on the same `svelte-check` a quarter of an hour later, and each
//! still held the one track until an operator cancelled it by hand (40
//! and 27 minutes), because [`auto_cancel_reason`] waits out
//! `stall_hours` (6) before it releases anything. The wait exists for a
//! train that may yet be repaired in place or whose verdict is not in;
//! a train whose BOTH halves have spoken and whose red names what failed
//! has nothing left to wait for. The six-hour rule stays as the backstop
//! for every red this does not judge.

use super::*;
use crate::train_gate::Standing;

/// The failing checks a JUDGED red names — Some only when all of it is
/// on the record, else None and the stall rule decides as before:
///
/// - the train has not merged (the rule every cancel keeps);
/// - BOTH halves have spoken: the forge's verdict is `failing` or
///   `green` (not pending, not aborted), and the train gate FINISHED —
///   `Failed` or `Green` (a gate still running, refused, lost or never
///   filed has not judged the assembled tree);
/// - at least one half is red;
/// - no failing forge check declares a refusal — infrastructure is not
///   a verdict (`verdict_strikes_cars`);
/// - the red NAMES what failed: the forge's failing check contexts, and
///   the gate's failed checks off its receipt's `fails` lines. A red
///   that names nothing is not judged here.
///
/// The names come back forge first, then the gate's, deduplicated.
pub(crate) fn judged_red_checks(
    train: &Value,
    forge_verdict: &str,
    gate: Option<&Standing>,
    rollup: Option<&Value>,
    gate_fails: &[String],
) -> Option<Vec<String>> {
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let forge_red = match forge_verdict {
        "failing" => true,
        "green" => false,
        _ => return None,
    };
    let gate_red = match gate {
        Some(Standing::Failed) => true,
        Some(Standing::Green) => false,
        _ => return None,
    };
    if !(forge_red || gate_red) || any_failing_check_refused(rollup) {
        return None;
    }
    // `failing_checks` answers "unnamed check" for an entry with neither
    // `context` nor `name`; that is the absence of a name, not one.
    let mut named: Vec<String> = if forge_red {
        failing_checks(rollup)
            .into_iter()
            .filter(|c| c != "unnamed check")
            .collect()
    } else {
        Vec::new()
    };
    if gate_red {
        for check in gate_check_names(gate_fails) {
            if !named.contains(&check) {
                named.push(check);
            }
        }
    }
    (!named.is_empty()).then_some(named)
}

/// The check a gate receipt's `fails` line names — the text before its
/// first `:` (`svelte-check: | Error: …`, `test: <name> - FAILED`), as
/// the gate runner writes every rung (infra/gate-runner/run.sh
/// `detail`). A line with no `:` names nothing. In first-seen order,
/// once each.
pub(crate) fn gate_check_names(gate_fails: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in gate_fails {
        let Some((check, _)) = line.split_once(':') else {
            continue;
        };
        let check = check.trim();
        if !check.is_empty() && !out.iter().any(|c| c == check) {
            out.push(check.to_string());
        }
    }
    out
}

/// The reason a judged red's cars carry back to the dock (their
/// `skip_reason`) and the `cancelled` step records: the failing checks
/// by name, and the wait it did not take — so nobody reading a car
/// released in minutes wonders where the six hours went.
pub(crate) fn judged_red_cancel_reason(named: &[String], stall_hours: i64) -> String {
    format!(
        "judged red: {} failed, CI and the train gate both finished — cancelled at the first \
         reconcile after the verdict rather than after the {stall_hours}h stall rule (a2d4d842); \
         cars released to board a later train",
        named.join(", ")
    )
}

/// Every repo file a failure text LOCATES, as `path` from a
/// `path:line:col` the text ties to a FAILURE — never to a warning.
/// Three shapes, each measured on a real red:
///
/// - `panicked at <path>:<line>:<col>` — a Rust test (the gate
///   runner's located rung; [`fails_entry_path`] reads the same);
/// - `--> <path>:<line>:<col>` under an `error…` header — rustc (a
///   `warning` header's arrow is not a failure);
/// - a line that is only `<path>:<line>:<col>`, followed by one that
///   starts `Error` — svelte-check as both the gate and the forge
///   print it (trains f7bd1e9d and 02801b05, 2026-09-24); the same
///   shape followed by `Warn` is a warning and is skipped.
///
/// Forge log lines carry a runner timestamp and ANSI colour inside the
/// path itself (`…/apps/web/\x1b[32msrc/…`), so both come off first.
/// Paths are normalised (`..` resolved) but may stay ABSOLUTE — the
/// gate's `/gate-target/repo/…`, the forge's `/workspace/<owner>/<repo>/…`
/// — and [`cars_to_hold`] matches them to a car's repo-relative files
/// by path suffix. Sorted, once each.
pub(crate) fn located_failures(text: &str) -> Vec<String> {
    let lines: Vec<String> = text.lines().map(clean_log_line).collect();
    let mut out: Vec<String> = Vec::new();
    let mut under_error = false;
    for (i, line) in lines.iter().enumerate() {
        if let Some((_, rest)) = line.split_once("panicked at ") {
            if let Some(path) = rest
                .split_whitespace()
                .next()
                .and_then(|t| location_path(t.strip_suffix(':').unwrap_or(t)))
            {
                out.push(path);
            }
            continue;
        }
        if line.starts_with("error") {
            under_error = true;
            continue;
        }
        if line.starts_with("warning") {
            under_error = false;
            continue;
        }
        if let Some(loc) = line.strip_prefix("--> ") {
            if under_error && let Some(path) = location_path(loc.trim()) {
                out.push(path);
            }
            continue;
        }
        if !line.contains(char::is_whitespace)
            && let Some(path) = location_path(line)
            && lines[i + 1..]
                .iter()
                .find(|l| !l.is_empty())
                .is_some_and(|next| next.starts_with("Error"))
        {
            out.push(path);
        }
    }
    let mut out: Vec<String> = out.iter().map(|p| normalise_path(p)).collect();
    out.sort();
    out.dedup();
    out
}

/// One log line as a reader sees it: ANSI escapes removed, a leading
/// runner timestamp (`2026-09-24T17:19:53.3447198Z `) dropped, trimmed.
fn clean_log_line(raw: &str) -> String {
    let mut plain = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        plain.push(c);
    }
    let trimmed = plain.trim();
    let is_stamp = |t: &str| {
        t.len() >= 20
            && t.ends_with('Z')
            && t.as_bytes().get(4) == Some(&b'-')
            && t.contains('T')
            && t.bytes().take(4).all(|b| b.is_ascii_digit())
    };
    match trimmed.split_once(' ') {
        Some((head, rest)) if is_stamp(head) => rest.trim().to_string(),
        None if is_stamp(trimmed) => String::new(),
        _ => trimmed.to_string(),
    }
}

/// The path of a `<path>:<line>:<col>` token, or None when the token is
/// not that shape.
fn location_path(token: &str) -> Option<String> {
    let mut parts = token.rsplitn(3, ':');
    let (col, line, path) = (parts.next()?, parts.next()?, parts.next()?);
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (numeric(col) && numeric(line) && !path.is_empty()).then(|| path.to_string())
}

/// `a/b/../c` → `a/c`, keeping a leading `/`.
fn normalise_path(path: &str) -> String {
    let mut segs: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let joined = segs.join("/");
    if path.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Every file the verdict locates, across both halves: the gate
/// receipt's `fails` lines and `fails_excerpt`, and the log tail the
/// forge attached to each FAILING check. Sorted, once each.
pub(crate) fn verdict_located_files(
    gate_fails: &[String],
    gate_excerpt: &[(String, String)],
    rollup: Option<&Value>,
) -> Vec<String> {
    let forge_tails = rollup
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
        .filter_map(|c| c.get("log_tail").and_then(Value::as_str));
    let mut out: Vec<String> = gate_fails
        .iter()
        .map(String::as_str)
        .chain(gate_excerpt.iter().map(|(_, text)| text.as_str()))
        .chain(forge_tails)
        .flat_map(located_failures)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Does a located path name this repo-relative file? Equal, or — for a
/// file below the repo root — the located path ends in `/<file>`. A
/// top-level file matches only itself: a car's `Cargo.toml` is not
/// `/gate-target/repo/crates/x/Cargo.toml`.
fn names_file(located: &str, file: &str) -> bool {
    located == file
        || (file.contains('/')
            && located
                .strip_suffix(file)
                .is_some_and(|head| head.ends_with('/')))
}

/// Which cars the verdict names: for each located file, the ONE car
/// aboard that changed it — a file two cars changed names neither, and
/// a file no car changed names nobody (the release is all that red
/// earns). `(car id, the repo files only it changed that the verdict
/// located)`, in the order the cars are given.
pub(crate) fn cars_to_hold(
    located: &[String],
    car_files: &[(String, Vec<String>)],
) -> Vec<(String, Vec<String>)> {
    let mut held: Vec<(String, Vec<String>)> = Vec::new();
    for loc in located {
        let owners: Vec<(&String, &String)> = car_files
            .iter()
            .filter_map(|(car, files)| files.iter().find(|f| names_file(loc, f)).map(|f| (car, f)))
            .collect();
        let [(car, file)] = owners.as_slice() else {
            continue;
        };
        match held.iter_mut().find(|(c, _)| c == *car) {
            Some((_, files)) => {
                if !files.contains(file) {
                    files.push((*file).clone());
                }
            }
            None => held.push(((*car).clone(), vec![(*file).clone()])),
        }
    }
    held.sort_by_key(|(car, _)| car_files.iter().position(|(c, _)| c == car));
    held
}

/// The hold a named car carries on its review step — the dock brake
/// `boss hold` writes, so `boss release` takes it off. It says which
/// train, which check, which file, and what to do, because the next
/// reader is whoever is rebuilding the car.
pub(crate) fn judged_red_hold_reason(train_id: &str, named: &[String], files: &[String]) -> String {
    format!(
        "held by the conductor: train {} was judged red ({}) in {}, a file only this car changed \
         in that consist — rebuild it on current main, re-gate, then `boss release` it (a2d4d842)",
        id8(train_id),
        named.join(", "),
        files.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- the two real trains of 2026-09-24 ----------------------------------
    //
    // Both fixtures are the record's own shapes: the forge rollup's four
    // checks as the `ci` step stamped them, the failing check's log tail
    // as the forge returned it (ANSI colour and runner timestamps
    // included), and the gate-run receipt's `fails` lines and
    // `fails_excerpt` as the runner wrote them. Trimmed, never retyped.

    const F7BD_ID: &str = "f7bd1e9d-e070-4c06-8852-a19a4902a682";
    const O280_ID: &str = "02801b05-ae47-476a-b08d-2cd979ab1978";

    fn train(id: &str, merged: &str) -> Value {
        json!({
            "id": id,
            "status": "open",
            "metadata": {},
            "steps": [
                {"spec_slug": "ci", "title": "Yard inspection — CI verdict", "status": "completed",
                 "metadata": {"result": "failing", "completed_at": "2026-09-24T17:20:32Z"}},
                {"spec_slug": "merged", "title": "DEPARTED — merged into main", "status": merged,
                 "metadata": {}}
            ]
        })
    }

    /// train f7bd1e9d's forge rollup: `CI / web` red, its log tail
    /// locating the error the way svelte-check prints it through the
    /// forge runner.
    fn f7bd_rollup() -> Value {
        json!([
            {"context": "CI / build-image (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / locomotive (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / web (pull_request)", "conclusion": "FAILURE", "description": "",
             "log_tail": "…(log tail — earlier lines omitted)\n\
        2026-09-24T17:19:53.3447098Z     expect(got.source).toBe('server');\u{1b}[39m\n\
        2026-09-24T17:19:53.3447148Z \n\
        2026-09-24T17:19:53.3447198Z /workspace/david/boss/apps/web/\u{1b}[32msrc/it/yard/phone-strip.test.ts\u{1b}[39m:101:32\n\
        2026-09-24T17:19:53.3448213Z \u{1b}[31mError\u{1b}[39m: Conversion of type '{ thirds: { third: string; regions: string[]; }[]; }' may be a mistake\n\
        2026-09-24T17:19:53.3448738Z \n\
        2026-09-24T17:19:53.3449978Z /workspace/david/boss/apps/web/\u{1b}[32m../../libs/web-kit/src/ui/Tabs.svelte\u{1b}[39m:20:1\n\
        2026-09-24T17:19:53.3450155Z \u{1b}[33mWarn\u{1b}[39m: Non-interactive element `<nav>` cannot have interactive role 'tablist'\n\
        2026-09-24T17:19:53.3453744Z \u{1b}[31msvelte-check found 3 errors and 74 warnings in 29 files\n\
        2026-09-24T17:19:53.4273826Z \u{1b}[39merror: script \"typecheck\" exited with code 1\n\
        2026-09-24T17:19:54.1584137Z Job 'web' failed"},
            {"context": "CI / reclaim (pull_request)", "conclusion": "SUCCESS", "description": ""}
        ])
    }

    /// gate-run c9aa4dac's receipt `fails`, verbatim in shape.
    fn f7bd_gate_fails() -> Vec<String> {
        vec![
            "svelte-check: no cargo test failure in this check's output; 4 error line(s), first 4:".to_string(),
            "svelte-check: | Error: Type '{ window_hours: number; }' is not assignable to type 'Readonly<{ window_hours: number; }>' (+236 char(s); the Job log replay has the full text)".to_string(),
            "svelte-check: | error: script \"typecheck\" exited with code 1".to_string(),
        ]
    }

    /// gate-run c9aa4dac's receipt `fails_excerpt`: the first error's
    /// location fell in the omitted lead-in, the next two are located,
    /// and a WARNING's location sits beside them.
    fn f7bd_gate_excerpt() -> Vec<(String, String)> {
        vec![(
            "svelte-check".to_string(),
            "... (249 line(s) before the first failure marker omitted; the Job log replay has them)\n\
Error: Type '{ window_hours: number; }' is not assignable to type 'Readonly<{ window_hours: number; }>'.\n\
\n\
/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts:83:26\n\
Error: Conversion of type '{ thirds: { third: string; regions: string[]; }[]; }' may be a mistake\n\
    const got = thirdsOf({ ...regions(), thirds: served } as Regions);\n\
\n\
/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts:101:32\n\
Error: Conversion of type '{ thirds: { third: string; regions: string[]; }[]; }' may be a mistake\n\
\n\
/gate-target/repo/apps/web/../../libs/web-kit/src/ui/Tabs.svelte:20:1\n\
Warn: Non-interactive element `<nav>` cannot have interactive role 'tablist'\n\
\n\
====================================\n\
svelte-check found 3 errors and 74 warnings in 29 files\n\
error: script \"typecheck\" exited with code 1"
                .to_string(),
        )]
    }

    /// What each car aboard f7bd1e9d changed: car G (6c7c75f7,
    /// feat/it-phone-strip-map) owns the failing test file; the shed
    /// car (f13b5fd0) touched Rust only.
    fn f7bd_car_files() -> Vec<(String, Vec<String>)> {
        vec![
            (
                "6c7c75f7-4f34-4f5f-87c0-3737e92a66ed".to_string(),
                vec![
                    "apps/web/src/it/yard/PhoneStrip.svelte".to_string(),
                    "apps/web/src/it/yard/phone-strip.test.ts".to_string(),
                    "apps/web/src/it/yard/phone-strip.ts".to_string(),
                ],
            ),
            (
                "f13b5fd0-d15c-4c89-b3a2-3944b7b3dc52".to_string(),
                vec![
                    "crates/core/boss-jobs/src/car.rs".to_string(),
                    "crates/core/boss-jobs/src/regions.rs".to_string(),
                    "crates/orchestrators/boss-cli/src/orient.rs".to_string(),
                ],
            ),
        ]
    }

    #[test]
    fn train_f7bd1e9d_both_halves_red_and_named_is_judged() {
        let rollup = f7bd_rollup();
        let named = judged_red_checks(
            &train(F7BD_ID, "ready"),
            "failing",
            Some(&Standing::Failed),
            Some(&rollup),
            &f7bd_gate_fails(),
        )
        .expect("CI / web red and the train gate finished red: judged");
        assert_eq!(
            named,
            vec![
                "CI / web (pull_request)".to_string(),
                "svelte-check".to_string()
            ]
        );
        let reason = judged_red_cancel_reason(&named, 6);
        assert!(
            reason.contains("CI / web (pull_request)") && reason.contains("svelte-check"),
            "the reason names both halves' failing checks: {reason}"
        );
        assert!(
            reason.contains("6h"),
            "the reason says which wait it did not take: {reason}"
        );
    }

    /// 17:20Z to ~17:35Z on 2026-09-24: CI red, the train gate still
    /// running. Not judged yet — the gate finishes in minutes and its
    /// receipt is what locates the failure for the hold.
    #[test]
    fn a_red_forge_with_the_gate_still_running_is_not_yet_judged() {
        let rollup = f7bd_rollup();
        for gate in [
            None,
            Some(Standing::Pending),
            Some(Standing::Lost),
            Some(Standing::Refused("disk floor".into())),
            Some(Standing::Unavailable("no kubectl".into())),
        ] {
            assert_eq!(
                judged_red_checks(
                    &train(F7BD_ID, "ready"),
                    "failing",
                    gate.as_ref(),
                    Some(&rollup),
                    &[],
                ),
                None,
                "gate {gate:?} has not judged the assembled tree"
            );
        }
    }

    #[test]
    fn a_gate_red_with_the_forge_still_running_is_not_yet_judged() {
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "pending",
                Some(&Standing::Failed),
                None,
                &f7bd_gate_fails(),
            ),
            None
        );
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "aborted",
                Some(&Standing::Failed),
                None,
                &f7bd_gate_fails(),
            ),
            None
        );
    }

    /// Train #361's shape (2026-09-14): CI green, the train gate red and
    /// naming its check. Both halves spoke; the red is named.
    #[test]
    fn a_green_forge_and_a_named_red_gate_is_judged() {
        let green = json!([{"context": "CI / web", "conclusion": "SUCCESS"}]);
        let fails =
            vec!["test: the_real_run_refuses_while_the_host_declares_legacy_stack - FAILED".into()];
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "green",
                Some(&Standing::Failed),
                Some(&green),
                &fails,
            ),
            Some(vec!["test".to_string()])
        );
    }

    #[test]
    fn a_red_that_names_nothing_is_not_judged() {
        let unnamed = json!([{"conclusion": "FAILURE"}]);
        // An unnamed forge failure reads "unnamed check" in the rollup
        // reader; that is not a name.
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "failing",
                Some(&Standing::Green),
                Some(&unnamed),
                &[],
            ),
            None
        );
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "green",
                Some(&Standing::Failed),
                Some(&json!([{"context": "CI / web", "conclusion": "SUCCESS"}])),
                &[],
            ),
            None,
            "a red gate whose receipt names no check is not named"
        );
    }

    #[test]
    fn an_infrastructure_refusal_is_never_judged() {
        let refused = json!([
            {"context": "CI / locomotive", "conclusion": "FAILURE", "description": "refused: disk floor"}
        ]);
        assert_eq!(
            judged_red_checks(
                &train(O280_ID, "ready"),
                "failing",
                Some(&Standing::Failed),
                Some(&refused),
                &f7bd_gate_fails(),
            ),
            None
        );
    }

    #[test]
    fn a_merged_train_is_never_judged_whatever_its_checks_say() {
        let rollup = f7bd_rollup();
        assert_eq!(
            judged_red_checks(
                &train(F7BD_ID, "completed"),
                "failing",
                Some(&Standing::Failed),
                Some(&rollup),
                &f7bd_gate_fails(),
            ),
            None
        );
    }

    #[test]
    fn two_greens_are_not_a_red() {
        assert_eq!(
            judged_red_checks(
                &train(F7BD_ID, "ready"),
                "green",
                Some(&Standing::Green),
                Some(&json!([{"context": "CI / web", "conclusion": "SUCCESS"}])),
                &[],
            ),
            None
        );
    }

    #[test]
    fn a_gate_fails_line_names_the_check_before_its_colon() {
        assert_eq!(
            gate_check_names(&[
                "svelte-check: no cargo test failure in this check's output; 4 error line(s)"
                    .into(),
                "svelte-check: | error: script \"typecheck\" exited with code 1".into(),
                "test: a_test - panicked at crates/x/src/lib.rs:1:2: boom".into(),
                "no colon at all".into(),
            ]),
            vec!["svelte-check".to_string(), "test".to_string()]
        );
    }

    // -- locating the failure -----------------------------------------------

    #[test]
    fn the_gate_excerpt_locates_errors_and_not_warnings() {
        let (_, text) = &f7bd_gate_excerpt()[0];
        assert_eq!(
            located_failures(text),
            vec!["/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts".to_string()],
            "two located errors in one file, and Tabs.svelte's WARNING is not a failure"
        );
    }

    #[test]
    fn the_forge_log_tail_locates_through_colour_and_timestamps() {
        let rollup = f7bd_rollup();
        let tail = rollup[2]["log_tail"].as_str().unwrap();
        assert_eq!(
            located_failures(tail),
            vec!["/workspace/david/boss/apps/web/src/it/yard/phone-strip.test.ts".to_string()]
        );
    }

    #[test]
    fn a_panic_and_a_rustc_error_are_located_and_a_warning_is_not() {
        let text = "test: a_test - panicked at crates/core/boss-jobs/src/car.rs:12:5:\n\
                    warning: unused import\n  --> crates/a/src/warn.rs:1:5\n\
                    error[E0308]: mismatched types\n  --> crates/b/src/lib.rs:40:9\n";
        assert_eq!(
            located_failures(text),
            vec![
                "crates/b/src/lib.rs".to_string(),
                "crates/core/boss-jobs/src/car.rs".to_string(),
            ]
        );
    }

    #[test]
    fn the_verdict_locates_across_both_halves() {
        let rollup = f7bd_rollup();
        assert_eq!(
            verdict_located_files(&f7bd_gate_fails(), &f7bd_gate_excerpt(), Some(&rollup)),
            vec![
                "/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts".to_string(),
                "/workspace/david/boss/apps/web/src/it/yard/phone-strip.test.ts".to_string(),
            ]
        );
    }

    // -- holding the car the failure names ----------------------------------

    #[test]
    fn train_f7bd1e9d_holds_car_g_alone() {
        let rollup = f7bd_rollup();
        let located =
            verdict_located_files(&f7bd_gate_fails(), &f7bd_gate_excerpt(), Some(&rollup));
        assert_eq!(
            cars_to_hold(&located, &f7bd_car_files()),
            vec![(
                "6c7c75f7-4f34-4f5f-87c0-3737e92a66ed".to_string(),
                vec!["apps/web/src/it/yard/phone-strip.test.ts".to_string()]
            )],
            "the failing file is car G's alone; the shed car is released, not held"
        );
    }

    #[test]
    fn a_file_two_cars_changed_holds_neither() {
        let located =
            vec!["/gate-target/repo/apps/web/src/it/yard/phone-strip.test.ts".to_string()];
        let both = vec![
            (
                "car-a".to_string(),
                vec!["apps/web/src/it/yard/phone-strip.test.ts".to_string()],
            ),
            (
                "car-b".to_string(),
                vec!["apps/web/src/it/yard/phone-strip.test.ts".to_string()],
            ),
        ];
        assert!(cars_to_hold(&located, &both).is_empty());
    }

    #[test]
    fn a_file_no_car_changed_holds_nobody() {
        let located = vec!["/gate-target/repo/libs/web-kit/src/ui/Tabs.svelte".to_string()];
        assert!(cars_to_hold(&located, &f7bd_car_files()).is_empty());
    }

    #[test]
    fn a_top_level_file_matches_only_itself_never_a_same_named_file_deeper() {
        let located = vec!["/gate-target/repo/crates/x/Cargo.toml".to_string()];
        let cars = vec![("car-a".to_string(), vec!["Cargo.toml".to_string()])];
        assert!(
            cars_to_hold(&located, &cars).is_empty(),
            "the root Cargo.toml is not crates/x/Cargo.toml"
        );
        let exact = vec!["Cargo.toml".to_string()];
        assert_eq!(cars_to_hold(&exact, &cars).len(), 1);
    }

    #[test]
    fn two_files_each_one_cars_hold_both_cars() {
        let located = vec![
            "crates/a/src/lib.rs".to_string(),
            "/gate-target/repo/apps/web/src/b.ts".to_string(),
        ];
        let cars = vec![
            ("car-a".to_string(), vec!["crates/a/src/lib.rs".to_string()]),
            ("car-b".to_string(), vec!["apps/web/src/b.ts".to_string()]),
        ];
        assert_eq!(
            cars_to_hold(&located, &cars),
            vec![
                ("car-a".to_string(), vec!["crates/a/src/lib.rs".to_string()]),
                ("car-b".to_string(), vec!["apps/web/src/b.ts".to_string()]),
            ]
        );
    }

    #[test]
    fn the_hold_names_the_train_the_check_the_file_and_the_way_out() {
        let r = judged_red_hold_reason(
            F7BD_ID,
            &["CI / web (pull_request)".into(), "svelte-check".into()],
            &["apps/web/src/it/yard/phone-strip.test.ts".into()],
        );
        for want in [
            "f7bd1e9d",
            "CI / web (pull_request)",
            "apps/web/src/it/yard/phone-strip.test.ts",
            "boss release",
        ] {
            assert!(r.contains(want), "hold reason lacks {want:?}: {r}");
        }
    }
}
