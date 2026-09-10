//! Shared builders for a ship-a-change "car": the packet body and the
//! per-step evidence, plus the `Receipt` type they carry.
//!
//! WHY THIS IS IN CORE. Filing a car — POST the ship-a-change packet,
//! then fill its scope/build/gate steps with the receipt copied
//! verbatim — is done two ways now: by `boss park` (a human parks after
//! a green gate) and by the dispatcher's auto-park handler (a rule parks
//! on the gate-run's green step). If each built the packet its own way
//! they would drift, and the one field that must never be rebuilt is the
//! receipt (that is the bug `boss park` was created to kill). So the
//! pure builders live here, shared by both callers; the HTTP
//! orchestration (the POST and the PUTs) stays with each caller, because
//! that part legitimately differs. CLAUDE.md 9a: collapse the fact that
//! would otherwise live twice.

use serde_json::{Value, json};

/// What a green gate-run packet says about a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The receipt string, copied verbatim — never rebuilt from parts.
    pub raw: String,
    /// The head it vouches for, read back out for refusal messages and
    /// for the caller to compare against the branch.
    pub head: String,
    /// `full`, `--auto`, or whatever the runner was given.
    pub mode: String,
}

/// The instant stamp a car's gate step and the conductor's `review`
/// step share. RFC3339 to the SECOND, `Z`: dock queue time is
/// `review.completed_at` minus `gate.completed_at`, so sub-second
/// precision or an offset would break that subtraction by writer. ONE
/// definition — `boss gate`'s `stamp` delegates here — so the two ends
/// of the measurement cannot drift in format.
pub fn stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The ship-a-change packet body for a car.
pub fn car_body(
    branch: &str,
    summary: &str,
    backlog_item: Option<&str>,
    delivery_channel: Option<&str>,
) -> Value {
    let mut metadata = json!({ "branch": branch, "summary": summary });
    if let Some(item) = backlog_item {
        // A declared job edge — ref-checked by the API at the write,
        // which is what makes it safe to write here rather than by hand.
        // A mistyped id is refused instead of silently pointing at
        // nothing.
        metadata["backlog_item"] = json!(item);
    }
    // How this change ships (data/config/software/infra), derived at the
    // gate and carried here so the yard and channel-gated delivery read
    // it off the car. Absent when the gate could not classify the diff.
    if let Some(dc) = delivery_channel {
        metadata["delivery_channel"] = json!(dc);
    }
    json!({
        "kind": "ship-a-change",
        "title": summary_title(summary),
        "subject": {"subject_kind": "custom", "id": branch},
        "owner_id": "emp-david",
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": metadata,
    })
}

/// A car's title: the first sentence of its summary, trimmed.
///
/// Titles are what David reads on a board, so they get the summary's
/// opening claim rather than the branch name — `fix/a-dropped-lookup`
/// says less than "A dropped lookup does not red a train".
fn summary_title(summary: &str) -> String {
    let first = summary
        .split_terminator(['.', '\n'])
        .next()
        .unwrap_or(summary)
        .trim();
    let t = if first.is_empty() {
        summary.trim()
    } else {
        first
    };
    t.chars().take(120).collect()
}

/// The step titles a parked car fills, in the order the workflow runs
/// them. `ship-a-change` names them in its registry row, so a rename
/// there is a rename here — a caller refuses rather than guesses when a
/// step is missing.
pub const SCOPE: &str = "Declare the boundary";
pub const BUILD: &str = "Build it";
pub const GATE: &str = "Green, and observed working";

/// The trigger step a car OPENS at. `ship-a-change`'s first step, with
/// `ready_when: true`, so the POST that files the packet completes it
/// and `scope` is ready the moment the car exists.
pub const OPENED: &str = "Change started";

/// The registry SLUGS for the same steps — the primary key `find_step`
/// looks a step up by, with the titles above as its fallback. Paired
/// with their titles in one place so a lookup cannot drift from the
/// evidence written through it (CLAUDE.md §9a); `step_fields` carries
/// the pair through to every caller rather than letting each re-derive
/// it.
pub const OPENED_SLUG: &str = "opened";
pub const SCOPE_SLUG: &str = "scope";
pub const BUILD_SLUG: &str = "build";
pub const GATE_SLUG: &str = "gate";
pub const REVIEW_SLUG: &str = "review";

/// The evidence each of the three steps carries, and when it was filled,
/// as `(registry slug, step title, metadata)`.
///
/// When a car is filed at GREEN all three stamps are the same instant,
/// because filing it is one act. When a builder OPENED the car at build
/// start they are not: `scope` was filled then, and only the steps still
/// open are written now — see [`finish_writes`], which is what callers
/// use. What the stamps separate is the *dock* — gate's stamp is when
/// the car became ready, and the conductor's stamp on `review` is when
/// it boarded, so the difference is queue time and nothing else.
pub fn step_fields(
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
    receipt: &Receipt,
    now: chrono::DateTime<chrono::Utc>,
) -> [(&'static str, &'static str, Value); 3] {
    let at = stamp(now);
    [
        (
            SCOPE_SLUG,
            SCOPE,
            json!({"summary": summary, "excludes": excludes, "completed_at": at}),
        ),
        (BUILD_SLUG, BUILD, json!({"test": test, "completed_at": at})),
        (
            GATE_SLUG,
            GATE,
            json!({
                "gates": if receipt.mode.is_empty() { "full" } else { &receipt.mode },
                // VERBATIM. The whole point of the shared builder.
                "receipt": receipt.raw,
                "verified": verified,
                "completed_at": at,
            }),
        ),
    ]
}

/// The review step a parked car waits at. The conductor reads it as
/// the dock: a car whose review is `ready`/`active` is parked, and
/// boarding stamps `metadata.train` rather than completing it (so a
/// cancelled train can release the car by clearing the stamp).
pub const REVIEW: &str = "Open for review";

/// One step write a car's filer owes: which step, and the body to PUT.
///
/// The step ID is read OFF the car, never assembled — the same rule the
/// receipt lives by, for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepWrite {
    /// The step's id, as the packet reported it.
    pub step_id: String,
    /// Its title, so a message can name what was written.
    pub title: &'static str,
    /// `PUT /api/jobs/{car}/steps/{step_id}` body.
    pub body: Value,
}

