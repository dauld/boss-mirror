//! Verdicts — arrival, convergence, stalls, cancels, strikes, and the journal lines.

use super::*;

/// A step's `completed_at` evidence stamp, raw as stored. The
/// conductor stamps this on every step IT completes; steps closed by
/// other hands (the dispatcher's terminals) may not carry one.
pub(super) fn step_stamp<'a>(train: &'a Value, slug: &str, title: &str) -> Option<&'a str> {
    find_step(train, slug, title)
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("completed_at"))
        .and_then(Value::as_str)
}

pub(super) fn parse_stamp(s: Option<&str>) -> Option<DateTime<chrono::FixedOffset>> {
    s.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
}

fn secs_between(
    a: Option<DateTime<chrono::FixedOffset>>,
    b: Option<DateTime<chrono::FixedOffset>>,
) -> Value {
    match (a, b) {
        (Some(a), Some(b)) => json!((b - a).num_seconds()),
        _ => Value::Null,
    }
}

/// The deployed sha out of the deploy step's summary evidence
/// (`main@<sha>; ...`). None when the summary is absent or shaped
/// differently — the report never guesses.
fn deployed_generation(summary: &str) -> Option<&str> {
    summary
        .strip_prefix("main@")
        .and_then(|rest| rest.split([';', ' ']).next())
        .filter(|sha| !sha.is_empty())
}

/// The consist as the record states it — one entry per boarded car:
/// its short id, title, branch, and WHICH of the three item answers
/// the car gave.
///
/// ALL THREE, NOT ONLY THE CLOSING ONE (backlog 7f90c2ce). A car
/// states exactly one of `backlog_item` (this car IS the item's
/// build), `partial_item` (one piece of it) or `no_item_reason` (no
/// item, and which kind) — `ParkIntent::require_item_answer` refuses
/// to launch a gate without one, so the report's SOURCE cannot be
/// absent. Carrying only the closing edge made the other two answers
/// render as a missing field, which reads as "this car named no
/// item": a wrong answer where the record holds a right one. Measured
/// 2026-09-20 over the 200 most recent closed cars — 187 closing
/// links, 6 partial, 7 reasons — so 13 cars in that window reported
/// as unlinked while each had answered. The class is this codebase's
/// oldest: a wrong target answers instead of erroring, here applied
/// to a report rather than a query.
///
/// Ids are shortened (`id8`); the reason rides verbatim, because it
/// IS the answer. Absent keys are still omitted rather than nulled,
/// so a car filed before the refusal existed carries none of the
/// three and a reader can `get` rather than test.
///
/// ONE definition, two readers: the arrival report files it, and
/// the squash commit's body renders it (`squash_message`), so the
/// changelog git carries is the consist the packet record carries.
pub(crate) fn train_consist(boarded_cars: &[Value]) -> Vec<Value> {
    boarded_cars
        .iter()
        .map(|c| {
            let mut entry = json!({
                "car_id_short": id8(c.get("id").and_then(Value::as_str).unwrap_or("?")),
                "title": c.get("title").and_then(Value::as_str).unwrap_or_default(),
                "branch": c
                    .get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            });
            let stated = |key: &str| -> Option<&str> {
                c.get("metadata")
                    .and_then(|m| m.get(key))
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
            };
            if let Some(item) = stated(car::BACKLOG_ITEM) {
                entry[car::BACKLOG_ITEM] = json!(id8(item));
            }
            if let Some(item) = stated(car::PARTIAL_ITEM) {
                entry[car::PARTIAL_ITEM] = json!(id8(item));
            }
            if let Some(reason) = stated(car::NO_ITEM_REASON) {
                entry[car::NO_ITEM_REASON] = json!(reason);
            }
            entry
        })
        .collect()
}

