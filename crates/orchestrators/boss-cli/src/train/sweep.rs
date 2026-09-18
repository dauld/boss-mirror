//! The branch sweep at arrival.

use super::*;

/// One branch a landed car's record makes a claim about, with
/// everything the sweep needs to act on it — so the decision and its
/// evidence travel together instead of the caller re-deriving the
/// evidence from a car id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CarBranch {
    /// The branch on the forge.
    pub(crate) branch: String,
    /// The car whose record proves this branch's content landed.
    pub(crate) car: String,
    /// The head the record says the branch pointed at when the train
    /// carried its content: the boarded head for the car's own branch,
    /// the head recorded at the rerail for a branch it was re-railed
    /// off. `None` = nothing recorded one, and the head guard refuses
    /// (`SweepGuard::NoRecord`) rather than guessing.
    pub(crate) head: Option<String>,
    /// A branch the car was re-railed OFF, rather than the one it
    /// boarded. Only the journal wording differs — the guards do not.
    pub(crate) rerail_origin: bool,
}

/// Every branch ONE car's record makes a landing claim about, in the
/// order the sweep should consider them: the branch it boarded first,
/// then each branch a rerail moved it off, oldest first. Empty unless
/// the car's own bookkeeping completed — closed with the `merged`
/// outcome (an abandoned car closes too, but its branch holds unmerged
/// work; abandonment is a disposition, not a sweep). `main` and
/// unnamed branches drop out here, and a branch named twice appears
/// once.
///
/// THE RERAIL ORIGINAL IS THE SECOND HALF (packet 473fda1b, generator
/// 1). `boss rerail` moves a car to a new branch and leaves the
/// original on the forge; the arriving train deletes the branch it
/// MERGED, which is the new one. The original was never a car, so a
/// sweep that iterates boarded cars had nothing to act on and would
/// never consider it — permanent by construction, and 13 branches deep
/// when it was measured on 2026-09-10. The link is read from the
/// provenance `boss rerail` RECORDS on the car (`rerail_origins`, each
/// entry naming a branch and the head it carried when the car left
/// it), never from a `-rerail` suffix guessed off a name: a name is not
/// a record, and the head is what the guard needs anyway.
fn recorded_branches(car: &Value) -> Vec<(String, Option<String>, bool)> {
    let md = car.get("metadata");
    let landed = car.get("status").and_then(Value::as_str) == Some("closed")
        && md.and_then(|m| m.get("outcome")).and_then(Value::as_str) == Some("merged");
    if !landed {
        return Vec::new();
    }
    let own = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let origins = md
        .and_then(|m| m.get("rerail_origins"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|o| {
            (
                o.get("branch").and_then(Value::as_str).unwrap_or_default(),
                o.get("head")
                    .and_then(Value::as_str)
                    .filter(|h| !h.is_empty())
                    .map(str::to_string),
                true,
            )
        });
    let mut out: Vec<(String, Option<String>, bool)> = Vec::new();
    for (branch, head, origin) in
        std::iter::once((own, boarded_head(car).map(str::to_string), false)).chain(origins)
    {
        if branch.is_empty() || branch == "main" || out.iter().any(|(b, _, _)| b == branch) {
            continue;
        }
        out.push((branch.to_string(), head, origin));
    }
    out
}

/// Every branch of one car, as the sweep's decision record. A car with
/// no id cannot be named in a journal line, so it decides nothing.
fn car_branches(car: &Value) -> Vec<CarBranch> {
    let cid = car
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if cid.is_empty() {
        return Vec::new();
    }
    recorded_branches(car)
        .into_iter()
        .map(|(branch, head, rerail_origin)| CarBranch {
            branch,
            car: cid.clone(),
            head,
            rerail_origin,
        })
        .collect()
}

/// The branch-sweep decision at arrival (protocol decision, David):
/// train PRs squash-merge, so git ancestry can never prove a car's
/// content landed — the JOB RECORD is the proof. Given the cars a
/// landed train boarded and the branches still-open cars name, a
/// branch is deletable iff:
///   - the car's own bookkeeping completed: closed with the `merged`
///     outcome (an abandoned car closes too, but its branch holds
///     unmerged work — never touch it);
///   - the branch is named and is not `main`;
///   - no still-open car rides the same branch (a follow-up car's
///     claim keeps it alive).
/// It holds for the branch the car BOARDED and for every branch a
/// rerail moved it off — one definition, because both are the same
/// question asked of the same record (see `recorded_branches`).
/// Two landed cars naming one branch delete it once. Pure — the
/// forge call and the journal line belong to the caller.
pub(crate) fn deletable_branches(
    boarded_cars: &[Value],
    open_branches: &BTreeSet<String>,
) -> Vec<CarBranch> {
    decided_branches(boarded_cars, |b| !open_branches.contains(b))
}

/// The branch head recorded when this car boarded — stamped by the
/// assembly onto the car Job in the same update that stamps `train`
/// (see `board`). Absent or empty reads as no stamp at all.
/// The gate-receipt spot-check (David's "Agreed" on 742d1faa): what
/// makes a car's green claim honest at the moment it matters — boarding.
/// `None` = the receipt vouches for exactly the head being boarded;
/// `Some(reason)` = leave the car behind, with the reason named on it.
///
/// Catches exactly the two lies that cost red trains: a receipt from a
/// different commit than the branch now points at (gate, then "one more
/// tiny fix" pushed after), and a receipt that was never green (or was
/// taken on a dirty tree, which is the same claim with extra steps —
/// the gate reads the tree live, so dirty means "green about something
/// else"). A car with NO receipt is unverifiable and stays behind too:
/// this check exists because claims without receipts already shipped.
pub(crate) fn receipt_skip_reason(car: &Value, boarding_head: Option<&str>) -> Option<String> {
    // A RE-GATE SUPERSEDES THE ORIGINAL GATE, and is read in preference
    // to it. Filed as user feedback 64cae7e9 after 17 of 34 left-behinds
    // traced to stale receipts: when a branch legitimately moves — a
    // migration renumbered off a collision, a rebase onto a main that had
    // moved into the same file — the receipt correctly stops vouching for
    // the head, and the car was then UNREPAIRABLE. Completed steps are
    // immutable, so the only recourse was to abandon the packet and park
    // a fresh one, losing the car's history and costing a packet every
    // time. That happened twice more on 2026-08-28, to cars 4e78035e and
    // 8b831c5c, which is what moved this from a filed opinion to a fix.
    //
    // IT LIVES IN JOB METADATA, NOT IN A NEW STEP, and that is a
    // deliberate retreat from the shape the feedback proposed. A `regate`
    // STEP was built and validated clean, then abandoned: `blocked_by` is
    // derived from every step a predicate REFERENCES, and a referenced
    // step that is merely pending makes the API refuse to complete the
    // referring step (the defect in feedback 1538e93a). Because a regate
    // step must key on `job.metadata.skip_reason` to appear only when the
    // conductor has left the car behind, and a predicate referencing
    // job.metadata never SKIPS, it would sit pending forever on every
    // healthy car — and anything referencing it, `review` included, would
    // be blocked from completing. That is the conductor unable to board
    // anything. The engine cannot express an optional repair step safely
    // today; job metadata can, so the repair uses what works and the
    // step is filed as protocol work behind 1538e93a.
    let regate = car
        .get("metadata")
        .and_then(|m| m.get("regate_receipt"))
        .filter(|v| !v.is_null());
    let md_owned;
    let md: &Value = match regate {
        Some(r) => {
            md_owned = json!({ "receipt": r });
            &md_owned
        }
        None => {
            let gate = find_step(car, "gate", "Green, and observed working")?;
            gate.get("metadata")?
        }
    };
    // Present as a JSON string (how the gate step records it) or as an
    // object (tooling that parses before writing) — both are receipts.
    let receipt: Value = match md.get("receipt") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v @ Value::Object(_)) => v.clone(),
        _ => Value::Null,
    };
    if receipt.is_null() {
        return Some(
            "no machine receipt on the gate step — the green claim is unverifiable".into(),
        );
    }
    let verdict = receipt
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("?");
    if verdict != "green" {
        return Some(format!("gate receipt verdict is '{verdict}', not green"));
    }
    if receipt.get("dirty").and_then(Value::as_bool) == Some(true) {
        return Some(
            "gate receipt was taken on a dirty tree — it vouches for something else".into(),
        );
    }
    let receipt_head = receipt.get("head").and_then(Value::as_str).unwrap_or("");
    if receipt_head.is_empty() {
        return Some("gate receipt names no head — the green claim is unverifiable".into());
    }
    match boarding_head {
        Some(b) if commits_match(receipt_head, b) => None,
        Some(b) => Some(format!(
            "gate receipt is for {} but the branch boards {} — gated, then changed",
            &receipt_head[..receipt_head.len().min(8)],
            &b[..b.len().min(8)]
        )),
        // No boardable head resolved — the branch checks after this
        // will name that failure themselves; the receipt is not the
        // lie here.
        None => None,
    }
}