/// PURE: the id of `(slug, title)` on this car, or `None` when the
/// packet has no such step. The id a CLAIM needs
/// (`POST …/steps/{id}/claim`), which takes no body and so has nowhere
/// to carry a [`StepWrite`].
pub fn step_id_for(car: &Value, slug: &str, title: &str) -> Option<String> {
    find_step(car, slug, title)?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Has this step already been taken through? A completed or skipped step
/// is frozen — the step API refuses a metadata write to one and its 409
/// names the job-metadata door instead — so every writer below skips it
/// rather than discovering that at the wire.
fn already_done(step: &Value) -> bool {
    matches!(
        step.get("status").and_then(Value::as_str),
        Some("completed" | "skipped")
    )
}

/// The missing-step refusal both writers share: an in-flight packet is
/// pinned to the workflow version it was admitted under, so a car whose
/// steps do not answer to the slugs this code knows must refuse rather
/// than write evidence onto whatever step happened to be there.
fn no_such_step(slug: &str, title: &str) -> String {
    format!(
        "this car has no `{slug}` step ('{title}') — refusing to write evidence onto a \
         packet whose shape does not match the ship-a-change steps this build of boss \
         knows. An in-flight car is pinned to the workflow version it was admitted \
         under; read the packet before writing to it."
    )
}

/// PURE: the writes that OPEN a car at build start — `scope` completed
/// with the brief the builder was handed.
///
/// WHY THE SCOPE IS WRITTEN AT OPEN AND NOT AT GREEN. `scope` asks what
/// the change contains and what it deliberately excludes, and the
/// registry row says why: *"it asks a person ... BEFORE the work, which
/// is the only moment that decision keeps a PR small."* Filed at green
/// it was being recorded after the fact. A builder opening the car has
/// exactly that brief in hand, so it is stated when it is true.
///
/// `build` is NOT written here. It is taken through the CLAIM door
/// (`POST …/steps/{id}/claim`), a Ready→Active compare-and-set that
/// records the claimant and refuses a second one with a 409 naming the
/// holder — which is how "two agents on the same branch" becomes visible
/// instead of becoming a twin car.
///
/// IDEMPOTENT: a car whose scope is already declared yields no writes,
/// so a builder re-running the verb writes nothing rather than 409ing on
/// a frozen step.
pub fn open_writes(
    car: &Value,
    summary: &str,
    excludes: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<StepWrite>, String> {
    let Some(step) = find_step(car, SCOPE_SLUG, SCOPE) else {
        return Err(no_such_step(SCOPE_SLUG, SCOPE));
    };
    if already_done(step) {
        return Ok(Vec::new());
    }
    let Some(step_id) = step.get("id").and_then(Value::as_str) else {
        return Err(format!("the `{SCOPE_SLUG}` step on this car has no id"));
    };
    let at = stamp(now);
    Ok(vec![StepWrite {
        step_id: step_id.to_string(),
        title: SCOPE,
        body: json!({
            "status": "completed",
            "metadata": {"summary": summary, "excludes": excludes, "completed_at": at},
        }),
    }])
}

/// PURE: the writes a GREEN owes a car — each of scope/build/gate that
/// is not already completed, carrying [`step_fields`]' evidence.
///
/// ONE DEFINITION OF "WHAT A GREEN OWES A CAR", for the three callers
/// that owe it: `boss park`, the dispatcher's auto-park handler, and now
/// both of those acting on a car a builder OPENED rather than one they
/// just filed. The two cases differ only in which steps are already
/// done, which is a fact ON THE PACKET — so it is read off the packet
/// here instead of each caller deciding (CLAUDE.md §9a).
///
/// THE SKIP IS THE POINT. A car opened at build start has `scope`
/// completed already, and the step API refuses a metadata write to a
/// completed step. A green that re-sent `scope` would 409 on every car a
/// builder opened — which is to say, on every car, once they do.
pub fn finish_writes(
    car: &Value,
    summary: &str,
    excludes: &str,
    test: &str,
    verified: &str,
    receipt: &Receipt,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<StepWrite>, String> {
    let mut out = Vec::new();
    for (slug, title, metadata) in step_fields(summary, excludes, test, verified, receipt, now) {
        let Some(step) = find_step(car, slug, title) else {
            return Err(no_such_step(slug, title));
        };
        if already_done(step) {
            continue;
        }
        let Some(step_id) = step.get("id").and_then(Value::as_str) else {
            return Err(format!("the `{slug}` step on this car has no id"));
        };
        out.push(StepWrite {
            step_id: step_id.to_string(),
            title,
            body: json!({"status": "completed", "metadata": metadata}),
        });
    }
    Ok(out)
}

/// The metadata a car carries about its own BUILD, stamped when the
/// builder opens it.
///
/// THIS IS THE FACT THE MIDDLE THIRD RENDERS (backlog be025b44, design
/// 9e82ee62). Four builders ran for 19–47 minutes each on 2026-09-09 and
/// the yard showed an empty dock throughout, because a car's packet did
/// not exist until its gate went green. Who is building, on which host,
/// in which worktree, since when — none of it was anywhere. It is four
/// keys, and a board is a reader of them.
///
/// ABSENT, NEVER NULLED. The job-metadata door DELETES a key set to
/// null, so an unknown host must omit its key rather than strip one an
/// earlier write recorded.
pub fn build_start(
    actor: &str,
    host: &str,
    worktree: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("build_started_at".to_string(), json!(stamp(now)));
    let mut put = |k: &str, v: &str| {
        let v = v.trim();
        if !v.is_empty() {
            m.insert(k.to_string(), json!(v));
        }
    };
    put("built_by", actor);
    put("build_host", host);
    put("build_worktree", worktree);
    m
}

/// THE CAR CARRIES ITS PROBE. `boss prove` records a proof as a
/// command that ran plus the string it had to print; until 2026-09-08
/// that command was authored by an operator AFTER the car landed —
/// 43 landed cars sat at `proven: ready` for 2.5 days waiting for one
/// (backlog 28ac45ab). The builder who parks the car knows what the
/// change does and can write the probe then, so the park intent
/// carries it and the car records it under these keys, in exactly the
/// shape `boss prove --from-car` and the arrival rule re-run. ONE
/// definition of the key names, read by the gate verb, the auto-park
/// handler and the prove verb, so the writer and the readers cannot
/// drift (CLAUDE.md 9a).
pub const PROOF_PROBE: &str = "proof_probe";
/// The string the recorded probe must print — never optional: a probe
/// with no expectation is `echo hi`, which exits 0 too.
pub const PROOF_EXPECT: &str = "proof_expect";
/// An EVENT-BOUND car has no mechanical probe: what proves it is a
/// thing that has to happen (a stalled train, a red gate, an operator's
/// cancel). Recorded as prose naming the event and how to prove it
/// when it fires — the `proof_finding` vocabulary operators wrote by
/// hand on 2026-09-08 — so the yard can count it as waiting rather
/// than forgotten, and the arrival rule knows to leave it alone.
pub const PROOF_EVENT: &str = "proof_event";

/// The proof intent a car carries in its metadata: the probe + expect
/// pair, or the event that stands in for one. Absent keys are omitted
/// (never nulled), so merging this into a car body or a metadata
/// PATCH adds what was stated and touches nothing else.
pub fn proof_intent(
    probe: Option<&str>,
    expect: Option<&str>,
    event: Option<&str>,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    let mut put = |k: &str, v: Option<&str>| {
        if let Some(v) = v.filter(|s| !s.trim().is_empty()) {
            m.insert(k.to_string(), json!(v));
        }
    };
    put(PROOF_PROBE, probe);
    put(PROOF_EXPECT, expect);
    put(PROOF_EVENT, event);
    m
}

/// THE ITEM A CAR NAMES WITHOUT AUTHORISING ITS CLOSE.
///
/// `backlog_item` is a declared one-to-one job edge, and the arrival
/// rule `complete-feedback-branch-on-car-merged` follows exactly that
/// key to route the item's triage and complete its `build` — which
/// closes it. So a car that is ONE PIECE of a several-piece item must
/// not use it: on 2026-09-10 item cf0f5e2d was three pieces and held
/// car f73828a9 was piece (1) only, with piece (3) waiting on an
/// operator's explicit yes; linking it would have closed an item with
/// work outstanding. This key records the same provenance under a name
/// NO rule reads, which is what makes it inert at arrival.
pub const PARTIAL_ITEM: &str = "partial_item";
/// Why this car names no item at all — the answer a deliberately
/// item-less car gives (`--park-no-item`). Item-less cars legitimately
/// exist (a fix asked for in conversation, a defect found while
/// building something else); the reason is what lets a later reader
/// tell one from a car whose builder simply forgot, which is the
/// omission e1325456 measured thirteen times in nineteen.
pub const NO_ITEM_REASON: &str = "no_item_reason";

/// The item provenance a car carries beyond the closing edge: the item
/// it is one piece of, or the reason it names none. Absent and blank
/// values are omitted (never nulled), so merging this into a car body
/// or a metadata PATCH adds what was stated and touches nothing else —
/// the same contract [`proof_intent`] has.
pub fn item_provenance(
    partial_item: Option<&str>,
    no_item_reason: Option<&str>,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    let mut put = |k: &str, v: Option<&str>| {
        if let Some(v) = v.filter(|s| !s.trim().is_empty()) {
            m.insert(k.to_string(), json!(v));
        }
    };
    put(PARTIAL_ITEM, partial_item);
    put(NO_ITEM_REASON, no_item_reason);
    m
}

/// A step by its registry slug, falling back to its title. The same
/// lookup the conductor uses; one definition (CLAUDE.md 9a).
pub fn find_step<'a>(job: &'a Value, slug: &str, title: &str) -> Option<&'a Value> {
    let steps = job.get("steps").and_then(Value::as_array)?;
    steps
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .or_else(|| {
            steps
                .iter()
                .find(|s| s.get("title").and_then(Value::as_str) == Some(title))
        })
}

/// Is this car still PARKED — gated, filed, and waiting at the dock —
/// rather than boarded (a train stamped it) or past review?
///
/// ONE PREDICATE, THREE READERS. The conductor counts the dock with it
/// (`parked_ready` adds boarding's own refinements: a `hold`, a
/// `train/` branch). `boss park` and the dispatcher's auto-park handler
/// ask it the question this file exists for: when a branch is gated
/// AGAIN, is there a car to refresh, or is a new one due? Measured
/// 2026-09-05 (backlog d052afad): every re-gate filed a twin, the dock
/// held 10 cars for 6 branches, and the first car of each pair sat
/// there with a receipt for a head that no longer existed — left behind
/// by every train as "gated, then changed" until closed by hand.
///
/// Status is deliberately not read here: callers list `status=open`,
/// and a fixture without a top-level status must still answer.
pub fn is_parked(car: &Value) -> bool {
    let md = car.get("metadata");
    let branch = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if branch.is_empty() || is_set(md.and_then(|m| m.get("train"))) {
        return false;
    }
    matches!(
        find_step(car, REVIEW_SLUG, REVIEW)
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str),
        Some("ready" | "active")
    )
}