/// The squash commit's body: one line per car of the consist —
/// `branch — title [ship-a-change <id8>, backlog-item <id8>]`. The
/// subject stays the forge's default (`train: <window> (N changes)
/// (#NNN)`); this rides beneath it, so `git log` on the forge (and on
/// the mirror once it publishes real history at 1.0.0) reads as the
/// changelog and `git log --grep <packet short id>` finds the change.
/// Measured 2026-09-19 (backlog f252cb1c, design cb38d806 Q2):
/// origin/main was 610 linear commits, every body empty — the car
/// titles, branches and packet ids lived only in the pr-train
/// packet's arrival report and the forge PR body. Empty consist,
/// empty body: the forge treats a blank message as none.
pub(crate) fn squash_message(consist: &[Value]) -> String {
    let field = |c: &Value, k: &str| -> String {
        c.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    // The item answer, in the spelling that says WHICH answer it was —
    // a closing link, one piece of an item, or a stated reason for
    // naming none (backlog 7f90c2ce). git log is a reader of this line
    // too, so "no item: ..." beats a blank the reader cannot tell from
    // an omission.
    let item_answer = |c: &Value| -> String {
        let text = |k: &str| c.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        if let Some(i) = text(car::BACKLOG_ITEM) {
            format!(", backlog-item {i}")
        } else if let Some(i) = text(car::PARTIAL_ITEM) {
            format!(", part of {i}")
        } else if let Some(r) = text(car::NO_ITEM_REASON) {
            format!(", no item: {r}")
        } else {
            String::new()
        }
    };
    consist
        .iter()
        .map(|c| {
            let item = item_answer(c);
            format!(
                "{} — {} [ship-a-change {}{item}]",
                field(c, "branch"),
                field(c, "title"),
                field(c, "car_id_short"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The arrival report — the landing's final structured entry, filed
/// on the `arrived` step when the sweep visits an arrived train.
/// Everything derives from evidence the job record already holds:
/// the boarded cars (consist), the board-time skips the train
/// recorded (left_behind), the deployed generation, and the timings
/// the conductor's own `completed_at` stamps make derivable. Missing
/// evidence reads as null, never a guess — `arrived_at` stays null
/// until whatever completes the outcome step stamps a time, and no
/// CI round count appears because the record does not carry one.
pub(crate) fn arrival_report(train: &Value, boarded_cars: &[Value]) -> Value {
    let consist = train_consist(boarded_cars);
    let left_behind = train
        .get("metadata")
        .and_then(|m| m.get("left_behind"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let generation = find_step(train, "deployed", "Deployed to the playground")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("deployed"))
        .and_then(Value::as_str)
        .and_then(deployed_generation);
    let merged_sha = find_step(train, "merged", "Merged into main")
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("merge_ref"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let boarded = step_stamp(train, "collect", "Collect what is ready to board");
    let merged = step_stamp(train, "merged", "Merged into main");
    let deployed = step_stamp(train, "deployed", "Deployed to the playground");
    let arrived = step_stamp(train, "arrived", "Train arrived");
    let mut report = json!({
        "consist": consist,
        "left_behind": left_behind,
        "generation": generation,
        "timings": {
            "boarded_at": boarded,
            "merged_at": merged,
            "deployed_at": deployed,
            "arrived_at": arrived,
            "board_to_merge_s": secs_between(parse_stamp(boarded), parse_stamp(merged)),
            "merge_to_deploy_s": secs_between(parse_stamp(merged), parse_stamp(deployed)),
            "total_s": secs_between(parse_stamp(boarded), parse_stamp(arrived)),
        },
    });
    // The merged sha is the generation seen from the other end — a
    // short deploy sha prefixing the full merge sha is the SAME
    // commit, and repeating it would imply a divergence that is not
    // there. It appears only when genuinely distinct (or when the
    // deploy evidence is missing and it is the only sha on record).
    if let Some(m) =
        merged_sha.filter(|m| generation.is_none_or(|g| !(g.starts_with(m) || m.starts_with(g))))
    {
        report["merged_sha"] = json!(m);
    }
    report
}

/// The one-line form of the report — filed beside it as `summary`,
/// and the shape of the journal line. Reads the report, not the
/// world: unknowns print as "unknown" / "?", never as guesses.
pub(crate) fn arrival_summary(report: &Value) -> String {
    let n = report
        .get("consist")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let generation = report
        .get("generation")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let total = report
        .get("timings")
        .and_then(|t| t.get("total_s"))
        .and_then(Value::as_i64)
        .map_or_else(|| "?".to_string(), |s| s.to_string());
    format!("{n} cars; generation {generation}; total {total}s")
}

/// The `deployed`-step evidence the conductor stamps: it runs no
/// deploy of its own. It is a COMPLETION, not a block: there is
/// genuinely nothing for the conductor to deploy — the forge-host
/// cluster-deploy-runner converges the cluster on forge main — and the
/// downstream convergence-verification step is what confirms the
/// cluster actually took the merge. Until 2026-09-18 this was one arm
/// of a deploy-tree switch; the other arm pulled a host tree and ran
/// the bare-metal deploy scripts train #443 deleted, and left with
/// them (backlog ed64f852).
pub(crate) const NO_PLAYGROUND_DEPLOY_EVIDENCE: &str = "no playground deploy — the cluster converges on forge main via the deploy-runner \
     (deployment-as-network); nothing to deploy from the conductor";

/// Do two commit identifiers name the same commit? Shas arrive at
/// different lengths from different mouths — the merge_ref is the
/// forge's 12-char answer, `Capabilities.commit` is the full 40 the
/// image build baked in — so equality is prefix containment, gated at
/// >=7 chars a side so an empty or truncated report can never
/// accidentally "match".
pub(crate) fn commits_match(a: &str, b: &str) -> bool {
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(b) || b.starts_with(a))
}

/// What the `converged` step should do this reconcile pass.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConvergenceVerdict {
    /// The running cluster binary self-reports the merge commit —
    /// complete the step with that evidence.
    Converged,
    /// Not there yet and inside the patience window — say nothing,
    /// look again next pass.
    Waiting,
    /// Not there and past the window — file the loud packet (once).
    /// Waiting silently is the defect this verdict exists to end:
    /// measured at six unnoticed hours on 2026-08-19.
    Overdue,
}

/// The convergence decision, pure. `cluster_commit` is what the
/// cluster's health endpoint self-reported (None: unreachable, or a
/// binary from before the commit field existed — evidence of absence
/// is absence of evidence here, so it converges nothing and times out
/// like any other lag).
pub(crate) fn convergence_verdict(
    merge_ref: &str,
    cluster_commit: Option<&str>,
    merge_is_ancestor_of_cluster: Option<bool>,
    mins_since_merge: i64,
    alarm_after_mins: i64,
) -> ConvergenceVerdict {
    if let Some(c) = cluster_commit
        && commits_match(merge_ref, c)
    {
        return ConvergenceVerdict::Converged;
    }
    // Equality cannot see "the cluster rolled PAST this train". With
    // two trains in flight, the second's deploy overwrites the first's
    // evidence window: on 2026-09-02 train #176 wedged at converge
    // forever because the cluster self-reported #177's commit — which
    // CONTAINS #176's merge. Ancestry is the honest question ("does
    // the running commit include my merge"), answered by git at the
    // call site; None means git could not answer (no clone, unknown
    // commit) and converges nothing — absence of evidence, as ever.
    if merge_is_ancestor_of_cluster == Some(true) {
        return ConvergenceVerdict::Converged;
    }
    if mins_since_merge >= alarm_after_mins {
        ConvergenceVerdict::Overdue
    } else {
        ConvergenceVerdict::Waiting
    }
}

/// The newest `completed_at` stamp across a train's steps — when
/// progress last provably happened. None when no step carries a
/// parseable stamp.
pub(super) fn newest_completion(train: &Value) -> Option<&str> {
    train
        .get("steps")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|s| {
            let raw = s
                .get("metadata")
                .and_then(|m| m.get("completed_at"))
                .and_then(Value::as_str)?;
            Some((DateTime::parse_from_rfc3339(raw).ok()?, raw))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, raw)| raw)
}

/// The stall sentinel's decision, pure: an open train counts stalled
/// when its newest step completion is at least `threshold_hours` old,
/// and the age in whole hours comes back for the journal line. No
/// completion evidence means no basis — None, never a guess.
pub(crate) fn stall_age_hours(
    train: &Value,
    now: DateTime<Utc>,
    threshold_hours: i64,
) -> Option<i64> {
    let newest = DateTime::parse_from_rfc3339(newest_completion(train)?).ok()?;
    let age = (now.signed_duration_since(newest)).num_hours();
    (age >= threshold_hours).then_some(age)
}

/// Which boarded cars a cancelled train releases back to the dock:
/// the still-open ones. A closed or cancelled car's record is history
/// — merged or abandoned, either way not the cancel path's to touch.
/// A CAR IS RELEASED ONLY IF IT STILL SAYS IT IS OURS.
///
/// Two facts answer "which cars are on this train": the train's
/// `boarded_jobs` list and each car's own `metadata.train`. Only the
/// second is maintained — releasing a car clears the car's marker and
/// leaves the train's list naming it forever. `parked_ready` and
/// `receipt_skip_reason` both read the CAR, so the car is authoritative
/// in practice while this function iterates the copy that drifts.
///
/// Cancelling a long-dead train therefore used to strip cars off a
/// LIVE one. Done on 2026-08-27: finishing the cancel of e1de28a3
/// released three cars that had since reboarded onto 1597b4a4, the next
/// board swept them onto a third train, and two trains believed they
/// carried the same consist. Nothing warned, because from inside the
/// loop a stale id and a current one look identical.
///
/// So the train's list proposes and the car disposes. A car naming a
/// different train has moved on; a car naming none was already released
/// and re-stamping it would overwrite a `skip_reason` that already says
/// where it has been.
pub(crate) fn releasable_cars<'a>(cars: &'a [Value], train_id: &str) -> Vec<&'a Value> {
    cars.iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|c| {
            c.get("metadata")
                .and_then(|m| m.get("train"))
                .and_then(Value::as_str)
                .is_some_and(|t| t == train_id)
        })
        .collect()
}

/// The auto-cancel decision, pure: should reconcile kill this train and
/// release its consist? Some(reason) or None, and the reason is what
/// lands on every released car.
///
/// A red train holds its whole consist hostage — the cars carry a
/// `train` marker so `parked_ready` no longer counts them, and the
/// conductor merges only on green, so nothing recovers on its own.
/// Overnight that is the difference between a pipeline that keeps
/// running and one that stops at the first fault. This is the reversal
/// of the older rule that only the operator may cancel (David,
/// 2026-08-15, choosing auto-cancel with a two-strike hold): raising is
/// still protocol, but an unattended pipeline has nobody to raise to.
///
/// THE VERDICT MUST BE THE LIVE ONE. `reconcile` reads it from the
/// forge each pass; the train's `ci` step keeps whatever verdict it was
/// first stamped with and is NOT re-stamped when CI re-runs. Deciding
/// from the step would cancel a train whose repair had already been
/// pushed and gone green — the exact case this is meant to rescue. A
/// re-running check reads `pending`, which is not `failing`, so a train
/// under repair is left alone.
///
/// A STALLED TRAIN IS RELEASED THE SAME WAY, AND FOR THE SAME REASON: a
/// run that was killed before it judged anything will never answer, and
/// its cars are no less hostage than a red train's. What differs is
/// whether the cancel counts against them — see `verdict_strikes_cars`.
pub(crate) fn auto_cancel_reason(
    train: &Value,
    live_verdict: &str,
    now: DateTime<Utc>,
    stall_hours: i64,
) -> Option<String> {
    let judged = match live_verdict {
        "failing" => true,
        "aborted" => false,
        _ => return None,
    };
    // A merged train is not a candidate whatever its checks say — the
    // content landed and the remaining steps are bookkeeping.
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let age = stall_age_hours(train, now, stall_hours)?;
    Some(if judged {
        format!(
            "CI red and no progress for {age}h (threshold {stall_hours}h) — cars released to board a later train"
        )
    } else {
        format!(
            "CI run aborted with no verdict and no progress for {age}h (threshold {stall_hours}h) — cars released unstruck to board a later train"
        )
    })
}

/// The operator's cancel stamp, parsed: `(reason, by)` when
/// `metadata.cancel_requested` is an object carrying a non-empty
/// `reason` and a non-empty `by`. Anything else — absent, a bare
/// string, an empty reason — is not a request and the conductor does
/// nothing on it: the reason lands on every released car as its
/// `skip_reason`, and a cancel that cannot say why or by whom is not
/// one the record can carry.
fn cancel_request(train: &Value) -> Option<(String, String)> {
    let req = train
        .get("metadata")?
        .get("cancel_requested")?
        .as_object()?;
    let reason = req.get("reason")?.as_str()?.trim();
    let by = req.get("by")?.as_str()?.trim();
    (!reason.is_empty() && !by.is_empty()).then(|| (reason.to_string(), by.to_string()))
}

/// The operator's cancel decision, pure — the yard's cancel button
/// (7a24caf3). An operator stamps the train's metadata
/// (`PATCH /api/jobs/{id}/metadata` with `cancel_requested: {by,
/// reason, at}`) and `reconcile` honours it on its next pass the way
/// it auto-cancels a stalled red — except that an operator's verb
/// NEVER strikes the cars (see `cancel`, the CLI verb). Some(reason)
/// is what lands on every released car.
///
/// A merged train is not a candidate whatever the stamp says: the
/// content landed, and releasing its cars would re-board changes that
/// are already on main. That train gets `operator_cancel_refusal`.
pub(crate) fn operator_cancel_reason(train: &Value) -> Option<String> {
    if step_done(find_step(train, "merged", "Merged into main")) {
        return None;
    }
    let (reason, by) = cancel_request(train)?;
    Some(format!("operator cancel: {reason} (by {by})"))
}

/// The answer a merged train owes a cancel request it cannot honour,
/// stamped ONCE as `cancel_refused` (like `ci_overdue_since`): the
/// request stays on the record, the refusal says why, and the yard has
/// something to render instead of a button that silently did nothing.
pub(crate) fn operator_cancel_refusal(train: &Value) -> Option<String> {
    let merged = find_step(train, "merged", "Merged into main");
    if truthy(train.get("metadata").and_then(|m| m.get("cancel_refused")))
        || !step_done(merged)
        || cancel_request(train).is_none()
    {
        return None;
    }
    let merge_ref = merged
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("merge_ref"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Some(format!("already merged at {merge_ref}"))
}

/// Does this train's cancellation count against the cars aboard?
///
/// ONLY A RETURNED FAILING VERDICT. A strike is a claim that CI looked
/// at this consist and found it broken; two of them hold a car out of
/// the queue until a human looks (`car_hold_reason`). A run killed by an
/// infrastructure incident makes no such claim, and treating it as one
/// is how 2026-08-22 went: two trains stalled, their runs were cancelled
/// mid-flight, and the four cars aboard — every one of which test-merged
/// clean — took a strike on each train, hit the hold, and sat through
/// five departures before a human noticed.
///
/// The distinction lives on the release itself, not in a second counter:
/// the cars are released with the stall named in their `skip_reason`, so
/// the record says which question to ask without inventing a strike
/// nothing reads.
/// One forge commit status as one rollup entry — the ONLY place the
/// adapter decides which fields of a check the system of record keeps.
///
/// Pure, because this layer has dropped a field before and the drop was
/// silent: the check's NAME (`context`) was dropped, so every red train
/// in the SoR read `?:FAILURE` and 2026-09-02 cost two trips to the
/// forge API to learn that `test` had died on a disk floor, not on code.
/// The `description` is the other load-bearing field: locomotive.sh
/// posts a status whose description starts with `refused:` when the CI
/// host refuses BEFORE any check runs, and [`verdict_strikes_cars`]
/// spares every car aboard on that word alone (c186d63d). A rollup that
/// lost descriptions would turn every infrastructure refusal back into a
/// strike against innocent cars, silently. Pinned by
/// `a_refusal_survives_the_rollup_and_spares_the_cars`.
pub(crate) fn rollup_entry(st: &Value) -> Value {
    let verdict = st
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let conclusion = match verdict.as_str() {
        "success" => "SUCCESS",
        "failure" | "error" => "FAILURE",
        _ => "",
    };
    json!({
        "context": st.get("context").and_then(Value::as_str).unwrap_or_default(),
        // The forge's own one-line reason, when it gives one — free
        // provenance, and the refusal channel.
        "description": st
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "conclusion": conclusion,
        "status": if verdict == "pending" { "PENDING" } else { "COMPLETED" },
        // …/actions/runs/{run}/jobs/{index}: the run and the failing
        // job's position in it. `attach_failing_logs` resolves it to a
        // job id and pulls the log tail.
        "target_url": st.get("target_url").and_then(Value::as_str).unwrap_or_default(),
    })
}

pub(crate) fn verdict_strikes_cars(verdict: &str, rollup: Option<&Value>) -> bool {
    if verdict != "failing" {
        return false;
    }
    // A failing verdict strikes UNLESS a failing check says it REFUSED.
    // The locomotive job posts a commit status whose description starts
    // with `refused:` when it declines to run — a disk floor, a stale
    // runner image, a wrong uid — before any check has judged the tree.
    // Three times (2026-08-22, 09-02, 09-05 train #204) that refusal
    // was recorded as a plain red and struck every car aboard; two
    // strikes hold a car out until a human looks. The word must sit on
    // a FAILING check: a passing check that mentions refusing proves
    // nothing, and a bare failure with no description is a real red.
    !any_failing_check_refused(rollup)
}

/// Does any FAILING check in the rollup carry a `refused` description —
/// or declare a refusal in its own log?
///
/// Two channels for one fact, and the second exists because the first
/// is a network write at the moment the CI host is already in trouble
/// (c186d63d). locomotive.sh posts the `refused: <why>` status
/// best-effort; with no token, no GITHUB_SHA, or a rejected POST, it
/// says "the conductor will read this as a plain red" and every car
/// aboard is struck for a full disk. But it declares every refusal in
/// its OWN log first — `LOCOMOTIVE RED: <why>` — and the conductor
/// already attaches the failing job's log tail to the entry for the
/// alert. The intent is where another actor can read it; read it there.
pub(crate) fn any_failing_check_refused(rollup: Option<&Value>) -> bool {
    rollup
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
        .any(|c| {
            let description_refuses = c
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|d| d.trim_start().to_lowercase().starts_with("refused"));
            description_refuses
                || c.get("log_tail")
                    .and_then(Value::as_str)
                    .is_some_and(log_declares_refusal)
        })
}

/// The locomotive's own declaration, as it prints it: a line starting
/// `LOCOMOTIVE RED:` (locomotive.sh `say`). A LINE, at its start — a
/// test that happens to print the word, or a log quoting this rule,
/// is not a refusal. Pure and line-based so a wrapped or indented tail
/// still matches on the line it is on.
pub(crate) fn log_declares_refusal(tail: &str) -> bool {
    tail.lines().map(str::trim_start).any(|l| {
        l.starts_with("LOCOMOTIVE RED:")
            // The two lines locomotive.sh prints LAST when the status
            // could not be posted — the tail keeps the end of a log,
            // so these survive a truncation that lost the RED line.
            || (l.starts_with("locomotive:")
                && (l.contains("refusal not posted")
                    || l.contains("could not post the refusal status")))
    })
}

/// The names of the checks that FAILED, from the rollup — `context`
/// (status checks) or `name` (check runs), whichever the entry carries.
pub(crate) fn failing_checks(rollup: Option<&Value>) -> Vec<String> {
    rollup
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.get("conclusion").and_then(Value::as_str) == Some("FAILURE"))
        .map(|c| {
            c.get("context")
                .or_else(|| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("unnamed check")
                .to_string()
        })
        .collect()
}

/// The metadata a released car carries away from a cancelled train.
///
/// `train`/`boarded_head` cleared so the dock counts it again, why it
/// came back, and — only when the train's CI actually judged it —
/// one more red against its record.
pub(crate) fn release_stamps(
    car: &Value,
    reason: &str,
    strike: bool,
) -> Vec<(&'static str, Value)> {
    // The boarded head goes with the train stamp: this car boarded
    // nothing now, and a stale head is not evidence about whatever it
    // boards next.
    let mut stamps = vec![
        ("train", Value::Null),
        ("boarded_head", Value::Null),
        (
            "skip_reason",
            json!(format!("returned to dock: train cancelled ({reason})")),
        ),
    ];
    // Every car aboard a red train is counted, not just the guilty one —
    // which car turned the consist red is exactly what nobody knows yet.
    // One red is survivable (see `car_hold_reason`); it takes a second,
    // aboard a DIFFERENT consist, before boarding holds it.
    if strike {
        let reds = car
            .get("metadata")
            .and_then(|m| m.get("red_trains"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1;
        stamps.push(("red_trains", json!(reds)));
    }
    stamps
}

/// Has the CI verdict MOVED since it was last recorded?
///
/// THE BLIND SPOT THIS CLOSES. The `ci` step is completed exactly once,
/// the first time the rollup settles, and never looked at again. So the
/// verdict on a train that was repaired — pushed to, re-run, and gone
/// red a second time — is recorded nowhere and logged nowhere. On
/// 2026-08-15 train 20260815-0621 sat red for 45 minutes after a repair
/// with the system reporting nothing; it was found by querying the
/// forge by hand. The repair loop is exactly the path with no feedback,
/// which is the worst place to have none.
///
/// WHY THIS DOES NOT RE-STAMP THE STEP, which is the obvious fix and is
/// impossible: `update_step_at` freezes status, completed_on AND
/// METADATA on a terminal row, so the step's `result` cannot be
/// rewritten — and today's other lesson is not to design a path that
/// needs to un-complete a step. The train JOB's metadata is not frozen,
/// so the moving fact lives there, next to the immutable record of what
/// the verdict was when it first settled. Both are true and they are
/// different facts.
///
/// `pending` is never a change worth reporting: a re-run passes through
/// it on the way to an answer, and announcing it would make the signal
/// fire on every repair.
pub(crate) fn verdict_drift(recorded: Option<&str>, live: &str) -> Option<String> {
    let recorded = recorded?;
    if live == "pending" || live == recorded {
        return None;
    }
    Some(format!(
        "CI verdict moved {recorded} -> {live} since it was recorded"
    ))
}

/// CI has been asked and has not answered — the case `verdict_drift`
/// cannot see, because there is no verdict to compare.
///
/// Drift reports a verdict that MOVED. A runner that hangs, a job that
/// never reports, a queue nothing picks up: those produce no verdict at
/// all, so the train sits with its `ci` step incomplete and every
/// reconcile finds `pending` and says nothing. This is the backstop for
/// that, and only that.
///
/// THE THRESHOLD IS MEASURED, NOT GUESSED (David, 2026-08-15, choosing
/// 2x p90). Across 22 trains the pr->ci time had a median of ~33
/// minutes, a p90 of ~56, and a range of 10 to 169. Half again the
/// median would be ~50 minutes and would fire on six of those 22 — a
/// quarter of all trains, which is how an alert becomes furniture. Two
/// hours is roughly twice p90 and clears every train ever observed
/// except the 169-minute outlier, so when it fires it means something.
///
/// Worth recording alongside it, because it argues for LONGER trains:
/// that spread has no relationship to car count. A one-car train took
/// 63 minutes and an eight-car train took 12. The cost is per run, not
/// per car.
pub(crate) fn ci_overdue(
    train: &Value,
    now: DateTime<Utc>,
    threshold_hours: i64,
) -> Option<String> {
    // Only once the PR exists — before that there is nothing for CI to
    // answer about, and a train stuck earlier is the stall sentinel's.
    let asked = parse_stamp(step_stamp(train, "pr", "Open the batched PR"))?;
    if step_done(find_step(train, "ci", "CI verdict")) {
        return None;
    }
    let hours = now.signed_duration_since(asked).num_hours();
    (hours >= threshold_hours).then(|| {
        format!(
            "CI has not answered in {hours}h (threshold {threshold_hours}h) — no verdict, not a red one"
        )
    })
}

/// Why a train that is READY to merge is not being merged.
///
/// THE SILENT DECLINE THIS CLOSES. On 2026-09-04 a `boss train reconcile`
/// run by hand sat on a train with green CI and an OPEN PR and did
/// nothing — the merge arm requires `auto_merge`, which reads
/// `BOSS_TRAIN_AUTO_MERGE` from the environment, and a verb run by hand
/// inherits no unit (the conductor's ConfigMap is what sets it; see
/// §Doors). The else-branch was silence, so two reconciles reported a
/// clean pass while achieving nothing they were run for, and the
/// operator spent hours looking for a deeper fault that did not exist.
/// A conductor that declines to do the one thing it was run for owes an
/// answer, and the answer must name the reason: a verdict someone must
/// go re-derive is not a verdict.
///
/// ONLY THE GENUINELY-DECLINED CASE. Not-green and already-merged/closed
/// are the ordinary states of nearly every reconcile pass and are
/// reported elsewhere (`verdict_drift`, `ci_overdue`, the `merged` step);
/// naming them here would put a line on every train every ten minutes
/// and the signal would be furniture inside a day. Green + OPEN +
/// declined is the one state that looks like progress and is not.
pub(crate) fn merge_declined_reason(
    auto_merge: bool,
    verdict: &str,
    pr_state: Option<&str>,
) -> Option<&'static str> {
    if auto_merge || verdict != "green" || pr_state != Some("OPEN") {
        return None;
    }
    Some(
        "BOSS_TRAIN_AUTO_MERGE is not \"1\" (a verb run by hand inherits \
         no unit environment; the conductor's ConfigMap sets it)",
    )
}

/// The boarding hold, pure: a car released from that many red trains
/// stops boarding until someone looks at it. Without this the auto
/// cancel above is a loop — the same consist re-boards, goes red, and
/// cancels again all night, burning CI and landing nothing.
pub(crate) fn car_hold_reason(car: &Value, max_reds: i64) -> Option<String> {
    let reds = car
        .get("metadata")
        .and_then(|m| m.get("red_trains"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    (reds >= max_reds)
        .then(|| format!("held after {reds} red trains — needs a look before it boards again"))
}

/// The ONE branch a cancelled train may delete: its own `train/*`
/// assembly branch (the Job's subject id). Car branches hold the
/// cars' unmerged work and are never the cancel path's to touch —
/// this filter is the pin.
pub(crate) fn train_branch_to_delete(train: &Value) -> Option<String> {
    train
        .get("subject")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .filter(|b| b.starts_with("train/"))
        .map(str::to_string)
}

/// The branch an ARRIVED train sheds: its own `train/*` branch (the
/// same pin the cancel path deletes through), and only when the
/// record proves the happy landing — the `arrived` terminal strictly
/// `completed`, never `skipped`. A cancelled train closes with
/// `arrived` skipped, and its branch was the cancel verb's to delete
/// at cancel time.
///
/// `boss train cancel` has owned its branch since the verb existed;
/// nothing owned the branch after a HAPPY landing, and 62 stale
/// `train/*` branches accumulated on the forge between 08-13 and
/// 08-20 (packet ab3fa473). Squash merges are why nothing git-side
/// can ever classify them after the fact — the arrival record is the
/// proof, held here at exactly the right moment.
///
/// Gated on the forgejo adapter: the internal forge keeps merged PR
/// heads, which is the debt this cleans; GitHub auto-deletes them
/// repo-side, so under that adapter there is nothing to own.
pub(crate) fn arrival_branch_to_delete(train: &Value, forge_kind: &str) -> Option<String> {
    if forge_kind != "forgejo" {
        return None;
    }
    let arrived = find_step(train, "arrived", "Train arrived")?;
    (arrived.get("status").and_then(Value::as_str) == Some("completed"))
        .then(|| train_branch_to_delete(train))
        .flatten()
}

/// The journal line for an arrival cleanup's outcome — and the pin
/// that a failed delete is a LINE, never a failed arrival: the
/// `Result` is consumed here, so the caller has nothing left to
/// propagate. A leftover branch is housekeeping debt; a failed
/// arrival is an outage. Ok(false) (already gone) says nothing — the
/// sweep revisits an unsettled train every pass, and done work
/// narrated hourly reads as work happening.
pub(crate) fn arrival_cleanup_note(branch: &str, outcome: Result<bool>) -> Option<String> {
    match outcome {
        Ok(true) => Some(format!("deleted branch {branch} (train arrived)")),
        Ok(false) => None,
        Err(e) => Some(format!(
            "branch {branch} not deleted (arrival stands, debt noted): {e}"
        )),
    }
}

/// Resolve the operator's handle — a Job id, an id prefix, or the
/// train's PR url — against the open trains. Exactly one match or an
/// error saying what went wrong; an ambiguous prefix refuses rather
/// than guessing which train to cancel.
pub(crate) fn resolve_train<'a>(trains: &'a [Value], handle: &str) -> Result<&'a Value> {
    let matches: Vec<&Value> = trains
        .iter()
        .filter(|t| {
            let id = t.get("id").and_then(Value::as_str).unwrap_or_default();
            let pr_url = find_step(t, "pr", "Open the batched PR")
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("pr_url"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            id == handle || (!handle.is_empty() && id.starts_with(handle)) || pr_url == handle
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => bail!("no open train matches {handle:?}"),
        many => bail!(
            "{handle:?} is ambiguous — matches trains {}",
            many.iter()
                .map(|t| id8(t.get("id").and_then(Value::as_str).unwrap_or("?")))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// The DRY log lines mirror the python conductor's dict/list reprs —
// the journal is operator surface, and the port keeps its lines.

/// What a completed step announces: the step, the Job, and the
/// evidence just written to it.
///
/// The evidence half is the point. `complete_step` used to log only
/// `completed <step> on <id>`, which is byte-identical whether the
/// `ci` step recorded `result: green` or `result: failing`. Green was
/// loud only by accident — the merge path emits a second line — so a
/// red train produced strictly less output than a green one and read,
/// in the journal, as nothing having happened. Trains 46 and 47 both
/// went red inside an hour on 2026-08-16 and neither said so; the
/// second was missed by a log monitor that had already been widened
/// after the first (88c3890c).
///
/// Fields carrying nothing are omitted rather than printed as `None`
/// — most steps complete with no evidence at all, and a line ending
/// in `with {}` teaches readers to skip the tail of every line,
/// including the ones that matter.
pub(super) fn completion_log_line(
    label: &str,
    id8: &str,
    fields: &[(&str, Option<String>)],
) -> String {
    let evidence: Vec<(&str, Option<String>)> = fields
        .iter()
        .filter(|(_, v)| v.is_some())
        .cloned()
        .collect();
    if evidence.is_empty() {
        format!("completed {label} on {id8}")
    } else {
        format!("completed {label} on {id8} with {}", py_dict(&evidence))
    }
}

pub(super) fn py_dict(fields: &[(&str, Option<String>)]) -> String {
    let inner = fields
        .iter()
        .map(|(k, v)| match v {
            Some(v) => format!("'{k}': '{v}'"),
            None => format!("'{k}': None"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{inner}}}")
}

pub(super) fn py_keys(keys: &[&str]) -> String {
    let inner = keys
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

pub(super) fn py_pairs(cands: &[(Value, String)]) -> String {
    let inner = cands
        .iter()
        .map(|(j, b)| {
            let id = j.get("id").and_then(Value::as_str).unwrap_or("?");
            format!("('{}', '{b}')", id8(id))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::*;

    // A red train and a green one must not produce the same line.
    // Trains 46 and 47 both went red on 2026-08-16 and the journal
    // said `completed ci on <id>` for each — the same text train 45
    // produced going green.
    #[test]
    fn a_completed_step_says_what_it_recorded() {
        let red = completion_log_line(
            "ci",
            "e78859ab",
            &[("result", Some("failing".into())), ("notify_on_done", None)],
        );
        let green = completion_log_line(
            "ci",
            "810a7a3f",
            &[("result", Some("green".into())), ("notify_on_done", None)],
        );
        assert_ne!(
            red.replace("e78859ab", "X"),
            green.replace("810a7a3f", "X"),
            "red and green must be distinguishable without knowing the train id"
        );
        assert!(red.contains("failing"), "{red}");
        // A field with nothing in it is not evidence, and printing it
        // as None trains the reader to stop at the id.
        assert!(!red.contains("None"), "{red}");
    }

    // Most steps complete with no evidence; those lines stay as they
    // were rather than gaining an empty dict.
    #[test]
    fn a_step_with_no_evidence_logs_as_before() {
        assert_eq!(
            completion_log_line("assemble", "e78859ab", &[]),
            "completed assemble on e78859ab"
        );
        assert_eq!(
            completion_log_line("assemble", "e78859ab", &[("skipped", None)]),
            "completed assemble on e78859ab"
        );
    }

    // -- the drift sentinel (split-brain incident c4b4a6b0) ----------------
    // ----- convergence_verdict — installation is not the finish line
    //
    // fdff316c / 7e5ee013, decided 2026-08-19: the arrival report may
    // only fire once the RUNNING cluster binary self-reports the merge
    // commit, and a lag past the threshold files a packet instead of
    // waiting silently (six unnoticed hours, measured).

    #[test]
    fn commit_identities_match_by_prefix_with_a_floor() {
        let full = "4ee5bba7a17a0123456789abcdef0123456789ab";
        assert!(commits_match("4ee5bba7a17a", full), "short vs full");
        assert!(commits_match(full, "4ee5bba7a17a"), "full vs short");
        assert!(commits_match(full, full), "identical");
        assert!(!commits_match("4ee5bba7a17a", "9da5e4fe1234"), "different");
        // The floor: nothing under 7 chars can match anything — an
        // empty or truncated self-report must never read as converged.
        assert!(!commits_match("", full));
        assert!(!commits_match("4ee5bb", full), "6 chars is below the floor");
    }

    #[test]
    fn a_matching_self_report_converges_regardless_of_elapsed_time() {
        for mins in [0, 29, 500] {
            assert_eq!(
                convergence_verdict(
                    "4ee5bba7a17a",
                    Some("4ee5bba7a17a0123456789ab"),
                    None,
                    mins,
                    30
                ),
                ConvergenceVerdict::Converged,
            );
        }
    }

    #[test]
    fn no_or_wrong_report_waits_inside_the_window_and_alarms_past_it() {
        // None: unreachable, or a binary predating the commit field —
        // absence never converges and times out like any other lag.
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", None, None, 29, 30),
            ConvergenceVerdict::Waiting,
        );
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", None, None, 30, 30),
            ConvergenceVerdict::Overdue,
        );
        // The previous release still running: same shape.
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", Some("d92230071234"), None, 10, 30),
            ConvergenceVerdict::Waiting,
        );
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", Some("d92230071234"), None, 31, 30),
            ConvergenceVerdict::Overdue,
        );
    }
    /// The red-train forensics pin (2026-09-02): a verdict must NAME
    /// what failed. The forge adapter builds this rollup shape and
    /// `ci_check_summary` renders it — the two live apart, so the
    /// contract between them is pinned here (CLAUDE.md 9a). Before the
    /// adapter carried `context`, every red train recorded `?:FAILURE`
    /// and finding the answer cost three calls to the forge API — and
    /// the answer that time was that `test` had died on a disk floor,
    /// not on any code at all.
    #[test]
    fn a_red_check_is_named_in_the_recorded_verdict() {
        let rollup = json!([
            {"context": "build-image", "conclusion": "SUCCESS", "status": "COMPLETED"},
            {"context": "test", "conclusion": "FAILURE", "status": "COMPLETED"},
        ]);
        let summary = ci_check_summary(Some(&rollup));
        assert!(
            summary.contains("test:FAILURE"),
            "the failing check must be named, got: {summary}"
        );
        assert!(!summary.contains("?:"), "no anonymous checks: {summary}");
    }

    /// The rolled-past case (2026-09-02, train #176): the cluster
    /// self-reports a LATER commit that contains this train's merge.
    /// Equality misses; ancestry converges. And git's inability to
    /// answer (None) must never converge — absence of evidence.
    #[test]
    fn a_cluster_rolled_past_the_merge_still_converges_by_ancestry() {
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", Some("d92230071234"), Some(true), 500, 30),
            ConvergenceVerdict::Converged
        );
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", Some("d92230071234"), Some(false), 31, 30),
            ConvergenceVerdict::Overdue
        );
        assert_eq!(
            convergence_verdict("4ee5bba7a17a", None, None, 10, 30),
            ConvergenceVerdict::Waiting
        );
    }

    //
    // BOSS_JOBS_URL defaulted to localhost and the conductor silently
    // booked a whole window on the wrong instance. Preflight goes red
    // on a loopback jobs URL unless the box says it means it.

    // -- the arrival report ------------------------------------------------
    //
    // The landing's final structured entry: when the sweep visits an
    // arrived train, it composes what the record proves — the consist,
    // who got left behind, the generation, and the timings the
    // conductor's own `completed_at` stamps make derivable — and files
    // it on the `arrived` step. Missing evidence reads as null, never
    // a guess.

    fn boarded_cars() -> Vec<serde_json::Value> {
        vec![
            json!({"id": "car-1-uuid-long", "title": "Fix the thing",
                   "metadata": {"branch": "feat/x"}}),
            json!({"id": "car-2-uuid-long", "title": "Add the widget",
                   "metadata": {"branch": "feat/y"}}),
        ]
    }

    #[test]
    fn the_arrival_report_carries_consist_left_behind_and_timings() {
        let report = arrival_report(&arrived_train(), &boarded_cars());
        assert_eq!(
            report["consist"],
            json!([
                {"car_id_short": "car-1-uu", "title": "Fix the thing", "branch": "feat/x"},
                {"car_id_short": "car-2-uu", "title": "Add the widget", "branch": "feat/y"},
            ])
        );
        assert_eq!(
            report["left_behind"],
            json!([{"car_id_short": "car-3-id", "reason": "conflict: src/a.rs"}])
        );
        assert_eq!(report["generation"], json!("abc1234"));
        // merge_ref abc1234def56 IS the deployed generation (short sha
        // prefix) — not distinct, so no merged_sha key.
        assert!(report.get("merged_sha").is_none(), "same commit: {report}");
        assert_eq!(
            report["timings"]["boarded_at"],
            json!("2026-08-13T06:00:00Z")
        );
        assert_eq!(
            report["timings"]["merged_at"],
            json!("2026-08-13T06:05:00Z")
        );
        assert_eq!(
            report["timings"]["deployed_at"],
            json!("2026-08-13T06:12:00Z")
        );
        assert_eq!(
            report["timings"]["arrived_at"],
            json!("2026-08-13T06:20:00Z")
        );
        assert_eq!(report["timings"]["board_to_merge_s"], json!(300));
        assert_eq!(report["timings"]["merge_to_deploy_s"], json!(420));
        assert_eq!(report["timings"]["total_s"], json!(1200));
    }

    // The squash commit's body IS the consist (backlog f252cb1c,
    // design cb38d806 Q2): measured 2026-09-19, origin/main was 610
    // linear commits with every body empty, so git log read as 610
    // timestamps. One line per car, from the SAME consist the
    // arrival report files — branch, title, the ship-a-change short
    // id, and the backlog-item short id when the car names one — so
    // git log --grep <short id> finds the change on the forge.
    #[test]
    fn the_squash_message_lists_one_line_per_car_of_the_consist() {
        let mut cars = boarded_cars();
        cars[0]["metadata"][car::BACKLOG_ITEM] = json!("f252cb1c-1555-4cf6-9f93-19024bb166c3");
        let consist = train_consist(&cars);
        assert_eq!(consist[0][car::BACKLOG_ITEM], json!("f252cb1c"));
        assert!(
            consist[1].get(car::BACKLOG_ITEM).is_none(),
            "{}",
            consist[1]
        );
        assert_eq!(
            squash_message(&consist),
            "feat/x — Fix the thing [ship-a-change car-1-uu, backlog-item f252cb1c]\n\
             feat/y — Add the widget [ship-a-change car-2-uu]"
        );
        assert_eq!(squash_message(&[]), "");
    }

    // ALL THREE ANSWERS RIDE THE CONSIST, not only the closing one
    // (backlog 7f90c2ce). `require_item_answer` guarantees a car
    // states exactly one of three, so the report's source cannot be
    // absent — but until this landed only `backlog_item` was carried,
    // and a car that answered `--park-partial-item` or `--park-no-item`
    // rendered with NO field at all, identical to a car that answered
    // nothing. Measured 2026-09-20 over the 200 most recent closed
    // cars: 187 closing links, 6 partial, 7 reasons — so 13 in that
    // window read as unlinked when each had given an answer. A missing
    // field must not be how a stated fact is reported.
    #[test]
    fn every_item_answer_rides_the_consist_not_only_the_closing_one() {
        let cars = vec![
            json!({"id": "car-1-uuid-long", "title": "Fix the thing",
                   "metadata": {"branch": "feat/x",
                                "backlog_item": "f252cb1c-1555-4cf6-9f93-19024bb166c3"}}),
            json!({"id": "car-2-uuid-long", "title": "Add the widget",
                   "metadata": {"branch": "feat/y",
                                "partial_item": "9f00a805-cdbe-44de-94b1-c2e4dbb607b2"}}),
            json!({"id": "car-3-uuid-long", "title": "Tidy the stray",
                   "metadata": {"branch": "chore/z",
                                "no_item_reason": "found while building something else"}}),
        ];
        let consist = train_consist(&cars);
        assert_eq!(consist[0][car::BACKLOG_ITEM], json!("f252cb1c"));
        assert_eq!(consist[1]["partial_item"], json!("9f00a805"));
        assert_eq!(
            consist[2]["no_item_reason"],
            json!("found while building something else")
        );
        // Each car states ONE answer, so no entry carries a second.
        assert!(
            consist[1].get(car::BACKLOG_ITEM).is_none(),
            "{}",
            consist[1]
        );
        assert!(consist[2].get("partial_item").is_none(), "{}", consist[2]);
        assert_eq!(
            squash_message(&consist),
            "feat/x — Fix the thing [ship-a-change car-1-uu, backlog-item f252cb1c]\n\
             feat/y — Add the widget [ship-a-change car-2-uu, part of 9f00a805]\n\
             chore/z — Tidy the stray [ship-a-change car-3-uu, no item: found while \
             building something else]"
        );
    }

    #[test]
    fn a_distinct_merge_sha_is_reported() {
        let mut train = arrived_train();
        train["steps"][2]["metadata"]["deployed"] =
            json!("main@999aaaa; 0 applied; services: prod; web: deployed");
        let report = arrival_report(&train, &boarded_cars());
        assert_eq!(report["generation"], json!("999aaaa"));
        assert_eq!(report["merged_sha"], json!("abc1234def56"));
    }

    #[test]
    fn missing_evidence_reads_as_null_never_a_guess() {
        // A train whose steps carry no completed_at stamps (they
        // predate the stamping, or the dispatcher closed `arrived`)
        // and whose deploy summary is absent.
        let train = json!({
            "id": "train-78",
            "status": "closed",
            "metadata": {"boarded_jobs": ["car-1"]},
            "steps": [
                {"spec_slug": "collect", "title": "Collect what is ready to board",
                 "status": "completed", "metadata": {}},
                {"spec_slug": "merged", "title": "Merged into main",
                 "status": "completed", "metadata": {}},
                {"spec_slug": "arrived", "title": "Train arrived",
                 "status": "completed", "metadata": {}},
            ],
        });
        let report = arrival_report(&train, &boarded_cars());
        assert_eq!(report["left_behind"], json!([]));
        assert_eq!(report["generation"], Value::Null);
        // No deployed sha to compare against — the merge evidence is
        // absent too, so no merged_sha key appears.
        assert!(report.get("merged_sha").is_none());
        assert_eq!(report["timings"]["boarded_at"], Value::Null);
        assert_eq!(report["timings"]["arrived_at"], Value::Null);
        assert_eq!(report["timings"]["board_to_merge_s"], Value::Null);
        assert_eq!(report["timings"]["merge_to_deploy_s"], Value::Null);
        assert_eq!(report["timings"]["total_s"], Value::Null);
    }

    #[test]
    fn the_summary_reads_the_report_not_the_world() {
        let full = arrival_report(&arrived_train(), &boarded_cars());
        assert_eq!(
            arrival_summary(&full),
            "2 cars; generation abc1234; total 1200s"
        );
        let bare = arrival_report(&json!({"id": "t", "metadata": {}, "steps": []}), &[]);
        assert_eq!(
            arrival_summary(&bare),
            "0 cars; generation unknown; total ?s"
        );
    }

    // -- the stall sentinel ------------------------------------------------
    //
    // A train counts stalled when open and its newest step completion
    // is older than the threshold. Raising is protocol, cancelling is
    // judgment — the sentinel only makes the stall visible.

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn train_with_stamps(stamps: &[&str]) -> serde_json::Value {
        let steps: Vec<serde_json::Value> = stamps
            .iter()
            .map(|t| json!({"status": "completed", "metadata": {"completed_at": t}}))
            .collect();
        json!({"id": "t-1", "status": "open", "metadata": {}, "steps": steps})
    }

    #[test]
    fn a_train_past_the_threshold_counts_stalled() {
        let t = train_with_stamps(&["2026-08-13T00:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T08:30:00Z"), 6), Some(8));
        // The boundary counts: exactly at the threshold is stalled.
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), Some(6));
    }

    #[test]
    fn a_train_inside_the_threshold_is_not_stalled() {
        let t = train_with_stamps(&["2026-08-13T00:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T05:59:00Z"), 6), None);
    }

    #[test]
    fn the_newest_completion_is_the_stall_basis() {
        // Unordered stamps: the NEWEST one anchors the age (3h ago),
        // not the oldest (30h ago).
        let t = train_with_stamps(&["2026-08-12T00:00:00Z", "2026-08-13T03:00:00Z"]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), None);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T09:00:00Z"), 6), Some(6));
    }

    #[test]
    fn a_train_without_stamps_never_counts_stalled() {
        // No completion evidence, no basis — the sentinel never
        // guesses an age.
        let t = train_with_stamps(&[]);
        assert_eq!(stall_age_hours(&t, ts("2026-08-13T06:00:00Z"), 6), None);
    }

    // -- auto-cancelling a red train ---------------------------------------
    //
    // The overnight rule: a train that is red AND has stopped moving
    // releases its consist rather than holding it until morning.

    fn red_train(stamps: &[&str], merged: bool) -> serde_json::Value {
        let mut steps: Vec<serde_json::Value> = stamps
            .iter()
            .map(|t| json!({"status": "completed", "metadata": {"completed_at": t}}))
            .collect();
        steps.push(json!({
            "spec_slug": "merged",
            "title": "Merged into main",
            "status": if merged { "completed" } else { "ready" },
            "metadata": {}
        }));
        json!({"id": "t-1", "status": "open", "metadata": {}, "steps": steps})
    }

    #[test]
    fn a_red_train_that_stopped_moving_is_auto_cancelled() {
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        let r = auto_cancel_reason(&t, "failing", ts("2026-08-13T08:00:00Z"), 6);
        assert!(r.is_some(), "red and 8h stalled should cancel");
        assert!(r.unwrap().contains("8h"), "the reason carries the age");
    }

    #[test]
    fn a_red_train_inside_the_threshold_is_left_alone() {
        // Still young enough that a re-run or a repair may yet save it.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "failing", ts("2026-08-13T05:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_stalled_train_under_repair_is_not_cancelled() {
        // THE REGRESSION THIS EXISTS FOR: a repair has been pushed and
        // CI is re-running, so the LIVE verdict is `pending` even
        // though the train's own `ci` step still reads `failing` from
        // the first run. Deciding from the step would cancel the train
        // the repair was about to save.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "pending", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_green_stalled_train_is_never_auto_cancelled() {
        // Green and stalled means waiting on the merge, not broken —
        // cancelling would throw away a consist that is about to land.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        assert_eq!(
            auto_cancel_reason(&t, "green", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    #[test]
    fn a_merged_train_is_never_auto_cancelled() {
        // The content landed; red post-merge checks are not the
        // consist's problem and its cars must not be released.
        let t = red_train(&["2026-08-13T00:00:00Z"], true);
        assert_eq!(
            auto_cancel_reason(&t, "failing", ts("2026-08-13T09:00:00Z"), 6),
            None
        );
    }

    // -- honouring the operator's cancel request ---------------------------
    //
    // The yard's cancel button (7a24caf3): an operator stamps
    // `cancel_requested` on the train's metadata and reconcile honours
    // it — unstruck, and never on a train that already merged.

    fn requested_train(cancel_requested: serde_json::Value, merged: bool) -> serde_json::Value {
        let mut t = red_train(&[], merged);
        t["metadata"] = json!({ "cancel_requested": cancel_requested });
        t
    }

    #[test]
    fn an_operators_cancel_request_carries_its_reason_and_actor() {
        let t = requested_train(
            json!({"by": "emp-david", "reason": "bad consist", "at": "2026-09-07T01:00:00Z"}),
            false,
        );
        assert_eq!(
            operator_cancel_reason(&t).as_deref(),
            Some("operator cancel: bad consist (by emp-david)")
        );
        assert_eq!(
            operator_cancel_refusal(&t),
            None,
            "an honoured request is not also refused"
        );
    }

    #[test]
    fn no_stamp_is_no_request() {
        assert_eq!(operator_cancel_reason(&red_train(&[], false)), None);
    }

    #[test]
    fn a_request_without_a_reason_or_an_actor_is_not_honoured() {
        // The reason lands on every released car as its skip_reason; a
        // cancel that cannot say why is not one the conductor acts on.
        let t = requested_train(json!({"by": "emp-david", "reason": "  "}), false);
        assert_eq!(operator_cancel_reason(&t), None);
        let t = requested_train(json!({"reason": "bad consist"}), false);
        assert_eq!(operator_cancel_reason(&t), None, "no actor, no request");
    }

    #[test]
    fn a_malformed_stamp_is_not_a_request() {
        let t = requested_train(json!("please cancel"), false);
        assert_eq!(operator_cancel_reason(&t), None);
        assert_eq!(operator_cancel_refusal(&t), None);
    }

    #[test]
    fn a_merged_train_refuses_the_request_once_instead_of_releasing_its_cars() {
        let mut t = requested_train(json!({"by": "emp-david", "reason": "too late"}), true);
        t["steps"][0]["metadata"]["merge_ref"] = json!("abc1234def56");
        assert_eq!(operator_cancel_reason(&t), None, "the content landed");
        assert_eq!(
            operator_cancel_refusal(&t).as_deref(),
            Some("already merged at abc1234def56")
        );
        // Stamped once: a refused train does not re-refuse every pass.
        t["metadata"]["cancel_refused"] = json!("already merged at abc1234def56");
        assert_eq!(operator_cancel_refusal(&t), None);
    }

    // -- a stall is not a red train ----------------------------------------
    //
    // 2026-08-22: two trains stalled through infrastructure incidents.
    // Their runs were cancelled mid-flight, never judging anything, and
    // the conductor read that as red — four innocent cars took a strike
    // each aboard both trains, hit the two-strike hold, and sat through
    // five departures until a human noticed. All four test-merged clean.

    #[test]
    fn a_train_whose_run_was_aborted_still_releases_its_consist() {
        // The release is right — the cars should not be held hostage
        // overnight by a run that will never answer.
        let t = red_train(&["2026-08-13T00:00:00Z"], false);
        let r = auto_cancel_reason(&t, "aborted", ts("2026-08-13T08:00:00Z"), 6)
            .expect("aborted and 8h stalled should release the consist");
        assert!(r.contains("8h"), "the reason carries the age: {r}");
        assert!(
            r.contains("no verdict"),
            "the reason must name the stall, not imply a judgment: {r}"
        );
    }

    #[test]
    fn an_infrastructure_refusal_strikes_no_car() {
        // The locomotive refused before any check ran (train #204,
        // 2026-09-05: 65GB free on the forge, need 70GB) and said so on
        // its commit status. Nothing judged the cars.
        let refused = json!([
            {"context": "CI / build-image (pull_request)", "conclusion": "SUCCESS", "description": ""},
            {"context": "CI / locomotive refusal", "conclusion": "FAILURE",
             "description": "refused: 65GB free on the workspace filesystem, need 70GB"},
        ]);
        assert!(!verdict_strikes_cars("failing", Some(&refused)));
        // A judged red strikes.
        let real_red = json!([
            {"context": "CI / test (pull_request)", "conclusion": "FAILURE", "description": "3 checks failed"},
        ]);
        assert!(verdict_strikes_cars("failing", Some(&real_red)));
        // No description is not a refusal claim.
        let bare = json!([{"context": "CI / test (pull_request)", "conclusion": "FAILURE"}]);
        assert!(
            verdict_strikes_cars("failing", Some(&bare)),
            "no description is not a refusal claim"
        );
        // The word on a PASSING check proves nothing about the failing one.
        let green_mentions = json!([
            {"context": "CI / fast (pull_request)", "conclusion": "SUCCESS", "description": "refused nothing"},
            {"context": "CI / web (pull_request)", "conclusion": "FAILURE", "description": "svelte-check"},
        ]);
        assert!(verdict_strikes_cars("failing", Some(&green_mentions)));
        // Only a failing verdict can strike at all.
        assert!(!verdict_strikes_cars("aborted", Some(&refused)));
    }

    /// THE REFUSAL WITHOUT THE NETWORK (c186d63d part 1). The sparing
    /// rested on locomotive.sh's best-effort status POST: with no token,
    /// no GITHUB_SHA, or a rejected POST, its own log said "the conductor
    /// will read this as a plain red" and the strike happened anyway —
    /// a network write at the moment the host is already in trouble.
    /// But the locomotive declares every refusal in its OWN log first
    /// (`LOCOMOTIVE RED: <why>`), and the conductor already attaches the
    /// failing job's log tail to the rollup entry for the alert. The
    /// intent is where another actor can read it; read it there.
    #[test]
    fn a_refusal_declared_in_the_log_spares_the_cars_when_the_status_post_failed() {
        let post_failed = json!([
            {"context": "CI / locomotive (pull_request)", "conclusion": "FAILURE", "description": "",
             "log_tail": "locomotive: nproc=16 loadavg=0.4 stamp=ok free=61GB\n\
                          LOCOMOTIVE RED: 61GB free on the workspace filesystem, need 70GB.\n\
                          locomotive: no FORGE_TOKEN/GITHUB_SHA/GITHUB_REPOSITORY in the environment — refusal not posted, the conductor will read a plain red\n"},
        ]);
        assert!(
            !verdict_strikes_cars("failing", Some(&post_failed)),
            "the locomotive's own log declares the refusal; the cars are spared without the status"
        );
        // A tail that lost the RED line but kept the locomotive's last
        // words still reads as a refusal.
        let tail_only = json!([
            {"context": "CI / locomotive (pull_request)", "conclusion": "FAILURE", "description": "",
             "log_tail": "locomotive: could not post the refusal status — the conductor will read this as a plain red\n"},
        ]);
        assert!(!verdict_strikes_cars("failing", Some(&tail_only)));
        // A judged red whose log merely CONTAINS the word is still a red.
        let real_red = json!([
            {"context": "CI / test (pull_request)", "conclusion": "FAILURE", "description": "3 checks failed",
             "log_tail": "test refused_because_x ... FAILED\nthread panicked: assertion failed\n"},
        ]);
        assert!(verdict_strikes_cars("failing", Some(&real_red)));
        // The marker on a PASSING entry's log proves nothing.
        let green_log = json!([
            {"context": "CI / fast (pull_request)", "conclusion": "SUCCESS", "description": "", "log_tail": "LOCOMOTIVE RED: nope\n"},
            {"context": "CI / test (pull_request)", "conclusion": "FAILURE", "description": "2 failed"},
        ]);
        assert!(verdict_strikes_cars("failing", Some(&green_log)));
    }

    #[test]
    fn an_aborted_train_leaves_its_cars_unstruck() {
        // The strike is what was wrong. Nothing judged these cars.
        assert!(!verdict_strikes_cars("aborted", None));
        let car = json!({"id": "car-1", "metadata": {"red_trains": 1}});
        let stamps = release_stamps(&car, "CI aborted without a verdict", false);
        assert!(
            !stamps.iter().any(|(k, _)| *k == "red_trains"),
            "a stalled train must not touch the strike count"
        );
        // Released all the same: the train marker goes, so it boards again.
        assert_eq!(
            stamps.iter().find(|(k, _)| *k == "train").map(|(_, v)| v),
            Some(&Value::Null)
        );
    }

    #[test]
    fn a_genuinely_red_train_still_strikes_its_cars() {
        // The two-strike hold has to keep working — without it the
        // auto-cancel is a loop that burns CI all night.
        assert!(verdict_strikes_cars("failing", None));
        let car = json!({"id": "car-1", "metadata": {"red_trains": 1}});
        let stamps = release_stamps(&car, "CI red", true);
        assert_eq!(
            stamps
                .iter()
                .find(|(k, _)| *k == "red_trains")
                .map(|(_, v)| v),
            Some(&json!(2)),
            "a red release counts against every car aboard"
        );
    }

    #[test]
    fn only_a_failing_verdict_strikes() {
        // Neither silence nor success is a strike.
        assert!(!verdict_strikes_cars("pending", None));
        assert!(!verdict_strikes_cars("green", None));
    }

    // -- the CI verdict blind spot -----------------------------------------

    #[test]
    fn a_verdict_that_moves_after_recording_is_reported() {
        // The 2026-08-15 case: recorded failing, repaired, red again.
        // Nothing in the system said so for 45 minutes.
        assert!(verdict_drift(Some("failing"), "green").is_some());
        let note = verdict_drift(Some("green"), "failing").expect("green -> failing is a change");
        assert!(note.contains("green"), "the note names where it came from");
        assert!(note.contains("failing"), "and where it went");
    }

    #[test]
    fn an_unchanged_verdict_is_silent() {
        // Reconcile runs every ten minutes; a verdict that has not moved
        // must not produce a line each time or the signal is noise.
        assert_eq!(verdict_drift(Some("failing"), "failing"), None);
        assert_eq!(verdict_drift(Some("green"), "green"), None);
    }

    #[test]
    fn pending_is_not_a_change() {
        // A re-run passes through pending on its way to an answer.
        // Reporting it would fire on every repair, twice.
        assert_eq!(verdict_drift(Some("failing"), "pending"), None);
    }

    #[test]
    fn nothing_recorded_yet_is_not_drift() {
        // Before the step completes, the ordinary path records the
        // first verdict; this is only about the ones after it.
        assert_eq!(verdict_drift(None, "failing"), None);
    }

    #[test]
    fn ci_that_never_answers_is_reported_after_the_threshold() {
        // The case drift cannot see: no verdict at all, so there is
        // nothing to compare against.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"completed",
             "metadata":{"completed_at":"2026-08-15T06:00:00Z"}},
            {"spec_slug":"ci","title":"CI verdict","status":"ready","metadata":{}}
        ]});
        assert!(ci_overdue(&t, ts("2026-08-15T08:00:00Z"), 2).is_some());
        assert_eq!(ci_overdue(&t, ts("2026-08-15T07:30:00Z"), 2), None);
    }

    #[test]
    fn an_answered_ci_is_never_overdue() {
        // Red counts as answered. A red train is the stall sentinel's
        // problem and auto-cancel's; this signal is only about silence.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"completed",
             "metadata":{"completed_at":"2026-08-15T06:00:00Z"}},
            {"spec_slug":"ci","title":"CI verdict","status":"completed",
             "metadata":{"result":"failing","completed_at":"2026-08-15T06:20:00Z"}}
        ]});
        assert_eq!(ci_overdue(&t, ts("2026-08-15T20:00:00Z"), 2), None);
    }

    #[test]
    fn a_train_with_no_pr_yet_is_not_overdue() {
        // Nothing has been asked, so nothing is unanswered — a train
        // stuck before its PR belongs to the stall sentinel.
        let t = json!({"id":"t-1","status":"open","metadata":{},"steps":[
            {"spec_slug":"pr","title":"Open the batched PR","status":"ready","metadata":{}},
            {"spec_slug":"ci","title":"CI verdict","status":"pending","metadata":{}}
        ]});
        assert_eq!(ci_overdue(&t, ts("2026-08-16T00:00:00Z"), 2), None);
    }

    // -- the silent decline ------------------------------------------------

    #[test]
    fn a_mergeable_train_the_conductor_declines_to_merge_says_so() {
        // The 2026-09-04 case: green CI, an OPEN PR, and a reconcile run
        // by hand — so BOSS_TRAIN_AUTO_MERGE, which only the conductor's
        // unit sets, was absent. The train was not merged and nothing was
        // logged; two passes read as successful.
        let why = merge_declined_reason(false, "green", Some("OPEN"))
            .expect("green + OPEN + auto-merge off is a decline, not a no-op");
        assert!(
            why.contains("BOSS_TRAIN_AUTO_MERGE"),
            "the reason names the switch that is off: {why}"
        );
        assert!(
            why.contains("unit"),
            "and why a hand-run verb does not have it: {why}"
        );
    }

    #[test]
    fn a_train_the_conductor_does_merge_is_not_a_decline() {
        // The configured conductor merges; the merge itself is the line.
        assert_eq!(merge_declined_reason(true, "green", Some("OPEN")), None);
    }

    #[test]
    fn ordinary_states_are_not_declines() {
        // Reconcile runs every ten minutes over every open train. A train
        // whose CI has not answered, or that is red, or that already
        // landed, is not being declined anything — reporting those here
        // would be a line per train per pass.
        assert_eq!(merge_declined_reason(false, "pending", Some("OPEN")), None);
        assert_eq!(merge_declined_reason(false, "failing", Some("OPEN")), None);
        assert_eq!(merge_declined_reason(false, "green", Some("MERGED")), None);
        assert_eq!(merge_declined_reason(false, "green", Some("CLOSED")), None);
        assert_eq!(merge_declined_reason(false, "green", None), None);
    }

    // -- the two-strike hold -----------------------------------------------

    #[test]
    fn a_car_that_took_two_trains_red_is_held() {
        let car = json!({"id": "car-1", "metadata": {"red_trains": 2}});
        assert!(car_hold_reason(&car, policy().max_red_trains).is_some());
    }

    #[test]
    fn a_car_with_one_red_still_boards() {
        // One red is usually a neighbour's fault — holding on the first
        // would quarantine innocent cars and stall the queue.
        let car = json!({"id": "car-1", "metadata": {"red_trains": 1}});
        assert_eq!(car_hold_reason(&car, policy().max_red_trains), None);
        let fresh = json!({"id": "car-2", "metadata": {}});
        assert_eq!(car_hold_reason(&fresh, policy().max_red_trains), None);
    }

    /// The hold count is DATA now, and this is what that buys: raising
    /// it in the registry lets a car that two reds would have held keep
    /// boarding, with no code change and no train. The decision function
    /// itself never changed — it always took the threshold as an
    /// argument; what changed is where the argument comes from.
    #[test]
    fn the_hold_moves_when_the_policy_says_a_different_number() {
        let lenient = DeliveryPolicy {
            max_red_trains: 3,
            ..policy()
        };
        let two_reds = json!({"id": "car-1", "metadata": {"red_trains": 2}});
        assert_eq!(car_hold_reason(&two_reds, lenient.max_red_trains), None);

        let strict = DeliveryPolicy {
            max_red_trains: 1,
            ..policy()
        };
        let one_red = json!({"id": "car-2", "metadata": {"red_trains": 1}});
        assert!(car_hold_reason(&one_red, strict.max_red_trains).is_some());
    }

    // -- cancelling a train ------------------------------------------------

    #[test]
    fn cancel_releases_only_the_still_open_cars() {
        let open = json!({"id": "car-1", "status": "open",
                          "metadata": {"train": "t-1", "branch": "feat/x"}});
        let landed = landed_car("car-2", "feat/y");
        let mut cancelled = landed_car("car-3", "feat/z");
        cancelled["status"] = json!("cancelled");
        let cars = vec![open, landed, cancelled];
        let released: Vec<&str> = releasable_cars(&cars, "t-1")
            .iter()
            .map(|c| c.get("id").and_then(Value::as_str).unwrap())
            .collect();
        // Closed cars are history — merged or abandoned, not ours to
        // touch. Only the open car returns to the dock.
        assert_eq!(released, vec!["car-1"]);
    }

    /// A CANCEL MUST NOT STRIP A CAR OFF A DIFFERENT, LIVE TRAIN.
    ///
    /// The train's `boarded_jobs` is written once at boarding and never
    /// updated when a car is released, so a long-dead train keeps naming
    /// cars that have since reboarded elsewhere. Cancelling it then
    /// released them again — off a running consist.
    ///
    /// Done on 2026-08-27: cancelling e1de28a3 freed three cars that
    /// were legitimately aboard 1597b4a4, the next board swept them onto
    /// a third train, and two trains believed they carried the same
    /// three cars while the cars named a fourth. The car's own
    /// `metadata.train` is the field `parked_ready` and
    /// `receipt_skip_reason` both read, so it is authoritative; the
    /// train's list is the copy that drifts.
    #[test]
    fn cancel_leaves_a_car_that_has_since_boarded_another_train() {
        let mine = json!({"id": "car-1", "status": "open",
                          "metadata": {"train": "t-1", "branch": "feat/x"}});
        let moved_on = json!({"id": "car-2", "status": "open",
                              "metadata": {"train": "t-2", "branch": "feat/y"}});
        // A car released earlier carries no train at all. It is not ours
        // to re-release, and stamping it again would overwrite a
        // skip_reason that already explains where it has been.
        let already_free = json!({"id": "car-3", "status": "open",
                                  "metadata": {"branch": "feat/z"}});
        let cars = vec![mine, moved_on, already_free];
        let released: Vec<&str> = releasable_cars(&cars, "t-1")
            .iter()
            .map(|c| c.get("id").and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(
            released,
            vec!["car-1"],
            "only the car whose own metadata.train still names this train may be released"
        );
    }

    #[test]
    fn cancel_deletes_only_the_trains_own_branch_never_a_cars() {
        let train = json!({
            "id": "t-1",
            "subject": {"subject_kind": "custom", "id": "train/20260813-0600"},
        });
        assert_eq!(
            train_branch_to_delete(&train),
            Some("train/20260813-0600".to_string())
        );
        // A subject that is not a train/* branch — whatever went
        // wrong upstream, the cancel path deletes NO car branch.
        let odd = json!({
            "id": "t-2",
            "subject": {"subject_kind": "custom", "id": "feat/x"},
        });
        assert_eq!(train_branch_to_delete(&odd), None);
        assert_eq!(train_branch_to_delete(&json!({"id": "t-3"})), None);
    }

    #[test]
    fn the_arrival_cleanup_never_names_a_cars_branch() {
        let mut odd = arrived_train_with_branch();
        odd["subject"] = json!({"subject_kind": "custom", "id": "feat/x"});
        assert_eq!(arrival_branch_to_delete(&odd, "forgejo"), None);
        // The happy case, pure: an arrived record under forgejo names
        // the train's own branch and nothing else.
        assert_eq!(
            arrival_branch_to_delete(&arrived_train_with_branch(), "forgejo"),
            Some("train/20260820-0600".to_string())
        );
    }

    #[test]
    fn the_cleanup_narrates_deletes_and_failures_and_swallows_the_gone() {
        assert_eq!(
            arrival_cleanup_note("train/x", Ok(true)),
            Some("deleted branch train/x (train arrived)".to_string())
        );
        // Already gone says nothing: the sweep revisits an unsettled
        // train every pass, and done work narrated every pass reads
        // as work happening.
        assert_eq!(arrival_cleanup_note("train/x", Ok(false)), None);
        let line = arrival_cleanup_note("train/x", Err(anyhow!("HTTP 500: down")))
            .expect("a failure must be narrated");
        assert!(line.contains("train/x"), "{line}");
        assert!(line.contains("HTTP 500: down"), "{line}");
        assert!(line.contains("arrival stands"), "{line}");
    }

    #[test]
    fn a_cancel_handle_resolves_by_id_prefix_or_pr_url() {
        let a = json!({
            "id": "aaaa1111-2222-3333-4444-555566667777",
            "steps": [{"spec_slug": "pr", "title": "Open the batched PR",
                       "status": "completed",
                       "metadata": {"pr_url": "http://forge/repo/pulls/9"}}],
        });
        let b = json!({"id": "bbbb1111-0000-0000-0000-000000000000", "steps": []});
        let trains = vec![a, b];
        assert_eq!(
            resolve_train(&trains, "aaaa1111-2222-3333-4444-555566667777")
                .unwrap()
                .get("id"),
            trains[0].get("id")
        );
        assert_eq!(
            resolve_train(&trains, "bbbb1111").unwrap().get("id"),
            trains[1].get("id")
        );
        assert_eq!(
            resolve_train(&trains, "http://forge/repo/pulls/9")
                .unwrap()
                .get("id"),
            trains[0].get("id")
        );
        assert!(resolve_train(&trains, "cccc0000").is_err(), "no match");
        // An ambiguous prefix refuses rather than guessing a train.
        let twins = vec![
            json!({"id": "aaaa1111-x", "steps": []}),
            json!({"id": "aaaa1111-y", "steps": []}),
        ];
        assert!(resolve_train(&twins, "aaaa1111").is_err(), "ambiguous");
    }

    // -- the deployed-step evidence -------------------------------------------
    //
    // The conductor deploys nothing: the cluster-deploy-runner on the
    // forge converges the cluster on merge. The `deployed` step is
    // completed with this evidence and nothing else (the tree-backed
    // arm and its deploy-tree switch left on 2026-09-18).

    #[test]
    fn the_no_playground_deploy_evidence_names_the_convergence_path() {
        // The completion evidence the conductor stamps on the `deployed`
        // step. It reads as a COMPLETION (nothing to deploy), not a
        // block, and points at what actually deploys.
        let ev = NO_PLAYGROUND_DEPLOY_EVIDENCE;
        assert!(ev.contains("no playground deploy"), "states the skip: {ev}");
        assert!(
            ev.contains("converges on forge main"),
            "names where the deploy happens instead: {ev}"
        );
        assert!(
            ev.contains("deploy-runner"),
            "names the actor that deploys: {ev}"
        );
        assert!(
            ev.contains("nothing to deploy from the conductor"),
            "reads as a completion, not a block: {ev}"
        );
    }
}