pub(crate) fn boarded_head(car: &Value) -> Option<&str> {
    car.get("metadata")?
        .get("boarded_head")?
        .as_str()
        .filter(|s| !s.is_empty())
}

/// The sweep's second question, and the answers to it.
///
/// `deletable_branches` asks whether the job record proves the car's
/// CONTENT landed. It does not — it cannot — prove the branch still
/// holds only that content. Car 23923b40's known_gap is what the gap
/// costs: `fix/conductor-hardening` boarded at fc55e4d, two more
/// commits were pushed to the branch AFTER boarding, the train landed
/// carrying only the boarded ones, and the sweep deleted the branch
/// on a job record that was entirely correct. The unmerged commits
/// went with it.
///
/// So the sweep now deletes only what it can prove it carried: the
/// head recorded at ASSEMBLY time must still be the branch's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepGuard {
    /// The branch still points at exactly what boarded.
    Delete,
    /// Commits arrived after boarding — the train never carried them,
    /// and they live nowhere else.
    Moved { recorded: String, current: String },
    /// The branch EXISTS and no head is on the record (a car that
    /// boarded before the conductor recorded one). An unknown head is
    /// not evidence: the cost of keeping a stale branch is a stale
    /// branch; the cost of deleting a moved one is lost work.
    NoRecord,
    /// The branch is not on the forge — nothing left to sweep.
    Gone,
}

/// The head-guard decision, pure. Both shas are full 40-char heads —
/// the assembly records what `git rev-parse` merged, the guard reads
/// what the forge names now — so equality is the whole test.
///
/// The forge's answer is read FIRST, and an absent branch settles the
/// question whatever the record says. Ordering the record first
/// conflates "we cannot vouch for this branch" with "there is no such
/// branch", and the second is not a finding: nothing to delete,
/// nothing to rescue, nothing an operator can do. Job 1bd1fb3d is the
/// bill — every car that boarded before this guard existed has
/// neither a recorded head nor a surviving branch, so the record-first
/// order made each one a `NoRecord` line on every reconcile, forever.
///
/// The reorder is free: `branch_head` was already called
/// unconditionally for every deletable branch, so the sweep asks the
/// forge exactly as often as it did before.
pub(crate) fn sweep_guard(recorded: Option<&str>, current: Option<&str>) -> SweepGuard {
    let recorded = recorded.filter(|s| !s.is_empty());
    let current = current.filter(|s| !s.is_empty());
    match (recorded, current) {
        (_, None) => SweepGuard::Gone,
        (None, Some(_)) => SweepGuard::NoRecord,
        (Some(r), Some(c)) if r == c => SweepGuard::Delete,
        (Some(r), Some(c)) => SweepGuard::Moved {
            recorded: r.to_string(),
            current: c.to_string(),
        },
    }
}