/// Is this car OPEN AT BUILD — filed when its build STARTED, and not yet
/// carrying a gate verdict?
///
/// THE THIRD STATE A LIVE CAR CAN BE IN, and it did not exist until
/// 2026-09-10. While a car was only ever filed at green, `is_parked` and
/// `is_boarded` between them covered every live car. A car opened at
/// build start (backlog be025b44) is neither: its `gate` step has not
/// reported, so `review` is not open, so the dock does not hold it — and
/// correctly, because a car with no receipt must never board. It is
/// nonetheless THE car for its branch, which is the whole point: the
/// branch has exactly one packet from its first minute, so "no parked
/// car" can never again be read as "no car" and file a twin (d052afad,
/// 02165b1d, 6790175e — three measured twins, all that shape).
///
/// The gate step must be PRESENT and open. Absent means this build of
/// boss cannot read the packet's shape, and an unreadable car is not one
/// to adopt — refuse, do not guess.
pub fn is_building(car: &Value) -> bool {
    let md = car.get("metadata");
    let branch = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if branch.is_empty() || !is_open(car) || is_set(md.and_then(|m| m.get("train"))) {
        return false;
    }
    matches!(
        find_step(car, GATE_SLUG, GATE)
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str),
        Some("pending" | "ready" | "active")
    )
}

/// The car a builder already OPENED for `branch`, if one is still
/// building. `None` means no build is in flight for it under a packet.
pub fn building_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_building(c)
    })
}

/// The car a re-gate of `branch` refreshes, if one is still parked.
/// `None` means file a new car — the branch has none, or its car has
/// boarded or reached a terminal and that history is spent.
pub fn parked_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_parked(c)
    })
}

/// Is this car ABOARD a train — an open car the conductor stamped with
/// `metadata.train` and has not released?
///
/// The other half of `is_parked`: between them they cover every live
/// car. Boarding stamps the train rather than completing the review
/// step, and a cancelled train clears the stamp, so the stamp is the
/// whole test — plus "not closed", because a closed car's train stamp
/// is history (that is what `is_landed` reads).
pub fn is_boarded(car: &Value) -> bool {
    let md = car.get("metadata");
    let branch = md
        .and_then(|m| m.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    !branch.is_empty() && is_open(car) && is_set(md.and_then(|m| m.get("train")))
}

/// The live car this branch already has — aboard a train if one is,
/// otherwise the one parked at the dock, otherwise the one a builder
/// opened and is still building. `None` means the branch has no car: a
/// green may file one.
///
/// ONE PREDICATE, TWO CALLERS, ONE MEASURED BUG (backlog 02165b1d).
/// 2026-09-08 20:30:54: car d08a6418 boarded train #274. 20:31:20: a
/// duplicate gate-run went green and the auto-park handler filed car
/// ad54e95c for the SAME branch, which then sat on the loading dock
/// while the first rode — abandoned by hand. `parked_car_for` answered
/// `None` (correctly: the first car was no longer parked), and "no
/// parked car" was read as "no car". It is not. So both the handler and
/// `boss gate`'s launch guard ask THIS question — is there a live car
/// at all — from one definition (CLAUDE.md §9a).
/// THE BUILDING CAR IS A LIVE CAR TOO (be025b44). Since a builder opens
/// the car when the build starts, the commonest live state is no longer
/// "parked": it is "building", and a question that missed it would file
/// the fourth twin of this exact shape. Ordered as the failures were
/// measured — aboard, then at the dock, then building — so the earlier
/// answers cannot move.
pub fn open_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    let mine = |c: &&Value| c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch);
    cars.iter()
        .find(|c| mine(c) && is_boarded(c))
        .or_else(|| cars.iter().find(|c| mine(c) && is_open(c) && is_parked(c)))
        .or_else(|| cars.iter().find(|c| mine(c) && is_building(c)))
}

/// A car that already carried its branch to main: closed with
/// `outcome=merged`, or stamped `merged` by the conductor's landing (the
/// marker v3 ship-a-change gates its `merged` step on — the dispatcher
/// closes the Job from it, so a car can be marked before it is closed).
/// An abandoned or cancelled car is spent, not landed.
pub fn is_landed(car: &Value) -> bool {
    let md = car.get("metadata");
    let closed_merged = car.get("status").and_then(Value::as_str) == Some("closed")
        && md.and_then(|m| m.get("outcome")).and_then(Value::as_str) == Some("merged");
    // The conductor writes the marker as the STRING "true"; a bool is
    // accepted for the same meaning, and an explicit false is neither.
    let marked = match md.and_then(|m| m.get("merged")) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    };
    closed_merged || marked
}

/// The car that already landed `branch`, if one has — the question both
/// `boss gate` and the auto-park handler ask BEFORE filing. Measured
/// 2026-09-08 (backlog 610537b2): a re-gate of a landed branch reused a
/// gate-run that still carried park intent, and on green the handler
/// filed a twin car for content already on main; it boarded, its train
/// went red on an empty diff, and train and twin were cleaned up by
/// hand. One test of "landed" here so the CLI and the dispatcher cannot
/// disagree about it.
pub fn landed_car_for<'a>(cars: &'a [Value], branch: &str) -> Option<&'a Value> {
    cars.iter().find(|c| {
        c.pointer("/metadata/branch").and_then(Value::as_str) == Some(branch) && is_landed(c)
    })
}