/// The journal line a guard verdict earns — `None` when it earns
/// none. Pure, so "what does the operator hear" is a decision with a
/// test rather than a shape buried in the sweep loop.
///
/// The sweep's journal is an operator surface, and a line belongs
/// there only when a human could act on it. `Gone` is not that: the
/// branch is not on the forge, so there is nothing to delete and
/// nothing to rescue. Job 1bd1fb3d is the cost of getting this wrong
/// — every car that boarded before the head guard existed has no
/// recorded head and no surviving branch, and narrating that pair put
/// dozens of lines in every reconcile, forever, about branches swept
/// by hand hours earlier.
///
/// `Delete` is silent here too, but for the opposite reason: the
/// caller does the deleting and is the only one who knows whether it
/// was a dry run, a deletion, or a race lost to something faster.
pub(crate) fn sweep_note(guard: &SweepGuard, b: &CarBranch) -> Option<String> {
    match guard {
        SweepGuard::Gone | SweepGuard::Delete => None,
        SweepGuard::NoRecord if b.rerail_origin => Some(format!(
            "rerail original {} has no head on record — not deleting (car {} landed)",
            b.branch,
            id8(&b.car)
        )),
        SweepGuard::NoRecord => Some(format!(
            "branch {} has no boarded head on record — not deleting (car {} landed)",
            b.branch,
            id8(&b.car)
        )),
        SweepGuard::Moved { recorded, current } => Some(branch_moved_line(b, recorded, current)),
    }
}

/// The line the sweep journals when a branch outgrew its boarding —
/// operator surface, and the only notice that unmerged commits are
/// sitting on a branch the train did not carry.
pub(crate) fn branch_moved_line(b: &CarBranch, recorded: &str, current: &str) -> String {
    let since = if b.rerail_origin {
        "rerail original"
    } else {
        "branch"
    };
    let what = if b.rerail_origin {
        "moved since the rerail"
    } else {
        "moved since boarding"
    };
    format!(
        "{since} {} {what} ({} -> {}) — not deleting",
        b.branch,
        id8(recorded),
        id8(current)
    )
}

/// The journal line one train earns when its sweep fails mid-flight —
/// a boarded car fetched and 404'd (the Job was deleted), a malformed
/// arrival-report write, a forge blip. Pure and named for the same
/// reason reconcile's per-train isolation names its train: the sweep
/// is best-effort per train, and best-effort without a named line is
/// just silent. An abort here USED to take the whole sweep down on a
/// `?`, so every later pending train went unswept and its landed
/// branch piled up on the forge (disk debt).
pub(crate) fn sweep_train_failed_line(train: &str, err: &anyhow::Error) -> String {
    format!(
        "sweep: train {} failed this pass — isolated, other trains continue: {err}",
        id8(train)
    )
}

/// The journal line one un-sweepable branch earns inside an otherwise
/// healthy train — a forge blip on `branch_head`/`delete_branch`, say.
/// Isolated per-branch so a single bad branch cannot strand the
/// train's OTHER landed branches on the forge; the train stays
/// unstamped so the branch is revisited next pass rather than leaked.
pub(crate) fn sweep_branch_failed_line(branch: &str, car: &str, err: &anyhow::Error) -> String {
    format!(
        "sweep: branch {branch} (car {}) failed — isolated, other branches continue: {err}",
        id8(car)
    )
}

/// A train's sweep is settled once every boarded car has reached a
/// terminal status — each branch is then deleted, deliberately kept
/// (main / a still-open car's claim), or the car never landed and
/// its branch outlives the train. A car still open keeps the train
/// on the sweep list for the next reconcile.
pub(crate) fn sweep_settled(boarded_cars: &[Value]) -> bool {
    boarded_cars.iter().all(|car| {
        matches!(
            car.get("status").and_then(Value::as_str),
            Some("closed") | Some("cancelled")
        )
    })
}

/// The closed trains the sweep still owes a visit: a train that
/// boarded something and carries no `branches_swept` stamp. Swept
/// trains and cancelled ones (nothing boarded, so no car branch to
/// delete) drop out here, fetch-free — the list rows carry metadata,
/// so this costs no per-car reads.
///
/// Pure, and separate from the read, because WHICH trains are pending
/// and HOW MANY rows the read gathered are different questions. The
/// leak this file was filed for came from answering the second one
/// with `limit=50`: cancelled trains close about once a minute when
/// the consist check is refusing, so a 50-row window turns over in
/// under an hour and a train whose car is proven later than that was
/// never looked at again.
pub(crate) fn sweep_pending(closed_trains: &[Value]) -> Vec<&Value> {
    closed_trains
        .iter()
        .filter(|t| {
            let md = t.get("metadata");
            !truthy(md.and_then(|m| m.get("branches_swept")))
                && truthy(md.and_then(|m| m.get("boarded_jobs")))
        })
        .collect()
}

/// Branches this pass withheld ONLY because a still-open car claims
/// the same name — the deferral `deletable_branches` makes when a
/// follow-up car rides a branch a landed car already used.
///
/// The deferral is right (the open car's work is unmerged) but it is
/// not final: the claim lifts the moment that car reaches a terminal,
/// and the branch becomes deletable then. So a deferred branch must
/// keep its train UNSTAMPED — stamping over it marks the train done
/// sweeping while one of its branches can still become deletable, and
/// the branch leaks for good. Same failure the `branch_failures`
/// guard exists to stop, reached by the other door.
pub(crate) fn claim_deferred_branches(
    boarded_cars: &[Value],
    open_branches: &BTreeSet<String>,
) -> Vec<CarBranch> {
    decided_branches(boarded_cars, |b| open_branches.contains(b))
}

/// The two decisions above differ in one predicate — whether a live
/// car's claim on the name is what we are looking for — so they share
/// the enumeration. Collapsed rather than copied: the copy is how the
/// deferral loop came to know about a car's boarded branch and not
/// about the branches it was re-railed off, which is the other half of
/// the leak this fix closes (CLAUDE.md §9a).
fn decided_branches(boarded_cars: &[Value], wanted: impl Fn(&str) -> bool) -> Vec<CarBranch> {
    let mut out: Vec<CarBranch> = Vec::new();
    for car in boarded_cars {
        for b in car_branches(car) {
            if !wanted(&b.branch) || out.iter().any(|o| o.branch == b.branch) {
                continue;
            }
            out.push(b);
        }
    }
    out
}