/// The metadata patch that supersedes a parked car's gate receipt.
///
/// A completed step is frozen, so a fresh receipt rides the JOB as
/// `regate_receipt` — the key boarding (`receipt_skip_reason`) and
/// `boss receipt` both read in preference to the gate step. VERBATIM,
/// like the gate step's copy: a rebuilt receipt once let a wrong head
/// through. `skip_reason` is present-and-null on purpose — the metadata
/// door deletes a null key, so the conductor's "left behind" reason
/// goes with the stale receipt. One builder for `boss park`, the
/// auto-park handler and `boss rerail`, so the write cannot drift.
pub fn regate_patch(receipt: &Receipt, note: &str, delivery_channel: Option<&str>) -> Value {
    let mut patch = json!({
        "regate_receipt": receipt.raw,
        "skip_reason": Value::Null,
        "regate_note": note,
    });
    // Re-classify the channel from the re-gated diff and carry it, the
    // same field `car_body` stamps on a fresh car. Without this a
    // re-gate — the common path: every `--wait --park` of an existing
    // car, plus `boss park`/`boss rerail` — dropped `delivery_channel`,
    // so the yard and the channel mixes under-counted every branch that
    // was ever rebased or rerailed. Omitted (not nulled) when the diff
    // could not be classified: a null key is DELETED by the metadata
    // door, which would strip a channel the car already carried.
    if let Some(dc) = delivery_channel {
        patch["delivery_channel"] = json!(dc);
    }
    patch
}

/// Is this packet still open? Callers list `status=open`, so the field
/// is usually redundant — and a fixture without one must still answer —
/// but a list that also holds closed cars (the handler pages both) must
/// not read a closed car as a live one.
///
/// Public because `boss prove` asks it too: its read is deliberately
/// `kind=ship-a-change` with NO status filter (`--recheck` re-runs a
/// proof on a closed car), so it is the other caller holding a mixed
/// list. One definition for "is this car live", not a fourth copy of
/// `!= "closed"` (CLAUDE.md §9a).
pub fn is_open(car: &Value) -> bool {
    car.get("status").and_then(Value::as_str) != Some("closed")
}

/// A metadata stamp that is present: the conductor writes `train` as
/// an id and clears it to `null` on release; an empty string reads as
/// cleared too.
fn is_set(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The kind of linked packet a park may ROUTE. A `user-feedback`
/// packet's triage is the filer's own routing decision and stays
/// theirs — the same scoping the arrival rule's `route` arg carries
/// (`jobs.complete_linked_step`, dda0713c).
pub const TRIAGEABLE_KIND: &str = "backlog-item";

/// The routing step on that kind. Its `spec_slug` is the lookup;
/// `find_step`'s title fallback is given the same string because this
/// step has no separate title a car author could rely on.
pub const TRIAGE_SLUG: &str = "triage";

/// The disposition a parked car states: this car is the item's build.
pub const DISPOSITION_BUILD: &str = "build";

/// The step write a park owes the item its car links — the step to
/// complete and the body to PUT. The HTTP stays with each caller
/// (`boss park` and the dispatcher's auto-park handler); the DECISION
/// lives here once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriageWrite {
    /// The id of the item's routing step.
    pub step_id: String,
    /// `PUT /api/jobs/{item}/steps/{step_id}` body: completed, with the
    /// disposition and the evidence merged onto whatever the step
    /// already carried.
    pub body: Value,
}

/// PURE: the triage write parking this car owes the item it links — or
/// `None` when there is nothing for a park to state.
///
/// WHY A PARK DECIDES THE ROUTE. `--park-backlog-item <id>` says "this
/// car is that item's build". But the item's `build` step only OPENS
/// once its triage records `disposition = build`, so a car parked
/// against an un-triaged item links to a step nothing can advance: the
/// change gets built, gated, landed and proven while the item still
/// reads as undecided work, and the arrival rule finds nothing to
/// complete. Measured twice within an hour on 2026-09-09 (items
/// 5942f205 and af28e250; backlog ca76d8f9) — once walked through by
/// hand afterwards, once triaged by the builder so the arrival rule
/// would have something to complete. A car cannot be the build of
/// something nobody decided to build, so the flag that names it states
/// the decision at park time rather than leaving residue: an open
/// packet that looks like unfinished work when the work is live. That
/// residue is not free — past 100 open `ship-a-change` jobs a
/// parked-ready car falls off page 1 and never boards (874ea0ae).
///
/// IDEMPOTENT, AND NEVER DESTRUCTIVE. `Some` comes back only while the
/// routing step is still OPEN (`ready`/`active` — the same "open" the
/// arrival rule's route reads) AND carries no disposition. A triage a
/// person already completed, an item a person routed to `verify` /
/// `design` / `stale` / `decline`, a closed or cancelled item, an item
/// with no routing step, and a kind whose triage is not a park's to
/// make all answer `None` — so a re-gate, a refresh or a redelivery
/// writes nothing, and no human's disposition is ever overwritten.
pub fn triage_on_park(item: &Value, car_id: &str, branch: &str) -> Option<TriageWrite> {
    if item.get("kind").and_then(Value::as_str) != Some(TRIAGEABLE_KIND) {
        return None;
    }
    if matches!(
        item.get("status").and_then(Value::as_str),
        Some("closed" | "cancelled")
    ) {
        return None;
    }
    let step = find_step(item, TRIAGE_SLUG, TRIAGE_SLUG)?;
    if !matches!(
        step.get("status").and_then(Value::as_str),
        Some("ready" | "active")
    ) {
        return None;
    }
    let mut metadata = match step.get("metadata").cloned() {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    // Belt to the status guard's braces: a disposition already written
    // is a decision already made, whatever the step's status says.
    if metadata.contains_key("disposition") {
        return None;
    }
    let step_id = step.get("id").and_then(Value::as_str)?.to_string();
    metadata.insert("disposition".to_string(), json!(DISPOSITION_BUILD));
    metadata.insert(
        "evidence".to_string(),
        json!(park_triage_evidence(car_id, branch)),
    );
    Some(TriageWrite {
        step_id,
        // PATCH-on-PUT replaces top-level `metadata` wholesale, so the
        // step's existing keys ride along rather than being wiped.
        body: json!({ "status": "completed", "metadata": Value::Object(metadata) }),
    })
}

/// The `evidence` the routing step requires at done, naming WHAT made
/// the decision — a reader of the item should not have to go find out
/// why its route says `build`.
fn park_triage_evidence(car_id: &str, branch: &str) -> String {
    format!(
        "routed at park: car {} on {branch} is this item's build. \
         `--park-backlog-item` names the car as the build, so the route is stated when \
         the car is filed rather than left un-triaged for the arrival rule to find \
         nothing to advance (backlog ca76d8f9).",
        &car_id[..8.min(car_id.len())]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8";
    const GREEN: &str = r#"{"verdict": "green", "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8", "mode": "full", "fails": []}"#;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    /// The proof keys are written only when stated, so a car with no
    /// probe carries no `proof_probe: null` for a reader to trip on.
    #[test]
    fn proof_intent_writes_only_what_was_stated() {
        assert!(proof_intent(None, None, None).is_empty());
        assert!(proof_intent(Some("  "), None, None).is_empty());
        let p = proof_intent(Some("curl -s x | grep y"), Some("y"), None);
        assert_eq!(p[PROOF_PROBE], "curl -s x | grep y");
        assert_eq!(p[PROOF_EXPECT], "y");
        assert!(!p.contains_key(PROOF_EVENT));
        let e = proof_intent(None, None, Some("event-bound — the next yard cancel"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[PROOF_EVENT], "event-bound — the next yard cancel");
    }

    /// PROVENANCE WITHOUT THE CLOSE. Same write-only-what-was-stated
    /// contract as the proof keys — and the key that matters here is the
    /// one NOT written: `backlog_item` is what the arrival rule follows,
    /// so neither provenance key may collide with it.
    #[test]
    fn item_provenance_writes_only_what_was_stated_and_never_the_closing_edge() {
        assert!(item_provenance(None, None).is_empty());
        assert!(item_provenance(Some("  "), Some("\t")).is_empty());
        let p = item_provenance(Some("cf0f5e2d"), None);
        assert_eq!(p[PARTIAL_ITEM], "cf0f5e2d");
        assert!(!p.contains_key(NO_ITEM_REASON));
        assert!(
            !p.contains_key("backlog_item"),
            "a partial edge must never write the key the arrival rule follows"
        );
        let n = item_provenance(None, Some("David asked for this in conversation"));
        assert_eq!(n.len(), 1);
        assert_eq!(n[NO_ITEM_REASON], "David asked for this in conversation");
        assert_ne!(PARTIAL_ITEM, "backlog_item");
        assert_ne!(NO_ITEM_REASON, "backlog_item");
    }

    #[test]
    fn the_car_body_carries_the_fields_the_api_demands() {
        let b = car_body("feat/x", "A thing does the thing. And more.", None, None);
        assert!(
            b["metadata"].get("delivery_channel").is_none(),
            "no delivery_channel when None"
        );
        let d = car_body("feat/x", "A thing.", None, Some("data"));
        assert_eq!(
            d["metadata"]["delivery_channel"], "data",
            "the car carries the delivery channel the gate stamped"
        );
        for f in [
            "kind", "subject", "title", "owner_id", "status", "priority", "metadata", "tags",
        ] {
            assert!(b.get(f).is_some(), "car body is missing `{f}`");
        }
        assert_eq!(b["title"], "A thing does the thing");
        assert_eq!(b["subject"]["id"], "feat/x");
        assert!(b["metadata"].get("backlog_item").is_none());
    }

    #[test]
    fn a_backlog_edge_is_carried_when_given() {
        let b = car_body(
            "feat/x",
            "Summary",
            Some("de6f0c06-a341-4445-9f47-399dc27a60fb"),
            None,
        );
        assert_eq!(
            b["metadata"]["backlog_item"],
            "de6f0c06-a341-4445-9f47-399dc27a60fb"
        );
    }

    #[test]
    fn every_step_park_fills_says_when_it_was_filled() {
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: "full".to_string(),
        };
        let fields = step_fields("s", "e", "t", "v", &receipt, at("2026-08-29T04:15:09Z"));

        assert_eq!(fields.len(), 3);
        for (slug, title, md) in &fields {
            assert_eq!(
                md.get("completed_at").and_then(Value::as_str),
                Some("2026-08-29T04:15:09Z"),
                "{title} was filled without saying when"
            );
            assert!(
                !slug.is_empty(),
                "{title} carries the registry slug `find_step` looks it up by"
            );
        }
    }

    #[test]
    fn the_stamp_matches_the_one_the_conductor_writes() {
        // Dock queue time is `review.completed_at` (the conductor's)
        // minus `gate.completed_at` (this), so a format that differs by
        // writer is wrong only in the subtraction.
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: "full".to_string(),
        };
        let now = at("2026-08-29T04:15:09.847213Z");
        let fields = step_fields("s", "e", "t", "v", &receipt, now);
        let parked = fields[2].2["completed_at"].as_str().unwrap().to_string();

        assert_eq!(parked, stamp(now));
        assert!(
            !parked.contains('.') && parked.ends_with('Z'),
            "sub-second precision and offsets both break string comparison \
             against the conductor's stamps: {parked}"
        );
    }

    #[test]
    fn parking_does_not_disturb_the_evidence_it_already_carried() {
        let receipt = Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: String::new(),
        };
        let f = step_fields(
            "sum",
            "exc",
            "tst",
            "ver",
            &receipt,
            at("2026-08-29T04:15:09Z"),
        );
        assert_eq!(f[0].2["summary"], json!("sum"));
        assert_eq!(f[0].2["excludes"], json!("exc"));
        assert_eq!(f[1].2["test"], json!("tst"));
        assert_eq!(f[2].2["verified"], json!("ver"));
        // An empty mode still reads as a full gate.
        assert_eq!(f[2].2["gates"], json!("full"));
        assert_eq!(f[2].2["receipt"], json!(GREEN));
    }
}

#[cfg(test)]
mod regate_tests {
    use super::*;

    const GREEN: &str = r#"{"verdict": "green", "head": "0123456789abcdef0123456789abcdef01234567", "mode": "full", "fails": []}"#;

    fn receipt() -> Receipt {
        Receipt {
            raw: GREEN.to_string(),
            head: "0123456789abcdef0123456789abcdef01234567".to_string(),
            mode: "full".to_string(),
        }
    }

    /// A car as the jobs API lists it: open, branch in metadata, review
    /// step waiting — the shape both parkers and the conductor read.
    fn car(id: &str, branch: &str, review_status: &str, train: Value) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": branch, "train": train },
            "steps": [
                {"spec_slug": "gate", "title": GATE, "status": "completed"},
                {"spec_slug": "review", "title": REVIEW, "status": review_status},
            ]
        })
    }

    #[test]
    fn a_car_waiting_at_review_with_no_train_is_parked() {
        assert!(is_parked(&car("c1", "fix/x", "ready", Value::Null)));
        assert!(is_parked(&car("c1", "fix/x", "active", Value::Null)));
    }

    #[test]
    fn a_boarded_car_is_not_parked() {
        // The conductor stamps `metadata.train` when a car boards and
        // clears it (Null) when a cancelled train releases the car.
        assert!(!is_parked(&car("c1", "fix/x", "ready", json!("train-1"))));
        assert!(is_parked(&car("c1", "fix/x", "ready", json!(""))));
    }

    #[test]
    fn a_car_past_review_is_not_parked() {
        assert!(!is_parked(&car("c1", "fix/x", "completed", Value::Null)));
        assert!(!is_parked(&car("c1", "fix/x", "skipped", Value::Null)));
        assert!(!is_parked(&car("c1", "fix/x", "pending", Value::Null)));
    }

    #[test]
    fn a_car_naming_no_branch_is_not_parked() {
        assert!(!is_parked(&car("c1", "", "ready", Value::Null)));
    }

    #[test]
    fn the_parked_car_for_a_branch_is_the_one_still_at_the_dock() {
        let cars = vec![
            car("boarded", "fix/x", "ready", json!("train-9")),
            car("other", "fix/y", "ready", Value::Null),
            car("parked", "fix/x", "ready", Value::Null),
        ];
        let got = parked_car_for(&cars, "fix/x").expect("one car is parked");
        assert_eq!(got["id"], "parked");
        assert!(parked_car_for(&cars, "fix/z").is_none());
    }

    #[test]
    fn the_regate_patch_copies_the_receipt_verbatim_and_clears_the_skip() {
        let p = regate_patch(&receipt(), "why", None);
        // VERBATIM: the receipt string, not a rebuilt object.
        assert_eq!(p["regate_receipt"], json!(GREEN));
        // Present-and-null: the metadata door DELETES a null key, which
        // is how the conductor's "left behind" reason goes away.
        assert!(p.get("skip_reason").is_some_and(Value::is_null));
        assert_eq!(p["regate_note"], json!("why"));
    }

    #[test]
    fn the_regate_patch_carries_the_delivery_channel_when_known() {
        // A re-gate re-classifies and stamps the channel, so a rebased
        // or rerailed car is counted in the yard's mixes like a fresh one.
        let p = regate_patch(&receipt(), "why", Some("config"));
        assert_eq!(p["delivery_channel"], "config");
        // Unknown diff: OMITTED, never nulled — a null key is deleted by
        // the metadata door, which would strip a channel already on the car.
        let q = regate_patch(&receipt(), "why", None);
        assert!(
            q.get("delivery_channel").is_none(),
            "no delivery_channel key when the diff could not be classified"
        );
    }
}

#[cfg(test)]
mod landed_tests {
    use super::*;

    const BRANCH: &str = "fix/boot-never-refuses-over-an-unviable-workflow";

    /// The car that carried the branch on train #259, as the SoR holds it.
    fn landed() -> Value {
        json!({
            "id": "670087f4-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "closed",
            "metadata": {
                "branch": BRANCH, "merged": "true", "merge_ref": "b641f3adcf47",
                "outcome": "merged", "train": "3a476b50-7a3a-409f-a9bc-59266e44f331",
                "boarded_head": "a56b4a9"
            }
        })
    }