/// THE LEAK GUARD, in one predicate: a train may be stamped
/// `branches_swept` only when nothing it carries can still become
/// deletable. Three ways that is false, and each keeps the train on
/// the pending list for the next reconcile instead:
///   - a branch failed to sweep this pass (forge blip, 404 at
///     `get_job`) — revisit it;
///   - a branch was deferred to a still-open car's claim — the claim
///     lifts when that car closes;
///   - a boarded car has not reached a terminal — its branch becomes
///     deletable if it lands.
/// The stamp is what drops the train off the list, so stamping early
/// is not "a pass skipped", it is a branch left on the forge forever.
pub(crate) fn sweep_complete(
    branch_failures: usize,
    claim_deferred: usize,
    boarded_cars: &[Value],
) -> bool {
    branch_failures == 0 && claim_deferred == 0 && sweep_settled(boarded_cars)
}

/// A branch held back for a still-open car's claim says so: without a
/// line, an operator sees the same train re-swept every ten minutes
/// with nothing to show for it and no reason named.
pub(crate) fn claim_deferred_line(b: &CarBranch) -> String {
    format!(
        "sweep: {} kept — a still-open car claims it (car {} landed); train stays pending",
        sweep_subject(b),
        id8(&b.car)
    )
}

/// What the journal calls a branch the sweep acted on. A rerail
/// original is named as one: it was never a car of its own, which is
/// exactly why no earlier sweep could see it, and an operator reading
/// "deleted branch feat/x" for a branch no car ever carried has to go
/// re-derive where the deletion came from (§Diagnosis — a verdict must
/// name what it acted on).
pub(crate) fn sweep_subject(b: &CarBranch) -> String {
    if b.rerail_origin {
        format!("rerail original {}", b.branch)
    } else {
        format!("branch {}", b.branch)
    }
}

/// What a delete actually did, as OBSERVED — the forge's answer to
/// DELETE read against a `branch_head` taken afterwards. The answer
/// alone is a claim: 2xx says "deleted", 404 says "already gone", and
/// on 2026-09-11 six landed cars' branches were on the forge the next
/// morning under trains stamped `branches_swept`, with every condition
/// the sweep requires met — so the forge said one of those two things
/// and the branch stayed (backlog 1096b1a4). The merge is observed,
/// never assumed; so is the delete. `StillPresent` is a branch
/// failure: the train stays pending and the record names what the
/// forge said, so the next such morning is a one-read diagnosis
/// instead of a journal nobody outside the cluster can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepDelete {
    /// DELETE answered success and the branch is gone.
    Deleted,
    /// DELETE answered "already gone" and the branch is gone.
    AlreadyGone,
    /// The branch is still on the forge after DELETE answered.
    StillPresent { forge_said: &'static str },
}

pub(crate) fn sweep_delete_verdict(claimed_deleted: bool, after: Option<&str>) -> SweepDelete {
    match (claimed_deleted, after.filter(|h| !h.is_empty())) {
        (true, None) => SweepDelete::Deleted,
        (false, None) => SweepDelete::AlreadyGone,
        (true, Some(_)) => SweepDelete::StillPresent {
            forge_said: "deleted",
        },
        (false, Some(_)) => SweepDelete::StillPresent {
            forge_said: "already gone",
        },
    }
}

/// One row of the train's `sweep_report`: the record of what the sweep
/// decided and observed for one branch, this pass. Stamped onto the
/// train beside `branches_swept` so the stamp carries its evidence.
pub(crate) fn sweep_report_row(b: &CarBranch, outcome: &str) -> Value {
    json!({
        "branch": b.branch,
        "car": id8(&b.car),
        "rerail_origin": b.rerail_origin,
        "outcome": outcome,
    })
}

/// The rows for boarded cars that are NOT terminal yet — the reason a
/// train stays pending that `deletable_branches` cannot name, because
/// an open car decides nothing. Seen live on the first report
/// (2026-09-12, train 17:48): four rows said "gone before this pass"
/// and the train sat unstamped with no row for the fifth car, still at
/// `proven`; a reader had to infer it. Each open car's branch gets a
/// row saying which step holds it, so an unstamped train explains
/// itself completely.
pub(crate) fn unsettled_rows(boarded_cars: &[Value]) -> Vec<Value> {
    boarded_cars
        .iter()
        .filter(|car| {
            !matches!(
                car.get("status").and_then(Value::as_str),
                Some("closed") | Some("cancelled")
            )
        })
        .filter_map(|car| {
            let branch = car.pointer("/metadata/branch").and_then(Value::as_str)?;
            let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
            let at = car
                .get("steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|s| {
                    matches!(
                        s.get("status").and_then(Value::as_str),
                        Some("ready") | Some("active")
                    )
                })
                .and_then(|s| s.get("spec_slug").or_else(|| s.get("title")))
                .and_then(Value::as_str)
                .unwrap_or("?");
            Some(json!({
                "branch": branch,
                "car": id8(id),
                "rerail_origin": false,
                "outcome": format!(
                    "kept: car still open at `{at}` — the train stays pending until it closes"
                ),
            }))
        })
        .collect()
}