    /// The twin auto-park filed for the same branch, abandoned by hand.
    fn abandoned_twin() -> Value {
        json!({
            "id": "dfb98d07-0000-0000-0000-000000000000",
            "kind": "ship-a-change",
            "status": "closed",
            "metadata": { "branch": BRANCH, "outcome": "abandoned", "abandoned": "true" }
        })
    }

    #[test]
    fn a_closed_merged_car_is_the_landing() {
        let cars = vec![abandoned_twin(), landed()];
        let got = landed_car_for(&cars, BRANCH).expect("the merged car answers");
        assert_eq!(got["metadata"]["merge_ref"], "b641f3adcf47");
    }

    #[test]
    fn an_abandoned_car_is_spent_not_landed() {
        assert!(landed_car_for(&[abandoned_twin()], BRANCH).is_none());
    }

    /// The conductor stamps `merged` first and the dispatcher closes
    /// the Job after — the marker alone is a landing.
    #[test]
    fn the_merged_marker_counts_before_the_close() {
        let mut marked = landed();
        marked["status"] = json!("open");
        marked["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("outcome");
        assert!(is_landed(&marked));
        marked["metadata"]["merged"] = json!(true);
        assert!(is_landed(&marked));
        marked["metadata"]["merged"] = json!("false");
        assert!(!is_landed(&marked), "an explicit false is not a landing");
    }

    #[test]
    fn a_parked_car_is_not_a_landing_and_another_branch_does_not_answer() {
        let parked = json!({
            "id": "p", "status": "open",
            "metadata": { "branch": BRANCH },
            "steps": [{"spec_slug": "review", "title": REVIEW, "status": "ready"}]
        });
        assert!(landed_car_for(&[parked], BRANCH).is_none());
        assert!(landed_car_for(&[landed()], "feat/other").is_none());
    }
}

#[cfg(test)]
mod open_car_tests {
    use super::*;

    const BRANCH: &str = "feat/a-human-only-step-refuses-an-agent";

    /// Car d08a6418 as it stood at 20:31:20 UTC on 2026-09-08: gated
    /// green, filed, and BOARDED train #274 twenty-six seconds earlier —
    /// `metadata.train` stamped, its review step still waiting.
    fn boarded() -> Value {
        json!({
            "id": "d08a6418-e7af-484b-82bf-ab043229bfd6",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": {
                "branch": BRANCH,
                "boarded_head": "cd0c4f7bdadf2306a589c922f240a7ff963f70a7",
                "train": "d72ecdb9-c5a1-4d16-8a00-d934fd565002"
            },
            "steps": [
                {"spec_slug": "gate", "title": GATE, "status": "completed"},
                {"spec_slug": "review", "title": REVIEW, "status": "ready"},
            ]
        })
    }

    /// The twin the auto-park handler filed for the same branch 26
    /// seconds later — parked at the dock while the first rode the
    /// train, and abandoned by hand.
    fn twin() -> Value {
        json!({
            "id": "ad54e95c-b48a-4a4c-87f8-7aa8e4e19eff",
            "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": BRANCH },
            "steps": [
                {"spec_slug": "gate", "title": GATE, "status": "completed"},
                {"spec_slug": "review", "title": REVIEW, "status": "ready"},
            ]
        })
    }

    #[test]
    fn a_train_stamp_is_what_makes_a_car_boarded() {
        assert!(is_boarded(&boarded()));
        assert!(!is_boarded(&twin()), "no train stamp is not boarded");
        // A released car (the conductor nulls the stamp when a train is
        // cancelled) is back at the dock, not aboard.
        let mut released = boarded();
        released["metadata"]["train"] = Value::Null;
        assert!(!is_boarded(&released));
        // A closed car is history, whatever it once carried.
        let mut closed = boarded();
        closed["status"] = json!("closed");
        assert!(!is_boarded(&closed));
    }

    /// THE MEASURED CASE (02165b1d). With a car aboard a train the
    /// branch already HAS its car: the boarded one answers, so nothing
    /// files a second.
    #[test]
    fn a_boarded_car_is_the_open_car_for_its_branch() {
        let aboard = [boarded()];
        let got = open_car_for(&aboard, BRANCH).expect("a boarded car is an open car");
        assert_eq!(got["id"], "d08a6418-e7af-484b-82bf-ab043229bfd6");
        // Both shapes present: the boarded one wins, so "is this branch
        // already in transit" answers the same whether or not a twin
        // was already filed.
        let both = [twin(), boarded()];
        let got = open_car_for(&both, BRANCH).expect("a car answers");
        assert_eq!(got["id"], "d08a6418-e7af-484b-82bf-ab043229bfd6");
        assert!(is_boarded(got));
    }

    #[test]
    fn a_parked_car_is_the_open_car_when_nothing_has_boarded() {
        let dock = [twin()];
        let got = open_car_for(&dock, BRANCH).expect("a parked car is an open car");
        assert_eq!(got["id"], "ad54e95c-b48a-4a4c-87f8-7aa8e4e19eff");
        assert!(is_parked(got));
    }

    #[test]
    fn a_branch_with_no_live_car_has_none() {
        assert!(open_car_for(&[], BRANCH).is_none());
        assert!(open_car_for(&[boarded()], "feat/other").is_none());
        // Past review with no train: spent history, not a live car.
        let mut done = twin();
        done["steps"][1]["status"] = json!("completed");
        assert!(open_car_for(&[done], BRANCH).is_none());
        // Closed, however it closed.
        let mut abandoned = twin();
        abandoned["status"] = json!("closed");
        abandoned["metadata"]["abandoned"] = json!("true");
        assert!(open_car_for(&[abandoned], BRANCH).is_none());
    }
}

/// A PARKED CAR STATES ITS ITEM'S ROUTE (backlog ca76d8f9).
#[cfg(test)]
mod park_triage_tests {
    use super::*;

    const BRANCH: &str = "fix/a-parked-car-triages-its-item";
    const CAR_ID: &str = "9442139b-1616-4b8d-8a7a-d1e34ff96486";

    /// A `backlog-item` as `--park-backlog-item` usually finds it:
    /// filed, un-triaged, every route still pending.
    fn untriaged_item() -> Value {
        json!({
            "id": "5942f205-0f0e-4a51-9a31-2f8f3b0b7a11",
            "kind": "backlog-item",
            "status": "open",
            "steps": [
                { "id": "s-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
                { "id": "s-triage", "spec_slug": "triage", "status": "ready",
                  "metadata": { "context_md": "filed by a builder" } },
                { "id": "s-build", "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    #[test]
    fn parking_a_car_against_an_untriaged_item_routes_it_to_build() {
        let w = triage_on_park(&untriaged_item(), CAR_ID, BRANCH)
            .expect("an un-triaged item gets the route its car states");
        assert_eq!(w.step_id, "s-triage");
        assert_eq!(w.body["status"], "completed");
        assert_eq!(w.body["metadata"]["disposition"], DISPOSITION_BUILD);
        let evidence = w.body["metadata"]["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains("9442139b") && evidence.contains(BRANCH),
            "the evidence names the car and its branch: {evidence}"
        );
        // PUT replaces `metadata` wholesale, so what the step already
        // carried has to ride along.
        assert_eq!(w.body["metadata"]["context_md"], "filed by a builder");
    }

    /// The idempotence that makes this safe to run on every re-gate and
    /// every refresh: a triage a person already completed is a decision,
    /// and a park never overwrites it.
    #[test]
    fn an_already_triaged_item_is_left_exactly_alone() {
        let mut done = untriaged_item();
        done["steps"][1]["status"] = json!("completed");
        done["steps"][1]["metadata"] = json!({ "disposition": "verify", "evidence": "by hand" });
        assert!(triage_on_park(&done, CAR_ID, BRANCH).is_none());

        // A disposition written while the step is somehow still open is
        // a decision too.
        let mut decided = untriaged_item();
        decided["steps"][1]["metadata"] = json!({ "disposition": "decline" });
        assert!(triage_on_park(&decided, CAR_ID, BRANCH).is_none());

        // And a route nobody has opened is not one a park may complete.
        let mut pending = untriaged_item();
        pending["steps"][1]["status"] = json!("pending");
        assert!(triage_on_park(&pending, CAR_ID, BRANCH).is_none());
    }

    /// Claimed by a person is still open — the same "open" the arrival
    /// rule's route reads, one definition of it across both halves.
    #[test]
    fn an_active_routing_step_is_still_open() {
        let mut active = untriaged_item();
        active["steps"][1]["status"] = json!("active");
        assert!(triage_on_park(&active, CAR_ID, BRANCH).is_some());
    }

    /// A `user-feedback` packet's triage is the FILER's routing
    /// decision. A car answering one says so in its own evidence; it
    /// does not choose the filer's route for them.
    #[test]
    fn a_filers_own_packet_keeps_its_routing_decision() {
        let mut feedback = untriaged_item();
        feedback["kind"] = json!("user-feedback");
        assert!(triage_on_park(&feedback, CAR_ID, BRANCH).is_none());
    }

    #[test]
    fn a_terminal_or_stepless_item_is_untouched() {
        for terminal in ["closed", "cancelled"] {
            let mut gone = untriaged_item();
            gone["status"] = json!(terminal);
            assert!(
                triage_on_park(&gone, CAR_ID, BRANCH).is_none(),
                "{terminal}"
            );
        }
        let mut stepless = untriaged_item();
        stepless["steps"] = json!([]);
        assert!(triage_on_park(&stepless, CAR_ID, BRANCH).is_none());
        assert!(triage_on_park(&json!({}), CAR_ID, BRANCH).is_none());
    }
}

/// A CAR OPENS WHEN THE BUILD STARTS (backlog be025b44).
///
/// Until now a car's packet was filed by auto-park when the gate went
/// GREEN — the END of the build. Everything before that left no trace:
/// on 2026-09-08 three builder sessions died mid-flight and the only
/// symptom was a twin car appearing on the dock later, filed by a retry
/// loop that outlived its agent; on 2026-09-09 four builders ran for
/// 19–47 minutes each and the dock read empty throughout. The builder
/// now OPENS the car at build start, so a branch has exactly one packet
/// from its first minute and a green FINISHES that packet instead of
/// filing a second.
#[cfg(test)]
mod building_tests {
    use super::*;

    const BRANCH: &str = "feat/a-car-opens-when-the-build-starts";
    const HEAD: &str = "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8";
    const GREEN: &str = r#"{"verdict": "green", "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8", "mode": "full", "fails": []}"#;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    fn receipt() -> Receipt {
        Receipt {
            raw: GREEN.to_string(),
            head: HEAD.to_string(),
            mode: "full".to_string(),
        }
    }

    /// A car as `boss car open` leaves it: filed, its trigger
    /// auto-completed by the POST, `scope` completed with the brief the
    /// builder was handed, `build` CLAIMED (active, assignee = the actor
    /// running the verb), gate and review still ahead of it.
    ///
    /// `subject_branch` is the branch it was FILED under — its Subject —
    /// and `branch` the one it CARRIES. `boss rerail` repoints the second
    /// and leaves the first, which is how three twins got filed on
    /// 2026-09-08; the shared predicates read `metadata.branch`.
    fn building(id: &str, subject_branch: &str, branch: &str) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "subject": {"subject_kind": "custom", "id": subject_branch},
            "metadata": {
                "branch": branch,
                "build_started_at": "2026-09-10T18:00:00Z",
                "built_by": "claude@algedonic.dev",
            },
            "steps": [
                {"id": "s-opened", "spec_slug": "opened", "title": OPENED,
                 "status": "completed"},
                {"id": "s-scope", "spec_slug": "scope", "title": SCOPE,
                 "status": "completed",
                 "metadata": {"summary": "s", "excludes": "e"}},
                {"id": "s-build", "spec_slug": "build", "title": BUILD,
                 "status": "active", "assignee_id": "claude@algedonic.dev"},
                {"id": "s-gate", "spec_slug": "gate", "title": GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": REVIEW, "status": "pending"},
            ],
        })
    }

    /// A car as the POST alone leaves it — nothing completed but the
    /// trigger. The shape auto-park has always filed.
    fn fresh(id: &str, branch: &str) -> Value {
        json!({
            "id": id,
            "kind": "ship-a-change",
            "status": "open",
            "metadata": {"branch": branch},
            "steps": [
                {"id": "s-opened", "spec_slug": "opened", "title": OPENED,
                 "status": "completed"},
                {"id": "s-scope", "spec_slug": "scope", "title": SCOPE, "status": "ready"},
                {"id": "s-build", "spec_slug": "build", "title": BUILD, "status": "pending"},
                {"id": "s-gate", "spec_slug": "gate", "title": GATE, "status": "pending"},
                {"id": "s-review", "spec_slug": "review", "title": REVIEW, "status": "pending"},
            ],
        })
    }

    /// THE THIRD STATE A LIVE CAR CAN BE IN. `is_parked` and
    /// `is_boarded` covered every live car while cars were only ever
    /// filed at green; a car opened at build start is neither.
    #[test]
    fn a_car_opened_at_build_start_is_building_not_parked_and_not_boarded() {
        let c = building("b1", BRANCH, BRANCH);
        assert!(
            is_building(&c),
            "a car whose gate has not reported is building"
        );
        assert!(!is_parked(&c), "its review step is not open yet");
        assert!(!is_boarded(&c), "no train has stamped it");
        assert!(!is_landed(&c));
    }

    /// THE TWINNING FAILURE MODE, CLOSED. "No parked car" was read as
    /// "no car" three times (d052afad, 02165b1d, 6790175e) and each time
    /// a second car was filed. A building car must answer the same
    /// question, or opening cars early would invent a fourth twin.
    #[test]
    fn the_building_car_is_the_branchs_one_live_car() {
        let cars = [building("b1", BRANCH, BRANCH)];
        let got = open_car_for(&cars, BRANCH).expect("a building car is a live car");
        assert_eq!(got["id"], "b1");
        assert_eq!(
            building_car_for(&cars, BRANCH).and_then(|c| c["id"].as_str()),
            Some("b1")
        );
        assert!(building_car_for(&cars, "feat/other").is_none());
    }

    /// A car already at the dock or aboard a train still wins: those
    /// questions were measured first and their answers must not move.
    #[test]
    fn a_parked_or_boarded_car_still_answers_before_a_building_one() {
        let mut parked = building("p", BRANCH, BRANCH);
        parked["steps"][3]["status"] = json!("completed");
        parked["steps"][4]["status"] = json!("ready");
        let cars = [building("b1", BRANCH, BRANCH), parked];
        assert_eq!(open_car_for(&cars, BRANCH).unwrap()["id"], "p");

        let mut boarded = building("a", BRANCH, BRANCH);
        boarded["metadata"]["train"] = json!("train-9");
        let cars = [building("b1", BRANCH, BRANCH), boarded];
        assert_eq!(open_car_for(&cars, BRANCH).unwrap()["id"], "a");
        assert!(
            !is_building(&cars[1]),
            "a boarded car is aboard, whatever its gate step says"
        );
    }

    /// `boss rerail` repoints `metadata.branch` and leaves the Subject
    /// where the car was filed, so the predicate reads the branch the car
    /// CARRIES (backlog 6790175e).
    #[test]
    fn a_rerailed_building_car_answers_on_the_branch_it_carries() {
        let cars = [building("b1", BRANCH, "feat/rerailed")];
        assert_eq!(
            building_car_for(&cars, "feat/rerailed").and_then(|c| c["id"].as_str()),
            Some("b1")
        );
        assert!(
            building_car_for(&cars, BRANCH).is_none(),
            "the filing branch is history once a rerail repoints the car"
        );
    }

    #[test]
    fn a_gated_or_closed_or_unreadable_car_is_not_building() {
        let mut gated = building("b1", BRANCH, BRANCH);
        gated["steps"][3]["status"] = json!("completed");
        assert!(!is_building(&gated), "its gate has reported");

        let mut closed = building("b1", BRANCH, BRANCH);
        closed["status"] = json!("closed");
        assert!(!is_building(&closed));

        let mut nameless = building("b1", BRANCH, BRANCH);
        nameless["metadata"]["branch"] = json!("");
        assert!(!is_building(&nameless));

        // A packet whose gate step this version of boss cannot find is
        // not adopted: an unreadable shape must refuse, not guess.
        let mut stepless = building("b1", BRANCH, BRANCH);
        stepless["steps"] = json!([]);
        assert!(!is_building(&stepless));
    }

    /// WHAT AN OPEN WRITES: the brief, on `scope`, and nothing else. The
    /// build step is taken through the CLAIM door (Ready→Active as a
    /// compare-and-set, which records the claimant and refuses a second
    /// agent), not by a status PUT here.
    #[test]
    fn an_open_records_the_brief_on_scope_and_nothing_else() {
        let writes = open_writes(
            &fresh("f1", BRANCH),
            "does a thing",
            "not that",
            at("2026-09-10T18:00:00Z"),
        )
        .expect("a fresh car can be opened");
        assert_eq!(writes.len(), 1, "one write: scope");
        assert_eq!(writes[0].step_id, "s-scope");
        assert_eq!(writes[0].title, SCOPE);
        assert_eq!(writes[0].body["status"], "completed");
        assert_eq!(writes[0].body["metadata"]["summary"], "does a thing");
        assert_eq!(writes[0].body["metadata"]["excludes"], "not that");
        assert_eq!(
            writes[0].body["metadata"]["completed_at"], "2026-09-10T18:00:00Z",
            "the same stamp format every other writer uses"
        );
    }

    /// IDEMPOTENT: re-running the open on a car whose scope is already
    /// declared writes nothing, so a builder re-invoking the verb does
    /// not hit the 409 a completed step returns for a metadata write.
    #[test]
    fn opening_a_car_whose_scope_is_already_declared_writes_nothing() {
        let writes = open_writes(
            &building("b1", BRANCH, BRANCH),
            "s",
            "e",
            at("2026-09-10T18:00:00Z"),
        )
        .expect("an already-opened car is readable");
        assert!(writes.is_empty(), "scope is already completed: {writes:?}");
    }

    /// WHAT A GREEN OWES AN OPENED CAR: the steps it has NOT already
    /// completed. The step API refuses a metadata write to a completed
    /// step, so a green that re-sent `scope` would 409 on every car a
    /// builder opened.
    #[test]
    fn finishing_an_opened_car_skips_the_scope_the_open_already_completed() {
        let writes = finish_writes(
            &building("b1", BRANCH, BRANCH),
            "s",
            "e",
            "ran the tests",
            "seen working",
            &receipt(),
            at("2026-09-10T18:30:00Z"),
        )
        .expect("an opened car can be finished");
        let titles: Vec<&str> = writes.iter().map(|w| w.title).collect();
        assert_eq!(titles, vec![BUILD, GATE], "scope is already declared");
        assert_eq!(writes[0].step_id, "s-build");
        assert_eq!(writes[0].body["metadata"]["test"], "ran the tests");
        assert_eq!(writes[1].step_id, "s-gate");
        assert_eq!(
            writes[1].body["metadata"]["receipt"], GREEN,
            "the receipt rides verbatim, as it does on a fresh car"
        );
        assert_eq!(writes[1].body["metadata"]["verified"], "seen working");
    }

    /// And the path auto-park has always taken is unchanged: a car it
    /// just POSTed has nothing completed, so all three steps are filled
    /// in the order the workflow runs them.
    #[test]
    fn finishing_a_fresh_car_writes_all_three_steps_in_order() {
        let writes = finish_writes(
            &fresh("f1", BRANCH),
            "s",
            "e",
            "t",
            "v",
            &receipt(),
            at("2026-09-10T18:30:00Z"),
        )
        .expect("a fresh car can be finished");
        let titles: Vec<&str> = writes.iter().map(|w| w.title).collect();
        assert_eq!(titles, vec![SCOPE, BUILD, GATE]);
        assert_eq!(writes[0].step_id, "s-scope");
        for w in &writes {
            assert_eq!(w.body["status"], "completed");
            assert_eq!(
                w.body["metadata"]["completed_at"], "2026-09-10T18:30:00Z",
                "{} was filled without saying when",
                w.title
            );
        }
    }

    /// A SHAPE WE CANNOT READ REFUSES. An in-flight packet is pinned to
    /// the workflow version it was admitted under; if a car's steps do
    /// not answer to the slugs this code knows, filling it by guesswork
    /// would write evidence onto the wrong step.
    #[test]
    fn finishing_refuses_a_car_whose_steps_it_cannot_find() {
        let mut odd = fresh("f1", BRANCH);
        odd["steps"] = json!([{"id": "s-opened", "spec_slug": "opened", "title": OPENED,
                               "status": "completed"}]);
        let e = finish_writes(
            &odd,
            "s",
            "e",
            "t",
            "v",
            &receipt(),
            at("2026-09-10T18:30:00Z"),
        )
        .expect_err("a car with no scope step cannot be finished");
        assert!(
            e.contains("scope"),
            "the refusal names the missing step: {e}"
        );
        let e = open_writes(&odd, "s", "e", at("2026-09-10T18:00:00Z"))
            .expect_err("nor can it be opened");
        assert!(e.contains("scope"), "{e}");
    }

    /// THE MIDDLE THIRD NEEDS TO KNOW WHO AND WHERE. An agent working
    /// for forty minutes left no trace at all; the fact a board renders
    /// is the actor, the host and the worktree, stamped when the build
    /// started.
    #[test]
    fn the_build_start_stamp_names_the_actor_the_host_and_the_worktree() {
        let md = build_start(
            "claude@algedonic.dev",
            "boss-dev-0",
            "/work/boss/.claude/worktrees/agent-a23",
            at("2026-09-10T18:00:00Z"),
        );
        assert_eq!(md["built_by"], "claude@algedonic.dev");
        assert_eq!(md["build_host"], "boss-dev-0");
        assert_eq!(
            md["build_worktree"], "/work/boss/.claude/worktrees/agent-a23",
            "two agents on the same tree is a thing a reader must be able to see"
        );
        assert_eq!(md["build_started_at"], "2026-09-10T18:00:00Z");
        // Absent, never nulled: the metadata door DELETES a null key, so
        // an unknown worktree must not strip one a re-open recorded.
        let thin = build_start("claude@algedonic.dev", "", "", at("2026-09-10T18:00:00Z"));
        assert!(!thin.contains_key("build_host"));
        assert!(!thin.contains_key("build_worktree"));
        assert_eq!(thin["built_by"], "claude@algedonic.dev");
    }

    /// The step id a caller needs for the CLAIM — read off the car, not
    /// assembled from anything.
    #[test]
    fn the_build_step_id_comes_off_the_car() {
        assert_eq!(
            step_id_for(&building("b1", BRANCH, BRANCH), BUILD_SLUG, BUILD).as_deref(),
            Some("s-build")
        );
        assert!(step_id_for(&json!({"steps": []}), BUILD_SLUG, BUILD).is_none());
    }
}