/// The journal line for a delete the forge answered but did not
/// perform — the only kind of sweep line that must be loud.
pub(crate) fn sweep_still_present_line(b: &CarBranch, forge_said: &str, head: &str) -> String {
    format!(
        "sweep: {} STILL ON THE FORGE at {} after DELETE answered \"{forge_said}\" — not swept; train stays pending (car {} landed)",
        sweep_subject(b),
        &head[..head.len().min(8)],
        id8(&b.car)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::*;

    // -- the branch-sweep decision at arrival ------------------------------
    //
    // Train PRs squash-merge, so git ancestry can never prove a car's
    // content landed; the JOB RECORD is the proof (protocol decision,
    // David). These pin exactly which branches the conductor may
    // delete once a train has arrived.

    fn no_open() -> BTreeSet<String> {
        BTreeSet::new()
    }

    /// The branch names a sweep decision offers, in order.
    fn names(decided: &[CarBranch]) -> Vec<&str> {
        decided.iter().map(|b| b.branch.as_str()).collect()
    }

    #[test]
    fn a_landed_cars_branch_is_deletable() {
        let cars = vec![landed_car("car-1", "feat/x")];
        let decided = deletable_branches(&cars, &no_open());
        assert_eq!(names(&decided), vec!["feat/x"]);
        assert_eq!(decided[0].car, "car-1");
        assert!(
            !decided[0].rerail_origin,
            "the car's own branch is not a rerail original"
        );
    }

    #[test]
    fn a_car_still_open_keeps_its_branch() {
        // Bookkeeping incomplete — the dispatcher has not closed the
        // car yet, whatever the train did.
        let mut car = landed_car("car-1", "feat/x");
        car["status"] = json!("open");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn an_abandoned_car_keeps_its_branch() {
        // Abandoned cars close too — but their branch holds unmerged
        // work. Only the `merged` outcome is landing evidence.
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"]["outcome"] = json!("abandoned");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn a_closed_car_without_an_outcome_keeps_its_branch() {
        // Closed by hand, no terminal outcome on the record: not proof.
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"].as_object_mut().unwrap().remove("outcome");
        assert!(deletable_branches(&[car], &no_open()).is_empty());
    }

    #[test]
    fn main_is_never_deletable() {
        let cars = vec![landed_car("car-1", "main")];
        assert!(deletable_branches(&cars, &no_open()).is_empty());
    }

    #[test]
    fn a_branch_a_still_open_car_names_survives() {
        // A follow-up car may ride a landed car's branch; the open
        // car's claim wins.
        let open: BTreeSet<String> = ["feat/x".to_string()].into();
        let cars = vec![landed_car("car-1", "feat/x")];
        assert!(deletable_branches(&cars, &open).is_empty());
    }

    #[test]
    fn a_car_without_a_branch_contributes_nothing() {
        let empty = landed_car("car-1", "");
        assert!(deletable_branches(&[empty], &no_open()).is_empty());
        let mut none = landed_car("car-2", "feat/x");
        none["metadata"] = json!({"outcome": "merged"});
        assert!(deletable_branches(&[none], &no_open()).is_empty());
    }

    #[test]
    fn two_landed_cars_on_one_branch_delete_it_once() {
        let cars = vec![landed_car("car-1", "feat/x"), landed_car("car-2", "feat/x")];
        let decided = deletable_branches(&cars, &no_open());
        assert_eq!(names(&decided), vec!["feat/x"]);
        assert_eq!(decided[0].car, "car-1", "the first car named it");
    }

    // -- the rerail original (packet 473fda1b, generator 1) ---------------
    //
    // `boss rerail` moves a car to a NEW branch and leaves the original
    // on the forge. The train merges and deletes the branch it carried
    // — the rerail one — and the original was never a car, so a sweep
    // that iterates boarded cars has nothing to act on and will never
    // consider it: permanent by construction, 13 branches deep by
    // 2026-09-10. The fix reads the provenance the rerail RECORDED on
    // the car (`rerail_origins`), never a `-rerail` suffix guessed off
    // a name.

    /// The record `boss rerail` leaves: the car now rides `branch`, and
    /// each branch it was re-railed off is named with the head that
    /// branch carried at the moment the car left it.
    fn rerailed_car(id: &str, branch: &str, origins: &[(&str, &str)]) -> Value {
        let mut car = landed_car(id, branch);
        car["metadata"]["boarded_head"] = json!(format!("head-of-{branch}"));
        car["metadata"]["rerail_origins"] = json!(
            origins
                .iter()
                .map(|(b, h)| json!({"branch": b, "head": h}))
                .collect::<Vec<_>>()
        );
        car
    }

    #[test]
    fn a_landed_rerail_cars_original_branch_is_deletable_too() {
        // The pure case from the packet: the work landed under the
        // `-rerail` twin, and nothing would ever have swept the original.
        let cars = vec![rerailed_car(
            "car-1",
            "feat/x-rerail",
            &[("feat/x", "head-of-feat/x")],
        )];
        assert_eq!(
            names(&deletable_branches(&cars, &no_open())),
            vec!["feat/x-rerail", "feat/x"],
            "the original must be swept alongside the twin that carried it"
        );
    }

    #[test]
    fn a_rerail_original_a_live_car_claims_is_deferred_not_deleted() {
        // Someone re-used the original branch name for new work: the
        // live car's claim beats any landed car's deletion, and the
        // deferral keeps the train pending rather than stamping over it.
        let cars = vec![rerailed_car(
            "car-1",
            "feat/x-rerail",
            &[("feat/x", "head-of-feat/x")],
        )];
        let claimed: BTreeSet<String> = ["feat/x".to_string()].into_iter().collect();
        assert_eq!(
            names(&deletable_branches(&cars, &claimed)),
            vec!["feat/x-rerail"],
            "the claimed original survives; the twin is still swept"
        );
        assert_eq!(
            names(&claim_deferred_branches(&cars, &claimed)),
            vec!["feat/x"],
            "and the deferral is named, not silent"
        );
        assert!(
            !sweep_complete(0, claim_deferred_branches(&cars, &claimed).len(), &cars),
            "a deferred original must not be stamped over"
        );
    }

    #[test]
    fn an_abandoned_cars_rerail_original_keeps_its_branch() {
        // Abandonment is a DISPOSITION, not a sweep (packet 473fda1b):
        // the branch holds the only copy of work someone chose to stop,
        // and so does the branch it was re-railed off.
        let mut car = rerailed_car("car-1", "feat/x-rerail", &[("feat/x", "head-of-feat/x")]);
        car["metadata"]["outcome"] = json!("abandoned");
        assert!(
            deletable_branches(&[car], &no_open()).is_empty(),
            "neither the twin nor the original is landing evidence"
        );
    }

    #[test]
    fn a_car_re_railed_twice_offers_each_original_once() {
        // A second conflict re-rails an already-re-railed car, so the
        // chain is two deep. Every branch in it landed its content under
        // the car's current head — and an origin that repeats the car's
        // own branch must not be offered twice.
        let cars = vec![rerailed_car(
            "car-1",
            "feat/x-rerail-rerail",
            &[
                ("feat/x", "head-of-feat/x"),
                ("feat/x-rerail", "head-of-feat/x-rerail"),
                ("feat/x-rerail-rerail", "head-of-feat/x-rerail-rerail"),
            ],
        )];
        assert_eq!(
            names(&deletable_branches(&cars, &no_open())),
            vec!["feat/x-rerail-rerail", "feat/x", "feat/x-rerail"],
            "both originals, the car's own branch once, nothing invented"
        );
    }

    #[test]
    fn main_is_never_deletable_even_as_a_rerail_origin() {
        let cars = vec![rerailed_car(
            "car-1",
            "feat/x-rerail",
            &[("main", "head-of-main")],
        )];
        assert_eq!(
            names(&deletable_branches(&cars, &no_open())),
            vec!["feat/x-rerail"],
            "a malformed origin naming main must never reach the forge call"
        );
    }

    #[test]
    fn an_origin_without_a_branch_name_contributes_nothing() {
        let mut car = rerailed_car("car-1", "feat/x-rerail", &[]);
        car["metadata"]["rerail_origins"] = json!([{"head": "abc"}, {"branch": ""}, "feat/x"]);
        assert_eq!(
            names(&deletable_branches(&[car], &no_open())),
            vec!["feat/x-rerail"],
            "a malformed origins list costs the car's own sweep nothing"
        );
    }

    #[test]
    fn the_sweep_settles_only_when_every_boarded_car_is_terminal() {
        let landed = landed_car("car-1", "feat/x");
        let mut still_open = landed_car("car-2", "feat/y");
        still_open["status"] = json!("open");
        let mut cancelled = landed_car("car-3", "feat/z");
        cancelled["status"] = json!("cancelled");
        assert!(sweep_settled(std::slice::from_ref(&landed)));
        assert!(sweep_settled(&[landed.clone(), cancelled]));
        assert!(!sweep_settled(&[landed, still_open]));
        // Nothing boarded is trivially settled.
        assert!(sweep_settled(&[]));
    }

    // -- the sweep's coverage (packet 02069932) -----------------------------

    fn closed_train(id: &str, boarded: bool, swept: bool) -> Value {
        let mut md = serde_json::Map::new();
        if boarded {
            md.insert("boarded_jobs".into(), json!(["car-1"]));
        } else {
            md.insert("outcome".into(), json!("cancelled"));
        }
        if swept {
            md.insert("branches_swept".into(), json!("true"));
        }
        json!({"id": id, "kind": "pr-train", "status": "closed", "metadata": md})
    }

    /// THE COVERAGE LEAK. The sweep read the newest 50 closed trains
    /// under a comment claiming coverage was never capped. A refusing
    /// consist check closes a cancelled train about once a minute, so
    /// the window turned over in under an hour, and a train whose car
    /// was proven after that dropped out of it for good — its landed
    /// branch never inspected again. Paging is what makes the claim
    /// true: the arrived train sits behind 500 cancelled ones here,
    /// which is what the forge looked like on 2026-09-10.
    #[tokio::test]
    async fn the_sweep_reads_the_arrived_train_behind_a_window_of_refusals() {
        let mut all: Vec<Value> = (0..500)
            .map(|i| closed_train(&format!("cancelled-{i}"), false, false))
            .collect();
        all.push(closed_train("arrived-late-proof", true, false));
        // The counterfactual, and the whole defect: the old read took
        // the newest 50 rows, and there is nothing pending in them.
        assert!(
            sweep_pending(&all[..50]).is_empty(),
            "a capped read sees only the refusals — this is what leaked the branch"
        );

        let all_ref = &all;
        let gathered = list_all_pages(|offset| async move {
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
        })
        .await
        .unwrap();

        let pending = sweep_pending(&gathered);
        assert_eq!(
            pending.iter().map(|t| &t["id"]).collect::<Vec<_>>(),
            vec![&json!("arrived-late-proof")],
            "a train proven late must still be swept, however many trains closed since"
        );
    }

    #[test]
    fn a_swept_or_cancelled_train_costs_the_sweep_no_fetches() {
        let trains = vec![
            closed_train("swept", true, true),
            closed_train("cancelled", false, false),
            closed_train("pending", true, false),
        ];
        assert_eq!(
            sweep_pending(&trains)
                .iter()
                .map(|t| &t["id"])
                .collect::<Vec<_>>(),
            vec![&json!("pending")]
        );
    }

    /// A branch a still-open car claims is DEFERRED, not swept — the
    /// claim lifts when that car closes. Stamping the train over it
    /// marks the sweep done while the branch can still become
    /// deletable, and the branch leaks for good.
    #[test]
    fn a_branch_a_live_car_claims_keeps_its_train_pending() {
        let cars = vec![landed_car("car-1", "feat/x")];
        let claimed: BTreeSet<String> = ["feat/x".to_string()].into_iter().collect();
        assert!(
            deletable_branches(&cars, &claimed).is_empty(),
            "the live car's claim beats the landed car's deletion"
        );
        assert_eq!(
            names(&claim_deferred_branches(&cars, &claimed)),
            vec!["feat/x"],
            "and the deferral is named, not silent"
        );
        assert!(
            !sweep_complete(0, claim_deferred_branches(&cars, &claimed).len(), &cars),
            "a deferred branch must not be stamped over"
        );
        // No claim, nothing deferred, every car terminal — done.
        assert!(claim_deferred_branches(&cars, &no_open()).is_empty());
        assert!(sweep_complete(0, 0, &cars));
    }

    #[test]
    fn an_unlanded_car_defers_nothing() {
        let mut open = landed_car("car-1", "feat/x");
        open["status"] = json!("open");
        let mut abandoned = landed_car("car-2", "feat/y");
        abandoned["metadata"]["outcome"] = json!("abandoned");
        let claimed: BTreeSet<String> = ["feat/x".to_string(), "feat/y".to_string()]
            .into_iter()
            .collect();
        // Neither branch's content is on main, so neither was ever the
        // sweep's to delete — nothing to defer, and `main` is never a
        // car's branch to begin with.
        assert!(claim_deferred_branches(&[open, abandoned], &claimed).is_empty());
        let on_main = landed_car("car-3", "main");
        let main_claim: BTreeSet<String> = ["main".to_string()].into_iter().collect();
        assert!(claim_deferred_branches(&[on_main], &main_claim).is_empty());
    }

    #[test]
    fn the_stamp_waits_on_a_failed_branch_a_deferral_and_an_open_car() {
        let landed = landed_car("car-1", "feat/x");
        let mut still_open = landed_car("car-2", "feat/y");
        still_open["status"] = json!("open");
        assert!(sweep_complete(0, 0, std::slice::from_ref(&landed)));
        assert!(!sweep_complete(1, 0, std::slice::from_ref(&landed)));
        assert!(!sweep_complete(0, 1, std::slice::from_ref(&landed)));
        assert!(!sweep_complete(0, 0, &[landed, still_open]));
    }

    // -- the sweep's head guard (car 23923b40's known_gap) -----------------
    //
    // `fix/conductor-hardening` boarded at fc55e4d; two more commits
    // (705230b) were pushed to the branch AFTER boarding; the train
    // landed carrying only the boarded ones; the sweep read the job
    // record ("closed, outcome=merged" — true) and deleted the branch,
    // taking the unmerged commits with it. The job record proves the
    // CONTENT landed, never that the branch still holds only that
    // content. These pin the second question the sweep must now ask.

    const BOARDED: &str = "fc55e4d1a2b3c4d5e6f708192a3b4c5d6e7f8091";
    const MOVED: &str = "705230b9f8e7d6c5b4a39281706f5e4d3c2b1a09";

    #[test]
    fn a_branch_still_at_its_boarded_head_is_deleted() {
        assert_eq!(
            sweep_guard(Some(BOARDED), Some(BOARDED)),
            SweepGuard::Delete
        );
    }

    #[test]
    fn a_branch_that_moved_since_boarding_is_kept() {
        // The incident, exactly: the recorded head is not the branch's
        // head any more, so the delete would take work the train never
        // carried.
        assert_eq!(
            sweep_guard(Some(BOARDED), Some(MOVED)),
            SweepGuard::Moved {
                recorded: BOARDED.to_string(),
                current: MOVED.to_string(),
            }
        );
    }

    #[test]
    fn a_car_with_no_recorded_head_keeps_its_branch() {
        // An unknown head is not evidence. A car that boarded before
        // the conductor recorded heads keeps its branch: the cost of
        // keeping one is a stale branch, the cost of deleting one is
        // lost work. The branch has to EXIST for the question to mean
        // anything — see the Gone test for the other half.
        assert_eq!(sweep_guard(None, Some(BOARDED)), SweepGuard::NoRecord);
        // An empty stamp is no stamp.
        assert_eq!(sweep_guard(Some(""), Some(BOARDED)), SweepGuard::NoRecord);
    }

    #[test]
    fn a_branch_already_off_the_forge_is_nothing_to_sweep() {
        assert_eq!(sweep_guard(Some(BOARDED), None), SweepGuard::Gone);
        assert_eq!(sweep_guard(Some(BOARDED), Some("")), SweepGuard::Gone);
        // The forge's answer is asked FIRST, so an absent branch reads
        // Gone whatever the record says. Job 1bd1fb3d: every pre-guard
        // historical car has no recorded head AND no branch left, and
        // ordering the record first made each one a NoRecord line on
        // every reconcile, forever, about a branch swept by hand hours
        // earlier.
        assert_eq!(sweep_guard(None, None), SweepGuard::Gone);
        assert_eq!(sweep_guard(None, Some("")), SweepGuard::Gone);
        assert_eq!(sweep_guard(Some(""), None), SweepGuard::Gone);
    }

    /// One sweep decision, for the tests that are about the line it
    /// earns rather than about which branches were chosen.
    fn decided(branch: &str) -> CarBranch {
        CarBranch {
            branch: branch.to_string(),
            car: "car-1".to_string(),
            head: Some(BOARDED.to_string()),
            rerail_origin: false,
        }
    }

    #[test]
    fn only_a_branch_that_still_exists_is_worth_narrating() {
        // The sweep's journal is an operator surface: a line earns its
        // place by naming something a human can act on. A branch that
        // is not on the forge is not that — nothing to delete, nothing
        // to rescue, no action available.
        assert_eq!(sweep_note(&SweepGuard::Gone, &decided("fix/x")), None);
        // Delete narrates at the call site, which knows whether it was
        // a dry run, a deletion, or a race.
        assert_eq!(sweep_note(&SweepGuard::Delete, &decided("fix/x")), None);
        // The two keep-and-tell cases: the branch exists and the sweep
        // declined it, which is exactly what an operator must hear.
        let no_record = sweep_note(&SweepGuard::NoRecord, &decided("fix/x"))
            .expect("a surviving branch with no record is worth a line");
        assert!(no_record.contains("fix/x"), "{no_record}");
        assert!(
            no_record.contains("no boarded head on record"),
            "{no_record}"
        );
        let moved = sweep_note(
            &SweepGuard::Moved {
                recorded: BOARDED.to_string(),
                current: MOVED.to_string(),
            },
            &decided("fix/conductor-hardening"),
        )
        .expect("a branch that outgrew its boarding is worth a line");
        assert_eq!(
            moved,
            branch_moved_line(&decided("fix/conductor-hardening"), BOARDED, MOVED)
        );
        // A RERAIL ORIGINAL IS NAMED AS ONE, in both refusals. It never
        // boarded anything, so "no boarded head" and "moved since
        // boarding" would send an operator hunting for a boarding that
        // never happened (§Diagnosis — a verdict must name what failed).
        let origin = CarBranch {
            rerail_origin: true,
            ..decided("feat/x")
        };
        let no_head = sweep_note(&SweepGuard::NoRecord, &origin)
            .expect("a surviving original with no recorded head is worth a line");
        assert!(
            no_head.contains("rerail original feat/x") && no_head.contains("no head on record"),
            "{no_head}"
        );
        let origin_moved = sweep_note(
            &SweepGuard::Moved {
                recorded: BOARDED.to_string(),
                current: MOVED.to_string(),
            },
            &origin,
        )
        .expect("an original that moved after the rerail is worth a line");
        assert!(
            origin_moved.contains("rerail original feat/x")
                && origin_moved.contains("moved since the rerail"),
            "{origin_moved}"
        );
    }

    #[test]
    fn the_boarded_head_is_read_off_the_car_job() {
        let mut car = landed_car("car-1", "feat/x");
        car["metadata"]["boarded_head"] = json!(BOARDED);
        assert_eq!(boarded_head(&car), Some(BOARDED));
        // Absent, empty, or non-string reads as no stamp at all.
        assert_eq!(boarded_head(&landed_car("car-2", "feat/y")), None);
        let mut blank = landed_car("car-3", "feat/z");
        blank["metadata"]["boarded_head"] = json!("");
        assert_eq!(boarded_head(&blank), None);
        assert_eq!(boarded_head(&json!({"id": "car-4"})), None);
    }

    #[test]
    fn the_moved_branch_line_names_both_heads() {
        // Operator surface: the only notice that unmerged commits are
        // sitting on a branch the train did not carry.
        assert_eq!(
            branch_moved_line(&decided("fix/conductor-hardening"), BOARDED, MOVED),
            "branch fix/conductor-hardening moved since boarding \
             (fc55e4d1 -> 705230b9) — not deleting"
        );
    }

    /// An open boarded car earns a row naming the step that holds it —
    /// the fifth car of train 17:48 (2026-09-12), which the first live
    /// report left to inference.
    #[test]
    fn an_open_boarded_car_is_named_in_the_report_with_the_step_that_holds_it() {
        let cars = vec![
            json!({"id": "car-closed-0000", "status": "closed",
                   "metadata": {"branch": "fix/a", "outcome": "merged"}, "steps": []}),
            json!({"id": "car-open-000000", "status": "open",
                   "metadata": {"branch": "fix/the-metrics-timer"},
                   "steps": [{"spec_slug": "merged", "status": "completed"},
                             {"spec_slug": "proven", "status": "ready"}]}),
        ];
        let rows = unsettled_rows(&cars);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["branch"], "fix/the-metrics-timer");
        assert_eq!(rows[0]["car"], "car-open");
        let o = rows[0]["outcome"].as_str().unwrap();
        assert!(o.contains("still open") && o.contains("`proven`"), "{o}");
    }

    /// The forge's answer to DELETE is a claim; the read-back is the
    /// fact. Four combinations, two of which are the 2026-09-11 leak.
    #[test]
    fn a_delete_is_judged_by_the_read_back_not_the_answer() {
        assert_eq!(sweep_delete_verdict(true, None), SweepDelete::Deleted);
        assert_eq!(sweep_delete_verdict(false, None), SweepDelete::AlreadyGone);
        assert_eq!(
            sweep_delete_verdict(true, Some("abc")),
            SweepDelete::StillPresent {
                forge_said: "deleted"
            }
        );
        assert_eq!(
            sweep_delete_verdict(false, Some("abc")),
            SweepDelete::StillPresent {
                forge_said: "already gone"
            }
        );
        assert_eq!(
            sweep_delete_verdict(true, Some("")),
            SweepDelete::Deleted,
            "an empty head is no head"
        );
    }

    /// The journal names a rerail original as one. An operator reading
    /// "deleted branch feat/x" for a branch no car ever carried has to
    /// go re-derive where the deletion came from.
    #[test]
    fn the_journal_calls_a_rerail_original_what_it_is() {
        assert_eq!(sweep_subject(&decided("feat/x")), "branch feat/x");
        assert_eq!(
            sweep_subject(&CarBranch {
                rerail_origin: true,
                ..decided("feat/x")
            }),
            "rerail original feat/x"
        );
        // And the deferral line carries the same subject, so a held
        // original reads as one too.
        let held = claim_deferred_line(&CarBranch {
            rerail_origin: true,
            ..decided("feat/x")
        });
        assert!(
            held.contains("rerail original feat/x") && held.contains("train stays pending"),
            "{held}"
        );
    }

    #[test]
    fn a_failed_train_sweep_names_the_train() {
        let line = sweep_train_failed_line("tA-1234567890", &anyhow!("HTTP 500"));
        assert!(line.contains("tA-12345"), "must name the train: {line}");
        assert!(line.contains("HTTP 500"), "must carry the cause: {line}");
        assert!(
            line.contains("other trains continue"),
            "must say the fleet is not blocked: {line}"
        );
    }

    #[test]
    fn a_failed_branch_sweep_names_the_branch() {
        let line = sweep_branch_failed_line("fix/x", "car-abcdef1234", &anyhow!("forge blip"));
        assert!(line.contains("fix/x"), "must name the branch: {line}");
        assert!(line.contains("car-abcd"), "must name the car: {line}");
        assert!(line.contains("forge blip"), "must carry the cause: {line}");
        assert!(
            line.contains("other branches continue"),
            "must say the train's other branches are not blocked: {line}"
        );
    }
}
